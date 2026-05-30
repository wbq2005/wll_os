#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(naked_functions)]

extern crate alloc;

use core::arch::global_asm;

use polyhal::{PhysAddr};
use polyhal::common::PageAlloc;
use crate::console::putchar;

#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("entry_riscv64.asm"));

#[cfg(target_arch = "loongarch64")]
global_asm!(include_str!("entry_loongarch64.asm"));

// 模块声明
mod config;
mod console;
mod drivers;
mod fs;
mod lang_items;
mod logging;
mod mm;
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
                let paddr = PhysAddr::new(frame.ppn.addr());
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
    unsafe { core::arch::asm!("wfi"); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("idle 0"); }
}

#[cfg(target_arch = "riscv64")]
#[inline]
fn early_sbi_putchar(c: u8) {
    unsafe {
        core::arch::asm!(
            "li a7, 0x01",
            "mv a0, {0}",
            "ecall",
            in(reg) c as usize,
            out("a7") _,
            out("a0") _
        );
    }
}

#[cfg(target_arch = "riscv64")]
fn early_sbi_line(msg: &[u8]) {
    for &b in msg {
        early_sbi_putchar(b);
    }
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
    // UART MMIO debug markers: track execution flow without ecall
    // J=entry, K=init_dtb_once done, L=logging done, M=polyhal done
    // N=mm done, O=memory regions added, P=page_table done, Q=kernel_space done
    // R=trap init done, S=timer done, T=Hello printed, U=Memorry init, V=VirtIO done
    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[EARLY] rust_main reached\n");

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] A\n");

    // Guard against recursive calls
    use core::sync::atomic::{AtomicBool, Ordering};
    static INIT_GUARD: AtomicBool = AtomicBool::new(false);
    if INIT_GUARD.swap(true, Ordering::SeqCst) {
        #[cfg(target_arch = "loongarch64")]
        early_la_line(b"[DBG] RECURSIVE CALL - halting\n");
        loop { wait_for_interrupt(); }
    }

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] B\n");
    if let Err(_e) = polyhal::mem::init_dtb_once(PhysAddr::new(dtb_ptr)) {
        loop { wait_for_interrupt(); }
    }

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] C\n");
    logging::init(option_env!("LOG"));

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] D\n");
    polyhal::common::init(&KernelPageAlloc);

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] E\n");
    mm::init();

    #[cfg(target_arch = "riscv64")]
    {
        // DTB init is done above; now register available memory regions
        // with the buddy frame allocator so that elf.load() can allocate
        // user-space pages.
        let mut count = 0usize;
        for &(start, len) in polyhal::mem::get_mem_areas() {
            if len > 0 {
                let end = start.saturating_add(len);
                if end > start {
                    mm::frame_allocator::add_frames_range(start, end);
                    count += 1;
                }
            }
        }
        log::info!("[mm] Memory regions added to frame allocator: {}", count);
    }

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] F\n");
    #[cfg(target_arch = "loongarch64")]
    {
        let mut count = 0;
        for &(start, len) in polyhal::mem::get_mem_areas() {
            if len > 0 {
                let end = start.saturating_add(len);
                if end > start {
                    mm::frame_allocator::add_frames_range(start, end);
                    count += 1;
                }
            }
        }
        log::info!("[mm] Memory regions added: {}", count);
    }

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] G\n");
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
    early_la_line(b"[DBG] H\n");
    #[cfg(target_arch = "loongarch64")]
    mm::memory_set::init_kernel_space();

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] I\n");
    #[cfg(target_arch = "loongarch64")]
    trap::init();

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] J\n");
    #[cfg(target_arch = "loongarch64")]
    timer::init();

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] K\n");
    putchar(b'['); putchar(b'k'); putchar(b'e'); putchar(b'r'); putchar(b'n'); putchar(b'e');
    putchar(b'l'); putchar(b']'); putchar(b' '); putchar(b'H'); putchar(b'e'); putchar(b'l');
    putchar(b'l'); putchar(b'o'); putchar(b','); putchar(b' '); putchar(b'O'); putchar(b'S');
    putchar(b'!'); putchar(b'\n');

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] L\n");
    putchar(b'['); putchar(b'k'); putchar(b'e'); putchar(b'r'); putchar(b'n'); putchar(b'e');
    putchar(b'l'); putchar(b']'); putchar(b' '); putchar(b'M'); putchar(b'e'); putchar(b'm');
    putchar(b'o'); putchar(b'r'); putchar(b'y'); putchar(b' '); putchar(b'i'); putchar(b'n');
    putchar(b'i'); putchar(b't'); putchar(b'\n');

    #[cfg(target_arch = "riscv64")]
    {
        use crate::drivers::virtio_mmio_blk;
        use crate::fs::ext4_vol;
        putchar(b'['); putchar(b'V'); putchar(b'I'); putchar(b'R'); putchar(b'T'); putchar(b'I'); putchar(b'O'); putchar(b']'); putchar(b' '); putchar(b'P'); putchar(b'r'); putchar(b'o'); putchar(b'b'); putchar(b'i'); putchar(b'n'); putchar(b'g'); putchar(b'.'); putchar(b'.'); putchar(b'\n');
        if let Some(device) = unsafe { virtio_mmio_blk::probe_first_virtio_disk_from_dt(dtb_ptr) } {
            putchar(b'['); putchar(b'V'); putchar(b'I'); putchar(b'R'); putchar(b'T'); putchar(b'I'); putchar(b'O'); putchar(b']'); putchar(b' '); putchar(b'm'); putchar(b'o'); putchar(b'u'); putchar(b'n'); putchar(b't'); putchar(b'i'); putchar(b'n'); putchar(b'g'); putchar(b' '); putchar(b'e'); putchar(b'x'); putchar(b't'); putchar(b'4'); putchar(b'.'); putchar(b'.'); putchar(b'\n');
            ext4_vol::mount_block_device(device);
            putchar(b'['); putchar(b'E'); putchar(b'X'); putchar(b'T'); putchar(b'4'); putchar(b']'); putchar(b' '); putchar(b'm'); putchar(b'o'); putchar(b'u'); putchar(b'n'); putchar(b't'); putchar(b'e'); putchar(b'd'); putchar(b' '); putchar(b's'); putchar(b'u'); putchar(b'c'); putchar(b'c'); putchar(b'e'); putchar(b's'); putchar(b's'); putchar(b'f'); putchar(b'u'); putchar(b'l'); putchar(b'l'); putchar(b'y'); putchar(b'\n');
        } else {
            putchar(b'['); putchar(b'V'); putchar(b'I'); putchar(b'R'); putchar(b'T'); putchar(b'I'); putchar(b'O'); putchar(b']'); putchar(b' '); putchar(b'n'); putchar(b'o'); putchar(b't'); putchar(b' '); putchar(b'f'); putchar(b'o'); putchar(b'u'); putchar(b'n'); putchar(b'd'); putchar(b'\n');
        }
    }

    #[cfg(target_arch = "loongarch64")]
    {
        early_la_line(b"[DBG] M\n");
        early_la_line(b"[VIRTIO] Probing...\n");
        if let Some(_dev) = crate::drivers::virtio_pci_blk::probe_pci_virtio_blk() {
            early_la_line(b"[VIRTIO] found\n");
        } else {
            early_la_line(b"[VIRTIO] not found\n");
        }
        early_la_line(b"[DBG] N\n");
    }

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] O\n");
    fs::init();

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] P\n");
    task::add_initproc();

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] Q\n");
    putchar(b'['); putchar(b'k'); putchar(b'e'); putchar(b'r'); putchar(b'n'); putchar(b'e');
    putchar(b'l'); putchar(b']'); putchar(b' '); putchar(b'S'); putchar(b't'); putchar(b'a');
    putchar(b'r'); putchar(b't'); putchar(b'i'); putchar(b'n'); putchar(b'g'); putchar(b' ');
    putchar(b's'); putchar(b'c'); putchar(b'h'); putchar(b'e'); putchar(b'd'); putchar(b'.');
    putchar(b'\n');

    #[cfg(target_arch = "loongarch64")]
    early_la_line(b"[DBG] R\n");
    task::run_tasks();

    // Should never reach here
    loop { wait_for_interrupt(); }
}
