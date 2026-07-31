Evidence level: unverified diagnostic
Command: python scripts\run_buildstorm.py --arch riscv64 --image D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-rv-pub.img\sdcard-rv-pub.img --stage complete --timeout 600 --memory 16G --smp 8 --build-features buildstorm-diagnostics
Code state: ext4 inode metadata cache + RwLock path/dir cache; before VFS parent-cache scoped invalidation patch.
Result: timeout at 600s; BUILDSTORM_TOOLCHAIN ok and BUILDSTORM_MINIBUILD ok; reached early cargo pre-build crates, not complete.
