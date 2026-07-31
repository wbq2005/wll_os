use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use polyhal::VirtAddr;

use crate::mm::memory_set::MemorySet;
use crate::mm::page_table::PTEFlags;
use crate::task::TaskControlBlock;

static TEST_ROOT: AtomicUsize = AtomicUsize::new(0);
static COORDINATOR_CPU: AtomicUsize = AtomicUsize::new(0);
static SEEN_MASK: AtomicUsize = AtomicUsize::new(0);
static TLB_READY_MASK: AtomicUsize = AtomicUsize::new(0);
static DONE_MASK: AtomicUsize = AtomicUsize::new(0);
static RELEASE_TLB_WORKERS: AtomicBool = AtomicBool::new(false);

const HEAP_STRESS_CHUNK_BYTES: usize = 16 * 1024 * 1024;
const HEAP_STRESS_CHUNKS: usize = 10;
const ASID_TEST_VADDR: usize = 0x4000_0000;
const ASID_ISOLATION_ITERATIONS: usize = 64;

fn mapped_address_space(value: usize) -> MemorySet {
    let mut memory = MemorySet::new_bare();
    memory
        .insert_framed_area(
            VirtAddr::new(ASID_TEST_VADDR),
            VirtAddr::new(ASID_TEST_VADDR + crate::config::PAGE_SIZE),
            PTEFlags::R | PTEFlags::W,
        )
        .expect("ASID regression mapping");
    let physical = memory
        .translate(VirtAddr::new(ASID_TEST_VADDR))
        .expect("ASID regression translation");
    let physical_ptr = crate::drivers::hal::phys_to_virt_ram(physical.raw()) as *mut usize;
    unsafe { core::ptr::write_volatile(physical_ptr, value) };
    memory
}

fn observe_address_space(memory: &MemorySet) -> (usize, usize, usize) {
    let interrupts_were_enabled = crate::trap::interrupts::is_interrupt_enabled();
    crate::trap::interrupts::disable_interrupt();
    memory.activate();
    let active_asid = polyhal::pagetable::PageTable::current_asid();
    let active_root = polyhal::pagetable::PageTable::current().root().raw();
    #[cfg(target_arch = "riscv64")]
    let value = unsafe { core::ptr::read_volatile(ASID_TEST_VADDR as *const usize) };
    // PLV0 low addresses use DMW2 on LoongArch and bypass PGDL, so the
    // architecture-neutral kernel regression validates the root/ASID pair.
    // Real PLV3 translations remain covered by the user and official runs.
    #[cfg(target_arch = "loongarch64")]
    let value = 0;
    crate::trap::restore_kernel_page_table();
    if interrupts_were_enabled {
        crate::trap::interrupts::enable_interrupt();
    }
    (value, active_asid, active_root)
}

fn verify_asid_isolation_and_reuse() -> (usize, usize, usize) {
    const LEFT_VALUE: usize = 0x1357_9bdf;
    const RIGHT_VALUE: usize = 0x2468_ace0;
    const REUSED_VALUE: usize = 0x55aa_33cc;

    let left = mapped_address_space(LEFT_VALUE);
    let right = mapped_address_space(RIGHT_VALUE);
    let left_asid = left.address_space_id();
    let right_asid = right.address_space_id();
    if left_asid == 0 || right_asid == 0 || left_asid == right_asid {
        panic!(
            "[smp-regression] fail phase=asid-allocation left={} right={}",
            left_asid, right_asid
        );
    }

    for iteration in 0..ASID_ISOLATION_ITERATIONS {
        let (left_observed, left_active_asid, left_active_root) = observe_address_space(&left);
        let (right_observed, right_active_asid, right_active_root) = observe_address_space(&right);
        #[cfg(target_arch = "riscv64")]
        let translations_match = left_observed == LEFT_VALUE && right_observed == RIGHT_VALUE;
        #[cfg(target_arch = "loongarch64")]
        let translations_match = true;
        if !translations_match
            || left_active_asid != left_asid
            || right_active_asid != right_asid
            || left_active_root != left.address_space_root()
            || right_active_root != right.address_space_root()
        {
            panic!(
                "[smp-regression] fail phase=asid-isolation iteration={} left={:#x}/{}/{:#x} right={:#x}/{}/{:#x}",
                iteration,
                left_observed,
                left_active_asid,
                left_active_root,
                right_observed,
                right_active_asid,
                right_active_root
            );
        }
    }

    // Keep the old data frame alive so a stale translation cannot pass by
    // accidentally pointing at a frame immediately recycled by the allocator.
    let old_frame = left.areas[0].frames[0].clone();
    drop(left);

    let mut reused = None;
    for _ in 0..64 {
        let candidate = mapped_address_space(REUSED_VALUE);
        if candidate.address_space_id() == left_asid {
            reused = Some(candidate);
            break;
        }
        drop(candidate);
    }
    let reused = reused.expect("retired ASID was not recycled");
    let (reused_observed, reused_active_asid, reused_active_root) = observe_address_space(&reused);
    #[cfg(target_arch = "riscv64")]
    let reused_translation_matches = reused_observed == REUSED_VALUE;
    #[cfg(target_arch = "loongarch64")]
    let reused_translation_matches = true;
    if !reused_translation_matches
        || reused_active_asid != left_asid
        || reused_active_root != reused.address_space_root()
    {
        panic!("[smp-regression] fail phase=asid-reuse");
    }
    drop(old_frame);
    (left_asid, right_asid, reused.address_space_id())
}

fn stress_kernel_heap() -> usize {
    let mut checksum = 0usize;
    let mut chunks = Vec::with_capacity(HEAP_STRESS_CHUNKS);
    for index in 0..HEAP_STRESS_CHUNKS {
        let chunk = alloc::vec![index as u8; HEAP_STRESS_CHUNK_BYTES];
        checksum = checksum.wrapping_add(chunk[0] as usize);
        checksum = checksum.wrapping_add(chunk[HEAP_STRESS_CHUNK_BYTES - 1] as usize);
        chunks.push(chunk);
    }
    drop(chunks);
    checksum
}

fn worker() -> ! {
    let cpu = crate::platform::current_cpu_index();
    let bit = 1usize << cpu;
    SEEN_MASK.fetch_or(bit, Ordering::AcqRel);

    if cpu != COORDINATOR_CPU.load(Ordering::Acquire) {
        crate::platform::mark_current_address_space(TEST_ROOT.load(Ordering::Acquire));
        TLB_READY_MASK.fetch_or(bit, Ordering::AcqRel);
        while !RELEASE_TLB_WORKERS.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
        crate::platform::clear_current_address_space();
    }

    DONE_MASK.fetch_or(bit, Ordering::AcqRel);
    crate::task::exit_current_and_run_next(0);
    loop {
        core::hint::spin_loop();
    }
}

fn wait_for(mask: &AtomicUsize, expected: usize, phase: &str) {
    let deadline = crate::timer::get_time_us().saturating_add(5_000_000);
    while mask.load(Ordering::Acquire) != expected {
        if crate::timer::get_time_us() >= deadline {
            panic!(
                "[smp-regression] fail phase={} expected={:#x} observed={:#x}",
                phase,
                expected,
                mask.load(Ordering::Acquire)
            );
        }
        core::hint::spin_loop();
    }
}

pub fn run() {
    let heap_checksum = stress_kernel_heap();
    let (left_asid, right_asid, reused_asid) = verify_asid_isolation_and_reuse();
    #[cfg(target_arch = "riscv64")]
    let asid_check = "translation";
    #[cfg(target_arch = "loongarch64")]
    let asid_check = "root-csr";
    let expected = crate::platform::online_cpu_mask();
    let cpu_count = expected.count_ones() as usize;
    let coordinator_cpu = crate::platform::current_cpu_index();
    let coordinator_bit = 1usize << coordinator_cpu;
    if cpu_count < 2 || expected & coordinator_bit == 0 {
        panic!(
            "[smp-regression] fail phase=online expected_multi_cpu mask={:#x}",
            expected
        );
    }

    let root = crate::mm::page_table::kernel_page_table()
        .lock()
        .as_ref()
        .expect("kernel page table")
        .root()
        .raw();
    TEST_ROOT.store(root, Ordering::Release);
    COORDINATOR_CPU.store(coordinator_cpu, Ordering::Release);
    SEEN_MASK.store(0, Ordering::Release);
    TLB_READY_MASK.store(0, Ordering::Release);
    DONE_MASK.store(0, Ordering::Release);
    RELEASE_TLB_WORKERS.store(false, Ordering::Release);

    for cpu in 0..crate::config::MAX_CPUS {
        let bit = 1usize << cpu;
        if expected & bit == 0 {
            continue;
        }
        let task = TaskControlBlock::new_kernel_task(worker);
        task.set_affinity_mask(bit);
        crate::task::manager::add_task(task);
    }

    while SEEN_MASK.load(Ordering::Acquire) & coordinator_bit == 0 {
        if !crate::task::drain_kernel_ready_once() {
            core::hint::spin_loop();
        }
    }
    wait_for(&SEEN_MASK, expected, "dispatch");

    let remote_mask = expected & !coordinator_bit;
    wait_for(&TLB_READY_MASK, remote_mask, "tlb-ready");
    crate::platform::mark_current_address_space(root);
    crate::platform::tlb_shootdown(root);
    crate::platform::clear_current_address_space();

    RELEASE_TLB_WORKERS.store(true, Ordering::Release);
    wait_for(&DONE_MASK, expected, "completion");

    crate::println!(
        "[smp-regression] pass cpus={} mask={:#x} dispatch={:#x} tlb_targets={:#x} asid_pair={}/{} reused_asid={} asid_check={} isolation_iters={} heap_stress_mib={} checksum={}",
        cpu_count,
        expected,
        SEEN_MASK.load(Ordering::Acquire),
        remote_mask,
        left_asid,
        right_asid,
        reused_asid,
        asid_check,
        ASID_ISOLATION_ITERATIONS,
        HEAP_STRESS_CHUNKS * HEAP_STRESS_CHUNK_BYTES / 1024 / 1024,
        heap_checksum,
    );
}
