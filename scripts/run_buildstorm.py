#!/usr/bin/env python3
"""Run the unmodified official BuildStorm task from a reference ext4 image."""

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
        "features": [],
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
        "features": ["--no-default-features", "--features", "loongarch"],
        "qemu": "qemu-system-loongarch64",
        "windows_qemu": r"C:\Program Files\qemu\qemu-system-loongarch64.exe",
        "memory": "8G",
        "smp": 8,
        "args": ["-device", "virtio-blk-pci,drive=x0"],
    },
}

STAGE_MARKERS = {
    "toolchain": ("BUILDSTORM_TOOLCHAIN ok",),
    "minibuild": ("BUILDSTORM_TOOLCHAIN ok", "BUILDSTORM_MINIBUILD ok"),
    "complete": (
        "BUILDSTORM_TOOLCHAIN ok",
        "BUILDSTORM_MINIBUILD ok",
        "BUILDSTORM_COMPILE mode=multi ok=true",
    ),
}
PANIC_MARKERS = (b"Kernel panic", b"panicked at")
OOM_MARKERS = (b"Heap allocation error", b"Out of memory")


def qemu_path(config: dict[str, object]) -> str:
    found = shutil.which(str(config["qemu"]))
    if found:
        return found
    windows = Path(str(config["windows_qemu"]))
    if windows.exists():
        return str(windows)
    raise FileNotFoundError(f"cannot find {config['qemu']}")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def build(
    repo: Path, arch: str, extra_features: str | None
) -> tuple[Path, list[str], float, Path]:
    config = ARCHES[arch]
    cargo = shutil.which("cargo")
    if cargo is None:
        cargo_candidate = Path.home() / ".cargo" / "bin" / "cargo"
        if cargo_candidate.is_file():
            cargo = str(cargo_candidate)
    if cargo is None:
        raise FileNotFoundError("cargo is not on PATH or ~/.cargo/bin/cargo")
    command = [
        cargo, "+nightly-2025-01-18", "build", "--locked", "--offline",
        "--release", "--target", str(config["target"]), *config["features"],
    ]
    if extra_features:
        command.extend(["--features", extra_features])
    env = os.environ.copy()
    env.pop("WLL_INTERACTIVE", None)
    env["WLL_HARNESS_GROUPS"] = "buildstorm"
    env["WLL_HARNESS_LIBC"] = "glibc"
    env["LOG"] = ""
    build_log = repo / "_tmp" / f"release-build-{arch}.log"
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


def read_output(log: Path) -> str:
    try:
        return log.read_bytes().decode("utf-8", errors="replace")
    except FileNotFoundError:
        return ""


def read_new_output(log: Path, offset: int) -> tuple[bytes, int]:
    try:
        with log.open("rb") as stream:
            stream.seek(offset)
            data = stream.read()
            return data, stream.tell()
    except FileNotFoundError:
        return b"", offset


def run(
    repo: Path,
    arch: str,
    image: Path,
    stage: str,
    timeout: int,
    memory: str,
    smp: int,
    skip_build: bool,
    build_features: str | None,
) -> int:
    if not image.is_file():
        raise FileNotFoundError(image)
    config = ARCHES[arch]
    build_command = None
    build_elapsed_seconds = None
    build_log = None
    if skip_build:
        kernel = repo / "target" / str(config["target"]) / "release" / "wll_OS"
    else:
        kernel, build_command, build_elapsed_seconds, build_log = build(
            repo, arch, build_features
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
    qemu_version = subprocess.run(
        [args[0], "--version"],
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    ).stdout.splitlines()[0]
    markers = STAGE_MARKERS[stage]
    deadline = time.monotonic() + timeout
    started = time.monotonic()
    reached = False
    pending_markers = {marker.encode() for marker in markers}
    stop_patterns = (*PANIC_MARKERS, *OOM_MARKERS)
    overlap = max(len(pattern) for pattern in (*pending_markers, *stop_patterns)) - 1
    scan_tail = b""
    log_offset = 0
    with log.open("wb") as output:
        process = subprocess.Popen(
            args, stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT
        )
        try:
            while time.monotonic() < deadline and process.poll() is None:
                chunk, log_offset = read_new_output(log, log_offset)
                scan = scan_tail + chunk
                pending_markers = {
                    marker for marker in pending_markers if marker not in scan
                }
                if not pending_markers:
                    reached = True
                    break
                if any(pattern in scan for pattern in stop_patterns):
                    break
                scan_tail = scan[-overlap:] if overlap else b""
                time.sleep(0.25)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)

    text = read_output(log)
    panic_seen = any(marker.decode() in text for marker in PANIC_MARKERS)
    oom_seen = any(marker.decode() in text for marker in OOM_MARKERS)
    if not reached and all(marker in text for marker in markers):
        reached = True
    if reached:
        termination_reason = "stage_marker"
    elif panic_seen:
        termination_reason = "panic"
    elif oom_seen:
        termination_reason = "oom"
    elif process.returncode is not None and process.returncode != -9:
        termination_reason = "qemu_exit"
    else:
        termination_reason = "timeout"
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
                "build_command": build_command,
                "build_elapsed_seconds": build_elapsed_seconds,
                "build_log": str(build_log.resolve()) if build_log else None,
                "kernel_sha256": sha256(kernel),
                "qemu_version": qemu_version,
                "host_elapsed_seconds": time.monotonic() - started,
                "reached_stage_marker": reached,
                "panic_seen": panic_seen,
                "oom_seen": oom_seen,
                "termination_reason": termination_reason,
                "qemu_exit_code": process.returncode,
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
    parser.add_argument(
        "--timeout",
        type=int,
        default=15000,
        help="outer QEMU deadline; official complete compilation permits 14400 seconds",
    )
    parser.add_argument("--memory", help="override the architecture default")
    parser.add_argument("--smp", type=int, help="override the architecture default")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument(
        "--build-features",
        help="extra kernel Cargo features for an explicitly requested diagnostic build",
    )
    args = parser.parse_args()
    if args.timeout <= 0 or (args.smp is not None and args.smp <= 0):
        parser.error("--timeout and --smp must be positive")
    config = ARCHES[args.arch]
    memory = args.memory or str(config["memory"])
    smp = args.smp or int(config["smp"])
    return run(
        Path(__file__).resolve().parents[1], args.arch, args.image.resolve(),
        args.stage, args.timeout, memory, smp, args.skip_build,
        args.build_features,
    )


if __name__ == "__main__":
    raise SystemExit(main())
