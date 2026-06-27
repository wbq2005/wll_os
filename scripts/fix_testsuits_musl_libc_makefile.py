#!/usr/bin/env python3
"""Replace musl libc.so cp line with dual-path toolchain logic (educg vs zhou image)."""
from pathlib import Path
import sys


NEW_BLOCK = """\t@if [ -f /opt/riscv64--musl--bleeding-edge-2020.08-1/riscv64-buildroot-linux-musl/sysroot/lib/libc.so ]; then \\
\t	cp /opt/riscv64--musl--bleeding-edge-2020.08-1/riscv64-buildroot-linux-musl/sysroot/lib/libc.so sdcard/riscv/musl/lib; \\
\telif [ -f /opt/riscv64-lp64d--musl--bleeding-edge-2024.02-1/riscv64-buildroot-linux-musl/sysroot/lib/libc.so ]; then \\
\t	cp /opt/riscv64-lp64d--musl--bleeding-edge-2024.02-1/riscv64-buildroot-linux-musl/sysroot/lib/libc.so sdcard/riscv/musl/lib; \\
\telse \\
\t	echo >&2 "error: RISC-V musl libc.so not found (educg 2020.08 or zhou/lp64d 2024.02 toolchain)"; \\
\t	exit 1; \\
\tfi
"""

OLD_LINE = (
    "\tcp /opt/riscv64--musl--bleeding-edge-2020.08-1/riscv64-buildroot-linux-musl/"
    "sysroot/lib/libc.so sdcard/riscv/musl/lib\n"
)


def main() -> None:
    root = Path(sys.argv[1]).expanduser().resolve()
    makefile = root / "Makefile"
    if not makefile.is_file():
        print(f"error: not found: {makefile}", file=sys.stderr)
        sys.exit(1)
    t = makefile.read_text(encoding="utf-8")

    if OLD_LINE not in t:
        if "riscv64-lp64d--musl--bleeding-edge-2024.02-1/" in t and "musl libc.so not found" in t:
            print(f"OK (already patched): {makefile}")
            return

        print(
            "error: expected single cp line for musl libc.so not found; edit manually.",
            file=sys.stderr,
        )
        sys.exit(1)

    makefile.write_text(t.replace(OLD_LINE, NEW_BLOCK, 1), encoding="utf-8")
    print(f"OK: {makefile}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} /path/to/testsuits-for-oskernel", file=sys.stderr)
        sys.exit(2)
    main()
