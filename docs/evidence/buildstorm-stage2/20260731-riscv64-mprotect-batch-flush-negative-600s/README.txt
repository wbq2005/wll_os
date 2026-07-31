Evidence level: unverified diagnostic.

Command:
python scripts\run_buildstorm.py --arch riscv64 --image D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-rv-pub.img\sdcard-rv-pub.img --stage complete --timeout 600 --memory 16G --smp 8 --build-features buildstorm-diagnostics

Code state:
Temporary mprotect batch-remap experiment that avoided per-page local map_page flushes and performed one full local flush at the end.

Result:
Timeout at 600s. BUILDSTORM_TOOLCHAIN ok and BUILDSTORM_MINIBUILD ok, but no BUILDSTORM_COMPILE success marker.

Key 480s diagnostic phases:
mprotect syscall id 226 total_us=103.27s, readlink_lookup=70.68s, open_vfs_and_fd=86.41s, statx_vfs=102.63s.

Conclusion:
The batch-flush experiment did not reduce mprotect cost and was reverted. The remaining RV BuildStorm blocker is dominated by VFS metadata/open/readlink throughput, with mprotect still expensive but not improved by this flush strategy.
