# RISC-V64 mmap fallback production partial run

Classification: `unverified`.

This was a production RISC-V64 BuildStorm run using the unmodified official
image and `-m 16G -smp 8`, after the mmap fallback and lazy mprotect fixes. It
was manually stopped because the pre-build `cargo build -p tg-xtask` was still
in early dependencies after a long observation window and was not on pace to
finish within the 6250-second external timeout.

The raw serial log shows `BUILDSTORM_TOOLCHAIN ok`, `BUILDSTORM_MINIBUILD ok`,
and progress through early pre-build crates with no `failed to spawn thread`
panic and no temporary diagnostic lines. No complete marker was reached.
