# QEMU RISC-V 启动日志解析

## 整体启动流程

```
QEMU → OpenSBI (M-mode) → 内核 (S-mode)
```

## 日志详解

### 1. OpenSBI 信息

```
OpenSBI v1.7
```
- **OpenSBI**: RISC-V 的 Supervisor Binary Interface 实现，运行在最高特权级 M-mode
- **v1.7**: 版本号

### 2. 平台信息

```
Platform Name               : riscv-virtio,qemu
Platform Features           : medeleg
Platform HART Count         : 1
```
- **Platform**: QEMU 的 virt 虚拟平台
- **HART**: Hardware Thread，即 CPU 核心数，这里是 1 核

### 3. 设备信息

```
Platform Console Device     : uart8250
Platform Timer Device       : aclint-mtimer @ 10000000Hz
```
- **Console**: 使用 8250 UART 串口（我们的代码操作的就是这个设备）
- **Timer**: 定时器，10MHz

### 4. 固件内存布局

```
Firmware Base               : 0x80000000
Firmware Size               : 317 KB
```
- OpenSBI 固件加载在物理地址 `0x80000000`

### 5. 关键：内核跳转地址

```
Domain0 Next Address        : 0x0000000080200000
Domain0 Next Mode           : S-mode
```
- **Next Address**: OpenSBI 将跳转到 `0x80200000` 执行我们的内核
- **Next Mode**: 进入 S-mode（Supervisor Mode，内核运行的特权级）

### 6. CPU 信息

```
Boot HART ID                : 0
Boot HART Priv Version      : v1.12
Boot HART Base ISA          : rv64imafdch
```
- **HART ID**: 0 号 CPU 启动
- **ISA**: rv64imafdch - 64位，支持整数、乘除、原子、浮点、压缩、超visor等扩展

### 7. 我们的内核输出

```
Hello, OS!
RISC-V 64 Kernel Started.
```
- 内核成功启动！
- 这是通过直接写 UART 寄存器输出的字符串

## 启动流程图

```
┌─────────┐    ┌─────────────┐    ┌─────────────┐
│  QEMU   │ →  │  OpenSBI    │ →  │   内核      │
│ 启动    │    │ (M-mode)    │    │ (S-mode)    │
└─────────┘    └─────────────┘    └─────────────┘
                    │                    │
                    ▼                    ▼
              初始化硬件            输出 "Hello, OS!"
              加载内核到            进入停机循环
              0x80200000
              跳转到内核
```

## 为什么内核能运行

1. **OpenSBI 完成硬件初始化**：设置中断、定时器、串口等
2. **OpenSBI 加载内核**：将 `kernel-rv` ELF 文件加载到 `0x80200000`
3. **OpenSBI 设置特权级**：从 M-mode 切换到 S-mode
4. **跳转到 `_start`**：执行我们的汇编入口代码
5. **设置栈指针**：为 Rust 代码准备栈
6. **调用 `rust_main()`**：执行 Rust 内核代码
7. **输出字符串**：通过 UART 发送字符到终端
