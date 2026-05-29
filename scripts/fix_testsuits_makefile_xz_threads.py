#!/usr/bin/env python3
"""Use multi-thread xz (-T0) for sdcard-*.img in testsuits/Makefile (much faster)."""
from pathlib import Path
import sys

def main() -> None:
    p = Path(sys.argv[1]).expanduser().resolve() / "Makefile"
    if not p.is_file():
        print(f"error: not found: {p}", file=sys.stderr)
        sys.exit(1)
    t = p.read_text(encoding="utf-8")
    t2 = t.replace("xz sdcard-rv.img", "xz -T0 sdcard-rv.img").replace(
        "xz sdcard-la.img", "xz -T0 sdcard-la.img"
    )
    if t == t2:
        print(f"OK (already or pattern missing): {p}")
        return
    p.write_text(t2, encoding="utf-8")
    print(f"OK: {p}")

if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} /path/to/testsuits-for-oskernel", file=sys.stderr)
        sys.exit(2)
    main()
