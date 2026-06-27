use core::ptr::NonNull;

use fdt_parser::{Fdt, FdtError};
use lazyinit::LazyInit;

use crate::{
    arch::MEM_VECTOR_CAPACITY,
    common::CPU_NUM,
    PhysAddr,
};
#[cfg(not(target_arch = "riscv64"))]
use crate::arch::consts::VIRT_ADDR_START;

/// 打印十六进制数（无前缀）
#[cfg(target_arch = "riscv64")]
fn print_hex(value: usize) {
    use crate::debug_console::DebugConsole;
    DebugConsole::puthex(value);
}

#[cfg(not(target_arch = "riscv64"))]
fn print_hex(value: usize) {
    use crate::debug_console::DebugConsole;
    DebugConsole::puthex(value);
}

/// 在 OpenSBI FDT 区域查找有效的 DTB
///
/// OpenSBI fw_dynamic 将 FDT blob 放在其地址空间的尾部（接近物理 RAM 顶端）。
/// 对于 QEMU virt (128MB)，OpenSBI 信息如下：
///   - Firmware Base: 0x80000000 (317KB)
///   - Domain0 Next Arg1 (DTB): 0x87e00000 (接近 RAM 顶端)
///   - RAM: 0x80000000 到 0x88000000 (128MB)
///
/// 我们首先尝试 OpenSBI 报告的 DTB 地址（通过 OpenSBI SBI call），
/// 然后尝试常见的已知位置（0x87e00000），最后做快速粗扫描。
#[cfg(target_arch = "riscv64")]
fn scan_fdt_in_opensbi_region() -> Option<usize> {
    // 方法1: 通过 OpenSBI FWFT extension 读取 DTB 地址
    // 已知 DTB 位置：QEMU virt (128MB) -> 0x87e00000
    const KNOWN_DTB: usize = 0x87e00000;

    print!("[DTB] Trying known DTB location 0x");
    print_hex(KNOWN_DTB);
    println!("...");

    // 先读取魔数验证地址是否可访问
    let magic_raw = unsafe { core::ptr::read_volatile(KNOWN_DTB as *const u32) };
    print!("[DTB] Raw read at 0x");
    print_hex(KNOWN_DTB);
    print!(" = 0x");
    print_hex(magic_raw as usize);
    println!(" (BE=0x");
    print_hex(u32::from_be(magic_raw) as usize);
    println!(")");

    if let Some(fdt) = check_fdt_at(KNOWN_DTB) {
        print!("[DTB] Found at known location! addr=0x");
        print_hex(fdt);
        println!();
        return Some(fdt);
    }

    // 方法2: 粗扫描：从 0x87000000 开始，16MB 范围，4KB 步进
    const COARSE_START: usize = 0x8700_0000;
    const COARSE_END: usize = 0x8800_0000;
    const COARSE_STEP: usize = 0x1000;  // 4KB

    print!("[DTB] Coarse scan from 0x");
    print_hex(COARSE_START);
    print!(" to 0x");
    print_hex(COARSE_END);
    println!(" (4KB step)...");

    let mut addr = COARSE_START;
    while addr < COARSE_END {
        if let Some(fdt) = check_fdt_at(addr) {
            print!("[DTB] Found at 0x");
            print_hex(fdt);
            println!("!");
            return Some(fdt);
        }
        addr += COARSE_STEP;
    }
    println!("[DTB] Coarse scan complete, not found.");

    None
}

/// 检查指定地址是否包含有效的 FDT
#[cfg(target_arch = "riscv64")]
fn check_fdt_at(addr: usize) -> Option<usize> {
    let magic = unsafe { core::ptr::read_volatile(addr as *const u32) };
    if u32::from_be(magic) == 0xD00DFEED {
        let totalsize = unsafe {
            core::ptr::read_volatile((addr + 4) as *const u32)
        };
        let ts = u32::from_be(totalsize) as usize;
        if ts > 0 && ts <= 0x10_0000 {
            return Some(addr);
        }
    }
    None
}

/// Simple fixed-size array-based vector for no_std environments
struct MemAreaArray {
    data: [(usize, usize); MEM_VECTOR_CAPACITY],
    len: usize,
}

impl MemAreaArray {
    const fn new() -> Self {
        Self {
            data: [(0, 0); MEM_VECTOR_CAPACITY],
            len: 0,
        }
    }

    fn push(&mut self, item: (usize, usize)) {
        if self.len < MEM_VECTOR_CAPACITY {
            self.data[self.len] = item;
            self.len += 1;
        }
    }

    fn iter(&self) -> impl Iterator<Item = &(usize, usize)> {
        self.data[..self.len].iter()
    }

    fn iter_mut(&mut self) -> impl Iterator<Item = &mut (usize, usize)> {
        self.data[..self.len].iter_mut()
    }
}

/// Memory Area
///
/// Memory Area with [MEM_VECTOR_CAPACITY].
static mut MEM_AREA: MemAreaArray = MemAreaArray::new();

/// Device Tree Infomation
///
/// [DTB_INFO] is a lazy init value
static DTB_INFO: LazyInit<(PhysAddr, usize)> = LazyInit::new();

/// Init Device Tree Binary Pointer
///
/// # Arguments
///
/// - `dtb_ptr` is the pointer to the device tree binary.
///
pub fn init_dtb_once(dtb_ptr: PhysAddr) -> Result<(), FdtError<'static>> {
    // Check if already initialized FIRST, before any debug prints.
    // This prevents spurious error messages when called again with 0
    // (e.g. from constructors during boot sequence).
    if DTB_INFO.is_inited() {
        return Ok(());
    }
    // 调试：打印 DTB 指针信息
    #[cfg(target_arch = "riscv64")]
    {
        let raw = dtb_ptr.raw();
        print!("[DTB] raw ptr = 0x");
        print_hex(raw);
        println!();
    }

    // RISC-V: 若传入指针为 0（OpenSBI fw_dynamic 未通过 a1 传递），尝试从已知区域扫描
    #[cfg(target_arch = "riscv64")]
    let resolved_ptr = if dtb_ptr.raw() == 0 {
        print!("[DTB] DTB ptr is 0, scanning for FDT...\n");
        if let Some(fdt_addr) = scan_fdt_in_opensbi_region() {
            print!("[DTB] Found FDT at 0x");
            print_hex(fdt_addr);
            println!();
            PhysAddr::new(fdt_addr)
        } else {
            println!("[DTB] FDT not found in OpenSBI region!");
            return Err(FdtError::BadPtr);
        }
    } else {
        dtb_ptr
    };
    #[cfg(not(target_arch = "riscv64"))]
    let resolved_ptr = dtb_ptr;

    // RISC-V 低地址恒等映射可直接访问物理内存；
    // LoongArch 开启 PG 后低地址必须经 DMW 窗口 (get_mut_ptr 会加 VIRT_ADDR_START)。
    #[cfg(target_arch = "riscv64")]
    let ptr = {
        let raw_addr = resolved_ptr.raw();
        if raw_addr < 0x8000_0000_0000_0000 {
            println!("[DTB] Using low addr direct mapping");
            NonNull::new(raw_addr as *mut u8)
        } else {
            println!("[DTB] Using get_mut_ptr (high addr)");
            NonNull::new(resolved_ptr.get_mut_ptr::<u8>())
        }
    };
    #[cfg(not(target_arch = "riscv64"))]
    let ptr = NonNull::new(dtb_ptr.get_mut_ptr::<u8>());

    let ptr_val = ptr.ok_or(FdtError::BadPtr)?;
    print!("[DTB] ptr = 0x");
    print_hex(ptr_val.as_ptr() as usize);
    println!();

    // 验证魔数 (DTB 魔数: 0xD00DFEED 大端)
    let magic = unsafe { core::ptr::read_volatile(ptr_val.as_ptr() as *const u32) };
    let magic_be = u32::from_be(magic);
    print!("[DTB] magic = 0x");
    print_hex(magic_be as usize);
    println!(" (expected 0xD00DFEED)");

    // 如果魔数不匹配，尝试交换字节序
    if magic_be != 0xD00DFEED {
        let magic_le = u32::from_le(magic);
        if magic_le == 0xD00DFEED {
            println!("[DTB] DTB is little-endian, swapping all bytes...");
            // QEMU virt 生成小端 DTB，但 fdt_parser 期望大端
            // 需要交换所有 u32 的字节

            // 获取 DTB 大小（小端格式）
            let totalsize = unsafe {
                let ts = core::ptr::read_volatile(ptr_val.as_ptr().add(4) as *const u32);
                u32::from_le(ts) as usize
            };
            print!("[DTB] DTB size = ");
            print_hex(totalsize);
            println!();

            // 使用固定大小缓冲区进行字节交换
            // DTB 大小通常不超过 64KB
            const MAX_DTB_SIZE: usize = 65536;
            let swap_size = core::cmp::min(totalsize, MAX_DTB_SIZE);

            // 在栈上创建临时缓冲区
            let mut temp_buf = [0u8; MAX_DTB_SIZE];

            // 复制并交换字节
            unsafe {
                let src = core::slice::from_raw_parts(ptr_val.as_ptr(), swap_size);
                for (i, chunk) in src.chunks(4).enumerate() {
                    if chunk.len() == 4 {
                        let val = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                        let swapped = val.swap_bytes();
                        temp_buf[i * 4..][..4].copy_from_slice(&swapped.to_be_bytes());
                    } else {
                        // 尾部字节直接复制
                        for (j, &b) in chunk.iter().enumerate() {
                            temp_buf[i * 4 + j] = b;
                        }
                    }
                }
                // 写回原位置
                core::ptr::copy_nonoverlapping(temp_buf.as_ptr(), ptr_val.as_ptr(), swap_size);
            }

            // 验证交换后的魔数
            let magic_after = unsafe { core::ptr::read_volatile(ptr_val.as_ptr() as *const u32) };
            print!("[DTB] magic after swap = ");
            print_hex(magic_after as usize);
            println!();
        } else {
            println!("[DTB] Invalid DTB magic!");
            return Err(FdtError::BadMagic);
        }
    }
    let fdt = Fdt::from_ptr(ptr_val)?;
    DTB_INFO.init_once((resolved_ptr, fdt.total_size()));
    fdt.memory()
        .flat_map(|x| x.regions())
        .for_each(|mm| unsafe {
            #[cfg(not(target_arch = "riscv64"))]
            add_memory_region(mm.address as _, mm.address as usize + mm.size);
            #[cfg(target_arch = "riscv64")]
            {
                let mut start = mm.address as _;
                let end = mm.address as usize + mm.size;

                // TODO: using dynamic to skip memory
                start += 0x200_000;

                add_memory_region(start, end);
            }
        });
    Ok(())
}

/// Get Flattened Device Tree
pub fn get_fdt() -> Result<Fdt<'static>, FdtError<'static>> {
    if !DTB_INFO.is_inited() {
        return Err(FdtError::BadPtr);
    }
    unsafe { Fdt::from_ptr(NonNull::new_unchecked(DTB_INFO.0.get_mut_ptr())) }
}

/// Allocate Memory From [MEM_AREA]
///
/// # Safety
///
/// - Ensure call this function in the primary core when booting
/// - Ensure no alignment required
pub unsafe fn alloc(alloc_size: usize) -> *mut u8 {
    let mem_area_ptr = core::ptr::addr_of_mut!(MEM_AREA);
    for (start, size) in unsafe { (*mem_area_ptr).iter_mut() } {
        if *size > alloc_size {
            let ptr = *start;
            *start += alloc_size;
            *size -= alloc_size;
            return ptr as _;
        }
    }
    unreachable!()
}

/// Parse Information from the device tree binary or Multiboot
///
/// Display information when booting
/// Initialize the variables and memory from device tree
#[inline]
pub fn parse_system_info() {
    display_info!();
    println!(include_str!("./banner.txt"));
    display_info!("Platform Arch", "{}", env!("HAL_ENV_ARCH"));
    if let Ok(fdt) = get_fdt() {
        display_info!("Boot HART ID", "{}", fdt.boot_cpuid_phys());
        display_info!("Boot HART Count", "{}", fdt.find_nodes("/cpus/cpu").count());
        CPU_NUM.init_once(fdt.find_nodes("/cpus/cpu").count());
        fdt.chosen().inspect(|chosen| {
            display_info!("Boot Args", "{}", chosen.bootargs().unwrap_or(""));
        });
        fdt.memory().flat_map(|x| x.regions()).for_each(|mm| {
            display_info!(
                "Platform Memory Region",
                "{:#p} - {:#018x}",
                mm.address,
                mm.address as usize + mm.size
            );
        });
    }
    get_mem_areas().for_each(|(address, size)| {
        display_info!(
            "Platform Memory Available",
            "{:#018x} - {:#018x}",
            address,
            address + size
        );
    });
}

/// Retrieves an iterator over the registered memory areas.
///
/// # Returns
///
/// An iterator yielding references to tuples `(start, end)`, where:
/// - `start` is the starting address of a memory area.
/// - `end` is the ending address of a memory area.
///
/// # Safety
///
/// - The caller must ensure that `MEM_AREA` is properly initialized before calling this function.
/// - Since this function returns an iterator over a static memory region, concurrent modification  
///   of `MEM_AREA` while iterating may lead to undefined behavior.
pub fn get_mem_areas<'a>() -> impl Iterator<Item = &'a (usize, usize)> {
    unsafe { (*core::ptr::addr_of!(MEM_AREA)).iter() }
}

/// Adds a memory region to the memblock.
///
/// # Parameters
/// - `start` - The starting address of the memory region.
/// - `end` - The ending address of the memory region.
///
/// # Safety
///
/// - This function must be called from a single thread; concurrent access is **not** safe.
/// - The caller must ensure that [MEM_VECTOR_CAPACITY] is sufficient to accommodate the memory region,  
///   otherwise, this function may result in out-of-bounds memory access or undefined behavior.
#[allow(function_casts_as_integer)]
pub unsafe fn add_memory_region(start: usize, end: usize) {
    if end - start == 0 {
        return;
    }
    let (dtb_s, dtb_e) = DTB_INFO
        .get()
        .map(|x| (x.0.raw(), x.0.raw() + x.1))
        .unwrap_or((0, 0));
    // RISC-V 本仓库内核按物理地址链接（如 0x80200000），符号即为物理地址；
    // 其他架构使用高位虚拟窗口，需减去 VIRT_ADDR_START 得到物理范围。
    let (self_s, self_e) = {
        extern "C" {
            fn _skernel();
            fn _end();
        }
        #[cfg(target_arch = "riscv64")]
        {
            // Hard-code the kernel image bounds since this is a fixed-address kernel.
            // Base address: 0x8020_0000, RO+DATA: ~2MB, BSS: up to 0x80A0_0000
            // These constants are conservative upper bounds; the frame allocator
            // will skip the actual range via its own bounds check.
            const KERNEL_START: usize = 0x8020_0000;
            const KERNEL_END: usize = 0x80A0_0000;
            (KERNEL_START, KERNEL_END)
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            let sk = _skernel as usize;
            let en = _end as usize;
            (sk - VIRT_ADDR_START, en - VIRT_ADDR_START)
        }
    };
    if start <= self_s && self_e <= end {
        if self_s - start > 0 {
            add_memory_region(start, self_s);
        }
        if end - self_e > 0 {
            add_memory_region(self_e, end);
        }
    } else if start <= dtb_s && dtb_e <= end {
        if dtb_s - start > 0 {
            add_memory_region(start, dtb_s);
        }
        if end - dtb_e > 0 {
            add_memory_region(dtb_e, end);
        }
    } else {
        (*core::ptr::addr_of_mut!(MEM_AREA)).push((start, end - start));
    }
}
