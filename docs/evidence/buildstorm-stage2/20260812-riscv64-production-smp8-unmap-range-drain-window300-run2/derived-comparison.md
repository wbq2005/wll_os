# Rejected unmap-range drain candidate

This candidate tested one MM hypothesis derived from the late diagnostic:
reduce `munmap` lock hold by draining the already-contiguous VMA interval,
unpublishing all affected PTEs, performing one synchronous remote shootdown,
and releasing removed resident frames only after shootdown completion.

The comparable baseline is
`20260812-riscv64-production-smp8-nested-cow-remap-window300/`. Both windows
used the unmodified official image, diagnostics disabled, QEMU 11.0.3, and
`-snapshot -m 8G -smp 8` for 300 seconds after the begin marker.

| metric | baseline | candidate | change |
| --- | ---: | ---: | ---: |
| `Compiling` lines | 23 | 23 | 0% |
| last crate | `ax-posix-api` | `ax-posix-api` | unchanged |
| first timed crate | 177.413 s | 174.230 s | 1.79% faster |
| 23rd crate | 187.227 s | 185.047 s | 1.16% faster |

Four cfg builds, dual production release builds, and dual-architecture SMP
lifecycle regressions passed. Host evidence showed zero swap, negligible
storage utilization, and activity on all eight TCG threads. The candidate
still failed the 5% retention floor and was reverted from production source.

The first attempted run did not reach the guest harness because a manually
prebuilt kernel omitted the runner's compile-time harness environment. It is
not performance evidence. Run 2 was rebuilt by the host runner and is the
only candidate window used above.

Candidate kernel SHA-256 is
`0dc6c3e5469a7e28ef439c01058141d8418a6893701cc1092496cc3867a5d914`;
image SHA-256 is
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`.
There is no `BUILDSTORM_COMPILE mode=multi ok=true` marker.
