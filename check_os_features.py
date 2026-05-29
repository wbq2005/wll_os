#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
操作系统 Week 1/2 任务完成状态检测脚本
检测并验证各项功能的实现状态
"""

import os
import re
import sys
from pathlib import Path
from dataclasses import dataclass
from typing import Optional, List, Tuple

# 设置 stdout 编码
if sys.platform == 'win32':
    import io
    sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8', errors='replace')
    sys.stderr = io.TextIOWrapper(sys.stderr.buffer, encoding='utf-8', errors='replace')

# 使用 ASCII 符号替代 Unicode
CHECK_PASS = "[OK]"
CHECK_FAIL = "[X]"
CHECK_WARN = "[?]"
BULLET = "*"

@dataclass
class CheckResult:
    name: str
    status: str  # "已完成", "部分完成", "未实现"
    details: List[str]
    files_checked: List[str]
    success_output: str

class OSFeatureChecker:
    def __init__(self, os_path: str):
        self.os_path = Path(os_path)
        self.mm_path = self.os_path / "src" / "mm"
        self.syscall_path = self.os_path / "src" / "syscall"
        self.task_path = self.os_path / "src" / "task"
        self.trap_path = self.os_path / "src" / "trap"

    def read_file(self, filepath: Path) -> Optional[str]:
        """读取文件内容"""
        try:
            return filepath.read_text(encoding='utf-8')
        except Exception as e:
            return None

    def check_buddy_allocator(self) -> CheckResult:
        """检查物理页帧分配器 (Buddy System)"""
        result = CheckResult(
            name="物理页帧分配器 (Buddy System)",
            status="未实现",
            details=[],
            files_checked=[],
            success_output="""
[Buddy Allocator Test]
- Frame allocation: OK
- Frame deallocation: OK
- FrameTracker RAII: OK
- Memory regions added: 2
- Free frames: 16384 (64MB / 4KB)
Test PASSED
"""
        )

        filepath = self.mm_path / "frame_allocator.rs"
        result.files_checked.append(str(filepath))
        content = self.read_file(filepath)

        if not content:
            result.details.append("文件不存在或无法读取")
            return result

        checks = [
            ("buddy_system_allocator crate", r"use buddy_system_allocator::FrameAllocator"),
            ("FrameAllocator 定义", r"static ref FRAME_ALLOCATOR.*FrameAllocator"),
            ("alloc_frame 函数", r"pub fn alloc_frame\(\)"),
            ("dealloc_frame 函数", r"pub fn dealloc_frame\("),
            ("add_frames_range 函数", r"pub fn add_frames_range\("),
            ("FrameTracker RAII", r"impl Drop for FrameTracker"),
        ]

        passed = 0
        for name, pattern in checks:
            if re.search(pattern, content):
                result.details.append(f"{CHECK_PASS} {name}")
                passed += 1
            else:
                result.details.append(f"{CHECK_FAIL} {name}")

        if passed >= 5:
            result.status = "已完成"
        elif passed >= 3:
            result.status = "部分完成"

        return result

    def check_heap_allocator(self) -> CheckResult:
        """检查内核堆分配器"""
        result = CheckResult(
            name="内核堆分配器",
            status="未实现",
            details=[],
            files_checked=[],
            success_output="""
[Heap Allocator Test]
- Kernel heap size: 8MB (0x800000)
- LockedHeap initialized: OK
- Allocation test: 1024 bytes at 0xFFFFFFFFC0200000
- Deallocation test: OK
- alloc_error_handler: registered
Test PASSED
"""
        )

        filepath = self.mm_path / "heap_allocator.rs"
        result.files_checked.append(str(filepath))
        content = self.read_file(filepath)

        if not content:
            result.details.append("文件不存在或无法读取")
            return result

        checks = [
            ("LockedHeap crate", r"use buddy_system_allocator::LockedHeap"),
            ("KERNEL_HEAP_SIZE 定义", r"const KERNEL_HEAP_SIZE"),
            ("全局分配器", r"#\[global_allocator\]"),
            ("HEAP_ALLOCATOR", r"static HEAP_ALLOCATOR.*LockedHeap"),
            ("init_heap 函数", r"pub fn init_heap\(\)"),
            ("alloc_error_handler", r"#\[alloc_error_handler\]"),
        ]

        passed = 0
        for name, pattern in checks:
            if re.search(pattern, content):
                result.details.append(f"{CHECK_PASS} {name}")
                passed += 1
            else:
                result.details.append(f"{CHECK_FAIL} {name}")

        if passed >= 5:
            result.status = "已完成"
        elif passed >= 3:
            result.status = "部分完成"

        return result

    def check_page_table(self) -> CheckResult:
        """检查页表管理 (SV39)"""
        result = CheckResult(
            name="页表管理 (SV39 for RISC-V)",
            status="未实现",
            details=[],
            files_checked=[],
            success_output="""
[Page Table Test]
- SV39 mode: enabled
- Page size: 4KB
- PTE flags: V|R|W|X|U|G|A|D
- map_page(0x1000, 0x80200000, R|W): OK
- translate(0x1000): 0x80200000
- unmap_page(0x1000): OK
- TLB flush: OK
Test PASSED
"""
        )

        filepath = self.mm_path / "page_table.rs"
        result.files_checked.append(str(filepath))
        content = self.read_file(filepath)

        if not content:
            result.details.append("文件不存在或无法读取")
            return result

        checks = [
            ("PageTable 导入", r"use polyhal::pagetable"),
            ("PTEFlags 定义", r"PTEFlags"),
            ("init_kernel_page_table", r"pub fn init_kernel_page_table\(\)"),
            ("map_page 函数", r"pub fn map_page\("),
            ("unmap_page 函数", r"pub fn unmap_page\("),
            ("translate 函数", r"pub fn translate\("),
            ("SV39 标志位", r"V.*R.*W.*X"),
        ]

        passed = 0
        for name, pattern in checks:
            if re.search(pattern, content):
                result.details.append(f"{CHECK_PASS} {name}")
                passed += 1
            else:
                result.details.append(f"{CHECK_FAIL} {name}")

        # 检查是否使用 polyhal 的封装
        if re.search(r"PageTableWrapper|PageTable", content):
            result.details.append("注: 使用 polyhal 的页表抽象层")

        if passed >= 5:
            result.status = "已完成"
        elif passed >= 3:
            result.status = "部分完成"

        return result

    def check_kernel_page_table_init(self) -> CheckResult:
        """检查内核页表初始化"""
        result = CheckResult(
            name="内核页表初始化",
            status="未实现",
            details=[],
            files_checked=[],
            success_output="""
[Kernel Page Table Init Test]
- Kernel space mapping: 0x80200000 - 0xFFFFFFFFC0200000
- Text segment: mapped (R-X)
- Data segment: mapped (RW-)
- BSS segment: mapped (RW-)
- Stack: mapped (RW-)
- SATP register: 0x8000000000080200
- CPU mode: S-mode with paging
Test PASSED
"""
        )

        # 检查 page_table.rs
        filepath = self.mm_path / "page_table.rs"
        result.files_checked.append(str(filepath))
        content = self.read_file(filepath)

        if content and re.search(r"pub fn init_kernel_page_table", content):
            result.details.append(f"{CHECK_PASS} init_kernel_page_table() 函数存在")
        else:
            result.details.append(f"{CHECK_FAIL} init_kernel_page_table() 函数不存在")

        # 检查调用位置
        main_file = self.os_path / "src" / "main.rs"
        if main_file.exists():
            main_content = self.read_file(main_file)
            if main_content and re.search(r"init_kernel_page_table|mm::init\(\)", main_content):
                result.details.append(f"{CHECK_PASS} 在 main.rs 中被调用")
            else:
                result.details.append(f"{CHECK_WARN} 未在 main.rs 中直接调用")

        # 检查 task 模块
        task_mod = self.task_path / "mod.rs"
        if task_mod.exists():
            task_content = self.read_file(task_mod)
            if task_content and re.search(r"init_kernel_page", task_content):
                result.details.append(f"{CHECK_PASS} 在 task::init_kernel_page() 中被调用")

        if len([d for d in result.details if d.startswith(CHECK_PASS)]) >= 2:
            result.status = "已完成"
        elif len([d for d in result.details if d.startswith(CHECK_PASS)]) >= 1:
            result.status = "部分完成"

        return result

    def check_memory_set(self) -> CheckResult:
        """检查用户地址空间管理 (MemorySet)"""
        result = CheckResult(
            name="用户地址空间管理 (MemorySet)",
            status="未实现",
            details=[],
            files_checked=[],
            success_output="""
[MemorySet Test]
- MemorySet::new_bare(): OK
- insert_framed_area(0x1000, 0x2000, R|W): OK
- Page table created: 0x80203000
- Frame allocation: 1 page
- from_kernel(): cloned kernel space
- activate(): SATP = 0x8000000000080300
- satp_token(): valid SV39 token
- unmap_area(0x1000, 0x2000): OK
- Clone for fork: OK
Test PASSED
"""
        )

        filepath = self.mm_path / "memory_set.rs"
        result.files_checked.append(str(filepath))
        content = self.read_file(filepath)

        if not content:
            result.details.append("文件不存在或无法读取")
            return result

        checks = [
            ("MemorySet 结构体", r"pub struct MemorySet"),
            ("new_bare 方法", r"pub fn new_bare\("),
            ("insert_framed_area", r"pub fn insert_framed_area\("),
            ("from_kernel", r"pub fn from_kernel\("),
            ("activate", r"pub fn activate\("),
            ("satp_token", r"pub fn satp_token\("),
            ("remove_area/unmap_area", r"(remove_area|unmap_area)"),
            ("Clone 实现", r"impl Clone for MemorySet"),
        ]

        passed = 0
        for name, pattern in checks:
            if re.search(pattern, content):
                result.details.append(f"{CHECK_PASS} {name}")
                passed += 1
            else:
                result.details.append(f"{CHECK_FAIL} {name}")

        if passed >= 6:
            result.status = "已完成"
        elif passed >= 4:
            result.status = "部分完成"

        return result

    def check_write_syscall(self) -> CheckResult:
        """检查 write syscall"""
        result = CheckResult(
            name="write syscall (输出到控制台)",
            status="未实现",
            details=[],
            files_checked=[],
            success_output="""
[Write Syscall Test]
- sys_write(1, "Hello", 5): 5 bytes written
- stdout output: Hello
- sys_write(2, "Error", 5): 5 bytes written
- stderr output: Error
- Null buffer check: EFAULT
- Zero count check: 0
- Large count check: OK
Test PASSED
"""
        )

        # 检查 fs.rs
        fs_file = self.syscall_path / "fs.rs"
        result.files_checked.append(str(fs_file))
        fs_content = self.read_file(fs_file)

        # 检查 mod.rs
        mod_file = self.syscall_path / "mod.rs"
        result.files_checked.append(str(mod_file))
        mod_content = self.read_file(mod_file)

        if fs_content:
            if re.search(r"pub fn sys_write", fs_content):
                result.details.append(f"{CHECK_PASS} sys_write() 函数存在")
            else:
                result.details.append(f"{CHECK_FAIL} sys_write() 函数不存在")

            if re.search(r"fd.*1.*stdout|stdout.*fd.*1", fs_content, re.IGNORECASE):
                result.details.append(f"{CHECK_PASS} 支持 stdout (fd=1)")
            if re.search(r"fd.*2.*stderr|stderr.*fd.*2", fs_content, re.IGNORECASE):
                result.details.append(f"{CHECK_PASS} 支持 stderr (fd=2)")
            if re.search(r"putchar|print", fs_content, re.IGNORECASE):
                result.details.append(f"{CHECK_PASS} 调用 putchar/print 输出")

        if mod_content:
            if re.search(r"SYSCALL_WRITE.*=.*64|sys_write", mod_content):
                result.details.append(f"{CHECK_PASS} 已注册到 syscall 分发器")
            else:
                result.details.append(f"{CHECK_FAIL} 未注册到 syscall 分发器")

        if len([d for d in result.details if d.startswith(CHECK_PASS)]) >= 3:
            result.status = "已完成"
        elif len([d for d in result.details if d.startswith(CHECK_PASS)]) >= 2:
            result.status = "部分完成"

        return result

    def check_trap_framework(self) -> CheckResult:
        """检查 Trap/中断处理框架"""
        result = CheckResult(
            name="Trap/中断处理框架 (polyhal-trap集成)",
            status="未实现",
            details=[],
            files_checked=[],
            success_output="""
[Trap Framework Test]
- Trap handler registered: OK
- stvec register: 0xFFFFFFFFC0200000
- Exception delegation: OK
- Interrupt delegation: OK
- User->Kernel switch: OK
- Kernel->User switch: OK
- Context save/restore: OK
Test PASSED
"""
        )

        # 检查 trap 模块
        trap_mod = self.trap_path / "mod.rs"
        result.files_checked.append(str(trap_mod))
        content = self.read_file(trap_mod)

        if not content:
            result.details.append("文件不存在或无法读取")
            return result

        checks = [
            ("trap_handler", r"fn trap_handler|pub fn handle_trap"),
            ("trap init", r"pub fn init\(\)|trap_init"),
            ("stvec 设置", r"stvec|TrapHandler"),
            ("context switch", r"TrapFrame|context"),
            ("syscall dispatch", r"syscall|handle_syscall"),
        ]

        passed = 0
        for name, pattern in checks:
            if re.search(pattern, content, re.IGNORECASE):
                result.details.append(f"{CHECK_PASS} {name}")
                passed += 1
            else:
                result.details.append(f"{CHECK_FAIL} {name}")

        if passed >= 4:
            result.status = "已完成"
        elif passed >= 2:
            result.status = "部分完成"

        return result

    def check_timer_interrupt(self) -> CheckResult:
        """检查定时器中断"""
        result = CheckResult(
            name="定时器中断",
            status="未实现",
            details=[],
            files_checked=[],
            success_output="""
[Timer Interrupt Test]
- Timer interrupt enabled: OK
- sip.STIP: set
- sie.STIE: enabled
- Timebase frequency: 10000000 Hz
- Set next timer: +100000 ticks
- Timer handler called: 10 times
- preemptive scheduling: working
Test PASSED
"""
        )

        # 检查 timer 模块
        timer_mod = self.os_path / "src" / "timer.rs"
        result.files_checked.append(str(timer_mod))
        content = self.read_file(timer_mod)

        if not content:
            result.details.append("文件不存在或无法读取")
            return result

        checks = [
            ("timer_init", r"pub fn init\(\)|timer_init"),
            ("set_next_trigger", r"set_next_timer|set_next_trigger"),
            ("timer interrupt handler", r"timer_handler|handle_timer"),
            ("timebase frequency", r"CLOCK_FREQ|timebase"),
        ]

        passed = 0
        for name, pattern in checks:
            if re.search(pattern, content, re.IGNORECASE):
                result.details.append(f"{CHECK_PASS} {name}")
                passed += 1
            else:
                result.details.append(f"{CHECK_FAIL} {name}")

        if passed >= 3:
            result.status = "已完成"
        elif passed >= 2:
            result.status = "部分完成"

        return result

    def check_context_switch(self) -> CheckResult:
        """检查基础上下文切换"""
        result = CheckResult(
            name="基础上下文切换",
            status="未实现",
            details=[],
            files_checked=[],
            success_output="""
[Context Switch Test]
- Task A running
- Context switch A -> B
- Task B running
- Context switch B -> A
- Task A resumed (PC: 0x80201234)
- Registers preserved: x1-x31
- satp preserved: 0x8000000000080200
- sepc preserved: 0x80201234
Test PASSED
"""
        )

        # 检查 task 模块
        task_mod = self.task_path / "mod.rs"
        result.files_checked.append(str(task_mod))
        content = self.read_file(task_mod)

        if not content:
            result.details.append("文件不存在或无法读取")
            return result

        checks = [
            ("TaskControlBlock", r"pub struct TaskControlBlock"),
            ("TaskStatus", r"enum TaskStatus|Ready|Running"),
            ("run_tasks", r"pub fn run_tasks\("),
            ("suspend_current", r"suspend_current|yield_now"),
            ("schedule", r"fn schedule\(|do_schedule"),
            ("TaskContext", r"TaskContext|context"),
        ]

        passed = 0
        for name, pattern in checks:
            if re.search(pattern, content, re.IGNORECASE):
                result.details.append(f"{CHECK_PASS} {name}")
                passed += 1
            else:
                result.details.append(f"{CHECK_FAIL} {name}")

        if passed >= 4:
            result.status = "已完成"
        elif passed >= 2:
            result.status = "部分完成"

        return result

    def check_basic_syscalls(self) -> CheckResult:
        """检查 read/exit 基础 syscall"""
        result = CheckResult(
            name="read/exit 基础 syscall",
            status="未实现",
            details=[],
            files_checked=[],
            success_output="""
[Basic Syscalls Test]
- sys_read(0, buf, 10): blocking wait
- sys_exit(0): process terminated
- sys_getpid(): returns 1
- sys_fork(): returns child pid 2
- sys_exec("/bin/sh"): OK
- sys_waitpid(-1, &status, 0): waits for child
Test PASSED
"""
        )

        mod_file = self.syscall_path / "mod.rs"
        result.files_checked.append(str(mod_file))
        mod_content = self.read_file(mod_file)

        fs_file = self.syscall_path / "fs.rs"
        result.files_checked.append(str(fs_file))
        fs_content = self.read_file(fs_file)

        # 检查 process.rs 中的实际函数定义
        process_file = self.syscall_path / "process.rs"
        result.files_checked.append(str(process_file))
        process_content = self.read_file(process_file)

        syscalls_to_check = [
            ("sys_read", r"pub fn sys_read", fs_content),
            ("sys_exit", r"pub fn sys_exit", process_content),
            ("sys_getpid", r"pub fn sys_getpid", process_content),
            ("sys_fork", r"pub fn sys_fork|SYSCALL_CLONE", process_content),
            ("sys_exec", r"pub fn sys_exec|SYSCALL_EXECVE", process_content),
            ("sys_waitpid", r"pub fn sys_wait|SYSCALL_WAIT4", process_content),
        ]

        passed = 0
        for name, pattern, content in syscalls_to_check:
            found = False
            if content and re.search(pattern, content, re.IGNORECASE):
                found = True
            elif mod_content and re.search(pattern.replace("pub fn ", ""), mod_content, re.IGNORECASE):
                found = True

            if found:
                result.details.append(f"{CHECK_PASS} {name}")
                passed += 1
            else:
                result.details.append(f"{CHECK_FAIL} {name}")

        if passed >= 4:
            result.status = "已完成"
        elif passed >= 2:
            result.status = "部分完成"

        return result

    def run_all_checks(self) -> List[CheckResult]:
        """运行所有检查"""
        print("=" * 70)
        print("操作系统 Week 1/2 功能检测脚本")
        print("=" * 70)
        print(f"检测路径: {self.os_path}")
        print()

        results = []

        # Week 1 检查
        print("-" * 70)
        print("Week 1 任务检查")
        print("-" * 70)

        results.append(self.check_buddy_allocator())
        results.append(self.check_heap_allocator())
        results.append(self.check_page_table())
        results.append(self.check_kernel_page_table_init())
        results.append(self.check_memory_set())
        results.append(self.check_write_syscall())

        # Week 2 检查
        print()
        print("-" * 70)
        print("Week 2 任务检查")
        print("-" * 70)

        results.append(self.check_trap_framework())
        results.append(self.check_timer_interrupt())
        results.append(self.check_context_switch())
        results.append(self.check_basic_syscalls())

        return results

    def print_report(self, results: List[CheckResult]):
        """打印检测报告"""
        print()
        print("=" * 70)
        print("检测详情报告")
        print("=" * 70)

        week1_completed = 0
        week1_total = 6
        week2_completed = 0
        week2_total = 4

        for i, result in enumerate(results):
            # Week 1 vs Week 2 分隔
            if i == 6:
                print()
                print("-" * 70)
                print("Week 2 详情")
                print("-" * 70)

            print()
            print(f"[{i+1}] {result.name}")
            print(f"    状态: {result.status}")

            if result.status == "已完成":
                if i < 6:
                    week1_completed += 1
                else:
                    week2_completed += 1

            print(f"    检查文件:")
            for f in result.files_checked:
                print(f"      - {f}")

            print(f"    检查项:")
            for detail in result.details:
                print(f"      {detail}")

            # 成功输出示例
            if result.status == "已完成":
                print(f"    预期成功输出:")
                for line in result.success_output.strip().split('\n'):
                    print(f"      {line}")

        # 总结
        print()
        print("=" * 70)
        print("总结")
        print("=" * 70)
        print(f"Week 1: {week1_completed}/{week1_total} 项已完成")
        print(f"Week 2: {week2_completed}/{week2_total} 项已完成")
        print()

        if week1_completed == week1_total:
            print("[OK] Week 1 全部完成，可以进入 Week 2")
        elif week1_completed >= 4:
            print("[?] Week 1 基本完成，建议完成剩余任务")
        else:
            print("[X] Week 1 尚未完成，继续开发")

        if week2_completed == week2_total:
            print("[OK] Week 2 全部完成，可以进行第1次提交")
        elif week2_completed > 0:
            print(f"[?] Week 2 进行中 ({week2_completed}/{week2_total})")


def main():
    # 自动检测 os 目录
    script_dir = Path(__file__).parent.absolute()
    os_path = script_dir / "os"

    # 如果当前目录下有 os 目录，使用当前目录
    if not os_path.exists():
        current_os = Path.cwd() / "os"
        if current_os.exists():
            os_path = current_os
        else:
            # 尝试从命令行参数获取
            if len(sys.argv) > 1:
                os_path = Path(sys.argv[1])
            else:
                print("错误: 找不到 os 目录")
                print(f"请确保在包含 'os' 目录的位置运行脚本")
                print(f"或提供路径: python {sys.argv[0]} <path_to_os>")
                sys.exit(1)

    if not os_path.exists():
        print(f"错误: 路径不存在: {os_path}")
        sys.exit(1)

    checker = OSFeatureChecker(os_path)
    results = checker.run_all_checks()
    checker.print_report(results)

    # 返回码
    week1_status = sum(1 for r in results[:6] if r.status == "已完成")
    if week1_status >= 6:
        sys.exit(0)  # 全部完成
    else:
        sys.exit(1)  # 有未完成项


if __name__ == "__main__":
    main()
