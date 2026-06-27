# 评测环境搭建与使用方法

本文档说明如何在本地搭建 OS 内核评测环境,获取真实分数。

---

## 一、评测机制概述

### 1.1 评分方式

评分系统 (`autotest-for-oskernel`) 从内核的**屏幕串口输出 (stdout)** 中解析测试结果。

评分脚本 (`judge_*.py`) 通过正则表达式解析特定格式的标记行来分割每个测试用例的输出数据,然后对数据进行断言验证。

### 1.2 输出标记格式

评分脚本期望的格式:

```
========== START <test_name> ==========
<test output line 1>
<test output line 2>
...
========== END <test_name> ==========
```

**关键**: `========== START ... ==========` 和 `========== END ... ==========` 是评分脚本的分割符。中间的内容是测试程序的 stdout 输出,评分脚本通过正则表达式验证其内容。

### 1.3 测试用例分类

根据 `autotest-for-oskernel/kernel/judge/` 中的脚本,测试分为:

| 测试类型 | 脚本 | 说明 |
|---|---|---|
| **basic-glibc** | `judge_basic-glibc.py` | glibc 基础系统调用测试 |
| **basic-musl** | `judge_basic-musl.py` | musl 基础系统调用测试 |
| **busybox-glibc** | `judge_busybox-glibc.py` | busybox 工具测试 |
| **busybox-musl** | `judge_busybox-musl.py` | busybox 工具测试 (musl) |
| **ltp-glibc** | `judge_ltp-glibc.py` | Linux Test Project syscall 测试 |
| **ltp-musl** | `judge_ltp-musl.py` | Linux Test Project syscall 测试 (musl) |
| **cyclictest-glibc** | `judge_cyclictest-glibc.py` | 实时性能测试 |
| **cyclictest-musl** | `judge_cyclictest-musl.py` | 实时性能测试 (musl) |

各测试类型的输出格式要求不同:
- **basic**: 期望 `========== START/END <name> ==========` 标记,中间为测试程序的原始 stdout
- **busybox**: 期望 `testcase <name> success/fail` 格式
- **ltp**: 期望 `RUN LTP CASE ... / END LTP CASE ...` 格式
- **cyclictest**: 期望特定的性能数据格式

---

## 二、完整本地评测环境搭建

### 2.1 准备工作目录

```bash
# 创建工作目录
mkdir evaluation && cd evaluation

# 克隆 autotest 仓库
git clone https://github.com/oscomp/autotest-for-oskernel.git

# 拉取 Docker 镜像
sudo docker pull zhouzhouyi/os-contest:20260510
```

### 2.2 准备测试数据

```bash
# 创建测试数据目录
mkdir testdata

# 复制评分脚本
cd autotest-for-oskernel
cp -rf kernel/judge/* ../testdata
cd ..

# 下载 SD 卡镜像 (RISC-V 和 LoongArch)
cd testdata
curl -L -o sdcard-rv.img.xz https://github.com/oscomp/testsuits-for-oskernel/releases/download/pre-20250615/sdcard-rv.img.xz
curl -L -o sdcard-la.img.xz https://github.com/oscomp/testsuits-for-oskernel/releases/download/pre-20250615/sdcard-la.img.xz

# 解压并压缩为 gzip 格式 (评分脚本期望 gz 格式)
unxz sdcard-rv.img.xz
unxz sdcard-la.img.xz
gzip sdcard-rv.img
gzip sdcard-la.img
cd ..
```

### 2.3 打包评分 Python 内核

```bash
cd autotest-for-oskernel/kernel
zip ../kernel.zip -r *
cd ../..
```

### 2.4 运行评测

```bash
sudo docker run --rm \
  -v $(pwd)/wll_os-master1:/coursegrader/submit \
  -v $(pwd)/testdata:/coursegrader/testdata \
  -v $(pwd)/autotest-for-oskernel:/cg \
  -v $(pwd)/testdata:/mnt/cghook/ \
  zhouzhouyi/os-contest:20260510 \
  python3 /cg/kernel.zip
```

> **注意**: 将 `wll_os-master1` 替换为你实际的内核代码目录路径。

---

## 三、内核代码编译

### 3.1 编译命令

```bash
# 进入项目目录
cd wll_os-master1

# 编译两个架构的内核
make all

# 或者分别编译:
# make build ARCH=riscv64    # 编译 RISC-V 内核 -> kernel-rv
# make build ARCH=loongarch64 # 编译 LoongArch 内核 -> kernel-la
```

编译完成后会生成两个内核文件:
- `kernel-rv`: RISC-V 64位内核
- `kernel-la`: 龙芯 LoongArch64 位内核

### 3.2 工具链要求

Docker 镜像中已安装:
- Rust nightly-2025-01-18
- `riscv64gc-unknown-none-elf` target
- `loongarch64-unknown-none` target
- QEMU 9.2.1 (包含 qemu-system-riscv64 和 qemu-system-loongarch64)

### 3.3 可选: 本地构建

如果你想本地构建:
```bash
rustup install nightly-2026-05-05
rustup target add riscv64gc-unknown-none-elf --toolchain nightly-2026-05-05
rustup target add loongarch64-unknown-none --toolchain nightly-2026-05-05
rustup component add rust-src --toolchain nightly-2026-05-05

make all
```

---

## 四、QEMU 本地运行测试

### 4.1 RISC-V QEMU 命令

```bash
# 需要先解压 sdcard 镜像 (非 xz 格式)
unxz sdcard-rv.img.xz

qemu-system-riscv64 \
  -machine virt \
  -kernel kernel-rv \
  -m 1G \
  -nographic \
  -smp 1 \
  -bios default \
  -drive file=sdcard-rv.img,if=none,format=raw,id=x0 \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  -no-reboot \
  -device virtio-net-device,netdev=net \
  -netdev user,id=net \
  -rtc base=utc
```

### 4.2 LoongArch QEMU 命令

```bash
unxz sdcard-la.img.xz

qemu-system-loongarch64 \
  -kernel kernel-la \
  -m 1G \
  -nographic \
  -smp 1 \
  -drive file=sdcard-la.img,if=none,format=raw,id=x0 \
  -device virtio-blk-pci,drive=x0,bus=virtio-mmio-bus.0 \
  -no-reboot \
  -device virtio-net-pci,netdev=net0 \
  -netdev user,id=net0,hostfwd=tcp::5555-:5555,hostfwd=udp::5555-:5555 \
  -rtc base=utc
```

---

## 五、大赛提交

1. 登录比赛网站 **course.educg.net**
2. 创建 GitLab 账号并上传内核代码
3. 在比赛平台填写 GitLab 项目 HTTPS 地址并提交
4. 等待自动评测完成,查看分数

### 5.1 提交要求

- 项目根目录必须包含 `Makefile`,执行 `make all` 后生成 `kernel-rv` 和 `kernel-la`
- 可选: 生成 `disk.img` 作为辅助磁盘镜像
- QEMU 会自动挂载官方的 sdcard 镜像 (包含测试用例)
- 内核需要扫描 sdcard 并运行测试脚本,输出结果到串口
- 测试完成后调用关机命令

---

## 六、第一阶段修复内容

### 修复 1: LoongArch 系统调用死循环 (P0)

**问题**: LoongArch trap handler 处理 syscall 异常时没有跳过 syscall 指令地址 (`era += 4`),导致 `ertn` 后 CPU 重新执行同一条 syscall 指令,陷入无限循环。

**文件**: `patch/polyhal-trap/src/trap/loongarch64.rs`

**修复前**:
```rust
Trap::Exception(Exception::Syscall) => TrapType::SysCall,
```

**修复后**:
```rust
Trap::Exception(Exception::Syscall) => {
    tf.era += 4;
    TrapType::SysCall
}
```

---

### 修复 2: RISC-V 评分输出格式不匹配 (P0)

**问题**: 评分脚本 `judge_basic-*.py` 寻找 `========== START <name> ==========` 和 `========== END <name> ==========` 标记来分割测试输出。原有 harness 输出的是 `#### OS COMP TEST GROUP START ... ####` 格式,评分脚本无法解析,导致所有测试的 `len(data) < N` 断言全部失败,得分 0。

**文件**: `os/src/task/mod.rs` 中的 `run_one_test_binary` 函数

**修复**: 添加评分脚本期望的标记:

```rust
// 测试开始前
console_write("========== START ");
console_write(name);
console_write(" ==========\n");

// 测试结束后
console_write("========== END ");
console_write(name);
console_write(" ==========\n");

// 出错提前返回时也需添加 END 标记
```

---

## 七、后续优化方向 (第二阶段)

1. **RISC-V**: 检查测试程序 stdout 输出格式是否匹配 `judge_basic-*.py` 中的正则断言
2. **LoongArch**: 验证 ext4 挂载后文件系统是否正常工作
3. **busybox 测试**: 适配 `testcase <name> success/fail` 输出格式
4. **LTP 测试**: 适配 `RUN LTP CASE / END LTP CASE` 输出格式
5. **ext4 write 支持**: 部分测试需要写文件,需确保 ext4 write 操作正确
6. **clock_gettime / nanosleep**: 时间相关 syscall 的正确实现
7. **mmap/munmap**: 内存映射 syscall 的正确实现

---

## 八、相关资源链接

| 资源 | 链接 |
|---|---|
| autotest-for-oskernel 仓库 | https://github.com/oscomp/autotest-for-oskernel |
| testsuits-for-oskernel 仓库 | https://github.com/oscomp/testsuits-for-oskernel |
| 官方 Docker 镜像 | `zhouzhouyi/os-contest:20260510` |
| 比赛平台 | http://course.educg.net |
| 评测工具链 | https://github.com/zhouzhouyi-hub/os-contest-image |
| RISC-V sdcard 镜像 | https://github.com/oscomp/testsuits-for-oskernel/releases/download/pre-20250615/sdcard-rv.img.xz |
| LoongArch sdcard 镜像 | https://github.com/oscomp/testsuits-for-oskernel/releases/download/pre-20250615/sdcard-la.img.xz |
