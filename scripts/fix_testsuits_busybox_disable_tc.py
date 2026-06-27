#!/usr/bin/env python3
"""
Disable BusyBox 'tc' applet in testsuits config/*/busybox-config-*.

Newer linux UAPI removes CBQ (TCA_CBQ_*); busybox 1.33 networking/tc.c breaks.
"""
from pathlib import Path
import re
import sys


def main() -> None:
    root = Path(sys.argv[1]).expanduser().resolve()
    pat = re.compile(
        r"^CONFIG_TC=y\s*\n^CONFIG_FEATURE_TC_INGRESS=y\s*$",
        re.MULTILINE,
    )
    repl = "# CONFIG_TC is not set\n# CONFIG_FEATURE_TC_INGRESS is not set"
    for name in ("busybox-config-riscv64", "busybox-config-loongarch64"):
        p = root / "config" / name
        if not p.is_file():
            print(f"error: not found: {p}", file=sys.stderr)
            sys.exit(1)
        t = p.read_text(encoding="utf-8").replace("\r\n", "\n")
        if "# CONFIG_TC is not set" in t:
            print(f"OK (already): {p}")
            continue
        new_t, n = pat.subn(repl, t, count=1)
        if n != 1:
            print(f"error: CONFIG_TC=y block not found: {p}", file=sys.stderr)
            sys.exit(1)
        p.write_text(new_t, encoding="utf-8")
        print(f"OK: {p}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(
            f"usage: {sys.argv[0]} /path/to/testsuits-for-oskernel",
            file=sys.stderr,
        )
        sys.exit(2)
    main()
