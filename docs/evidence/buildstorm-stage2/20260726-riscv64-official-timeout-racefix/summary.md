# RISC-V64 official complete timeout after activation-race fix

- Classification: `unverified`
- Configuration: unmodified official image, `-m 16G -smp 8`, `-snapshot`
- External timeout: 6250 seconds
- Host elapsed: 6250.391 seconds
- Kernel SHA-256: `1ca1dd2811acd36a11ed5fed39a2e6f5ccebb45a1dac84f4696ad81523db8857`
- Official suite commit: `1eac61d3becaa592c8ef12a7535f0ec6bb9e3e36`
- Result: toolchain and minibuild passed; no `BUILDSTORM_COMPILE` marker
- Last serial progress: untimed `tg-xtask` pre-build, `Compiling hashbrown v0.17.1`
- Panic/QEMU failure marker: none
- Official judge: 20.0/180 scripted points

This run used the source after moving active-address-space publication into
the locked `MemorySet::activate()` path. It is not an official complete pass
and does not satisfy the commit gate.
