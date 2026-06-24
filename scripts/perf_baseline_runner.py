#!/usr/bin/env python3
"""Phase-0 benchmark baseline runner.

The runner builds once, then runs QEMU in the contest Docker image against a
fresh per-run sdcard copy. Each run directory keeps the raw serial log plus the
metadata needed to compare or reproduce the run later.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import platform
import re
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path, PurePosixPath
from typing import Any


IMAGE = "zhouzhouyi/os-contest:20260510"
FOCUSED_LTP_CASES = (
    "writev01,setegid02,getgroups01,setgroups01,setgroups02,setgroups03,"
    "setgroups04,access01,open02,setfsuid01,setfsgid01,"
    "faccessat01,access02,open03,symlink01,readlink01,lstat01,"
    "symlink02,symlink03,symlink04,symlinkat01"
)

ARCHES = {
    "riscv64": {
        "make_arch": "riscv64",
        "target": "target/riscv64gc-unknown-none-elf/release/wll_OS",
        "sdcard": "sdcard-rv.img",
        "qemu": (
            "timeout --foreground {timeout}s "
            "qemu-system-riscv64 -machine virt -kernel {kernel} -m {mem} "
            "-nographic -smp {smp} -bios default "
            "-drive file={sdcard},if=none,format=raw,id=x0 "
            "-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 "
            "-no-reboot -device virtio-net-device,netdev=net "
            "-netdev user,id=net -rtc base=utc"
        ),
    },
    "loongarch64": {
        "make_arch": "loongarch64",
        "target": "target/loongarch64-unknown-none/release/wll_OS",
        "sdcard": "sdcard-la.img",
        "qemu": (
            "timeout --foreground {timeout}s "
            "qemu-system-loongarch64 -kernel {kernel} -m {mem} "
            "-nographic -smp {smp} "
            "-drive file={sdcard},if=none,format=raw,id=x0 "
            "-device virtio-blk-pci,drive=x0 -no-reboot "
            "-device virtio-net-pci,netdev=net0 -netdev user,id=net0 "
            "-rtc base=utc"
        ),
    },
}

SUITE_PROFILES = {
    "all": {
        "judge_suites": ("iozone", "libcbench", "lmbench", "ltp"),
        "iozone": "1",
        "lmbench": "1",
        "ltp": "1",
        "harness_groups": "",
        "ltp_cases": FOCUSED_LTP_CASES,
        "recommended_runs": 7,
    },
    "all-lmbench": {
        "judge_suites": ("iozone", "libcbench", "lmbench"),
        "iozone": "1",
        "lmbench": "1",
        "ltp": "0",
        "harness_groups": "",
        "ltp_cases": "",
        "recommended_runs": 7,
    },
    "iozone": {
        "judge_suites": ("iozone",),
        "iozone": "1",
        "lmbench": "0",
        "ltp": "0",
        "harness_groups": "iozone",
        "ltp_cases": "",
        "recommended_runs": 5,
    },
    "libcbench": {
        "judge_suites": ("libcbench",),
        "iozone": "1",
        "lmbench": "0",
        "ltp": "0",
        "harness_groups": "libcbench",
        "ltp_cases": "",
        "recommended_runs": 5,
    },
    "lmbench": {
        "judge_suites": ("lmbench",),
        "iozone": "0",
        "lmbench": "1",
        "ltp": "0",
        "harness_groups": "lmbench",
        "ltp_cases": "",
        "recommended_runs": 7,
    },
    "ltp": {
        "judge_suites": ("ltp",),
        "iozone": "0",
        "lmbench": "0",
        "ltp": "1",
        "harness_groups": "ltp",
        "ltp_cases": FOCUSED_LTP_CASES,
        "recommended_runs": 1,
    },
}

LIBCS = ("glibc", "musl")
GROUP_START_RE = re.compile(r"^#### OS COMP TEST GROUP START (?P<group>.*?) ####\n", re.M)


def repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def is_relative_to(path: Path, base: Path) -> bool:
    try:
        path.resolve().relative_to(base.resolve())
        return True
    except ValueError:
        return False


def rel_posix(repo: Path, path: Path) -> str:
    rel = path.resolve().relative_to(repo.resolve())
    return PurePosixPath(*rel.parts).as_posix()


def container_path(repo: Path, path: Path, results_root: Path | None = None) -> str:
    path = path.resolve()
    repo = repo.resolve()
    if is_relative_to(path, repo):
        return "/workspace/" + rel_posix(repo, path)
    if results_root is not None and is_relative_to(path, results_root):
        rel = path.relative_to(results_root.resolve())
        return "/results/" + PurePosixPath(*rel.parts).as_posix()
    raise ValueError(f"path is outside mounted roots: {path}")


def write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def write_json(path: Path, data: Any) -> None:
    write_text(path, json.dumps(data, ensure_ascii=False, indent=2, sort_keys=True) + "\n")


def run_host(cmd: list[str], cwd: Path, check: bool = True) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(cmd, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if check and proc.returncode != 0:
        raise RuntimeError(
            f"command failed ({proc.returncode}): {' '.join(cmd)}\n{proc.stdout}\n{proc.stderr}"
        )
    return proc


def docker_cmd(repo: Path, image: str, script: str, results_root: Path | None = None) -> list[str]:
    cmd = [
        "docker",
        "run",
        "--rm",
        "-v",
        f"{repo}:/workspace",
    ]
    if results_root is not None and not is_relative_to(results_root, repo):
        results_root.mkdir(parents=True, exist_ok=True)
        cmd.extend(["-v", f"{results_root}:/results"])
    cmd.extend(
        [
        "-w",
        "/workspace",
        image,
        "bash",
        "-lc",
        script,
        ]
    )
    return cmd


def run_docker_capture(
    repo: Path,
    image: str,
    script: str,
    out_path: Path,
    results_root: Path | None = None,
    timestamp_path: Path | None = None,
) -> int:
    cmd = docker_cmd(repo, image, script, results_root)
    with out_path.open("w", encoding="utf-8", errors="replace", newline="") as out:
        if timestamp_path is None:
            proc = subprocess.run(cmd, cwd=repo, text=True, stdout=out, stderr=subprocess.STDOUT)
            return proc.returncode

        start = time.monotonic()
        with timestamp_path.open("w", encoding="utf-8", errors="replace", newline="") as ts:
            proc = subprocess.Popen(
                cmd,
                cwd=repo,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                encoding="utf-8",
                errors="replace",
                bufsize=1,
            )
            assert proc.stdout is not None
            for line in proc.stdout:
                out.write(line)
                out.flush()
                ts.write(
                    json.dumps(
                        {
                            "elapsed_seconds": time.monotonic() - start,
                            "line": line.rstrip("\n"),
                        },
                        ensure_ascii=False,
                    )
                    + "\n"
                )
                ts.flush()
            return proc.wait()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def git_rev_text(repo: Path) -> str:
    parts: list[str] = []
    for cmd in (
        ["git", "rev-parse", "--abbrev-ref", "HEAD"],
        ["git", "rev-parse", "HEAD"],
        ["git", "status", "--short", "--branch"],
    ):
        proc = run_host(cmd, repo, check=False)
        parts.append(f"$ {' '.join(cmd)}")
        parts.append((proc.stdout or proc.stderr).strip())
        parts.append("")
    return "\n".join(parts).rstrip() + "\n"


def build_kernel(
    repo: Path,
    arch: str,
    suite: str,
    summary_dir: Path,
    image: str,
    dry_run: bool,
    results_root: Path,
    trace_test_commands: bool,
    trace_test_groups: str,
    harness_groups_override: str | None,
    ltp_cases_override: str | None,
) -> tuple[Path, str]:
    cfg = ARCHES[arch]
    profile = SUITE_PROFILES[suite]
    kernel = summary_dir / f"kernel-{arch}-{suite}"
    harness_groups = (
        profile["harness_groups"] if harness_groups_override is None else harness_groups_override
    )
    ltp_cases = profile["ltp_cases"] if ltp_cases_override is None else ltp_cases_override
    harness_prefix = f"WLL_HARNESS_GROUPS={shlex.quote(harness_groups)} " if harness_groups else ""
    ltp_cases_prefix = f"LTP_CASES={shlex.quote(ltp_cases)} " if ltp_cases else ""
    trace_prefix = ""
    if trace_test_commands:
        trace_prefix = "WLL_TRACE_TEST_COMMANDS=1 "
        if trace_test_groups:
            trace_prefix += f"WLL_TRACE_TEST_GROUPS={shlex.quote(trace_test_groups)} "
    script = (
        f"{harness_prefix}{ltp_cases_prefix}{trace_prefix}make ARCH={cfg['make_arch']} build "
        f"IOZONE={profile['iozone']} LMBENCH={profile['lmbench']} LTP={profile['ltp']} && "
        f"cp {cfg['target']} {container_path(repo, kernel, results_root)}"
    )
    write_text(summary_dir / "build-command.txt", script + "\n")
    write_text(
        summary_dir / "build-docker-command.txt",
        " ".join(docker_cmd(repo, image, script, results_root)) + "\n",
    )
    if dry_run:
        write_text(summary_dir / "build.log", "[dry-run] build skipped\n")
        return kernel, script

    code = run_docker_capture(repo, image, script, summary_dir / "build.log", results_root)
    if code != 0:
        raise RuntimeError(f"kernel build failed for {arch}/{suite}; see {summary_dir / 'build.log'}")
    write_text(summary_dir / "kernel.sha256", f"{sha256_file(kernel)}  {kernel.name}\n")
    return kernel, script


def make_qemu_script(
    repo: Path,
    arch: str,
    kernel: Path,
    sdcard: Path,
    timeout: int,
    mem: str,
    smp: int,
    results_root: Path,
) -> str:
    cfg = ARCHES[arch]
    return cfg["qemu"].format(
        timeout=timeout,
        kernel=container_path(repo, kernel, results_root),
        sdcard=container_path(repo, sdcard, results_root),
        mem=mem,
        smp=smp,
    )


def parser_value(item: dict[str, Any]) -> float:
    try:
        return float(item.get("res", item.get("result", item.get("pass", 0))) or 0)
    except (TypeError, ValueError):
        return 0.0


def parser_score(item: dict[str, Any]) -> float:
    try:
        return float(item.get("score", 0) or 0)
    except (TypeError, ValueError):
        return 0.0


def summarize_parser_items(items: list[dict[str, Any]]) -> dict[str, Any]:
    normalized = [
        {
            "name": str(item.get("name", "")),
            "result": parser_value(item),
            "baseline": item.get("baseline"),
            "score": parser_score(item),
        }
        for item in items
    ]
    low = sorted(normalized, key=lambda item: (item["score"], item["result"], item["name"]))[:8]
    return {
        "count": len(normalized),
        "zero": sum(1 for item in normalized if item["result"] <= 0),
        "score": sum(item["score"] for item in normalized),
        "top_low": low,
    }


def run_judge_parser(repo: Path, suite: str, libc: str, segment: str) -> tuple[int, dict[str, Any]]:
    parser = repo / "testdata" / f"judge_{suite}-{libc}.py"
    if not parser.exists():
        return 1, {"status": "parser_missing", "parser": str(parser)}
    proc = subprocess.run(
        [sys.executable, str(parser)],
        input=segment,
        cwd=repo,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if proc.returncode != 0:
        return proc.returncode, {
            "status": "parser_error",
            "parser": str(parser),
            "stderr": proc.stderr,
            "stdout": proc.stdout,
        }
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        return 1, {
            "status": "parser_json_error",
            "parser": str(parser),
            "error": str(exc),
            "stdout": proc.stdout,
        }
    if not isinstance(data, list):
        return 1, {"status": "parser_json_error", "parser": str(parser), "error": "JSON is not a list"}
    summary = summarize_parser_items(data)
    summary.update({"status": "ok", "parser": str(parser)})
    return 0, summary


def iter_group_segments(text: str):
    starts = list(GROUP_START_RE.finditer(text))
    for index, match in enumerate(starts):
        group = match.group("group")
        body_start = match.end()
        next_start = starts[index + 1].start() if index + 1 < len(starts) else len(text)
        end_marker = f"#### OS COMP TEST GROUP END {group} ####"
        end_pos = text.find(end_marker, body_start, next_start)
        if end_pos >= 0:
            yield group, text[body_start:end_pos], True
        else:
            yield group, text[body_start:next_start], False


def run_judge_summary(repo: Path, suite: str, serial_log: Path, out_path: Path) -> int:
    profile = SUITE_PROFILES[suite]
    wanted = set(profile["judge_suites"])
    text = serial_log.read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n")
    groups: dict[str, dict[str, Any]] = {}
    exit_code = 0
    for group, body, closed in iter_group_segments(text):
        if "-" not in group:
            continue
        group_suite, libc = group.rsplit("-", 1)
        if group_suite not in wanted or libc not in LIBCS:
            continue
        parser_code, summary = run_judge_parser(repo, group_suite, libc, body)
        if parser_code != 0:
            exit_code = parser_code
        summary.update(
            {
                "suite": group_suite,
                "libc": libc,
                "group": group,
                "closed": closed,
            }
        )
        if not closed:
            exit_code = exit_code or 1
        groups[group] = summary

    for group_suite in sorted(wanted):
        for libc in LIBCS:
            group = f"{group_suite}-{libc}"
            if group not in groups:
                groups[group] = {
                    "status": "missing",
                    "suite": group_suite,
                    "libc": libc,
                    "group": group,
                }
                exit_code = exit_code or 1

    write_json(
        out_path,
        {
            "source": str(serial_log),
            "suite_profile": suite,
            "required_suites": sorted(wanted),
            "groups": groups,
        },
    )
    return exit_code


def copy_sdcard(src: Path, dst: Path, dry_run: bool) -> str:
    if dry_run:
        write_text(dst.with_suffix(".dry-run.txt"), f"would copy {src} to {dst}\n")
        return "dry-run"
    shutil.copy2(src, dst)
    return sha256_file(dst)


def run_one(
    repo: Path,
    arch: str,
    suite: str,
    run_index: int,
    run_dir: Path,
    summary_dir: Path,
    kernel: Path,
    image: str,
    timeout: int,
    mem: str,
    smp: int,
    dry_run: bool,
    results_root: Path,
    delete_sdcard_copy: bool,
    timestamp_serial: bool,
    harness_groups_override: str | None,
    ltp_cases_override: str | None,
) -> dict[str, Any]:
    cfg = ARCHES[arch]
    profile = SUITE_PROFILES[suite]
    harness_groups = (
        profile["harness_groups"] if harness_groups_override is None else harness_groups_override
    )
    ltp_cases = profile["ltp_cases"] if ltp_cases_override is None else ltp_cases_override
    sdcard_src = repo / cfg["sdcard"]
    if not sdcard_src.exists() and not dry_run:
        raise FileNotFoundError(f"missing sdcard image: {sdcard_src}")

    run_dir.mkdir(parents=True, exist_ok=True)
    sdcard_dst = run_dir / "sdcard.img"
    digest = copy_sdcard(sdcard_src, sdcard_dst, dry_run)
    write_text(run_dir / "sdcard.sha256", f"{digest}  {sdcard_dst.name}\n")
    write_text(run_dir / "git-rev.txt", git_rev_text(repo))

    qemu_script = make_qemu_script(repo, arch, kernel, sdcard_dst, timeout, mem, smp, results_root)
    write_text(run_dir / "command.txt", qemu_script + "\n")
    write_text(run_dir / "docker-command.txt", " ".join(docker_cmd(repo, image, qemu_script, results_root)) + "\n")

    env = {
        "arch": arch,
        "suite": suite,
        "run_index": run_index,
        "cache_mode": "cold-copy",
        "judge_suites": list(profile["judge_suites"]),
        "recommended_runs": profile["recommended_runs"],
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "docker_image": image,
        "qemu_timeout_seconds": timeout,
        "qemu_mem": mem,
        "qemu_smp": smp,
        "timestamp_serial": timestamp_serial,
        "make": {
            "ARCH": cfg["make_arch"],
            "IOZONE": profile["iozone"],
            "LMBENCH": profile["lmbench"],
            "LTP": profile["ltp"],
            "WLL_HARNESS_GROUPS": harness_groups,
            "LTP_CASES": ltp_cases,
        },
        "paths": {
            "kernel": str(kernel),
            "sdcard_source": str(sdcard_src),
            "sdcard_copy": str(sdcard_dst),
            "summary_dir": str(summary_dir),
        },
        "host": {
            "platform": platform.platform(),
            "python": sys.version,
        },
    }
    write_json(run_dir / "env.json", env)

    start = time.monotonic()
    serial_log = run_dir / "serial.log"
    timestamp_path = run_dir / "serial-timestamps.jsonl" if timestamp_serial else None
    if dry_run:
        write_text(serial_log, "[dry-run] qemu skipped\n")
        if timestamp_path is not None:
            write_text(timestamp_path, "")
        exit_code = 0
    else:
        exit_code = run_docker_capture(
            repo,
            image,
            qemu_script,
            serial_log,
            results_root,
            timestamp_path=timestamp_path,
        )
    elapsed = time.monotonic() - start
    write_text(run_dir / "exit-code.txt", f"{exit_code}\n")
    write_json(run_dir / "timing.json", {"elapsed_seconds": elapsed, "qemu_exit_code": exit_code})

    judge_code = 0
    if dry_run:
        write_json(run_dir / "judge-summary.json", {"source": str(serial_log), "status": "dry-run"})
    else:
        judge_code = run_judge_summary(repo, suite, serial_log, run_dir / "judge-summary.json")
    sdcard_deleted = False
    if delete_sdcard_copy and sdcard_dst.exists():
        sdcard_dst.unlink()
        sdcard_deleted = True
    return {
        "run_index": run_index,
        "run_dir": str(run_dir),
        "serial_log": str(serial_log),
        "sdcard_deleted": sdcard_deleted,
        "qemu_exit_code": exit_code,
        "judge_exit_code": judge_code,
        "elapsed_seconds": elapsed,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", choices=["riscv64", "loongarch64"], required=True)
    parser.add_argument("--suite", choices=sorted(SUITE_PROFILES), required=True)
    parser.add_argument("--runs", type=int, help="measured runs; defaults to phase-0 recommendation")
    parser.add_argument("--timeout", type=int, default=3600)
    parser.add_argument("--mem", default="1G")
    parser.add_argument("--smp", type=int, default=1)
    parser.add_argument("--image", default=IMAGE)
    parser.add_argument("--results-root", type=Path, default=Path("results"))
    parser.add_argument(
        "--delete-sdcard-copy",
        action="store_true",
        help="delete each run's copied sdcard.img after serial/judge-summary files are written",
    )
    parser.add_argument("--dry-run", action="store_true", help="create metadata without building, copying, or running")
    parser.add_argument(
        "--timestamp-serial",
        action="store_true",
        help="write serial-timestamps.jsonl next to serial.log without changing serial.log",
    )
    parser.add_argument(
        "--harness-groups",
        help="override WLL_HARNESS_GROUPS for diagnostic subset runs",
    )
    parser.add_argument(
        "--ltp-cases",
        help="override LTP_CASES for external diagnostic LTP subset runs",
    )
    parser.add_argument(
        "--trace-test-commands",
        action="store_true",
        help="build the harness with shell command tracing for test scripts",
    )
    parser.add_argument(
        "--trace-test-groups",
        default="lmbench",
        help="comma-separated test groups to trace when --trace-test-commands is set; use 'all' for every script",
    )
    args = parser.parse_args()

    if args.runs is not None and args.runs <= 0:
        parser.error("--runs must be positive")
    if args.ltp_cases and args.suite != "ltp":
        parser.error("--ltp-cases is only supported with --suite ltp")

    repo = repo_root()
    profile = SUITE_PROFILES[args.suite]
    runs = args.runs or int(profile["recommended_runs"])
    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M")
    prefix = f"{stamp}-{args.arch}-{args.suite}"
    results_root = (repo / args.results_root).resolve()
    summary_dir = results_root / f"{prefix}-summary"
    summary_dir.mkdir(parents=True, exist_ok=True)

    write_text(summary_dir / "git-rev.txt", git_rev_text(repo))
    write_json(
        summary_dir / "env.json",
        {
            "arch": args.arch,
            "suite": args.suite,
            "runs": runs,
            "recommended_runs": profile["recommended_runs"],
            "meets_recommended_runs": runs >= int(profile["recommended_runs"]),
            "cache_mode": "cold-copy",
            "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "dry_run": args.dry_run,
            "docker_image": args.image,
            "delete_sdcard_copy": args.delete_sdcard_copy,
            "timestamp_serial": args.timestamp_serial,
            "harness_groups_override": args.harness_groups,
            "ltp_cases": args.ltp_cases if args.ltp_cases is not None else profile["ltp_cases"],
            "trace_test_commands": args.trace_test_commands,
            "trace_test_groups": args.trace_test_groups if args.trace_test_commands else "",
        },
    )

    kernel, build_script = build_kernel(
        repo,
        args.arch,
        args.suite,
        summary_dir,
        args.image,
        args.dry_run,
        results_root,
        args.trace_test_commands,
        args.trace_test_groups,
        args.harness_groups,
        args.ltp_cases,
    )
    run_results = []
    for run_index in range(1, runs + 1):
        run_dir = results_root / f"{prefix}-{run_index:02d}"
        print(f"[{args.arch}/{args.suite}] run {run_index}/{runs}: {run_dir}", flush=True)
        run_results.append(
            run_one(
                repo,
                args.arch,
                args.suite,
                run_index,
                run_dir,
                summary_dir,
                kernel,
                args.image,
                args.timeout,
                args.mem,
                args.smp,
                args.dry_run,
                results_root,
                args.delete_sdcard_copy,
                args.timestamp_serial,
                args.harness_groups,
                args.ltp_cases,
            )
        )

    run_code = 0
    judge_code = 0
    summary_groups: dict[str, list[dict[str, Any]]] = {}
    for item in run_results:
        judge_path = Path(item["run_dir"]) / "judge-summary.json"
        if not judge_path.exists():
            judge_code = judge_code or 1
            continue
        data = json.loads(judge_path.read_text(encoding="utf-8"))
        for group, group_summary in data.get("groups", {}).items():
            summary_groups.setdefault(group, []).append(group_summary)
    write_json(
        summary_dir / "judge-summary.json",
        {
            "suite_profile": args.suite,
            "runs": len(run_results),
            "groups": summary_groups,
        },
    )
    for item in run_results:
        judge_code = judge_code or int(item.get("judge_exit_code", 0))
        run_code = run_code or int(item.get("qemu_exit_code", 0))
    write_json(
        summary_dir / "runs.json",
        {
            "build_script": build_script,
            "run_exit_code": run_code,
            "judge_exit_code": judge_code,
            "runs": run_results,
        },
    )
    print(f"[{args.arch}/{args.suite}] summary: {summary_dir}", flush=True)
    return 0 if run_code == 0 and judge_code == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
