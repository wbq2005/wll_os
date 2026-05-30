# OS contest 2026 - Makefile

ARCH ?= riscv64
INIT ?= test
LOG ?= OFF
RUSTUP_TOOLCHAIN ?= $(shell rustup default 2>/dev/null | sed 's/ .*//')
ifeq ($(findstring nightly,$(RUSTUP_TOOLCHAIN)),)
    RUSTUP_TOOLCHAIN := $(shell rustup toolchain list 2>/dev/null | sed -n 's/^\(nightly[^ ]*\).*/\1/p' | head -n 1)
endif
ifeq ($(strip $(RUSTUP_TOOLCHAIN)),)
    RUSTUP_TOOLCHAIN := nightly
endif

ifeq ($(ARCH),riscv64)
    TARGET := riscv64gc-unknown-none-elf
    CARGO_EXTRA :=
else ifeq ($(ARCH),loongarch64)
    TARGET := loongarch64-unknown-none
    CARGO_EXTRA := --no-default-features --features loongarch
endif

.PHONY: all build clean check check-sdcard prepare-cargo-config unpack-sdcard print-phase2-gate
all:
	@echo "Building for RISC-V..."
	$(MAKE) ARCH=riscv64 build INIT=$(INIT) LOG=$(LOG)
	cp target/riscv64gc-unknown-none-elf/release/wll_OS kernel-rv
	@echo "RISC-V build done: kernel-rv"
	@echo "Building for LoongArch..."
	$(MAKE) ARCH=loongarch64 build INIT=$(INIT) LOG=$(LOG)
	cp target/loongarch64-unknown-none/release/wll_OS kernel-la
	@echo "LoongArch build done: kernel-la"

print-phase2-gate:
	@echo "[phase2] basic 子集建议覆盖: fork clone pipe yield wait waitpid exit（wait4 阻塞 + pipe2 O_NONBLOCK）。"
	@echo "[phase3] RISC-V QEMU 附加 virtio-blk + ext4 命令见 README.md「Phase 3 运行时根盘」。"

check-sdcard:
	@if [ "$(ARCH)" = "riscv64" ]; then \
		IMG="sdcard-rv.img"; \
	else \
		IMG="sdcard-la.img"; \
	fi; \
	IMG_XZ="$$IMG.xz"; \
	if [ ! -f "$$IMG" ] && [ -f "$$IMG_XZ" ]; then \
		if command -v xz >/dev/null 2>&1; then \
			echo "检测到 $$IMG 缺失，正在自动解压 $$IMG_XZ ..."; \
			if xz -dc "$$IMG_XZ" > "$$IMG.tmp" && mv -f "$$IMG.tmp" "$$IMG"; then \
				echo "已自动生成 $$IMG"; \
			else \
				rm -f "$$IMG.tmp"; \
				echo "错误: 自动解压 $$IMG_XZ 失败。"; \
				exit 1; \
			fi; \
		else \
			echo "警告: 缺少 $$IMG，且当前环境无 xz，无法从 $$IMG_XZ 自动解压。继续编译（评测/运行时依赖 virtio ext4 或本地稍后解压镜像）。"; \
		fi; \
	fi; \
	if [ ! -f "$$IMG" ]; then \
		echo "警告: 缺少 $$IMG；os/build.rs 将使用空 MemFS 预载。评测/运行时应由 virtio 块设备上的 ext4 提供 /init；本地可 make unpack-sdcard 或放置 sdcard-*.img 用于预载与 QEMU。"; \
	fi

prepare-cargo-config:
	@mkdir -p .cargo
	@if [ -d vendor ]; then find vendor -name cargo-checksum.json -exec sh -c 'for f do cp "$$f" "$$(dirname "$$f")/.cargo-checksum.json"; done' sh {} +; fi
	cp oscargo/config.toml .cargo/config.toml

unpack-sdcard:
	@if command -v xz >/dev/null 2>&1; then \
		if [ -f sdcard-rv.img.xz ] && [ ! -f sdcard-rv.img ]; then \
			if xz -dc sdcard-rv.img.xz > sdcard-rv.img.tmp && mv -f sdcard-rv.img.tmp sdcard-rv.img; then \
				echo "已解压 sdcard-rv.img"; \
			else \
				rm -f sdcard-rv.img.tmp; \
				echo "错误: 解压 sdcard-rv.img.xz 失败"; \
				exit 1; \
			fi; \
		fi; \
		if [ -f sdcard-la.img.xz ] && [ ! -f sdcard-la.img ]; then \
			if xz -dc sdcard-la.img.xz > sdcard-la.img.tmp && mv -f sdcard-la.img.tmp sdcard-la.img; then \
				echo "已解压 sdcard-la.img"; \
			else \
				rm -f sdcard-la.img.tmp; \
				echo "错误: 解压 sdcard-la.img.xz 失败"; \
				exit 1; \
			fi; \
		fi; \
	else \
		echo "警告: 未检测到 xz，请手动解压 sdcard-rv.img.xz / sdcard-la.img.xz"; \
	fi

# 编译目标
# rustflags 已全部写入 os/Cargo.toml，无需复制 cargo_config 目录
build:
	@echo "Building kernel for $(ARCH)..."
	$(MAKE) prepare-cargo-config
	$(MAKE) check-sdcard ARCH=$(ARCH)
	cd os && cargo +$(RUSTUP_TOOLCHAIN) build --locked --offline --release --target $(TARGET) $(CARGO_EXTRA)

# 快速检查（不做链接，更快，适合开发阶段验证代码）
check:
	@echo "Checking kernel for $(ARCH)..."
	$(MAKE) prepare-cargo-config
	cd os && cargo +$(RUSTUP_TOOLCHAIN) check --locked --offline --release --target $(TARGET) $(CARGO_EXTRA)

# 清理
clean:
	cd os && cargo clean
	@rm -f kernel-rv kernel-la
