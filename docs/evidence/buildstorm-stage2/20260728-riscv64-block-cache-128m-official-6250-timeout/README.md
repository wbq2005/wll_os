# RISC-V64 128 MiB block-cache official timeout

Classification: `unverified`.

The unmodified official image and script ran with `-m 16G -smp 8` and the
6250-second external timeout. The raw log contains the official toolchain and
minibuild success markers, but no `BUILDSTORM_COMPILE mode=multi ok=true`.
The official judge therefore awards only 20.0/180 scripted points. This
directory is not official-pass evidence and does not satisfy the submission
gate.

The runner JSON records the exact QEMU arguments, QEMU version, production
kernel SHA-256, release-build command and host elapsed time. `official-judge.txt`
is the unmodified judge output. The image, suite script and judge identities
are recorded in the cumulative design document.
