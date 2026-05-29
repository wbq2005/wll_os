/// ELF 文件格式解析和加载器
///
/// 用于解析 ELF 文件并加载到进程的地址空间
use alloc::vec::Vec;
use polyhal::VirtAddr;

use crate::mm::memory_set::MemorySet;
use crate::mm::page_table::PTEFlags;
use crate::utils::error::SysErrNo;

/// ELF 魔数
pub const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

/// ELF 文件头
#[repr(C)]
#[derive(Debug)]
pub struct ElfHeader {
    /// 魔数 [0x7f, 'E', 'L', 'F']
    pub magic: [u8; 4],
    /// 位宽: 1=32位, 2=64位
    pub class: u8,
    /// 字节序: 1=小端, 2=大端
    pub data: u8,
    /// ELF 版本
    pub version: u8,
    /// ABI 类型
    pub os_abi: u8,
    /// ABI 版本
    pub abi_version: u8,
    /// 填充
    pub pad: [u8; 7],
    /// 文件类型: 1=可重定位, 2=可执行, 3=共享对象
    pub e_type: u16,
    /// 目标架构
    pub e_machine: u16,
    /// ELF 版本
    pub e_version: u32,
    /// 程序入口地址
    pub e_entry: usize,
    /// 程序头表偏移
    pub e_phoff: usize,
    /// 节头表偏移
    pub e_shoff: usize,
    /// 标志
    pub e_flags: u32,
    /// ELF 头大小
    pub e_ehsize: u16,
    /// 程序头表项大小
    pub e_phentsize: u16,
    /// 程序头表项数量
    pub e_phnum: u16,
    /// 节头表项大小
    pub e_shentsize: u16,
    /// 节头表项数量
    pub e_shnum: u16,
    /// 节名字符串表索引
    pub e_shstrndx: u16,
}

/// 程序头（段描述符）
#[repr(C)]
#[derive(Debug, Clone)]
pub struct ProgramHeader {
    /// 段类型
    pub p_type: u32,
    /// 标志
    pub p_flags: u32,
    /// 段在文件中的偏移
    pub p_offset: usize,
    /// 段在内存中的虚拟地址
    pub p_vaddr: usize,
    /// 段的物理地址（未使用）
    pub p_paddr: usize,
    /// 段在文件中的大小
    pub p_filesz: usize,
    /// 段在内存中的大小
    pub p_memsz: usize,
    /// 对齐方式
    pub p_align: usize,
}

/// 段类型
pub const PT_NULL: u32 = 0;     // 忽略
pub const PT_LOAD: u32 = 1;     // 可加载段
pub const PT_DYNAMIC: u32 = 2;  // 动态链接信息
pub const PT_INTERP: u32 = 3;   // 解释器路径
pub const PT_NOTE: u32 = 4;     // 辅助信息
pub const PT_SHLIB: u32 = 5;    // 保留
pub const PT_PHDR: u32 = 6;     // 程序头表

/// ELF 文件解析器
pub struct ElfFile<'a> {
    /// ELF 数据
    pub data: &'a [u8],
    /// ELF 头
    pub header: &'a ElfHeader,
    /// 程序头列表
    pub program_headers: Vec<&'a ProgramHeader>,
}

impl<'a> ElfFile<'a> {
    /// 从字节数组解析 ELF 文件
    pub fn parse(data: &'a [u8]) -> Result<Self, SysErrNo> {
        // 检查文件大小
        if data.len() < core::mem::size_of::<ElfHeader>() {
            log::error!("[elf] File too small for ELF header");
            return Err(SysErrNo::ENOEXEC);
        }

        // 检查魔数
        if data[0..4] != ELF_MAGIC {
            log::error!("[elf] Invalid ELF magic: {:x?}", &data[0..4]);
            return Err(SysErrNo::ENOEXEC);
        }

        // 解析 ELF 头
        let header = unsafe { &*(data.as_ptr() as *const ElfHeader) };

        // 检查是否为 64 位
        if header.class != 2 {
            log::error!("[elf] Not a 64-bit ELF file: {}", header.class);
            return Err(SysErrNo::ENOEXEC);
        }

        // 检查是否为可执行文件或共享对象
        if header.e_type != 2 && header.e_type != 3 {
            log::error!("[elf] Not an executable or shared object: {}", header.e_type);
            return Err(SysErrNo::ENOEXEC);
        }

        // 检查目标架构 (RISC-V = 243, LoongArch = 258)
        #[cfg(target_arch = "riscv64")]
        if header.e_machine != 243 {
            log::error!("[elf] Wrong architecture: {}, expected RISC-V (243)", header.e_machine);
            return Err(SysErrNo::ENOEXEC);
        }

        #[cfg(target_arch = "loongarch64")]
        if header.e_machine != 258 {
            log::error!("[elf] Wrong architecture: {}, expected LoongArch (258)", header.e_machine);
            return Err(SysErrNo::ENOEXEC);
        }

        // 解析程序头表
        let mut program_headers = Vec::new();
        let phoff = header.e_phoff;
        let phnum = header.e_phnum as usize;
        let phentsize = header.e_phentsize as usize;

        for i in 0..phnum {
            let offset = phoff + i * phentsize;
            if offset + phentsize > data.len() {
                log::error!("[elf] Program header {} out of bounds", i);
                return Err(SysErrNo::ENOEXEC);
            }
            let ph = unsafe { &*(data.as_ptr().add(offset) as *const ProgramHeader) };
            program_headers.push(ph);
        }

        log::info!("[elf] Parsed ELF file: entry={:#x}, {} program headers", header.e_entry, phnum);

        Ok(ElfFile {
            data,
            header,
            program_headers,
        })
    }

    /// 获取入口点地址
    pub fn entry(&self) -> usize {
        self.header.e_entry
    }

    pub fn interp_path(&self) -> Option<&'a str> {
        let ph = self.program_headers.iter().find(|ph| ph.p_type == PT_INTERP)?;
        let start = ph.p_offset;
        let end = start.checked_add(ph.p_filesz)?;
        if start >= end || end > self.data.len() {
            return None;
        }
        let bytes = &self.data[start..end];
        let nul = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        core::str::from_utf8(&bytes[..nul]).ok()
    }

    /// 加载 ELF 到内存空间
    ///
    /// 返回 (MemorySet, 用户栈顶地址, 入口地址)
    pub fn phdr_vaddr(&self, bias: usize) -> usize {
        self.program_headers.iter()
            .find(|ph| ph.p_type == PT_PHDR)
            .map(|ph| ph.p_vaddr + bias)
            .unwrap_or_else(|| {
                self.program_headers.iter()
                    .filter(|ph| ph.p_type == PT_LOAD)
                    .map(|ph| ph.p_vaddr)
                    .min()
                    .unwrap_or(0)
                    + self.header.e_phoff
                    + bias
            })
    }

    pub fn phnum(&self) -> usize {
        self.header.e_phnum as usize
    }

    pub fn entry_with_bias(&self, bias: usize) -> usize {
        self.entry() + bias
    }

    pub fn load_segments_into(&self, memory_set: &mut MemorySet, bias: usize) -> Result<(), SysErrNo> {
        for ph in &self.program_headers {
            if ph.p_type != PT_LOAD {
                continue;
            }
            let start_va = VirtAddr::new(ph.p_vaddr + bias);
            let end_va = VirtAddr::new(ph.p_vaddr + ph.p_memsz + bias);
            let mut flags = PTEFlags::U | PTEFlags::V;
            if ph.p_flags & 1 != 0 { flags |= PTEFlags::X; }
            if ph.p_flags & 2 != 0 { flags |= PTEFlags::W; }
            if ph.p_flags & 4 != 0 { flags |= PTEFlags::R; }
            memory_set.insert_framed_area(start_va, end_va, flags);

            if ph.p_filesz > 0 {
                let src_start = ph.p_offset;
                let src_end = ph.p_offset + ph.p_filesz;
                if src_end > self.data.len() {
                    return Err(SysErrNo::ENOEXEC);
                }
                let src = &self.data[src_start..src_end];
                let page_size = crate::config::PAGE_SIZE;
                let mut vaddr = ph.p_vaddr + bias;
                let mut src_offset = 0usize;
                while src_offset < src.len() {
                    if let Some(paddr) = memory_set.translate(VirtAddr::new(vaddr)) {
                        let page_offset = vaddr % page_size;
                        let copy_len = (src.len() - src_offset).min(page_size - page_offset);
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                src[src_offset..].as_ptr(),
                                paddr.get_mut_ptr::<u8>(),
                                copy_len,
                            );
                        }
                        src_offset += copy_len;
                    }
                    vaddr += page_size - (vaddr % page_size);
                }
            }

            if ph.p_memsz > ph.p_filesz {
                let bss_start = ph.p_vaddr + ph.p_filesz + bias;
                let bss_end = ph.p_vaddr + ph.p_memsz + bias;
                for addr in bss_start..bss_end {
                    if let Some(pa) = memory_set.translate(VirtAddr::new(addr)) {
                        unsafe { *(pa.raw() as *mut u8) = 0; }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn load(&self) -> Result<(MemorySet, usize, usize), SysErrNo> {
        // UART marker: 'A' = elf.load() entry
        let mut memory_set = MemorySet::from_kernel();

        // 加载所有 LOAD 段
        for ph in &self.program_headers {
            if ph.p_type == PT_LOAD {
                log::info!(
                    "[elf] Loading segment: vaddr={:#x}, filesz={:#x}, memsz={:#x}, offset={:#x}",
                    ph.p_vaddr, ph.p_filesz, ph.p_memsz, ph.p_offset
                );

                // 计算虚拟地址范围（页对齐）
                let start_va = VirtAddr::new(ph.p_vaddr);
                let end_va = VirtAddr::new(ph.p_vaddr + ph.p_memsz);

                // 确定权限
                let mut flags = PTEFlags::U | PTEFlags::V; // 用户可访问
                if ph.p_flags & 1 != 0 {
                    flags |= PTEFlags::X; // 可执行
                }
                if ph.p_flags & 2 != 0 {
                    flags |= PTEFlags::W; // 可写
                }
                if ph.p_flags & 4 != 0 {
                    flags |= PTEFlags::R; // 可读
                }

                // 为段分配物理页帧并建立映射
                // UART marker: 'C' = before insert_framed_area
                memory_set.insert_framed_area(start_va, end_va, flags);
                // UART marker: 'D' = after insert_framed_area
                // UART marker: 'F' = after all segment processing
                // 复制文件内容到内存（按页批量复制，避免逐字节翻译）
                if ph.p_filesz > 0 {
                    let src_start = ph.p_offset;
                    let src_end = ph.p_offset + ph.p_filesz;
                    if src_end > self.data.len() {
                        log::error!("[elf] Segment data out of bounds");
                        return Err(SysErrNo::ENOEXEC);
                    }
                    let src = &self.data[src_start..src_end];

                    // 按页复制：每个页内先计算偏移再整页 memcpy
                    let page_size = crate::config::PAGE_SIZE;
                    let mut vaddr_page = ph.p_vaddr;
                    let mut src_offset = 0usize;

                    while src_offset < src.len() {
                        let va = VirtAddr::new(vaddr_page);
                        if let Some(paddr) = memory_set.translate(va) {
                            // 计算本轮复制量（取整到页边界，最后一轮可能不足一页）
                            let page_start_in_vaddr = vaddr_page - (vaddr_page / page_size * page_size);
                            let remaining_in_page = page_size - page_start_in_vaddr;
                            let copy_len = (src.len() - src_offset).min(remaining_in_page);

                            let dst_ptr = paddr.get_mut_ptr::<u8>();
                            unsafe {
                                core::ptr::copy_nonoverlapping(
                                    src[src_offset..].as_ptr(),
                                    dst_ptr,
                                    copy_len,
                                );
                            }
                            src_offset += copy_len;
                        }
                        vaddr_page += page_size;
                    }
                }

                // 清零 BSS 段 (p_memsz > p_filesz 的区域)
                if ph.p_memsz > ph.p_filesz {
                    let bss_start = ph.p_vaddr + ph.p_filesz;
                    let bss_end = ph.p_vaddr + ph.p_memsz;
                    let page_size = crate::config::PAGE_SIZE;

                    // 逐页清零 BSS 区域
                    let mut vaddr = (bss_start / page_size) * page_size;
                    while vaddr < bss_end {
                        let va = VirtAddr::new(vaddr);
                        if let Some(paddr) = memory_set.translate(va) {
                            let page_start = vaddr;
                            let page_end = (vaddr + page_size).min(bss_end);
                            let bss_in_this_page = page_end - bss_start.max(page_start);
                            if bss_in_this_page > 0 {
                                let zero_start = if page_start < bss_start {
                                    bss_start - page_start
                                } else {
                                    0
                                };
                                let dst_ptr = paddr.get_mut_ptr::<u8>();
                                unsafe {
                                    core::ptr::write_bytes(dst_ptr.add(zero_start), 0, bss_in_this_page);
                                }
                            }
                        }
                        vaddr += page_size;
                    }
                }
            }
        }

        // 分配用户栈
        // 用户栈从高地址向下增长
        let user_stack_top = crate::config::USER_STACK_TOP;
        let user_stack_bottom = user_stack_top - crate::config::USER_STACK_SIZE;
        log::info!("[elf] Allocating user stack: {:#x} - {:#x}", user_stack_bottom, user_stack_top);

        memory_set.insert_framed_area(
            VirtAddr::new(user_stack_bottom),
            VirtAddr::new(user_stack_top),
            PTEFlags::U | PTEFlags::R | PTEFlags::W | PTEFlags::V,
        );

        // UART marker: 'E' = all load done
        Ok((memory_set, user_stack_top, self.entry()))
    }
}

/// 检查数据是否为有效的 ELF 文件
pub fn is_elf_file(data: &[u8]) -> bool {
    data.len() >= 4 && data[0..4] == ELF_MAGIC
}
