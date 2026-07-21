#!/usr/bin/env python3
"""Run the unmodified official BuildStorm task from a reference ext4 image."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import time
from pathlib import Path


ARCHES = {
    "riscv64": {
        "target": "riscv64gc-unknown-none-elf",
        "features": [],
        "qemu": "qemu-system-riscv64",
        "windows_qemu": r"C:\Program Files\qemu\qemu-system-riscv64.exe",
        "args": [
            "-machine", "virt", "-bios", "default",
            "-device", "virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0",
        ],
    },
    "loongarch64": {
        "target": "loongarch64-unknown-none",
        "features": ["--no-default-features", "--features", "loongarch"],
        "qemu": "qemu-system-loongarch64",
        "windows_qemu": r"C:\Program Files\qemu\qemu-system-loongarch64.exe",
        "args": ["-device", "virtio-blk-pci,drive=x0"],
    },
}

STAGE_MARKERS = {
    "toolchain": "BUILDSTORM_TOOLCHAIN ",
    "minibuild": "BUILDSTORM_MINIBUILD ",
    "complete": "BUILDSTORM_COMPILE ",
}


def qemu_path(config: dict[str, object]) -> str:
    found = shutil.which(str(config["qemu"]))
    if found:
        return found
    windows = Path(str(config["windows_qemu"]))
    if windows.exists():
        return str(windows)
    raise FileNotFoundError(f"cannot find {config['qemu']}")


def build(repo: Path, arch: str) -> Path:
    config = ARCHES[arch]
    command = [
        "cargo", "+nightly-2025-01-18", "build", "--locked", "--offline",
        "--release", "--target", str(config["target"]), *config["features"],
    ]
    env = os.environ.copy()
    env.pop("WLL_INTERACTIVE", None)
    env["WLL_HARNESS_GROUPS"] = "buildstorm"
    env["LOG"] = ""
    subprocess.run(command, cwd=repo / "os", env=env, check=True)
    return repo / "target" / str(config["target"]) / "release" / "wll_OS"


def read_output(log: Path) -> str:
    try:
        return log.read_bytes().decode("utf-8", errors="replace")
    except FileNotFoundError:
        return ""


def run(
    repo: Path,
    arch: str,
    image: Path,
    stage: str,
    timeout: int,
    memory: str,
    smp: int,
    skip_build: bool,
) -> int:
    if not image.is_file():
        raise FileNotFoundError(image)
    config = ARCHES[arch]
    kernel = (
        repo / "target" / str(config["target"]) / "release" / "wll_OS"
        if skip_build else build(repo, arch)
    )
    if not kernel.is_file():
        raise FileNotFoundError(kernel)

    log_dir = repo / "_tmp"
    log_dir.mkdir(exist_ok=True)
    log = log_dir / f"buildstorm-{arch}-{stage}.log"
    metadata = log_dir / f"buildstorm-{arch}-{stage}.json"
    args = [
        qemu_path(config), "-snapshot", "-kernel", str(kernel),
        "-m", memory, "-smp", str(smp), "-display", "none",
        "-monitor", "none", "-serial", "stdio",
        "-drive", f"file={image},if=none,format=raw,id=x0", "-no-reboot",
        *config["args"],
    ]
    marker = STAGE_MARKERS[stage]
    deadline = time.monotonic() + timeout
    started = time.monotonic()
    reached = False
    with log.open("wb") as output:
        process = subprocess.Popen(
            args, stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT
        )
        try:
            while time.monotonic() < deadline and process.poll() is None:
                text = read_output(log)
                if marker in text:
                    reached = True
                    break
                if "Kernel panic" in text or "panicked at" in text:
                    break
                time.sleep(0.25)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)

    text = read_output(log)
    result_lines = [
        line for line in text.splitlines()
        if line.startswith("BUILDSTORM_")
    ]
    metadata.write_text(
        json.dumps(
            {
                "arch": arch,
                "stage": stage,
                "image": str(image.resolve()),
                "image_bytes": image.stat().st_size,
                "memory": memory,
                "smp": smp,
                "host_elapsed_seconds": time.monotonic() - started,
                "reached_stage_marker": reached,
                "result_lines": result_lines,
                "qemu_args": args,
            },
            indent=2,
            ensure_ascii=False,
        ) + "\n",
        encoding="utf-8",
    )
    print(f"[{arch}] stage={stage} reached={reached} log={log}")
    for line in result_lines:
        print(line)
    return 0 if reached else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", choices=ARCHES, required=True)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--stage", choices=STAGE_MARKERS, default="toolchain")
    parser.add_argument("--timeout", type=int, default=600)
    parser.add_argument("--memory", default="8G")
    parser.add_argument("--smp", type=int, default=8)
    parser.add_argument("--skip-build", action="store_true")
    args = parser.parse_args()
    if args.timeout <= 0 or args.smp <= 0:
        parser.error("--timeout and --smp must be positive")
    return run(
        Path(__file__).resolve().parents[1], args.arch, args.image.resolve(),
        args.stage, args.timeout, args.memory, args.smp, args.skip_build,
    )


if __name__ == "__main__":
    raise SystemExit(main())
