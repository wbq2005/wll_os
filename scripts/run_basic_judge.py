#!/usr/bin/env python3
"""Build, run, and judge OSComp basic tests in the contest Docker image."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


IMAGE = "zhouzhouyi/os-contest:20260510"


ARCHES = {
    "riscv64": {
        "make_arch": "riscv64",
        "kernel": "target/riscv64gc-unknown-none-elf/release/wll_OS",
        "copy": "kernel-rv.test",
        "qemu": (
            "timeout --foreground {timeout}s "
            "qemu-system-riscv64 -machine virt -kernel kernel-rv.test -m 1G "
            "-nographic -smp 1 -bios default "
            "-drive file=sdcard-rv.img,if=none,format=raw,id=x0 "
            "-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 "
            "-no-reboot -device virtio-net-device,netdev=net "
            "-netdev user,id=net -rtc base=utc"
        ),
    },
    "loongarch64": {
        "make_arch": "loongarch64",
        "kernel": "target/loongarch64-unknown-none/release/wll_OS",
        "copy": "kernel-la.test",
        "qemu": (
            "timeout --foreground {timeout}s "
            "qemu-system-loongarch64 -kernel kernel-la.test -m 1G "
            "-nographic -smp 1 "
            "-drive file=sdcard-la.img,if=none,format=raw,id=x0 "
            "-device virtio-blk-pci,drive=x0 -no-reboot "
            "-device virtio-net-pci,netdev=net0 -netdev user,id=net0 "
            "-rtc base=utc"
        ),
    },
}


def run_docker(repo: Path, arch: str, timeout: int, image: str) -> tuple[int, str]:
    cfg = ARCHES[arch]
    script = (
        f"make ARCH={cfg['make_arch']} build HARNESS_GROUPS=basic HARNESS_LIBC=both && "
        f"cp {cfg['kernel']} {cfg['copy']} && "
        f"{cfg['qemu'].format(timeout=timeout)}"
    )
    cmd = [
        "docker",
        "run",
        "--rm",
        "-v",
        f"{repo}:/workspace",
        "-w",
        "/workspace",
        image,
        "bash",
        "-lc",
        script,
    ]
    proc = subprocess.run(
        cmd,
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    return proc.returncode, proc.stdout


def extract_group(output: str, group: str) -> str | None:
    start = f"#### OS COMP TEST GROUP START {group} ####"
    end = f"#### OS COMP TEST GROUP END {group} ####"
    start_idx = output.find(start)
    if start_idx < 0:
        return None
    end_idx = output.find(end, start_idx)
    if end_idx < 0:
        return output[start_idx:]
    return output[start_idx : end_idx + len(end)]


def run_judge(repo: Path, text: str, libc: str) -> tuple[int, int, list[str]]:
    judge = repo / "testdata" / f"judge_basic-{libc}.py"
    proc = subprocess.run(
        [sys.executable, str(judge)],
        input=text,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if proc.returncode != 0:
        print(proc.stderr, file=sys.stderr)
        raise RuntimeError(f"judge failed: {judge}")
    data = json.loads(proc.stdout)
    passed = sum(item["pass"] for item in data)
    total = sum(item["all"] for item in data)
    failed = [
        f"{item['name']} {item['pass']}/{item['all']}"
        for item in data
        if item["pass"] != item["all"]
    ]
    return passed, total, failed


def judge_arch(repo: Path, arch: str, output: str) -> bool:
    ok = True
    for libc in ("glibc", "musl"):
        group = f"basic-{libc}"
        segment = extract_group(output, group)
        if segment is None:
            print(f"[{arch}] missing group {group}")
            ok = False
            continue
        passed, total, failed = run_judge(repo, segment, libc)
        print(f"[{arch}] {group}: {passed}/{total}")
        if failed:
            ok = False
            for item in failed:
                print(f"  FAIL {item}")
    return ok


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--arch", choices=["riscv64", "loongarch64", "both"], default="both")
    parser.add_argument("--image", default=IMAGE)
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()

    repo = Path(__file__).resolve().parents[1]
    out_dir = repo / "_tmp"
    out_dir.mkdir(exist_ok=True)
    arches = list(ARCHES) if args.arch == "both" else [args.arch]

    all_ok = True
    for arch in arches:
        print(f"[{arch}] building and running basic in Docker...")
        code, output = run_docker(repo, arch, args.timeout, args.image)
        log_path = out_dir / f"basic-{arch}.log"
        log_path.write_text(output, encoding="utf-8", errors="replace")
        print(f"[{arch}] qemu/docker exit code: {code}, log: {log_path}")
        if code != 0:
            all_ok = False
        all_ok = judge_arch(repo, arch, output) and all_ok

    return 0 if all_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
