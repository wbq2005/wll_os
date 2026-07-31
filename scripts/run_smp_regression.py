#!/usr/bin/env python3
"""Run the architecture-neutral SMP scheduler and TLB transport regression."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import time
from pathlib import Path


ARCHES = {
    "riscv64": {
        "target": "riscv64gc-unknown-none-elf",
        "features": "riscv,smp-regression",
        "qemu": "qemu-system-riscv64",
        "windows_qemu": r"C:\Program Files\qemu\qemu-system-riscv64.exe",
        "memory": "8G",
        "smp": 8,
        "args": [
            "-machine", "virt", "-bios", "default",
            "-device", "virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0",
        ],
    },
    "loongarch64": {
        "target": "loongarch64-unknown-none",
        "features": "loongarch,smp-regression",
        "qemu": "qemu-system-loongarch64",
        "windows_qemu": r"C:\Program Files\qemu\qemu-system-loongarch64.exe",
        "memory": "8G",
        "smp": 8,
        "args": ["-device", "virtio-blk-pci,drive=x0"],
    },
}


def resolve_qemu(config: dict[str, object]) -> str:
    found = shutil.which(str(config["qemu"]))
    if found:
        return found
    fallback = Path(str(config["windows_qemu"]))
    if fallback.is_file():
        return str(fallback)
    raise FileNotFoundError(config["qemu"])


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def build(repo: Path, arch: str) -> tuple[Path, list[str], float, Path]:
    config = ARCHES[arch]
    env = os.environ.copy()
    env.pop("WLL_HARNESS_GROUPS", None)
    env.pop("WLL_INTERACTIVE", None)
    # Keep the serial marker atomic: concurrent per-CPU INFO logs can interleave
    # at byte granularity even though the regression itself has completed.
    env["LOG"] = "warn"
    command = [
        "cargo", "+nightly-2025-01-18", "build", "--locked", "--offline",
        "--release", "--target", str(config["target"]),
        "--no-default-features", "--features", str(config["features"]),
    ]
    build_log = repo / "_tmp" / f"smp-regression-build-{arch}.log"
    build_log.parent.mkdir(exist_ok=True)
    started = time.monotonic()
    with build_log.open("wb") as output:
        subprocess.run(
            command,
            cwd=repo / "os",
            env=env,
            stdout=output,
            stderr=subprocess.STDOUT,
            check=True,
        )
    elapsed = time.monotonic() - started
    kernel = repo / "target" / str(config["target"]) / "release" / "wll_OS"
    return kernel, command, elapsed, build_log


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", choices=ARCHES, required=True)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--timeout", type=int, default=30)
    parser.add_argument("--skip-build", action="store_true")
    args = parser.parse_args()

    repo = Path(__file__).resolve().parents[1]
    config = ARCHES[args.arch]
    image = args.image.resolve()
    if not image.is_file():
        raise FileNotFoundError(image)
    build_command = None
    build_elapsed_seconds = None
    build_log = None
    if args.skip_build:
        kernel = repo / "target" / str(config["target"]) / "release" / "wll_OS"
    else:
        kernel, build_command, build_elapsed_seconds, build_log = build(repo, args.arch)

    log = repo / "_tmp" / f"smp-regression-{args.arch}.log"
    metadata = repo / "_tmp" / f"smp-regression-{args.arch}.json"
    command = [
        resolve_qemu(config), "-snapshot", "-kernel", str(kernel),
        "-m", str(config["memory"]), "-smp", str(config["smp"]),
        "-display", "none", "-monitor", "none",
        "-serial", "stdio", "-drive", f"file={image},if=none,format=raw,id=x0",
        "-no-reboot", *config["args"],
    ]
    qemu_version = subprocess.run(
        [command[0], "--version"],
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    ).stdout.splitlines()[0]
    started = time.monotonic()
    deadline = started + args.timeout
    passed = False
    with log.open("wb") as output:
        process = subprocess.Popen(
            command, stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT
        )
        try:
            offset = 0
            tail = b""
            while time.monotonic() < deadline and process.poll() is None:
                with log.open("rb") as stream:
                    stream.seek(offset)
                    chunk = stream.read()
                    offset = stream.tell()
                scan = tail + chunk
                if b"[smp-regression] pass" in scan:
                    passed = True
                    break
                if b"[smp-regression] fail" in scan or b"panicked at" in scan:
                    break
                tail = scan[-128:]
                time.sleep(0.1)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)

    text = log.read_text(encoding="utf-8", errors="replace")
    if not passed and "[smp-regression] pass" in text:
        passed = True
    result_lines = [line for line in text.splitlines() if "[smp-regression]" in line]
    metadata.write_text(
        json.dumps(
            {
                "classification": "capability-pass" if passed else "unverified",
                "arch": args.arch,
                "image": str(image),
                "image_bytes": image.stat().st_size,
                "memory": config["memory"],
                "smp": config["smp"],
                "build_command": build_command,
                "build_elapsed_seconds": build_elapsed_seconds,
                "build_log": str(build_log.resolve()) if build_log else None,
                "kernel_sha256": sha256(kernel),
                "qemu_version": qemu_version,
                "host_elapsed_seconds": time.monotonic() - started,
                "passed": passed,
                "result_lines": result_lines,
                "qemu_args": command,
            },
            indent=2,
            ensure_ascii=False,
        )
        + "\n",
        encoding="utf-8",
    )
    for line in result_lines:
        print(line)
    print(f"[{args.arch}] passed={passed} log={log}")
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
