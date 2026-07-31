Evidence level: unverified diagnostic.

Command:
python scripts\run_buildstorm.py --arch riscv64 --image D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-rv-pub.img\sdcard-rv-pub.img --stage complete --timeout 600 --memory 16G --smp 8 --build-features buildstorm-diagnostics

Code state:
Blocked-owner scheduling boundary patch on top of existing VFS/readlink/ext4 metadata cache and ASID work.

Result:
Timeout at 600s. BUILDSTORM_TOOLCHAIN ok and BUILDSTORM_MINIBUILD ok, but no BUILDSTORM_COMPILE success marker.

Key 480s diagnostic phases:
readlink_lookup=69.51s, open_vfs_and_fd=88.39s, statx_vfs=97.14s, readlink_parent_resolve=13.11s.

Additional finding:
The previous Ready/running_cpu/blocking_cpu ownership anomaly disappeared in the final diagnostic snapshot, indicating the patch fixed a real scheduler state-machine issue. Remaining throughput blockers are mprotect/page-table cost plus VFS metadata/open/readlink cost.
