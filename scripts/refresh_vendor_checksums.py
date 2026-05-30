#!/usr/bin/env python3
"""Refresh Cargo vendor checksums after judge-side hidden-file filtering."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import sys


ROOT = Path(__file__).resolve().parents[1]
VENDOR = ROOT / "vendor"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    if not VENDOR.is_dir():
        return 0

    missing: list[str] = []
    for checksum_path in sorted(VENDOR.glob("*/.cargo-checksum.json")):
        crate_dir = checksum_path.parent
        data = json.loads(checksum_path.read_text(encoding="utf-8"))
        files = data.get("files")
        if not isinstance(files, dict):
            continue

        for rel_path in sorted(files):
            source_path = crate_dir / rel_path
            if not source_path.is_file():
                missing.append(str(source_path.relative_to(ROOT)))
                continue
            files[rel_path] = sha256(source_path)

        checksum_path.write_text(
            json.dumps(data, separators=(",", ":"), ensure_ascii=False) + "\n",
            encoding="utf-8",
            newline="\n",
        )

    if missing:
        for path in missing:
            print(f"missing vendor checksum input: {path}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
