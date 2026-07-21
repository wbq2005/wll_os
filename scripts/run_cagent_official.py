#!/usr/bin/env python3
"""Run the unmodified glibc CAgent task from a reference image."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import time
from pathlib import Path


CAGENT_CASES = {
    "factorial": (13.5, 20_000),
    "date": (13.5, 20_000),
    "network": (20.0, 25_000),
    "cpu": (13.5, 20_000),
    "kernel": (13.5, 20_000),
    "fs-create": (20.0, 25_000),
    "fs-readwrite": (20.0, 30_000),
    "fs-directory": (20.0, 30_000),
    "fs-search": (27.0, 35_000),
    "fs-usage": (20.0, 25_000),
}

ARCHES = {
    "riscv64": {
        "target": "riscv64gc-unknown-none-elf",
        "features": [],
        "qemu": "qemu-system-riscv64",
        "windows_qemu": r"C:\Program Files\qemu\qemu-system-riscv64.exe",
        "args": [
            "-machine", "virt", "-bios", "default",
            "-device", "virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0",
            "-device", "virtio-net-device,netdev=net",
            "-netdev", "user,id=net",
        ],
    },
    "loongarch64": {
        "target": "loongarch64-unknown-none",
        "features": ["--no-default-features", "--features", "loongarch"],
        "qemu": "qemu-system-loongarch64",
        "windows_qemu": r"C:\Program Files\qemu\qemu-system-loongarch64.exe",
        "args": [
            "-device", "virtio-blk-pci,drive=x0",
            "-device", "virtio-net-pci,netdev=net0",
            "-netdev", "user,id=net0",
        ],
    },
}

CAGENT_END = re.compile(r"^#### OS COMP TEST GROUP END cagent(?:-glibc)? ####$", re.MULTILINE)
CAGENT_RESULT = re.compile(r"testcase\s+cagent\s+(\S+)\s+(pass|reject)\s+(\d+)")
SHA256 = re.compile(r"\b([0-9a-f]{64})\b")


def qemu_path(config: dict[str, object]) -> str:
    found = shutil.which(str(config["qemu"]))
    if found:
        return found
    windows = Path(str(config["windows_qemu"]))
    if windows.exists():
        return str(windows)
    raise FileNotFoundError(f"cannot find {config['qemu']}")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(8 * 1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def command_output(command: list[str], cwd: Path | None = None) -> str | None:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
    except OSError:
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def image_script_sha256(image: Path, container_image: str) -> str | None:
    docker = shutil.which("docker")
    if docker is None:
        return None
    command = [
        docker,
        "run",
        "--rm",
        "-v",
        f"{image.resolve()}:/image:ro",
        container_image,
        "sh",
        "-lc",
        "debugfs -R 'dump -p /glibc/cagent_testcode.sh /tmp/cagent.sh' /image "
        ">/dev/null 2>&1 && sha256sum /tmp/cagent.sh",
    ]
    output = command_output(command)
    if output is None:
        return None
    match = SHA256.search(output)
    return match.group(1) if match else None


def build(repo: Path, arch: str) -> Path:
    config = ARCHES[arch]
    command = [
        "cargo", "+nightly-2025-01-18", "build", "--locked", "--offline",
        "--release", "--target", str(config["target"]), *config["features"],
    ]
    env = os.environ.copy()
    env.pop("WLL_INTERACTIVE", None)
    env["WLL_HARNESS_GROUPS"] = "cagent"
    env["LOG"] = ""
    subprocess.run(command, cwd=repo / "os", env=env, check=True)
    return repo / "target" / str(config["target"]) / "release" / "wll_OS"


def parse_results(text: str) -> dict[str, object]:
    records: dict[str, tuple[str, int]] = {}
    for name, status, elapsed_ms in CAGENT_RESULT.findall(text):
        if name in CAGENT_CASES:
            records[name] = (status, int(elapsed_ms))

    cases = []
    score = 0.0
    for name, (weight, timeout_ms) in CAGENT_CASES.items():
        status, elapsed_ms = records.get(name, ("missing", 0))
        passed = status == "pass"
        bonus = weight * 0.1 if passed and 0 < elapsed_ms < timeout_ms / 2 else 0.0
        case_score = round(weight + bonus, 2) if passed else 0.0
        score += case_score
        cases.append(
            {
                "name": name,
                "status": status,
                "elapsed_ms": elapsed_ms,
                "score": case_score,
            }
        )

    return {
        "completed_script": bool(CAGENT_END.search(text)),
        "cases": cases,
        "passed_cases": sum(case["status"] == "pass" for case in cases),
        "score_by_public_judge": round(score, 2),
        "public_judge_max": 199.1,
        "all_cases_passed": all(case["status"] == "pass" for case in cases),
    }


def run(
    repo: Path,
    arch: str,
    image: Path,
    timeout: int,
    memory: str,
    smp: int,
    skip_build: bool,
    hash_image: bool,
    hash_guest_script: bool,
    container_image: str,
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
    log = log_dir / f"cagent-official-{arch}.log"
    metadata = log_dir / f"cagent-official-{arch}.json"
    qemu = qemu_path(config)
    args = [
        qemu, "-snapshot", "-kernel", str(kernel),
        "-m", memory, "-smp", str(smp), "-display", "none",
        "-monitor", "none", "-serial", "stdio",
        "-drive", f"file={image},if=none,format=raw,id=x0", "-no-reboot",
        "-rtc", "base=utc", *config["args"],
    ]

    deadline = time.monotonic() + timeout
    started = time.monotonic()
    completed = False
    with log.open("wb") as output:
        process = subprocess.Popen(
            args, stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT
        )
        try:
            while time.monotonic() < deadline and process.poll() is None:
                current = log.read_bytes().decode("utf-8", errors="replace")
                if CAGENT_END.search(current):
                    completed = True
                    break
                if "Kernel panic" in current or "panicked at" in current:
                    break
                time.sleep(0.2)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)

    text = log.read_text(encoding="utf-8", errors="replace")
    results = parse_results(text)
    result_lines = [line for line in text.splitlines() if line.startswith("testcase cagent ")]
    guest_script_hash = image_script_sha256(image, container_image) if hash_guest_script else None
    image_hash = sha256_file(image) if hash_image else None
    kernel_hash = sha256_file(kernel)
    metadata.write_text(
        json.dumps(
            {
                "arch": arch,
                "image": str(image.resolve()),
                "image_bytes": image.stat().st_size,
                "image_sha256": image_hash,
                "guest_script": "/glibc/cagent_testcode.sh",
                "guest_script_sha256": guest_script_hash,
                "kernel": str(kernel.resolve()),
                "kernel_sha256": kernel_hash,
                "git_head": command_output(["git", "rev-parse", "HEAD"], repo),
                "git_dirty": bool(command_output(["git", "status", "--porcelain"], repo)),
                "memory": memory,
                "smp": smp,
                "host_elapsed_seconds": time.monotonic() - started,
                "observed_end_marker": completed,
                "results": results,
                "result_lines": result_lines,
                "qemu_version": command_output([qemu, "--version"]),
                "qemu_args": args,
            },
            indent=2,
            ensure_ascii=False,
        ) + "\n",
        encoding="utf-8",
    )
    print(
        f"[{arch}] completed={results['completed_script']} "
        f"passed={results['passed_cases']}/10 "
        f"score={results['score_by_public_judge']}/{results['public_judge_max']} "
        f"log={log}"
    )
    for line in result_lines:
        print(line)
    return 0 if results["completed_script"] and results["all_cases_passed"] else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", choices=ARCHES, required=True)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--memory", default="1G")
    parser.add_argument("--smp", type=int, default=1)
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--hash-image", action="store_true")
    parser.add_argument("--skip-guest-script-hash", action="store_true")
    parser.add_argument("--container-image", default="zhouzhouyi/os-contest:20260510")
    args = parser.parse_args()
    if args.timeout <= 0 or args.smp <= 0:
        parser.error("--timeout and --smp must be positive")
    return run(
        Path(__file__).resolve().parents[1],
        args.arch,
        args.image.resolve(),
        args.timeout,
        args.memory,
        args.smp,
        args.skip_build,
        args.hash_image,
        not args.skip_guest_script_hash,
        args.container_image,
    )


if __name__ == "__main__":
    raise SystemExit(main())
