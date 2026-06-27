# 与 Makefile 中 -Z build-std、.cargo 里 [unstable] build-std 一致，使用 nightly
FROM rust:bookworm

# 设置环境变量
ENV DEBIAN_FRONTEND=noninteractive

# 安装系统依赖和交叉编译工具链
RUN apt-get update && apt-get install -y \
    # 基础工具
    build-essential \
    make \
    git \
    curl \
    wget \
    vim \
    nano \
    # QEMU 模拟器
    qemu-system-riscv64 \
    qemu-system-misc \
    qemu-utils \
    # RISC-V 交叉编译工具链
    gcc-riscv64-linux-gnu \
    binutils-riscv64-linux-gnu \
    # LoongArch 交叉编译工具链（如果需要）
    gcc-loongarch64-linux-gnu \
    binutils-loongarch64-linux-gnu \
    # 调试工具
    gdb-multiarch \
    # 其他工具
    device-tree-compiler \
    && rm -rf /var/lib/apt/lists/*

# 默认工具链与评测/仓库 rust-toolchain.toml 一致（挂载项目后仍受 toolchain 文件约束）
RUN rustup default nightly && \
    rustup component add rust-src llvm-tools-preview && \
    rustup target add riscv64gc-unknown-none-elf && \
    rustup target add loongarch64-unknown-none

# 安装 cargo-binutils 用于 objcopy 等工具
RUN cargo install cargo-binutils

# 创建工作目录
WORKDIR /workspace

# 设置默认命令
CMD ["/bin/bash"]
