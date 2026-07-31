#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(naked_functions)]

extern crate alloc;

use core::arch::global_asm;

use crate::console::putchar;
use polyhal::common::PageAlloc;
use polyhal::PhysAddr;

extern "C" {
    fn _secondary_start();
    static _smp_boot_stacks: u8;
}

#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("entry_riscv64.asm"));

#[cfg(target_arch = "loongarch64")]
global_asm!(include_str!("entry_loongarch64.asm"));

// 模块声明
mod config;
#[cfg(feature = "buildstorm-diagnostics")]
mod buildstorm_diagnostics;
mod console;
mod cpu;
mod drivers;
mod fs;
mod lang_items;
mod logging;
mod mm;
mod perf_counters;
mod platform;
#[cfg(feature = "smp-regression")]
mod smp_regression;
mod syscall;
mod task;
mod timer;
mod trap;
mod utils;

/// 内核页分配器实现
pub struct KernelPageAlloc;

impl PageAlloc for KernelPageAlloc {
    fn alloc(&self) -> PhysAddr {
        mm::frame_allocator::alloc_frame()
            .map(|frame| {
                let paddr = PhysAddr::new(frame.ppn().addr());
                paddr.clear_len(crate::config::PAGE_SIZE);
                core::mem::forget(frame);
                paddr
            })
            .unwrap_or_else(|| PhysAddr::new(0))
    }

    fn dealloc(&self, paddr: PhysAddr) {
        if let Ok(ppn) = mm::frame_allocator::PhysPageNum::try_from(paddr) {
            mm::frame_allocator::dealloc_frame(ppn);
        }
    }
}

/// 等待中断
#[inline]
fn wait_for_interrupt() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("wfi");
    }
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        core::arch::asm!("idle 0");
    }
}

fn add_available_frames(start: usize, end: usize) -> usize {
    extern "C" {
        fn _start();
        fn _end();
    }

    let kernel_start = _start as usize;
    // The linker _end includes .bss, including the static kernel heap. The DTB
    // memory list may only exclude a conservative image range, so clip it again
    // before handing pages to the frame allocator.
    let kernel_end = ((_end as usize).saturating_add(crate::config::PAGE_SIZE - 1)
        / crate::config::PAGE_SIZE)
        * crate::config::PAGE_SIZE;

    if end <= start {
        return 0;
    }
    if end <= kernel_start || start >= kernel_end {
        mm::frame_allocator::add_frames_range(start, end);
        return 1;
    }

    let mut count = 0usize;
    if start < kernel_start {
        mm::frame_allocator::add_frames_range(start, kernel_start);
        count += 1;
    }
    if end > kernel_end {
        mm::frame_allocator::add_frames_range(kernel_end, end);
        count += 1;
    }
    count
}

#[cfg(target_arch = "loongarch64")]
const EARLY_LA_UART_BASE: usize = 0x8000_0000_1fe0_01e0;
#[cfg(target_arch = "loongarch64")]
const EARLY_LA_UART_THR: usize = 0;
#[cfg(target_arch = "loongarch64")]
const EARLY_LA_UART_LSR: usize = 5;
#[cfg(target_arch = "loongarch64")]
const EARLY_LA_UART_LSR_THRE: u8 = 0x20;

#[cfg(target_arch = "loongarch64")]
#[inline]
fn early_la_putchar(c: u8) {
    unsafe {
        while core::ptr::read_volatile((EARLY_LA_UART_BASE + EARLY_LA_UART_LSR) as *const u8)
            & EARLY_LA_UART_LSR_THRE
            == 0
        {}
        core::ptr::write_volatile((EARLY_LA_UART_BASE + EARLY_LA_UART_THR) as *mut u8, c);
    }
}

#[cfg(target_arch = "loongarch64")]
fn early_la_line(msg: &[u8]) {
    for &b in msg {
        early_la_putchar(b);
    }
}

/// 内核入口函数
#[no_mangle]
#[inline(never)]
pub extern "C" fn rust_main(hartid: usize, dtb_ptr: usize) -> ! {
    // Guard against recursive calls
    use core::sync::atomic::{AtomicBool, Ordering};
    static INIT_GUARD: AtomicBool = AtomicBool::new(false);
    if INIT_GUARD.swap(true, Ordering::SeqCst) {
        loop {
            wait_for_interrupt();
        }
    }

    if let Err(_e) = polyhal::mem::init_dtb_once(PhysAddr::new(dtb_ptr)) {
        loop {
            wait_for_interrupt();
        }
    }
    logging::init(option_env!("LOG"));
    polyhal::common::init(&KernelPageAlloc);
    mm::init();
    platform::init(hartid);

    #[cfg(target_arch = "riscv64")]
    {
        // DTB init is done above; now register available memory regions
        // with the buddy frame allocator so that elf.load() can allocate
        // user-space pages.
        let mut count = 0usize;
        for &(start, len) in polyhal::mem::get_mem_areas() {
            if len > 0 {
                let end = start.saturating_add(len);
                count += add_available_frames(start, end);
            }
        }
        log::info!("[mm] Memory regions added to frame allocator: {}", count);
    }

    #[cfg(target_arch = "loongarch64")]
    {
        let mut count = 0;
        for &(start, len) in polyhal::mem::get_mem_areas() {
            if len > 0 {
                let end = start.saturating_add(len);
                count += add_available_frames(start, end);
            }
        }
        log::info!("[mm] Memory regions added: {}", count);
    }

    // The heap allocator converts physical frames through the architecture's
    // normal RAM mapping (LoongArch DMW1 or RISC-V identity mapping).
    let heap_growth =
        mm::heap_allocator::grow_from_frame_allocator(platform::total_memory_bytes());
    log::info!(
        "[heap] dynamic extension={} MiB",
        heap_growth / 1024 / 1024
    );

    #[cfg(target_arch = "loongarch64")]
    mm::page_table::init_kernel_page_table();
    #[cfg(target_arch = "riscv64")]
    {
        mm::page_table::init_kernel_page_table();
        mm::memory_set::init_kernel_space();
        trap::init();
        timer::init();
    }

    #[cfg(target_arch = "loongarch64")]
    mm::memory_set::init_kernel_space();

    #[cfg(target_arch = "loongarch64")]
    trap::init();

    #[cfg(target_arch = "loongarch64")]
    timer::init();

    platform::init_local_ipi();
    trap::interrupts::enable_interrupt();
    platform::mark_current_online();

    putchar(b'[');
    putchar(b'k');
    putchar(b'e');
    putchar(b'r');
    putchar(b'n');
    putchar(b'e');
    putchar(b'l');
    putchar(b']');
    putchar(b' ');
    putchar(b'H');
    putchar(b'e');
    putchar(b'l');
    putchar(b'l');
    putchar(b'o');
    putchar(b',');
    putchar(b' ');
    putchar(b'O');
    putchar(b'S');
    putchar(b'!');
    putchar(b'\n');

    putchar(b'[');
    putchar(b'k');
    putchar(b'e');
    putchar(b'r');
    putchar(b'n');
    putchar(b'e');
    putchar(b'l');
    putchar(b']');
    putchar(b' ');
    putchar(b'M');
    putchar(b'e');
    putchar(b'm');
    putchar(b'o');
    putchar(b'r');
    putchar(b'y');
    putchar(b' ');
    putchar(b'i');
    putchar(b'n');
    putchar(b'i');
    putchar(b't');
    putchar(b'\n');

    #[cfg(target_arch = "riscv64")]
    {
        use crate::drivers::virtio_mmio_blk;
        use crate::fs::{block_dev, ext4_vol};
        putchar(b'[');
        putchar(b'V');
        putchar(b'I');
        putchar(b'R');
        putchar(b'T');
        putchar(b'I');
        putchar(b'O');
        putchar(b']');
        putchar(b' ');
        putchar(b'P');
        putchar(b'r');
        putchar(b'o');
        putchar(b'b');
        putchar(b'i');
        putchar(b'n');
        putchar(b'g');
        putchar(b'.');
        putchar(b'.');
        putchar(b'\n');
        if let Some(device) = unsafe { virtio_mmio_blk::probe_first_virtio_disk_from_dt(dtb_ptr) } {
            putchar(b'[');
            putchar(b'V');
            putchar(b'I');
            putchar(b'R');
            putchar(b'T');
            putchar(b'I');
            putchar(b'O');
            putchar(b']');
            putchar(b' ');
            putchar(b'm');
            putchar(b'o');
            putchar(b'u');
            putchar(b'n');
            putchar(b't');
            putchar(b'i');
            putchar(b'n');
            putchar(b'g');
            putchar(b' ');
            putchar(b'e');
            putchar(b'x');
            putchar(b't');
            putchar(b'4');
            putchar(b'.');
            putchar(b'.');
            putchar(b'\n');
            let root_device = block_dev::register_virtio_disk(device);
            ext4_vol::mount_block_device(root_device);
            putchar(b'[');
            putchar(b'E');
            putchar(b'X');
            putchar(b'T');
            putchar(b'4');
            putchar(b']');
            putchar(b' ');
            putchar(b'm');
            putchar(b'o');
            putchar(b'u');
            putchar(b'n');
            putchar(b't');
            putchar(b'e');
            putchar(b'd');
            putchar(b' ');
            putchar(b's');
            putchar(b'u');
            putchar(b'c');
            putchar(b'c');
            putchar(b'e');
            putchar(b's');
            putchar(b's');
            putchar(b'f');
            putchar(b'u');
            putchar(b'l');
            putchar(b'l');
            putchar(b'y');
            putchar(b'\n');
        } else {
            putchar(b'[');
            putchar(b'V');
            putchar(b'I');
            putchar(b'R');
            putchar(b'T');
            putchar(b'I');
            putchar(b'O');
            putchar(b']');
            putchar(b' ');
            putchar(b'n');
            putchar(b'o');
            putchar(b't');
            putchar(b' ');
            putchar(b'f');
            putchar(b'o');
            putchar(b'u');
            putchar(b'n');
            putchar(b'd');
            putchar(b'\n');
        }
    }

    #[cfg(target_arch = "loongarch64")]
    {
        early_la_line(b"[VIRTIO] Probing...\n");
        if let Some(dev) = crate::drivers::virtio_pci_blk::probe_pci_virtio_blk() {
            early_la_line(b"[VIRTIO] found\n");
            let root_device = crate::fs::block_dev::register_virtio_disk(dev);
            crate::fs::ext4_vol::mount_block_device(root_device);
            early_la_line(b"[EXT4] mounted successfully\n");
        } else {
            early_la_line(b"[VIRTIO] not found\n");
        }
    }
    fs::init();
    let secondary_stack_base = core::ptr::addr_of!(_smp_boot_stacks) as usize;
    platform::start_secondary_cpus(
        hartid,
        _secondary_start as usize,
        secondary_stack_base,
        crate::config::SMP_BOOT_STACK_SIZE,
    );
    #[cfg(feature = "smp-regression")]
    smp_regression::run();
    task::add_initproc();
    putchar(b'[');
    putchar(b'k');
    putchar(b'e');
    putchar(b'r');
    putchar(b'n');
    putchar(b'e');
    putchar(b'l');
    putchar(b']');
    putchar(b' ');
    putchar(b'S');
    putchar(b't');
    putchar(b'a');
    putchar(b'r');
    putchar(b't');
    putchar(b'i');
    putchar(b'n');
    putchar(b'g');
    putchar(b' ');
    putchar(b's');
    putchar(b'c');
    putchar(b'h');
    putchar(b'e');
    putchar(b'd');
    putchar(b'.');
    putchar(b'\n');

    task::run_tasks();

    // Should never reach here
    loop {
        wait_for_interrupt();
    }
}

/// Entry used only after the platform boot protocol has started a secondary
/// CPU with its own stack. Global memory, drivers and the kernel page table are
/// already initialized by the boot CPU.
#[no_mangle]
pub extern "C" fn rust_secondary_main(_hardware_id: usize) -> ! {
    while !platform::secondary_released() {
        core::hint::spin_loop();
    }
    trap::restore_kernel_page_table();
    trap::init();
    timer::init();
    platform::init_local_ipi();
    trap::interrupts::enable_interrupt();
    platform::mark_current_online();
    task::run_secondary_tasks()
}
