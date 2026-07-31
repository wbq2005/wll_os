# RISC-V64 readlink result cache diagnostic

Classification: `unverified`.

This run used the unmodified official RISC-V64 BuildStorm image with `-m 16G
-smp 8` and the `buildstorm-diagnostics` kernel feature. It is not official
score evidence.

The readlink result cache reduced repeated memfs/ext4 readlink probing. At the
240-second snapshot, `readlink_lookup` dropped from 44.9s in the previous
diagnostic to 31.7s, while `readlink_memfs + readlink_ext4` dropped from about
10.5s to about 0.32s. By the later snapshot, `statx_vfs` and `open_vfs_and_fd`
remained dominant, so VFS metadata lookup is still the next bottleneck.
