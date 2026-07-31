Evidence level: unverified diagnostic
Command: python scripts\run_buildstorm.py --arch riscv64 --image D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-rv-pub.img\sdcard-rv-pub.img --stage complete --timeout 600 --memory 16G --smp 8 --build-features buildstorm-diagnostics
Code state: ext4 inode metadata cache + RwLock path/dir cache + VFS parent-resolution prefix invalidation.
Result: timeout at 600s; BUILDSTORM_TOOLCHAIN ok and BUILDSTORM_MINIBUILD ok; reached equivalent/allocator-api2 after once_cell/thiserror, not complete.
Key 240s phases: readlink_parent_resolve=5.03s, statx_vfs=18.09s, readlink_lookup=28.04s.
