Evidence level: unverified diagnostic.

Command:
python scripts\run_buildstorm.py --arch riscv64 --image D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-rv-pub.img\sdcard-rv-pub.img --stage complete --timeout 600 --memory 16G --smp 8 --build-features buildstorm-diagnostics

Code state:
Parent symlink-free fast path on top of the blocked-owner scheduling boundary patch, existing VFS/readlink/ext4 caches, and temporary metadata internal diagnostics.

Result:
Timeout at 600s. BUILDSTORM_TOOLCHAIN ok and BUILDSTORM_MINIBUILD ok, but no BUILDSTORM_COMPILE success marker.

Key 480s diagnostic phases:
statx_vfs=89.01s, metadata_lookup_parent_resolve=40.39s, metadata_lookup_metadata=66.58s, metadata_memfs=22.90s, metadata_ext4_metadata_with_kind=22.00s.

Conclusion:
The fast path is directionally positive but not sufficient. Remaining RV BuildStorm throughput is dominated by metadata overlay probing and ext4 metadata lookup.
