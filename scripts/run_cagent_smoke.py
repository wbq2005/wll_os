#!/usr/bin/env python3
"""Run the public CAgent-equivalent task through the kernel test harness."""

from __future__ import annotations

import argparse
import gzip
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path


CASES = {
    "factorial",
    "date",
    "cpu",
    "kernel",
    "network",
    "fs-create",
    "fs-readwrite",
    "fs-directory",
    "fs-usage",
    "fs-search",
}

ARCHES = {
    "riscv64": {
        "target": "riscv64gc-unknown-none-elf",
        "features": [],
        "kernel": "wll_OS",
        "image": "sdcard-rv.img",
        "qemu": "qemu-system-riscv64",
        "windows_qemu": r"C:\Program Files\qemu\qemu-system-riscv64.exe",
        "args": [
            "-machine", "virt", "-m", "1G", "-display", "none",
            "-monitor", "none", "-serial", "stdio", "-smp", "1",
            "-bios", "default", "-drive", "file={image},if=none,format=raw,id=x0",
            "-device", "virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0",
            "-no-reboot", "-rtc", "base=utc",
        ],
    },
    "loongarch64": {
        "target": "loongarch64-unknown-none",
        "features": ["--no-default-features", "--features", "loongarch"],
        "kernel": "wll_OS",
        "image": "sdcard-la.img",
        "qemu": "qemu-system-loongarch64",
        "windows_qemu": r"C:\Program Files\qemu\qemu-system-loongarch64.exe",
        "args": [
            "-m", "1G", "-display", "none", "-monitor", "none",
            "-serial", "stdio", "-smp", "1",
            "-drive", "file={image},if=none,format=raw,id=x0",
            "-device", "virtio-blk-pci,drive=x0", "-no-reboot", "-rtc", "base=utc",
        ],
    },
}


def qemu_path(config: dict[str, object]) -> str:
    name = str(config["qemu"])
    found = shutil.which(name)
    if found:
        return found
    windows = Path(str(config["windows_qemu"]))
    if windows.exists():
        return str(windows)
    raise FileNotFoundError(f"cannot find {name}")


def build(repo: Path, arch: str) -> Path:
    config = ARCHES[arch]
    toolchain = "nightly-2025-01-18"
    command = [
        "cargo", f"+{toolchain}", "build", "--locked", "--offline", "--release",
        "--target", str(config["target"]), *config["features"],
    ]
    env = os.environ.copy()
    env.pop("WLL_INTERACTIVE", None)
    env["WLL_HARNESS_GROUPS"] = "cagent"
    subprocess.run(command, cwd=repo / "os", env=env, check=True)
    return repo / "target" / str(config["target"]) / "release" / str(config["kernel"])


def linux_path(path: Path) -> str:
    if os.name != "nt":
        return str(path)
    drive = path.drive.rstrip(":").lower()
    suffix = path.resolve().as_posix().split(":", 1)[1]
    return f"/mnt/{drive}{suffix}"


def debugfs(image: Path, request: str) -> subprocess.CompletedProcess[str]:
    if os.name == "nt":
        workspace = image.parent.parent.resolve() if image.parent.name == "_tmp" else image.parent.resolve()
        request = request.replace(linux_path(workspace), "/workspace")
        image_in_container = "/workspace/" + image.resolve().relative_to(workspace).as_posix()
        command = [
            "docker", "run", "--rm", "-v", f"{workspace}:/workspace", "-w", "/workspace",
            "zhouzhouyi/os-contest:20260510", "debugfs", "-w", "-R", request,
            image_in_container,
        ]
    else:
        command = ["debugfs", "-w", "-R", request, linux_path(image)]
    return subprocess.run(
        command,
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )


def prepare_smoke_image(repo: Path, arch: str, configured_name: str) -> Path:
    compressed = repo / "testdata" / f"sdcard-{'rv' if arch == 'riscv64' else 'la'}.img.gz"
    if not compressed.exists():
        return repo / configured_name
    output = repo / "_tmp" / f"cagent-base-{arch}.img"
    output.parent.mkdir(exist_ok=True)
    if output.exists() and output.stat().st_mtime_ns >= compressed.stat().st_mtime_ns:
        return output
    temporary = output.with_suffix(".img.tmp")
    with gzip.open(compressed, "rb") as source, temporary.open("wb") as destination:
        shutil.copyfileobj(source, destination, length=8 * 1024 * 1024)
    temporary.replace(output)
    return output


def run(repo: Path, arch: str, timeout: int, skip_build: bool, script: Path | None = None) -> int:
    config = ARCHES[arch]
    kernel = (
        repo / "target" / str(config["target"]) / "release" / str(config["kernel"])
        if skip_build else build(repo, arch)
    )
    image = prepare_smoke_image(repo, arch, str(config["image"]))
    if not kernel.exists() or not image.exists():
        raise FileNotFoundError(f"missing kernel or image: {kernel}, {image}")

    args = [qemu_path(config), "-snapshot", "-kernel", str(kernel)]
    args.extend(str(arg).format(image=image) for arg in config["args"])
    guest_script = "/musl/cagent_testcode.sh"
    source_script = script or (repo / "testdata" / "cagent_public_equivalent.sh")
    existing = debugfs(image, f"stat {guest_script}").stdout
    if "File not found" not in existing and "not found" not in existing.lower():
        raise RuntimeError(f"refusing to overwrite existing guest path {guest_script}")
    injected = debugfs(image, f"write {linux_path(source_script)} {guest_script}")
    if "Allocated inode" not in injected.stdout:
        raise RuntimeError(f"debugfs injection failed:\n{injected.stdout}")
    log_dir = repo / "_tmp"
    log_dir.mkdir(exist_ok=True)
    log = log_dir / f"cagent-smoke-{arch}.log"
    deadline = time.monotonic() + timeout
    try:
        with log.open("wb") as log_file:
            process = subprocess.Popen(
                args,
                stdin=subprocess.PIPE,
                stdout=log_file,
                stderr=subprocess.STDOUT,
            )
            try:
                while time.monotonic() < deadline and process.poll() is None:
                    current = log.read_bytes().decode("utf-8", errors="replace")
                    if "CAGENT_PUBLIC_EQUIVALENT_DONE" in current:
                        break
                    if "Segmentation fault" in current or "Kernel panic" in current:
                        break
                    time.sleep(0.2)
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=5)
    finally:
        removed = debugfs(image, f"rm {guest_script}")
        if "File not found" in removed.stdout:
            print(f"warning: temporary guest script was already absent: {guest_script}")

    output = log.read_text(encoding="utf-8", errors="replace")

    results: dict[str, str] = {}
    for line in output.splitlines():
        if not line.startswith("CASE_RESULT "):
            continue
        fields = dict(
            field.split("=", 1) for field in line.split()[1:] if "=" in field
        )
        if "name" in fields and "status" in fields:
            results[fields["name"]] = fields["status"]

    missing = sorted(CASES - results.keys())
    failed = sorted(name for name, status in results.items() if status != "OK")
    print(f"[{arch}] passed={len(CASES) - len(missing) - len(failed)}/{len(CASES)} log={log}")
    if missing:
        print("missing:", ", ".join(missing))
    if failed:
        print("failed:", ", ".join(failed))
    return 0 if not missing and not failed else 1


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--arch", choices=ARCHES, default="riscv64")
    parser.add_argument("--timeout", type=int, default=90)
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--script", type=Path)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    script = args.script.resolve() if args.script else None
    return run(repo, args.arch, args.timeout, args.skip_build, script)


if __name__ == "__main__":
    sys.exit(main())
