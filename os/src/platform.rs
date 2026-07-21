//! Runtime platform facts discovered from the boot device tree.
//!
//! Keep hardware discovery separate from the scheduler's current capabilities:
//! a board may expose several CPUs in DTB while wll_OS still schedules on the
//! boot CPU only.  User-visible affinity must report the latter until SMP is
//! actually enabled.

use core::sync::atomic::{AtomicUsize, Ordering};

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
pub fn init() {
    let Ok(fdt) = polyhal::mem::get_fdt() else {
        log::warn!("[platform] no usable DTB; using conservative defaults");
        return;
    };

    let cpu_count = fdt.find_nodes("/cpus/cpu").count().max(1);
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
pub const fn online_cpu_count() -> usize {
    1
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
