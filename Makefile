# OS contest 2026 - Makefile

ARCH ?= riscv64
INIT ?= test
INTERACTIVE ?= 0
LOG ?= OFF
DEV_PRELOAD ?= 0
LIBCTEST ?= 0
IOZONE ?= 1
LMBENCH ?= 1
LTP ?= 0
# Diagnostic builds add lifecycle markers and aggregate counters without
# changing production behavior.  Keep the production default disabled; the
# diagnostic submission archive sets this to 1 explicitly.
BUILDSTORM_DIAGNOSTICS ?= 0
# Final submissions must not stop after the CAgent task.  Keep both official
# finals groups in the plain `make all` artifact; focused runners override this
# value explicitly when they need one workload in isolation.
HARNESS_GROUPS ?= cagent,buildstorm
# The 2026 finals score only the glibc suites.  Keep musl available as an
# explicit capability run without launching a second, unscored BuildStorm by
# default after the scored glibc workload completes.
HARNESS_LIBC ?= glibc
FOCUSED_LTP_CASES_BASE := writev01,setegid02,getgroups01,setgroups01,setgroups02,setgroups03,setgroups04,access01,open02,setfsuid01,setfsgid01,faccessat01,access02,open03,symlink01,readlink01,lstat01,lstat02,symlink02,symlink03,symlink04,symlinkat01,readlinkat01,setitimer02,setitimer01,getitimer01,getitimer02,mkdir02,mkdir03,mkdir04,mkdir05,rmdir01,rmdir02,rmdir03,fallocate02,fallocate03,mknod09,open10,open11,open12,unlink09,futex_wait05,prctl08,prctl09,epoll_wait02,epoll_pwait03,epoll_wait01,epoll_wait03,epoll_wait04,epoll_ctl01,epoll_ctl02,epoll_create1_01,epoll_create1_02,splice01,splice03,splice04,splice07,eventfd2_01,eventfd2_02,eventfd2_03,eventfd01,eventfd02,eventfd03,eventfd04,eventfd05,chown02,chown04,fchown04,lchown02,fchmod04,fchmod05,fchmod06,utimes01,truncate03,getsid02,waitid07,waitid08,waitid10
FOCUSED_LTP_CASES_20260629 := copy_file_range01,creat01,creat03,creat04,creat05,faccessat02,faccessat201,faccessat202,fallocate04,fchmodat01,fchmodat02,fchownat01,fchownat02,fcntl14,fcntl17,fcntl36,ftruncate03,getxattr01,getxattr02,link02,link04,link05,linkat01,listxattr01,mkdirat01,mkdirat02,open14,openat01,openat02,openat03,openat04,openat201,openat202,openat203,readlinkat02,removexattr01,rename01,rename03,rename04,rename05,rename06,rename07,rename08,rename09,rename10,rename12,rename13,rename14,renameat201,renameat202,sendfile06,setxattr01,setxattr02,statx01,statx02,statx03,truncate02,unlinkat01
FOCUSED_LTP_CASES_20260630 := fcntl23
FOCUSED_LTP_CASES_20260630_PGRP := getpgid01,getpgid02,setpgid01,setpgid02,setsid01,getsid01
FOCUSED_LTP_CASES_20260630_NET := socket01,socket02,socketpair01,socketpair02,bind01,bind02,listen01,accept01,accept4_01,connect01,sendto01,recvfrom01,getsockname01,getpeername01,getsockopt01,setsockopt01
FOCUSED_LTP_CASES ?= $(FOCUSED_LTP_CASES_BASE),$(FOCUSED_LTP_CASES_20260629),$(FOCUSED_LTP_CASES_20260630),$(FOCUSED_LTP_CASES_20260630_PGRP),$(FOCUSED_LTP_CASES_20260630_NET)
RUSTUP_TOOLCHAIN ?= $(shell sed -n 's/^channel[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' rust-toolchain.toml 2>/dev/null | head -n 1)
ifeq ($(strip $(RUSTUP_TOOLCHAIN)),)
    # Keep judge builds on the repository-pinned compiler. Falling back to the
    # host default is only for checkouts without rust-toolchain.toml.
    RUSTUP_TOOLCHAIN := $(shell rustup default 2>/dev/null | sed 's/ .*//')
endif
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
ifeq ($(DEV_PRELOAD),1)
    DEV_PRELOAD_EXTRA := --features dev-preload
else
    DEV_PRELOAD_EXTRA :=
endif
ifeq ($(LIBCTEST),1)
    LIBCTEST_EXTRA := --features libctest
else
    LIBCTEST_EXTRA :=
endif
ifeq ($(IOZONE),1)
    IOZONE_EXTRA := --features iozone
else
    IOZONE_EXTRA :=
endif
ifeq ($(LMBENCH),1)
    LMBENCH_EXTRA := --features lmbench
else
    LMBENCH_EXTRA :=
endif
ifeq ($(LTP),1)
    LTP_EXTRA := --features ltp
else
    LTP_EXTRA :=
endif
ifeq ($(BUILDSTORM_DIAGNOSTICS),1)
    BUILDSTORM_DIAGNOSTICS_EXTRA := --features buildstorm-diagnostics
else
    BUILDSTORM_DIAGNOSTICS_EXTRA :=
endif
ifneq ($(strip $(LTP_CASES)),)
    LTP_CASES_ENV := LTP_CASES="$(LTP_CASES)"
else
    LTP_CASES_ENV :=
endif
NO_PRELOAD_PATTERN := _testcode\.sh|busybox_cmd\.txt|testcase busybox

.PHONY: all build clean check check-sdcard check-kernel-no-preload prepare-cargo-config unpack-sdcard print-phase2-gate print-ltp-cases
all:
	@echo "Building for RISC-V..."
	$(MAKE) ARCH=riscv64 build INIT=$(INIT) LOG=$(LOG) LTP=1 LTP_CASES=$(FOCUSED_LTP_CASES) BUILDSTORM_DIAGNOSTICS=$(BUILDSTORM_DIAGNOSTICS)
	cp target/riscv64gc-unknown-none-elf/release/wll_OS kernel-rv
	$(MAKE) check-kernel-no-preload
	@echo "RISC-V build done: kernel-rv"
	@echo "Building for LoongArch..."
	$(MAKE) ARCH=loongarch64 build INIT=$(INIT) LOG=$(LOG) LTP=1 LTP_CASES=$(FOCUSED_LTP_CASES) BUILDSTORM_DIAGNOSTICS=$(BUILDSTORM_DIAGNOSTICS)
	cp target/loongarch64-unknown-none/release/wll_OS kernel-la
	@echo "LoongArch build done: kernel-la"

print-phase2-gate:
	@echo "[phase2] basic 子集建议覆盖: fork clone pipe yield wait waitpid exit（wait4 阻塞 + pipe2 O_NONBLOCK）。"
	@echo "[phase3] RISC-V QEMU 附加 virtio-blk + ext4 命令见 README.md「Phase 3 运行时根盘」。"

print-ltp-cases:
	@printf '%s\n' '$(FOCUSED_LTP_CASES)'

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
	@if [ -f scripts/restore_vendor_hidden.sh ]; then tr -d '\015' < scripts/restore_vendor_hidden.sh | bash; fi
	@if [ -f scripts/refresh_vendor_checksums.py ]; then \
		if command -v python3 >/dev/null 2>&1; then \
			python3 scripts/refresh_vendor_checksums.py; \
		elif command -v python >/dev/null 2>&1; then \
			python scripts/refresh_vendor_checksums.py; \
		else \
			echo "error: python3/python is required to refresh vendor checksums"; \
			exit 1; \
		fi; \
	fi
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
	@if [ "$(DEV_PRELOAD)" = "1" ]; then $(MAKE) check-sdcard ARCH=$(ARCH); fi
	cd os && WLL_INTERACTIVE=$(INTERACTIVE) WLL_HARNESS_GROUPS=$(HARNESS_GROUPS) WLL_HARNESS_LIBC=$(HARNESS_LIBC) $(LTP_CASES_ENV) cargo +$(RUSTUP_TOOLCHAIN) build --locked --offline --release --target $(TARGET) $(CARGO_EXTRA) $(DEV_PRELOAD_EXTRA) $(LIBCTEST_EXTRA) $(IOZONE_EXTRA) $(LMBENCH_EXTRA) $(LTP_EXTRA) $(BUILDSTORM_DIAGNOSTICS_EXTRA)

# 快速检查（不做链接，更快，适合开发阶段验证代码）
check:
	@echo "Checking kernel for $(ARCH)..."
	$(MAKE) prepare-cargo-config
	cd os && WLL_INTERACTIVE=$(INTERACTIVE) WLL_HARNESS_GROUPS=$(HARNESS_GROUPS) WLL_HARNESS_LIBC=$(HARNESS_LIBC) $(LTP_CASES_ENV) cargo +$(RUSTUP_TOOLCHAIN) check --locked --offline --release --target $(TARGET) $(CARGO_EXTRA) $(DEV_PRELOAD_EXTRA) $(LIBCTEST_EXTRA) $(IOZONE_EXTRA) $(LMBENCH_EXTRA) $(LTP_EXTRA) $(BUILDSTORM_DIAGNOSTICS_EXTRA)

check-kernel-no-preload:
	@if [ ! -f kernel-rv ]; then \
		echo "error: kernel-rv missing; build/copy it before running the preload content check"; \
		exit 1; \
	fi
	@if command -v rg >/dev/null 2>&1; then \
		if rg -a '$(NO_PRELOAD_PATTERN)' kernel-rv; then \
			echo "error: kernel-rv embeds test harness data; default builds must rely on runtime EXT4"; \
			exit 1; \
		else \
			echo "kernel-rv preload content check passed"; \
		fi; \
	else \
		echo "warning: rg not found; using grep -aE fallback for preload content check"; \
		if grep -aE '$(NO_PRELOAD_PATTERN)' kernel-rv; then \
			echo "error: kernel-rv embeds test harness data; default builds must rely on runtime EXT4"; \
			exit 1; \
		else \
			echo "kernel-rv preload content check passed"; \
		fi; \
	fi

# 清理
clean:
	cd os && cargo clean
	@rm -f kernel-rv kernel-la
