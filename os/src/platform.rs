//! Runtime platform facts discovered from the boot device tree.
//!
//! Keep hardware discovery separate from the scheduler's current capabilities:
//! a board may expose several CPUs in DTB while wll_OS still schedules on the
//! boot CPU only.  User-visible affinity must report the latter until SMP is
//! actually enabled.

use core::sync::atomic::{fence, AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::config::MAX_CPUS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum PlatformKind {
    Unknown = 0,
    QemuVirt = 1,
    VisionFive2 = 2,
    Loongson2K1000La = 3,
}

static PLATFORM_KIND: AtomicUsize = AtomicUsize::new(PlatformKind::Unknown as usize);
static PHYSICAL_CPU_COUNT: AtomicUsize = AtomicUsize::new(1);
static TOTAL_MEMORY_BYTES: AtomicUsize = AtomicUsize::new(0);
static GOLDFISH_RTC_BASE: AtomicUsize = AtomicUsize::new(0);
static CPU_IDS: [AtomicUsize; MAX_CPUS] =
    [const { AtomicUsize::new(usize::MAX) }; MAX_CPUS];
static CPU_STATES: [AtomicUsize; MAX_CPUS] =
    [const { AtomicUsize::new(CpuState::Absent as usize) }; MAX_CPUS];
static SECONDARY_RELEASED: AtomicBool = AtomicBool::new(false);
static IDLE_MASK: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_ADDRESS_SPACE: [AtomicUsize; MAX_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_CPUS];
static TLB_GENERATION: AtomicUsize = AtomicUsize::new(0);
static TLB_REQUEST: [AtomicUsize; MAX_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_CPUS];
static TLB_ACK: [AtomicUsize; MAX_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_CPUS];
static TLB_SHOOTDOWN_LOCK: Mutex<()> = Mutex::new(());

const IPI_RESCHEDULE: u32 = 1 << 1;
const IPI_TLB_SHOOTDOWN: u32 = 1 << 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum CpuState {
    Absent = 0,
    Present = 1,
    Starting = 2,
    Online = 3,
    Failed = 4,
}

fn classify(model: &str, compatible: impl Iterator<Item = &'static str>) -> PlatformKind {
    let model_lower = model.to_ascii_lowercase();
    let mut saw_starfive = model_lower.contains("starfive")
        || model_lower.contains("visionfive")
        || model_lower.contains("jh7110");
    let mut saw_loongson = model_lower.contains("loongson")
        && (model_lower.contains("2k1000") || model_lower.contains("2k1000la"));
    let mut saw_qemu = model_lower.contains("qemu") || model_lower.contains("virt");

    for item in compatible {
        let lower = item.to_ascii_lowercase();
        saw_starfive |= lower.contains("starfive")
            && (lower.contains("visionfive") || lower.contains("jh7110"));
        saw_loongson |=
            lower.contains("loongson") && (lower.contains("2k1000") || lower.contains("2k1000la"));
        saw_qemu |= lower.contains("qemu")
            || lower.contains("riscv-virtio")
            || lower.contains("dummy-virt");
    }

    if saw_starfive {
        PlatformKind::VisionFive2
    } else if saw_loongson {
        PlatformKind::Loongson2K1000La
    } else if saw_qemu {
        PlatformKind::QemuVirt
    } else {
        PlatformKind::Unknown
    }
}

/// Capture immutable platform facts after `polyhal::mem::init_dtb_once`.
pub fn init(boot_hardware_id: usize) {
    let Ok(fdt) = polyhal::mem::get_fdt() else {
        log::warn!("[platform] no usable DTB; using conservative defaults");
        return;
    };

    let mut cpu_count = 0usize;
    for node in fdt.find_nodes("/cpus/cpu") {
        if cpu_count >= MAX_CPUS {
            log::warn!("[platform] ignoring CPU beyond MAX_CPUS={}", MAX_CPUS);
            break;
        }
        let hardware_id = node
            .reg()
            .and_then(|mut regions| regions.next())
            .map(|region| region.address as usize)
            .unwrap_or(cpu_count);
        CPU_IDS[cpu_count].store(hardware_id, Ordering::Relaxed);
        CPU_STATES[cpu_count].store(CpuState::Present as usize, Ordering::Relaxed);
        cpu_count += 1;
    }
    if cpu_count == 0 {
        CPU_IDS[0].store(boot_hardware_id, Ordering::Relaxed);
        CPU_STATES[0].store(CpuState::Present as usize, Ordering::Relaxed);
        cpu_count = 1;
    }
    PHYSICAL_CPU_COUNT.store(cpu_count, Ordering::Release);

    let total_memory = fdt
        .memory()
        .flat_map(|memory| memory.regions())
        .fold(0usize, |total, region| total.saturating_add(region.size));
    TOTAL_MEMORY_BYTES.store(total_memory, Ordering::Release);

    let root = fdt.find_nodes("/").next();
    let model = root
        .as_ref()
        .and_then(|node| node.find_property("model"))
        .map(|property| property.str())
        .unwrap_or("");
    let kind = root
        .as_ref()
        .map(|node| classify(model, node.compatibles()))
        .unwrap_or(PlatformKind::Unknown);
    PLATFORM_KIND.store(kind as usize, Ordering::Release);

    #[cfg(target_arch = "riscv64")]
    if let Some(node) = fdt.find_compatible(&["google,goldfish-rtc"]).next() {
        if let Some(reg) = node.reg().and_then(|mut regions| regions.next()) {
            GOLDFISH_RTC_BASE.store(reg.address as usize, Ordering::Release);
        }
    }

    log::info!(
        "[platform] kind={:?}, physical_cpus={}, memory={} MiB, model={}",
        kind,
        cpu_count,
        total_memory / 1024 / 1024,
        model
    );
}

pub fn kind() -> PlatformKind {
    match PLATFORM_KIND.load(Ordering::Acquire) {
        1 => PlatformKind::QemuVirt,
        2 => PlatformKind::VisionFive2,
        3 => PlatformKind::Loongson2K1000La,
        _ => PlatformKind::Unknown,
    }
}

pub fn model_name() -> &'static str {
    match kind() {
        PlatformKind::QemuVirt => "QEMU virt",
        PlatformKind::VisionFive2 => "StarFive VisionFive 2 (JH7110)",
        PlatformKind::Loongson2K1000La => "Loongson 2K1000LA",
        PlatformKind::Unknown => "unknown device-tree platform",
    }
}

pub fn physical_cpu_count() -> usize {
    PHYSICAL_CPU_COUNT.load(Ordering::Acquire).max(1)
}

/// CPUs on which the scheduler can currently run user tasks.
pub fn online_cpu_count() -> usize {
    online_cpu_mask().count_ones() as usize
}

pub fn possible_cpu_mask() -> usize {
    let count = physical_cpu_count().min(MAX_CPUS);
    if count == usize::BITS as usize {
        usize::MAX
    } else {
        (1usize << count) - 1
    }
}

pub fn online_cpu_mask() -> usize {
    let mut mask = 0usize;
    for cpu in 0..physical_cpu_count().min(MAX_CPUS) {
        if cpu_state(cpu) == CpuState::Online {
            mask |= 1usize << cpu;
        }
    }
    mask
}

pub fn cpu_state(cpu: usize) -> CpuState {
    match CPU_STATES
        .get(cpu)
        .map(|state| state.load(Ordering::Acquire))
        .unwrap_or(CpuState::Absent as usize)
    {
        1 => CpuState::Present,
        2 => CpuState::Starting,
        3 => CpuState::Online,
        4 => CpuState::Failed,
        _ => CpuState::Absent,
    }
}

pub fn cpu_hardware_id(cpu: usize) -> Option<usize> {
    let id = CPU_IDS.get(cpu)?.load(Ordering::Acquire);
    (id != usize::MAX).then_some(id)
}

#[inline]
pub fn current_hardware_cpu_id() -> usize {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        let value: usize;
        core::arch::asm!("mv {}, tp", out(reg) value, options(nomem, nostack));
        value.saturating_sub(1)
    }
    #[cfg(target_arch = "loongarch64")]
    {
        loongArch64::register::cpuid::read().core_id()
    }
}

pub fn current_cpu_index() -> usize {
    let hardware_id = current_hardware_cpu_id();
    for cpu in 0..physical_cpu_count().min(MAX_CPUS) {
        if CPU_IDS[cpu].load(Ordering::Acquire) == hardware_id {
            return cpu;
        }
    }
    0
}

pub fn mark_current_online() {
    let cpu = current_cpu_index();
    CPU_STATES[cpu].store(CpuState::Online as usize, Ordering::Release);
    log::info!(
        "[smp] cpu={} hardware_id={} online",
        cpu,
        current_hardware_cpu_id()
    );
}

pub fn secondary_released() -> bool {
    SECONDARY_RELEASED.load(Ordering::Acquire)
}

pub fn start_secondary_cpus(
    boot_hardware_id: usize,
    secondary_entry: usize,
    stack_base: usize,
    stack_size: usize,
) {
    SECONDARY_RELEASED.store(true, Ordering::Release);
    let expected = physical_cpu_count().min(MAX_CPUS);
    for cpu in 0..expected {
        let Some(hardware_id) = cpu_hardware_id(cpu) else {
            continue;
        };
        if hardware_id == boot_hardware_id {
            continue;
        }
        CPU_STATES[cpu].store(CpuState::Starting as usize, Ordering::Release);
        let stack_top = stack_base + (cpu + 1) * stack_size;
        if !polyhal::multicore::boot_core(hardware_id, secondary_entry, stack_top) {
            CPU_STATES[cpu].store(CpuState::Failed as usize, Ordering::Release);
        }
    }

    let deadline = crate::timer::get_time_us().saturating_add(5_000_000);
    while online_cpu_count() < expected && crate::timer::get_time_us() < deadline {
        core::hint::spin_loop();
    }
    for cpu in 0..expected {
        if cpu_state(cpu) == CpuState::Starting {
            CPU_STATES[cpu].store(CpuState::Failed as usize, Ordering::Release);
            log::warn!("[smp] cpu={} failed to reach online state", cpu);
        }
    }
    log::info!(
        "[smp] online_mask={:#x} online={} present={}",
        online_cpu_mask(),
        online_cpu_count(),
        physical_cpu_count()
    );
}

pub fn init_local_ipi() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        riscv::register::sie::set_ssoft();
    }
    #[cfg(target_arch = "loongarch64")]
    {
        use loongArch64::register::ecfg::{self, LineBasedInterrupt};
        let mut enabled = ecfg::read().lie();
        enabled.insert(LineBasedInterrupt::IPI);
        ecfg::set_lie(enabled);
        let _ = clear_local_ipi();
    }
}

#[cfg(target_arch = "loongarch64")]
fn clear_local_ipi() -> u32 {
    let status: u32;
    unsafe {
        core::arch::asm!(
            "iocsrrd.w {status}, {status_addr}",
            "iocsrwr.w {status}, {clear_addr}",
            status = out(reg) status,
            status_addr = in(reg) loongArch64::consts::LOONGARCH_IOCSR_IPI_STATUS,
            clear_addr = in(reg) loongArch64::consts::LOONGARCH_IOCSR_IPI_CLEAR,
            options(nostack)
        );
    }
    status
}

fn send_ipi_to_cpu(cpu: usize, action: u32) {
    let Some(hardware_id) = cpu_hardware_id(cpu) else {
        return;
    };
    #[cfg(target_arch = "riscv64")]
    {
        let _ = action;
        let _ = sbi_rt::send_ipi(1, hardware_id);
    }
    #[cfg(target_arch = "loongarch64")]
    loongArch64::ipi::send_ipi_single(hardware_id, action);
}

pub fn notify_cpu(cpu: usize) {
    if cpu < MAX_CPUS && cpu != current_cpu_index() && cpu_state(cpu) == CpuState::Online {
        send_ipi_to_cpu(cpu, IPI_RESCHEDULE);
    }
}

pub fn notify_runnable() {
    let targets = IDLE_MASK.load(Ordering::Acquire) & online_cpu_mask();
    let current = current_cpu_index();
    for cpu in 0..MAX_CPUS {
        if cpu != current && targets & (1usize << cpu) != 0 {
            send_ipi_to_cpu(cpu, IPI_RESCHEDULE);
        }
    }
}

pub fn prepare_idle() {
    IDLE_MASK.fetch_or(1usize << current_cpu_index(), Ordering::Release);
    fence(Ordering::SeqCst);
}

pub fn finish_idle() {
    IDLE_MASK.fetch_and(!(1usize << current_cpu_index()), Ordering::Release);
}

pub fn handle_ipi(action: usize) {
    #[cfg(target_arch = "riscv64")]
    let _ = action;
    #[cfg(target_arch = "loongarch64")]
    if action & IPI_TLB_SHOOTDOWN as usize != 0 {
        polyhal::pagetable::TLB::flush_all();
        let cpu = current_cpu_index();
        let generation = TLB_REQUEST[cpu].load(Ordering::Acquire);
        TLB_ACK[cpu].store(generation, Ordering::Release);
    }
}

pub fn acknowledge_local_ipi() -> usize {
    #[cfg(target_arch = "riscv64")]
    {
        0
    }
    #[cfg(target_arch = "loongarch64")]
    {
        clear_local_ipi() as usize
    }
}

pub fn mark_current_address_space(root: usize) {
    let cpu = current_cpu_index();
    // Publish the active root before sampling the deferred generation. If a
    // concurrent shooter publishes first, this CPU observes the request and
    // flushes locally. If this store happens first, the shooter observes the
    // active root and includes this CPU in the synchronous remote flush.
    ACTIVE_ADDRESS_SPACE[cpu].store(root, Ordering::SeqCst);
    let request = TLB_REQUEST[cpu].load(Ordering::Acquire);
    if TLB_ACK[cpu].load(Ordering::Acquire) != request {
        polyhal::pagetable::TLB::flush_all();
        TLB_ACK[cpu].store(request, Ordering::Release);
    }
}

pub fn clear_current_address_space() {
    ACTIVE_ADDRESS_SPACE[current_cpu_index()].store(0, Ordering::Release);
}

pub fn tlb_shootdown(address_space_root: usize) {
    tlb_shootdown_inner(address_space_root, false);
}

fn tlb_shootdown_inner(address_space_root: usize, all_cpus: bool) {
    if address_space_root == 0 {
        return;
    }
    let mut targets = online_cpu_mask() & !(1usize << current_cpu_index());
    #[cfg(target_arch = "loongarch64")]
    if !all_cpus {
        for cpu in 0..MAX_CPUS {
            if targets & (1usize << cpu) != 0
                && ACTIVE_ADDRESS_SPACE[cpu].load(Ordering::Acquire) != address_space_root
            {
                targets &= !(1usize << cpu);
            }
        }
    }
    #[cfg(target_arch = "riscv64")]
    if !all_cpus {
        let generation = TLB_GENERATION.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
        for cpu in 0..MAX_CPUS {
            if targets & (1usize << cpu) != 0 {
                TLB_REQUEST[cpu].store(generation, Ordering::Release);
            }
        }
        fence(Ordering::SeqCst);
        for cpu in 0..MAX_CPUS {
            if targets & (1usize << cpu) != 0
                && ACTIVE_ADDRESS_SPACE[cpu].load(Ordering::Acquire) != address_space_root
            {
                targets &= !(1usize << cpu);
            }
        }
    }
    if targets == 0 {
        return;
    }
    #[cfg(target_arch = "riscv64")]
    {
        let mut hart_mask_base = usize::MAX;
        for cpu in 0..MAX_CPUS {
            if targets & (1usize << cpu) != 0 {
                if let Some(hardware_id) = cpu_hardware_id(cpu) {
                    hart_mask_base = hart_mask_base.min(hardware_id);
                }
            }
        }
        if hart_mask_base != usize::MAX {
            let mut hart_mask = 0usize;
            let mut mask_fits = true;
            for cpu in 0..MAX_CPUS {
                if targets & (1usize << cpu) != 0 {
                    if let Some(hardware_id) = cpu_hardware_id(cpu) {
                        let offset = hardware_id - hart_mask_base;
                        if offset >= usize::BITS as usize {
                            mask_fits = false;
                            break;
                        }
                        hart_mask |= 1usize << offset;
                    }
                }
            }
            if mask_fits {
                let _ =
                    sbi_rt::remote_sfence_vma(hart_mask, hart_mask_base, 0, usize::MAX);
            } else {
                for cpu in 0..MAX_CPUS {
                    if targets & (1usize << cpu) != 0 {
                        if let Some(hardware_id) = cpu_hardware_id(cpu) {
                            let _ =
                                sbi_rt::remote_sfence_vma(1, hardware_id, 0, usize::MAX);
                        }
                    }
                }
            }
        }
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let _guard = TLB_SHOOTDOWN_LOCK.lock();
        if !all_cpus {
            targets = online_cpu_mask() & !(1usize << current_cpu_index());
            for cpu in 0..MAX_CPUS {
                if targets & (1usize << cpu) != 0
                    && ACTIVE_ADDRESS_SPACE[cpu].load(Ordering::Acquire) != address_space_root
                {
                    targets &= !(1usize << cpu);
                }
            }
            if targets == 0 {
                return;
            }
        }
        let generation = TLB_GENERATION.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
        for cpu in 0..MAX_CPUS {
            if targets & (1usize << cpu) != 0 {
                TLB_REQUEST[cpu].store(generation, Ordering::Release);
                send_ipi_to_cpu(cpu, IPI_TLB_SHOOTDOWN);
            }
        }
        for cpu in 0..MAX_CPUS {
            if targets & (1usize << cpu) != 0 {
                let mut spins = 0usize;
                while TLB_ACK[cpu].load(Ordering::Acquire) != generation {
                    if !all_cpus
                        && ACTIVE_ADDRESS_SPACE[cpu].load(Ordering::Acquire)
                            != address_space_root
                    {
                        break;
                    }
                    if cpu_state(cpu) != CpuState::Online {
                        break;
                    }
                    core::hint::spin_loop();
                    spins += 1;
                    if spins == 1_000_000 {
                        // IOCSR IPI status bits can be coalesced with an
                        // already-pending reschedule interrupt. Retrying the
                        // idempotent TLB action prevents a lost edge from
                        // turning one shootdown into an unbounded global stall.
                        fence(Ordering::SeqCst);
                        send_ipi_to_cpu(cpu, IPI_TLB_SHOOTDOWN);
                        spins = 0;
                    }
                }
            }
        }
    }
}

/// Invalidate every TLB before retired ASIDs become eligible for reuse.
pub fn flush_tlb_all_cpus() {
    polyhal::pagetable::TLB::flush_all();
    tlb_shootdown_inner(usize::MAX, true);
}

pub fn total_memory_bytes() -> usize {
    TOTAL_MEMORY_BYTES.load(Ordering::Acquire)
}

pub fn cpu_isa() -> &'static str {
    #[cfg(target_arch = "riscv64")]
    {
        "rv64imafdc"
    }
    #[cfg(target_arch = "loongarch64")]
    {
        match kind() {
            PlatformKind::Loongson2K1000La => "loongarch64 (LA264)",
            _ => "loongarch64",
        }
    }
}

/// Read the QEMU goldfish RTC, whose time registers contain Unix nanoseconds.
/// Real-board RTC backends are deliberately separate follow-up drivers.
#[cfg(target_arch = "riscv64")]
pub fn realtime_ns() -> Option<u64> {
    let paddr = GOLDFISH_RTC_BASE.load(Ordering::Acquire);
    if paddr == 0 {
        return None;
    }
    crate::trap::restore_kernel_page_table();
    let base = crate::drivers::hal::phys_to_virt_mmio(paddr) as usize;
    let (high, low) = unsafe {
        loop {
            let high_before = core::ptr::read_volatile((base + 4) as *const u32);
            let low = core::ptr::read_volatile(base as *const u32);
            let high_after = core::ptr::read_volatile((base + 4) as *const u32);
            if high_before == high_after {
                break (high_before, low);
            }
        }
    };
    let ns = ((high as u64) << 32) | low as u64;
    // Reject an uninitialised clock instead of presenting a plausible 1970 date.
    (ns >= 946_684_800_000_000_000).then_some(ns)
}

#[cfg(not(target_arch = "riscv64"))]
pub fn realtime_ns() -> Option<u64> {
    None
}
