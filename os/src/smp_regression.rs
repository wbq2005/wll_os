use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use polyhal::VirtAddr;

use crate::mm::frame_allocator;
use crate::mm::map_area::{MapArea, MapAreaBacking, ResidentSet};
use crate::mm::memory_set::MemorySet;
use crate::mm::page_table::PTEFlags;
use crate::task::TaskControlBlock;

static TEST_ROOT: AtomicUsize = AtomicUsize::new(0);
static COORDINATOR_CPU: AtomicUsize = AtomicUsize::new(0);
static SEEN_MASK: AtomicUsize = AtomicUsize::new(0);
static TLB_READY_MASK: AtomicUsize = AtomicUsize::new(0);
static DONE_MASK: AtomicUsize = AtomicUsize::new(0);
static RELEASE_TLB_WORKERS: AtomicBool = AtomicBool::new(false);
// The lifecycle probe records only the first terminal user trap.  It is
// intentionally feature-local: it lets the independent regression expose its
// failure boundary without adding a per-fault production log path.
static LIFECYCLE_TERMINAL_RECORDED: AtomicBool = AtomicBool::new(false);
static LIFECYCLE_TERMINAL_KIND: AtomicUsize = AtomicUsize::new(0);
static LIFECYCLE_TERMINAL_VADDR: AtomicUsize = AtomicUsize::new(0);
static LIFECYCLE_TERMINAL_SEPC: AtomicUsize = AtomicUsize::new(0);
static LIFECYCLE_TASK_DONE: AtomicBool = AtomicBool::new(false);
static NAMESPACE_READY_MASK: AtomicUsize = AtomicUsize::new(0);
static NAMESPACE_DONE_MASK: AtomicUsize = AtomicUsize::new(0);
static NAMESPACE_ACK_MASK: AtomicUsize = AtomicUsize::new(0);
static NAMESPACE_EPOCH: AtomicUsize = AtomicUsize::new(0);
static NAMESPACE_STOP: AtomicBool = AtomicBool::new(false);

const HEAP_STRESS_CHUNK_BYTES: usize = 16 * 1024 * 1024;
const HEAP_STRESS_CHUNKS: usize = 10;
const ASID_TEST_VADDR: usize = 0x4000_0000;
const ASID_ISOLATION_ITERATIONS: usize = 64;
const RESIDENT_TEST_VADDR: usize = 0x4100_0000;
const SHARED_LEFT_VADDR: usize = 0x4200_0000;
const SHARED_RIGHT_VADDR: usize = 0x4300_0000;
const HIGH_ARENA_TEST_VADDR: usize = crate::config::user_va::HIGH_MMAP_ARENA.start + 0x20_0000;
const NAMESPACE_ROOT: &str = "/tmp/.wll_namespace_lifecycle";
const NAMESPACE_LEFT: &str = "/tmp/.wll_namespace_lifecycle/left";
const NAMESPACE_RIGHT: &str = "/tmp/.wll_namespace_lifecycle/right";
const NAMESPACE_PULSE: &str = "/tmp/.wll_namespace_lifecycle/pulse";
const NAMESPACE_ITERATIONS: usize = 64;

/// Exercise sparse ResidentSet ownership and topology-transfer APIs
/// independently of the contest workload.
fn verify_resident_set_api() {
    let mut pages = ResidentSet::new();
    let mut initial = Vec::new();
    for _ in 0..2 {
        initial.push(frame_allocator::alloc_frame().expect("resident API frame allocation"));
    }
    if pages.insert_run(0, initial).is_err() {
        panic!("[smp-regression] fail phase=resident-set-api initial-run");
    }
    if pages
        .insert(
            4,
            frame_allocator::alloc_frame().expect("resident API sparse allocation"),
        )
        .is_err()
    {
        panic!("[smp-regression] fail phase=resident-set-api sparse-insert");
    }
    if pages.len() != 3
        || pages.lookup(0).is_none()
        || pages.lookup(3).is_some()
        || pages.iter_range(1, 5).count() != 2
    {
        panic!("[smp-regression] fail phase=resident-set-api lookup");
    }

    let suffix = pages
        .extract_range(1, 5)
        .expect("resident API sparse extraction");
    if suffix.len() != 2
        || pages.len() != 1
        || suffix.lookup(0).is_none()
        || suffix.lookup(3).is_none()
    {
        panic!("[smp-regression] fail phase=resident-set-api extraction");
    }

    let mut left = ResidentSet::new();
    if left
        .insert(
            0,
            frame_allocator::alloc_frame().expect("resident API merge left allocation"),
        )
        .is_err()
    {
        panic!("[smp-regression] fail phase=resident-set-api merge-left");
    }
    let mut right = ResidentSet::new();
    if right
        .insert(
            0,
            frame_allocator::alloc_frame().expect("resident API merge right allocation"),
        )
        .is_err()
    {
        panic!("[smp-regression] fail phase=resident-set-api merge-right");
    }
    left.merge_from_at(2, &mut right);
    if left.lookup(2).is_none() || !right.is_empty() {
        panic!("[smp-regression] fail phase=resident-set-api merge");
    }
    let split = left.split_off(2).expect("resident API split");
    if split.lookup(0).is_none() || left.lookup(2).is_some() {
        panic!("[smp-regression] fail phase=resident-set-api split");
    }
    let mut split = split;
    left.merge_from_at(2, &mut split);
    if left.lookup(2).is_none() || !split.is_empty() {
        panic!("[smp-regression] fail phase=resident-set-api merge-after-split");
    }
    let drained = pages.drain_all();
    if !pages.is_empty() || drained.len() != 1 {
        panic!("[smp-regression] fail phase=resident-set-api drain");
    }
    drop(drained);
    drop(suffix);

    // Sparse VMA topology must not depend on where the last resident page
    // happens to be.  A split in a trailing hole yields an empty right owner;
    // merging it later must restore the right page at its original VPN.
    let page_size = crate::config::PAGE_SIZE;
    let base = VirtAddr::new(RESIDENT_TEST_VADDR);
    let mut area = MapArea::new(
        base,
        VirtAddr::new(RESIDENT_TEST_VADDR + 8 * page_size),
        PTEFlags::R,
    );
    if area
        .append_resident(frame_allocator::alloc_frame().expect("resident API VMA left allocation"))
        .is_err()
    {
        panic!("[smp-regression] fail phase=resident-set-api VMA-left-insert");
    }
    let mut right = area
        .split_at(VirtAddr::new(RESIDENT_TEST_VADDR + 4 * page_size))
        .expect("resident API sparse VMA trailing-hole split");
    if right.has_frames() {
        panic!("[smp-regression] fail phase=resident-set-api trailing-hole-split");
    }
    if right
        .append_resident(frame_allocator::alloc_frame().expect("resident API VMA right allocation"))
        .is_err()
    {
        panic!("[smp-regression] fail phase=resident-set-api VMA-right-insert");
    }
    area.merge_with(right);
    if area.resident().lookup(4).is_none() || area.resident().lookup(8).is_some() {
        panic!("[smp-regression] fail phase=resident-set-api sparse-vma-merge");
    }
}

/// Verify the bridge between sparse ResidentSet storage
/// and user-memory lifecycle operations.  No official command, path, marker,
/// or expected contest output participates in this regression.
fn verify_resident_memory_lifecycle() {
    let page = crate::config::PAGE_SIZE;
    let flags = PTEFlags::U | PTEFlags::R | PTEFlags::W | PTEFlags::V;

    let mut parent = MemorySet::new_bare();
    parent
        .insert_lazy_area_with_backing(
            VirtAddr::new(RESIDENT_TEST_VADDR),
            VirtAddr::new(RESIDENT_TEST_VADDR + 4 * page),
            flags,
            MapAreaBacking::Anonymous,
        )
        .expect("resident lifecycle lazy area");
    parent
        .handle_page_fault(RESIDENT_TEST_VADDR + page, true, false)
        .expect("resident lifecycle anonymous fault");
    if parent
        .translate(VirtAddr::new(RESIDENT_TEST_VADDR + page))
        .is_none()
    {
        panic!("[smp-regression] fail phase=resident-memory anonymous-fault");
    }

    let mut child = parent.fork_cow().expect("resident lifecycle fork COW");
    let parent_page = parent
        .translate(VirtAddr::new(RESIDENT_TEST_VADDR + page))
        .expect("resident lifecycle parent mapping");
    child
        .handle_page_fault(RESIDENT_TEST_VADDR + 3 * page, true, false)
        .expect("resident lifecycle COW hole first store");
    if child
        .translate(VirtAddr::new(RESIDENT_TEST_VADDR + 3 * page))
        .is_none()
    {
        panic!("[smp-regression] fail phase=resident-memory cow-hole");
    }
    child
        .handle_page_fault(RESIDENT_TEST_VADDR + page, true, false)
        .expect("resident lifecycle COW write");
    let child_page = child
        .translate(VirtAddr::new(RESIDENT_TEST_VADDR + page))
        .expect("resident lifecycle child mapping");
    if parent_page == child_page {
        panic!("[smp-regression] fail phase=resident-memory cow-isolation");
    }
    parent
        .handle_page_fault(RESIDENT_TEST_VADDR + page, true, false)
        .expect("resident lifecycle parent COW write");
    if !parent.regression_mapping_is_writable(VirtAddr::new(RESIDENT_TEST_VADDR + page)) {
        panic!("[smp-regression] fail phase=resident-memory private-leaf-write");
    }
    let nested_child = parent
        .fork_cow()
        .expect("resident lifecycle nested fork COW");
    if parent.regression_mapping_is_writable(VirtAddr::new(RESIDENT_TEST_VADDR + page))
        || nested_child.regression_mapping_is_writable(VirtAddr::new(RESIDENT_TEST_VADDR + page))
    {
        panic!("[smp-regression] fail phase=resident-memory nested-fork-readonly");
    }
    let nested_child_page = nested_child
        .translate(VirtAddr::new(RESIDENT_TEST_VADDR + page))
        .expect("resident lifecycle nested child mapping");
    parent
        .handle_page_fault(RESIDENT_TEST_VADDR + page, true, false)
        .expect("resident lifecycle nested parent COW write");
    if parent
        .translate(VirtAddr::new(RESIDENT_TEST_VADDR + page))
        .expect("resident lifecycle nested parent mapping")
        == nested_child_page
    {
        panic!("[smp-regression] fail phase=resident-memory nested-cow-isolation");
    }
    child
        .protect_range(
            VirtAddr::new(RESIDENT_TEST_VADDR + page),
            VirtAddr::new(RESIDENT_TEST_VADDR + 3 * page),
            flags,
        )
        .expect("resident lifecycle cross-range mprotect");
    child
        .unmap_range(
            VirtAddr::new(RESIDENT_TEST_VADDR + page),
            VirtAddr::new(RESIDENT_TEST_VADDR + 2 * page),
        )
        .expect("resident lifecycle partial munmap");
    if child
        .translate(VirtAddr::new(RESIDENT_TEST_VADDR + page))
        .is_some()
    {
        panic!("[smp-regression] fail phase=resident-memory partial-munmap");
    }
    parent.release_user_areas();
    if !parent.areas.is_empty() {
        panic!("[smp-regression] fail phase=resident-memory release");
    }
    drop(child);
    drop(nested_child);
    drop(parent);

    let shared_frames =
        alloc::vec![frame_allocator::alloc_frame().expect("shared frame allocation")];
    let mut left = MemorySet::new_bare();
    let mut right = MemorySet::new_bare();
    left.insert_shared_framed_area(
        VirtAddr::new(SHARED_LEFT_VADDR),
        VirtAddr::new(SHARED_LEFT_VADDR + page),
        flags,
        MapAreaBacking::SharedMemory {
            shmid: 1,
            base: SHARED_LEFT_VADDR,
            offset: 0,
        },
        &shared_frames,
    )
    .expect("resident lifecycle shared left");
    right
        .insert_shared_framed_area(
            VirtAddr::new(SHARED_RIGHT_VADDR),
            VirtAddr::new(SHARED_RIGHT_VADDR + page),
            flags,
            MapAreaBacking::SharedMemory {
                shmid: 1,
                base: SHARED_RIGHT_VADDR,
                offset: 0,
            },
            &shared_frames,
        )
        .expect("resident lifecycle shared right");
    let left_page = left
        .translate(VirtAddr::new(SHARED_LEFT_VADDR))
        .expect("resident lifecycle shared left mapping");
    let right_page = right
        .translate(VirtAddr::new(SHARED_RIGHT_VADDR))
        .expect("resident lifecycle shared right mapping");
    if left_page != right_page {
        panic!("[smp-regression] fail phase=resident-memory shared-identity");
    }
    left.unmap_range(
        VirtAddr::new(SHARED_LEFT_VADDR),
        VirtAddr::new(SHARED_LEFT_VADDR + page),
    )
    .expect("resident lifecycle shared unmap");
    if right.translate(VirtAddr::new(SHARED_RIGHT_VADDR)).is_none() {
        panic!("[smp-regression] fail phase=resident-memory shared-lifetime");
    }
    drop(left);
    drop(right);
    drop(shared_frames);

    let shared_memory = crate::task::new_shared_memory_set(MemorySet::new_bare());
    let clone_vm_view = shared_memory.clone();
    if !alloc::sync::Arc::ptr_eq(&shared_memory, &clone_vm_view) {
        panic!("[smp-regression] fail phase=resident-memory clone-vm-sharing");
    }
    drop(clone_vm_view);
    drop(shared_memory);

    crate::println!("[smp-regression] pass phase=resident-memory-lifecycle");
}

fn verify_high_arena_memory_lifecycle() {
    let page = crate::config::PAGE_SIZE;
    let flags = PTEFlags::U | PTEFlags::R | PTEFlags::W | PTEFlags::V;
    let start = HIGH_ARENA_TEST_VADDR;
    let mut parent = MemorySet::new_bare();

    parent
        .insert_lazy_area_with_backing(
            VirtAddr::new(start),
            VirtAddr::new(start + 4 * page),
            flags,
            MapAreaBacking::Anonymous,
        )
        .expect("high arena lazy area");
    if parent.translate(VirtAddr::new(start + page)).is_some() {
        panic!("[smp-regression] fail phase=high-arena eager-translation");
    }
    parent
        .handle_page_fault(start + page, true, false)
        .expect("high arena anonymous fault");
    let parent_page = parent
        .translate(VirtAddr::new(start + page))
        .expect("high arena parent mapping");

    // Software translation checks cannot detect an over-broad root ownership
    // rule.  Activate this address space and touch the high VA through the
    // hardware translation path before restoring the kernel root.
    let interrupts_were_enabled = crate::trap::interrupts::is_interrupt_enabled();
    crate::trap::interrupts::disable_interrupt();
    parent.activate();
    #[cfg(target_arch = "riscv64")]
    {
        let sum_was_enabled = riscv::register::sstatus::read().sum();
        if !sum_was_enabled {
            unsafe { riscv::register::sstatus::set_sum() };
        }
        let high_ptr = (start + page) as *mut usize;
        unsafe {
            core::ptr::write_volatile(high_ptr, 0xa5a5_5a5a_1122_3344usize);
            if core::ptr::read_volatile(high_ptr) != 0xa5a5_5a5a_1122_3344usize {
                panic!("[smp-regression] fail phase=high-arena-hardware-translation");
            }
        }
        if !sum_was_enabled {
            unsafe { riscv::register::sstatus::clear_sum() };
        }
    }
    #[cfg(target_arch = "loongarch64")]
    {
        // LoongArch PLV0 cannot directly dereference a PLV3 leaf.  Validate
        // the same resident frame through its cached DMW1 alias instead.
        let high_ptr = crate::drivers::hal::phys_to_virt_ram(parent_page.raw()) as *mut usize;
        unsafe {
            core::ptr::write_volatile(high_ptr, 0xa5a5_5a5a_1122_3344usize);
            if core::ptr::read_volatile(high_ptr) != 0xa5a5_5a5a_1122_3344usize {
                panic!("[smp-regression] fail phase=high-arena-frame-alias");
            }
        }
    }
    crate::trap::restore_kernel_page_table();
    if interrupts_were_enabled {
        crate::trap::interrupts::enable_interrupt();
    }

    let mut child = parent.fork_cow().expect("high arena fork COW");
    child
        .handle_page_fault(start + page, true, false)
        .expect("high arena child COW write");
    if child
        .translate(VirtAddr::new(start + page))
        .expect("high arena child mapping")
        == parent_page
    {
        panic!("[smp-regression] fail phase=high-arena cow-isolation");
    }
    child
        .protect_range(
            VirtAddr::new(start + page),
            VirtAddr::new(start + 3 * page),
            flags,
        )
        .expect("high arena partial mprotect");
    child
        .unmap_range(VirtAddr::new(start + page), VirtAddr::new(start + 2 * page))
        .expect("high arena partial munmap");
    if child.translate(VirtAddr::new(start + page)).is_some()
        || child.translate(VirtAddr::new(start + 2 * page)).is_none()
    {
        panic!("[smp-regression] fail phase=high-arena partial-range");
    }
    drop(child);
    drop(parent);

    let recreated = MemorySet::new_bare();
    if recreated.translate(VirtAddr::new(start + page)).is_some() {
        panic!("[smp-regression] fail phase=high-arena stale-root");
    }
    crate::println!("[smp-regression] pass phase=high-arena-memory-lifecycle");
}

pub(crate) fn reset_user_memory_lifecycle_diagnostic() {
    LIFECYCLE_TERMINAL_KIND.store(0, Ordering::Relaxed);
    LIFECYCLE_TERMINAL_VADDR.store(0, Ordering::Relaxed);
    LIFECYCLE_TERMINAL_SEPC.store(0, Ordering::Relaxed);
    LIFECYCLE_TERMINAL_RECORDED.store(false, Ordering::Release);
}

pub(crate) fn note_user_memory_lifecycle_terminal_trap(kind: usize, vaddr: usize, sepc: usize) {
    if LIFECYCLE_TERMINAL_RECORDED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        LIFECYCLE_TERMINAL_KIND.store(kind, Ordering::Release);
        LIFECYCLE_TERMINAL_VADDR.store(vaddr, Ordering::Release);
        LIFECYCLE_TERMINAL_SEPC.store(sepc, Ordering::Release);
    }
}

pub(crate) fn user_memory_lifecycle_terminal_trap() -> Option<(usize, usize, usize)> {
    if !LIFECYCLE_TERMINAL_RECORDED.load(Ordering::Acquire) {
        return None;
    }
    Some((
        LIFECYCLE_TERMINAL_KIND.load(Ordering::Acquire),
        LIFECYCLE_TERMINAL_VADDR.load(Ordering::Acquire),
        LIFECYCLE_TERMINAL_SEPC.load(Ordering::Acquire),
    ))
}

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
    let old_frame = left.areas[0]
        .resident()
        .lookup(0)
        .expect("ASID regression mapping must own its frame")
        .clone();
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

/// Run the user-lifecycle probe from a real kernel task.  The production
/// harness launches user programs from a kernel task too; running it directly
/// on the boot stack leaves no current task or orphan reaper, making the
/// regression exercise a different and unstable lifecycle boundary.
fn user_memory_lifecycle_driver() -> ! {
    crate::task::harness::run_user_memory_lifecycle_regression();
    LIFECYCLE_TASK_DONE.store(true, Ordering::Release);
    crate::task::exit_current_and_run_next(0);
    loop {
        core::hint::spin_loop();
    }
}

fn run_user_memory_lifecycle_regression() {
    LIFECYCLE_TASK_DONE.store(false, Ordering::Release);
    let driver = TaskControlBlock::new_kernel_task(user_memory_lifecycle_driver);
    crate::task::manager::add_task(driver.clone());

    // Completion is published before the driver unwinds through the kernel
    // scheduler.  Do not start the following SMP phase until that task has
    // also released its CPU and reached Zombie state.
    while !LIFECYCLE_TASK_DONE.load(Ordering::Acquire)
        || driver.status() != crate::task::TaskStatus::Zombie
    {
        if !crate::task::drain_kernel_ready_once() {
            core::hint::spin_loop();
        }
    }
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

fn namespace_reader_worker() -> ! {
    let bit = 1usize << crate::platform::current_cpu_index();
    NAMESPACE_READY_MASK.fetch_or(bit, Ordering::AcqRel);
    let mut last_epoch = 0usize;

    while !NAMESPACE_STOP.load(Ordering::Acquire) {
        let before = NAMESPACE_EPOCH.load(Ordering::Acquire);
        let exists = crate::fs::ext4_vol::lookup_kind(NAMESPACE_PULSE).is_some();
        let after = NAMESPACE_EPOCH.load(Ordering::Acquire);
        if before != 0 && before == after && before != last_epoch {
            let expected_exists = before & 1 == 1;
            if exists != expected_exists {
                panic!(
                    "[smp-regression] fail phase=namespace-cache epoch={} expected_exists={} observed_exists={}",
                    before, expected_exists, exists
                );
            }
            NAMESPACE_ACK_MASK.fetch_or(bit, Ordering::AcqRel);
            last_epoch = before;
        }
    }

    NAMESPACE_DONE_MASK.fetch_or(bit, Ordering::AcqRel);
    crate::task::exit_current_and_run_next(0);
    loop {
        core::hint::spin_loop();
    }
}

fn verify_namespace_lifecycle(expected: usize, coordinator_bit: usize) {
    use crate::fs::ext4_vol;

    if ext4_vol::lookup_kind(NAMESPACE_ROOT).is_some() {
        panic!("[smp-regression] fail phase=namespace-private-root-exists");
    }
    ext4_vol::mkdir_ext4(NAMESPACE_ROOT).expect("namespace regression root");
    ext4_vol::mkdir_ext4(NAMESPACE_LEFT).expect("namespace regression left directory");
    ext4_vol::mkdir_ext4(NAMESPACE_RIGHT).expect("namespace regression right directory");

    let reader_mask = expected & !coordinator_bit;
    NAMESPACE_READY_MASK.store(0, Ordering::Release);
    NAMESPACE_DONE_MASK.store(0, Ordering::Release);
    NAMESPACE_ACK_MASK.store(0, Ordering::Release);
    NAMESPACE_EPOCH.store(0, Ordering::Release);
    NAMESPACE_STOP.store(false, Ordering::Release);
    for cpu in 0..crate::config::MAX_CPUS {
        let bit = 1usize << cpu;
        if reader_mask & bit == 0 {
            continue;
        }
        let task = TaskControlBlock::new_kernel_task(namespace_reader_worker);
        task.set_affinity_mask(bit);
        crate::task::manager::add_task(task);
    }
    wait_for(
        &NAMESPACE_READY_MASK,
        reader_mask,
        "namespace-readers-ready",
    );

    for iteration in 0..NAMESPACE_ITERATIONS {
        NAMESPACE_ACK_MASK.store(0, Ordering::Release);
        ext4_vol::create_regular_ext4(NAMESPACE_PULSE).expect("namespace regression create pulse");
        NAMESPACE_EPOCH.store(iteration * 2 + 1, Ordering::Release);
        wait_for(
            &NAMESPACE_ACK_MASK,
            reader_mask,
            "namespace-positive-visible",
        );

        NAMESPACE_ACK_MASK.store(0, Ordering::Release);
        ext4_vol::unlink_regular_file(NAMESPACE_PULSE).expect("namespace regression unlink pulse");
        NAMESPACE_EPOCH.store(iteration * 2 + 2, Ordering::Release);
        wait_for(
            &NAMESPACE_ACK_MASK,
            reader_mask,
            "namespace-negative-visible",
        );
    }

    NAMESPACE_STOP.store(true, Ordering::Release);
    wait_for(&NAMESPACE_DONE_MASK, reader_mask, "namespace-readers-done");

    const LEFT_ITEM: &str = "/tmp/.wll_namespace_lifecycle/left/item";
    const LEFT_RENAMED: &str = "/tmp/.wll_namespace_lifecycle/left/renamed";
    const RIGHT_ITEM: &str = "/tmp/.wll_namespace_lifecycle/right/item";
    let ino = ext4_vol::create_regular_ext4(LEFT_ITEM).expect("namespace regression item");
    ext4_vol::rename_ext4(LEFT_ITEM, LEFT_RENAMED, false)
        .expect("namespace regression same-parent rename");
    if ext4_vol::lookup_kind(LEFT_ITEM).is_some() || ext4_vol::lookup_kind(LEFT_RENAMED).is_none() {
        panic!("[smp-regression] fail phase=namespace-same-parent-rename");
    }
    ext4_vol::rename_ext4(LEFT_RENAMED, RIGHT_ITEM, false)
        .expect("namespace regression cross-parent rename");
    if ext4_vol::lookup_kind(LEFT_RENAMED).is_some() || ext4_vol::lookup_kind(RIGHT_ITEM).is_none()
    {
        panic!("[smp-regression] fail phase=namespace-cross-parent-rename");
    }

    let payload = b"open-unlink-lifetime";
    ext4_vol::open_regular_ino(ino);
    let written = ext4_vol::ext4_write_at(ino, 0, payload).expect("namespace regression write");
    if written != payload.len() {
        panic!("[smp-regression] fail phase=namespace-open-unlink-write");
    }
    ext4_vol::unlink_regular_file(RIGHT_ITEM).expect("namespace regression open unlink");
    if ext4_vol::lookup_kind(RIGHT_ITEM).is_some() {
        panic!("[smp-regression] fail phase=namespace-open-unlink-path");
    }
    let mut observed = [0u8; 20];
    let read = ext4_vol::ext4_read_at(ino, 0, &mut observed).expect("namespace regression read");
    if read != payload.len() || &observed[..read] != payload {
        panic!("[smp-regression] fail phase=namespace-open-unlink-data");
    }
    ext4_vol::close_regular_ino(ino);

    ext4_vol::remove_empty_dir_ext4(NAMESPACE_LEFT).expect("namespace regression remove left");
    ext4_vol::remove_empty_dir_ext4(NAMESPACE_RIGHT).expect("namespace regression remove right");
    ext4_vol::remove_empty_dir_ext4(NAMESPACE_ROOT).expect("namespace regression remove root");
    crate::println!(
        "[smp-regression] pass phase=namespace-lifecycle iterations={} readers={}",
        NAMESPACE_ITERATIONS,
        reader_mask.count_ones()
    );
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
    crate::syscall::other::verify_interval_timer_state_machine();
    let heap_checksum = stress_kernel_heap();
    let (left_asid, right_asid, reused_asid) = verify_asid_isolation_and_reuse();
    verify_resident_set_api();
    verify_resident_memory_lifecycle();
    if crate::config::user_va::HAS_HIGH_MMAP_ARENA {
        verify_high_arena_memory_lifecycle();
    }
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
    verify_namespace_lifecycle(expected, coordinator_bit);

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

    // Keep the existing ASID/SMP transport sequence intact.  The independent
    // user lifecycle probe runs only after every worker has completed.
    run_user_memory_lifecycle_regression();

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
