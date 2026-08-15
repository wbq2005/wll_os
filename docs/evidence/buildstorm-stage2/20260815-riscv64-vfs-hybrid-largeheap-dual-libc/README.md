# RISC-V64 dual-libc heap lifecycle validation

## Classification

- `official-pass` for the glibc BuildStorm result: the unchanged official
  image, required `-m 8G -smp 8` resources, raw serial log, and official judge
  produce `180/180` scripted points.
- `capability-pass` for the complete glibc-to-musl lifecycle: the direct
  single-QEMU launch is intentionally needed to keep the kernel alive after
  the glibc marker, so this proves the heap capability but is not a separate
  platform score claim.

## Configuration

- Architecture: RISC-V64
- QEMU: `QEMU emulator version 11.0.3`
- QEMU resources: `-m 8G -smp 8 -snapshot`
- Image: `/srv/buildstorm/images/sdcard-rv-pub.img`
- Kernel source commit: `2acfed66b145ee0c48b55cc1046513ab20290a17`
- Build environment: `WLL_HARNESS_GROUPS=buildstorm`, `WLL_HARNESS_LIBC` unset
- Single QEMU instance; no concurrent runner remained after completion

## Result

The serial log contains both phases in one kernel lifetime:

```text
BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=872.80 cores=8 bytes=1683456 arch=riscv64
#### OS COMP TEST GROUP END buildstorm-glibc ####
#### OS COMP TEST GROUP START buildstorm-multi-musl ####
BUILDSTORM_RESULT mode=multi status=OK rc=0 cores=8 elapsed_s=912.33 artifact=target/riscv64-unknown-linux-musl/release/arceos-helloworld bytes=1683456
#### OS COMP TEST GROUP END buildstorm-multi-musl ####
[harness] ALL TESTS DONE, shutting down
```

No `Heap allocation error`, `Kernel panic`, or `No runnable init task` marker
was observed. The prior evaluator log failed immediately after entering musl
with a 277,827,240-byte allocation request. The tested change reserves up to
one quarter of RAM for the dynamic kernel heap, capped at 2 GiB, while
preserving the buddy allocator ownership and locking protocol.

The official judge result is summarized in `official-judge.txt`; it reports
`180.0 / 180.0` and `elapsed=873s` for the RISC-V64 glibc compile marker.

## Provenance

- Raw serial: `serial.log`
- Kernel SHA-256: `kernel.sha256`
- Image SHA-256: `image.sha256`
- Source status: `kernel-source-status.txt`
- Heap diff used by the run: retained in the local uncommitted evidence copy
