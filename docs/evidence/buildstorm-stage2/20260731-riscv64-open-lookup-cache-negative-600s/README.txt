Evidence level: unverified diagnostic.

Command:
python scripts\run_buildstorm.py --arch riscv64 --image D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-rv-pub.img\sdcard-rv-pub.img --stage complete --timeout 600 --memory 16G --smp 8 --build-features buildstorm-diagnostics

Code state:
Temporary open_path single ext4 lookup / mem_is_dir reuse experiment on top of the existing parent-cache and ext4 metadata cache work.

Result:
Timeout at 600s. BUILDSTORM_TOOLCHAIN ok and BUILDSTORM_MINIBUILD ok, but no BUILDSTORM_COMPILE success marker.

Key 480s diagnostic phases:
readlink_lookup=64.20s, open_vfs_and_fd=88.47s, statx_vfs=97.07s, readlink_parent_resolve=12.24s.

Conclusion:
The experiment is not a clear improvement over the prior parent-cache diagnostic and did not improve the first remaining BuildStorm blocker. It was reverted before continuing.
