Evidence level: unverified diagnostic.

Command:
python scripts\run_buildstorm.py --arch riscv64 --image D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-rv-pub.img\sdcard-rv-pub.img --stage complete --timeout 600 --memory 16G --smp 8 --build-features buildstorm-diagnostics

Code state:
Additional metadata_for_lookup()/metadata() diagnostic phases on top of the blocked-owner scheduling boundary patch and existing VFS/ext4 caches.

Result:
Timeout at 600s. BUILDSTORM_TOOLCHAIN ok and BUILDSTORM_MINIBUILD ok, but no BUILDSTORM_COMPILE success marker.

Key 480s diagnostic phases:
statx_vfs=94.38s, metadata_lookup_parent_resolve=45.43s, metadata_lookup_metadata=68.17s, metadata_memfs=22.24s, metadata_ext4_metadata_with_kind=22.95s.

Conclusion:
The next RV BuildStorm bottleneck is repeated parent symlink resolution and overlay/ext4 metadata probing during statx/open/readlink-heavy Rust builds.
