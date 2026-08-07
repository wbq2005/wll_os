#!/usr/bin/env python3
"""Run an unmodified BuildStorm image for a fixed serial-marker window.

The runner deliberately owns only the host-side process and evidence files.
With ``-snapshot`` the guest target directory is an ephemeral QEMU overlay, so
it is not mounted or inspected from the host.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any

from run_buildstorm import ARCHES, build, qemu_path, sha256


BEGIN_MARKER = b"BUILDSTORM_BEGIN mode=multi"
PANIC_MARKERS = (b"Kernel panic", b"panicked at")
COMPILING_RE = re.compile(r"^\s*Compiling\s+(.+?)\s*$", re.MULTILINE)
FINISHED_RE = re.compile(r"^\s*Finished\s+", re.MULTILINE)
COMPILING_LINE_RE = re.compile(r"^\s*Compiling\s+(.+?)\s*$")
FINISHED_LINE_RE = re.compile(r"^\s*Finished\s+")


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def git_state(repo: Path) -> dict[str, str]:
    def output(*command: str) -> str:
        return subprocess.run(
            command, cwd=repo, text=True, encoding="utf-8", errors="replace",
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False,
        ).stdout

    diff = output("git", "diff", "--binary", "HEAD")
    return {
        "commit": output("git", "rev-parse", "HEAD").strip(),
        "branch": output("git", "branch", "--show-current").strip(),
        "status": output("git", "status", "--short", "--branch"),
        "dirty_diff_sha256": hashlib.sha256(diff.encode("utf-8")).hexdigest(),
    }


def read_after(path: Path, offset: int) -> tuple[bytes, int]:
    try:
        with path.open("rb") as stream:
            stream.seek(offset)
            data = stream.read()
            return data, stream.tell()
    except FileNotFoundError:
        return b"", offset


def start_sampler(command: list[str], path: Path) -> subprocess.Popen[bytes] | None:
    if shutil.which(command[0]) is None:
        path.write_text(f"unavailable: {command[0]}\n", encoding="ascii")
        return None
    output = path.open("wb")
    return subprocess.Popen(command, stdout=output, stderr=subprocess.STDOUT)


def stop_sampler(process: subprocess.Popen[bytes] | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def capture(command: list[str], path: Path) -> None:
    with path.open("wb") as output:
        subprocess.run(command, stdout=output, stderr=subprocess.STDOUT, check=False)


def diag_fields(line: str) -> dict[str, int | str]:
    """Parse one fixed-format aggregate diagnostic line without log heuristics."""
    fields: dict[str, int | str] = {}
    for token in line.split()[1:]:
        key, separator, value = token.partition("=")
        if not separator:
            continue
        try:
            fields[key] = int(value)
        except ValueError:
            fields[key] = value
    return fields


def parse_guest_aggregate(text: str) -> dict[str, Any] | None:
    """Return only the last complete 10-second diagnostic snapshot.

    The serial log remains authoritative.  This host-side projection avoids
    manually transcribing counters while intentionally ignoring per-process
    diagnostic lines and all pre-final snapshots.
    """
    lines = text.splitlines()
    snapshots = [
        index for index, line in enumerate(lines)
        if line.startswith("BUILDSTORM_DIAG snapshot=")
    ]
    if not snapshots:
        return None

    result: dict[str, Any] = {
        "snapshot": None,
        "cpus": {},
        "work": {},
        "fault_sources": {},
        "fault_resolutions": {},
        "blocks": {},
        "wake_to_run": {},
        "wait_actors": {},
        "blocked_owners": {},
        "locks": [],
        "vma_slots": None,
    }
    for line in lines[snapshots[-1]:]:
        if not line.startswith("BUILDSTORM_DIAG "):
            continue
        body = line[len("BUILDSTORM_DIAG "):]
        fields = diag_fields(line)
        if body.startswith("snapshot="):
            result["snapshot"] = fields
        elif body.startswith("cpu="):
            cpu = fields.get("cpu")
            if isinstance(cpu, int):
                result["cpus"][str(cpu)] = fields
        elif body.startswith("mm "):
            result["mm"] = fields
        elif body.startswith("cache "):
            result["cache"] = fields
        elif body.startswith("work="):
            name = fields.get("work")
            if isinstance(name, str):
                result["work"][name] = fields
        elif body.startswith("fault_source="):
            name = fields.get("fault_source")
            if isinstance(name, str):
                result["fault_sources"][name] = fields
        elif body.startswith("fault_resolution="):
            name = fields.get("fault_resolution")
            if isinstance(name, str):
                result["fault_resolutions"][name] = fields
        elif body.startswith("block="):
            name = fields.get("block")
            if isinstance(name, str):
                result["blocks"][name] = fields
        elif body.startswith("wake_to_run "):
            name = fields.get("block")
            if isinstance(name, str):
                result["wake_to_run"][name] = fields
        elif body.startswith("wait_actor="):
            name = fields.get("wait_actor")
            if isinstance(name, str):
                result["wait_actors"][name] = fields
        elif body.startswith("blocked_owner "):
            cpu = fields.get("cpu")
            if isinstance(cpu, int):
                result["blocked_owners"][str(cpu)] = fields
        elif body.startswith("poll "):
            result["poll"] = fields
        elif body.startswith("ipc "):
            result["ipc"] = fields
        elif body.startswith("fd_lifecycle "):
            result["fd_lifecycle"] = fields
        elif body.startswith("vma_slots "):
            result["vma_slots"] = fields
        elif body.startswith("lock_rank="):
            result["locks"].append(fields)
    return result


def summarize_serial(serial: Path) -> dict[str, Any]:
    text = serial.read_bytes().decode("utf-8", errors="replace")
    compiling = COMPILING_RE.findall(text)
    return {
        "compiling_lines": len(compiling),
        "last_crate": compiling[-1] if compiling else None,
        "finished_lines": len(FINISHED_RE.findall(text)),
        "buildstorm_result_lines": [
            line for line in text.splitlines() if line.startswith("BUILDSTORM_")
        ],
        "guest_aggregate_lines": [
            line for line in text.splitlines()
            if "[buildstorm-diag]" in line or line.startswith("BUILDSTORM_DIAG ")
        ],
        "guest_aggregate_final": parse_guest_aggregate(text),
        "panic_seen": any(marker.decode() in text for marker in PANIC_MARKERS),
    }


def record_progress_lines(
    data: bytes,
    pending: bytes,
    marker_at: float,
    timeline: list[dict[str, Any]],
) -> bytes:
    """Record complete post-marker Cargo progress lines with host time.

    QEMU writes serial output to a regular file and the runner polls it every
    200 ms.  Timestamps therefore have sub-second polling precision and are
    suitable only for comparable A/B windows, never guest timing or scoring.
    """
    combined = pending + data
    lines = combined.split(b"\n")
    pending = lines.pop() if lines else b""
    observed_at = time.monotonic() - marker_at
    for raw in lines:
        line = raw.rstrip(b"\r").decode("utf-8", errors="replace")
        compiling = COMPILING_LINE_RE.match(line)
        if compiling:
            timeline.append({
                "kind": "compiling",
                "after_marker_seconds": observed_at,
                "value": compiling.group(1),
            })
        elif FINISHED_LINE_RE.match(line):
            timeline.append({
                "kind": "finished",
                "after_marker_seconds": observed_at,
                "value": line.strip(),
            })
    return pending


def run(args: argparse.Namespace) -> int:
    repo = Path(__file__).resolve().parents[1]
    image = args.image.resolve()
    if not image.is_file():
        raise FileNotFoundError(image)
    if args.window <= 0 or args.begin_timeout <= 0 or args.smp <= 0:
        raise ValueError("window, begin timeout, and smp must be positive")

    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    serial = output / "serial.log"
    config = ARCHES[args.arch]
    build_command: list[str] | None = None
    build_elapsed: float | None = None
    build_log: Path | None = None
    if args.skip_build:
        kernel = repo / "target" / str(config["target"]) / "release" / "wll_OS"
    else:
        kernel, build_command, build_elapsed, build_log = build(repo, args.arch, args.build_features)
    if not kernel.is_file():
        raise FileNotFoundError(kernel)

    qemu = qemu_path(config)
    qemu_args = [
        qemu, "-snapshot", "-kernel", str(kernel), "-m", args.memory,
        "-smp", str(args.smp), "-display", "none", "-monitor", "none",
        "-serial", "stdio", "-drive", f"file={image},if=none,format=raw,id=x0",
        "-no-reboot", *config["args"],
    ]
    qemu_version = subprocess.run(
        [qemu, "--version"], text=True, encoding="utf-8", errors="replace",
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False,
    ).stdout.splitlines()[0]
    metadata: dict[str, Any] = {
        "arch": args.arch,
        "memory": args.memory,
        "smp": args.smp,
        "window_seconds": args.window,
        "begin_timeout_seconds": args.begin_timeout,
        "production_diagnostics_disabled": not bool(args.build_features),
        "image": str(image),
        "image_sha256": sha256(image),
        "kernel": str(kernel.resolve()),
        "kernel_sha256": sha256(kernel),
        "source": git_state(repo),
        "build_command": build_command,
        "build_elapsed_seconds": build_elapsed,
        "build_log": str(build_log.resolve()) if build_log else None,
        "qemu_version": qemu_version,
        "qemu_args": qemu_args,
        "target_snapshot_stats": {
            "available": False,
            "reason": "-snapshot keeps guest target writes in an ephemeral QEMU overlay; host does not mount or inspect the official image",
        },
    }
    write_json(output / "launch.json", metadata)

    start = time.monotonic()
    marker_at: float | None = None
    serial_offset = 0
    marker_scan_tail = b""
    progress_pending = b""
    progress_timeline: list[dict[str, Any]] = []
    samplers: list[subprocess.Popen[bytes] | None] = []
    process: subprocess.Popen[bytes] | None = None
    try:
        with serial.open("wb") as serial_out:
            process = subprocess.Popen(qemu_args, stdin=subprocess.DEVNULL, stdout=serial_out,
                                       stderr=subprocess.STDOUT)
            while process.poll() is None:
                data, serial_offset = read_after(serial, serial_offset)
                if marker_at is None:
                    marker_scan = marker_scan_tail + data
                    marker_index = marker_scan.find(BEGIN_MARKER)
                    if marker_index >= 0:
                        marker_at = time.monotonic()
                        marker_end = marker_scan.find(b"\n", marker_index)
                        if marker_end >= 0:
                            progress_pending = record_progress_lines(
                                marker_scan[marker_end + 1:], b"", marker_at,
                                progress_timeline,
                            )
                        pid = str(process.pid)
                        samplers = [
                            start_sampler(["pidstat", "-d", "-r", "-u", "-w", "-h", "-p", pid, "1"], output / "host-pidstat.log"),
                            start_sampler(["iostat", "-dx", "1"], output / "host-iostat.log"),
                            start_sampler(["vmstat", "1"], output / "host-vmstat.log"),
                        ]
                    elif time.monotonic() - start >= args.begin_timeout:
                        break
                    marker_scan_tail = marker_scan[-(len(BEGIN_MARKER) - 1):]
                elif time.monotonic() - marker_at >= args.window:
                    break
                elif data:
                    progress_pending = record_progress_lines(
                        data, progress_pending, marker_at, progress_timeline,
                    )
                time.sleep(0.20)
    finally:
        if process is not None:
            capture(["ps", "-L", "-p", str(process.pid), "-o", "pid,tid,psr,pcpu,stat,etime,comm"], output / "host-qemu-threads-final.log")
        for sampler in samplers:
            stop_sampler(sampler)
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=10)

    summary = summarize_serial(serial)
    summary.update({
        "qemu_exit_code_before_termination": process.returncode if process is not None else None,
        "marker_seen": marker_at is not None,
        "host_elapsed_seconds": time.monotonic() - start,
        "marker_window_elapsed_seconds": time.monotonic() - marker_at if marker_at else None,
        "progress_timeline": progress_timeline,
    })
    write_json(output / "summary.json", summary)
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    return 0 if marker_at is not None and not summary["panic_seen"] else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", choices=ARCHES, required=True)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--smp", type=int, required=True)
    parser.add_argument("--memory", default="8G")
    parser.add_argument("--window", type=int, default=300)
    parser.add_argument("--begin-timeout", type=int, default=1800)
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--build-features", help="explicit diagnostic-only kernel features")
    return run(parser.parse_args())


if __name__ == "__main__":
    raise SystemExit(main())
