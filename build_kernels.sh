#!/usr/bin/env bash
set -euo pipefail

cd /work

echo "[1/6] 清理易冲突目录..."
rm -rf os/.cargo

echo "[2/6] 确保镜像已解压..."
make unpack-sdcard

echo "[3/6] 使用容器内 target 目录..."
export CARGO_TARGET_DIR=/tmp/os-target
LTP_CASES="$(make -s print-ltp-cases)"

echo "[4/6] 编译 RISC-V..."
rm -rf os/.cargo
make ARCH=riscv64 build INIT=test LOG=OFF LTP=1 LTP_CASES="$LTP_CASES"
cp /tmp/os-target/riscv64gc-unknown-none-elf/release/wll_OS kernel-rv

echo "[5/6] 编译 LoongArch..."
rm -rf os/.cargo
make ARCH=loongarch64 build INIT=test LOG=OFF LTP=1 LTP_CASES="$LTP_CASES"
cp /tmp/os-target/loongarch64-unknown-none/release/wll_OS kernel-la

echo "[6/6] 产物检查..."
ls -lh kernel-rv kernel-la
echo "完成。"
