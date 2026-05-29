#!/usr/bin/env python3
"""Normalize testsuits Makefile.sub busybox rule for GCC 12+ cross builds.

Uses extra -Wno-* on CC line, adds -Wno-uninitialized for busybox 1.33+tftp/hush.
Idempotent when the canonical block is already present.
"""
from pathlib import Path
import re
import sys

_BUSYBOX_BLOCK = """busybox: .PHONY
	cp config/busybox-config-$(ARCH) busybox/.config
	$(MAKE) -C busybox CC="$(CC) -static -Wno-error -Wno-uninitialized -Wno-stringop-overflow -Wno-restrict -Wno-unused-result -Wno-maybe-uninitialized" STRIP=$(STRIP) silentoldconfig
	$(MAKE) -C busybox CC="$(CC) -static -Wno-error -Wno-uninitialized -Wno-stringop-overflow -Wno-restrict -Wno-unused-result -Wno-maybe-uninitialized" STRIP=$(STRIP) -j
	cp busybox/busybox $(DESTDIR)/
	cp scripts/busybox/* $(DESTDIR)/
"""


def main() -> None:
    root = Path(sys.argv[1]).expanduser().resolve()
    mk = root / "Makefile.sub"
    if not mk.is_file():
        print(f"error: not found: {mk}", file=sys.stderr)
        sys.exit(1)
    text_norm = mk.read_text(encoding="utf-8").replace("\r\n", "\n")
    canon = _BUSYBOX_BLOCK.replace("\r\n", "\n")

    if canon in text_norm:
        print(f"OK (already canonical busybox block): {mk}")
        return

    text = text_norm.replace(
        "$(MAKE) -C busybox CC=\"$(CC) -static\" STRIP=$(STRIP) olddefconfig",
        "$(MAKE) -C busybox CC=\"$(CC) -static\" STRIP=$(STRIP) silentoldconfig",
    )

    new_text, n = re.subn(
        r"^busybox:\s*\.PHONY\n(?:\t[^\n]*\n)+",
        canon,
        text,
        count=1,
        flags=re.MULTILINE,
    )
    if n != 1:
        print(
            "error: could not replace busybox block; edit Makefile.sub manually",
            file=sys.stderr,
        )
        sys.exit(1)

    mk.write_text(new_text + ("" if new_text.endswith("\n") else "\n"), encoding="utf-8")
    print(f"OK: {mk}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(
            f"usage: {sys.argv[0]} /path/to/testsuits-for-oskernel",
            file=sys.stderr,
        )
        sys.exit(2)
    main()
