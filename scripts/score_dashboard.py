#!/usr/bin/env python3
"""Summarize score-bearing suites from real OSComp serial logs.

The script deliberately shells out to the existing judge_*.py parsers so the
dashboard follows the same scoring rules as the judge-facing workflow.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any


SUITES = ("iozone", "lmbench", "libcbench")
LIBCS = ("glibc", "musl")
ARCH_ALIASES = {
    "rv": "riscv64",
    "riscv": "riscv64",
    "riscv64": "riscv64",
    "la": "loongarch64",
    "loongarch": "loongarch64",
    "loongarch64": "loongarch64",
}
GROUP_ALIASES = {
    "iozone": "iozone",
    "lmbench": "lmbench",
    "libcbench": "libcbench",
    "libc-bench": "libcbench",
}
ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
START_RE = re.compile(r"^#### OS COMP TEST GROUP START (?P<group>.+?) ####\s*$", re.MULTILINE)
END_RE = re.compile(r"^#### OS COMP TEST GROUP END (?P<group>.+?) ####\s*$", re.MULTILINE)


def read_serial_log(path: Path) -> str:
    data = path.read_bytes()
    if not data:
        return ""

    for encoding in ("utf-8-sig", "utf-16", "utf-16le"):
        try:
            text = data.decode(encoding)
        except UnicodeDecodeError:
            continue
        if "\x00" not in text[:200]:
            return ANSI_RE.sub("", text.replace("\r\n", "\n").replace("\r", "\n"))

    text = data.decode("utf-8", errors="replace")
    return ANSI_RE.sub("", text.replace("\r\n", "\n").replace("\r", "\n"))


def parse_log_spec(raw: str) -> tuple[str, Path]:
    if "=" not in raw:
        raise argparse.ArgumentTypeError("expected ARCH=PATH, for example rv=_tmp/run.log")
    arch_raw, path_raw = raw.split("=", 1)
    arch = ARCH_ALIASES.get(arch_raw.strip().lower())
    if arch is None:
        raise argparse.ArgumentTypeError(f"unknown arch {arch_raw!r}")
    path = Path(path_raw)
    if not path.exists():
        raise argparse.ArgumentTypeError(f"log path does not exist: {path}")
    return arch, path


def parse_group_name(name: str) -> tuple[str, str] | None:
    for libc in LIBCS:
        suffix = f"-{libc}"
        if name.endswith(suffix):
            suite_raw = name[: -len(suffix)]
            suite = GROUP_ALIASES.get(suite_raw)
            if suite is not None:
                return suite, libc
    return None


def line_offsets(text: str) -> list[int]:
    offsets = [0]
    for match in re.finditer("\n", text):
        offsets.append(match.end())
    return offsets


def offset_to_line(offsets: list[int], pos: int) -> int:
    lo, hi = 0, len(offsets)
    while lo + 1 < hi:
        mid = (lo + hi) // 2
        if offsets[mid] <= pos:
            lo = mid
        else:
            hi = mid
    return lo + 1


def extract_segments(text: str, source: Path) -> dict[tuple[str, str], dict[str, Any]]:
    offsets = line_offsets(text)
    starts: list[dict[str, Any]] = []
    for match in START_RE.finditer(text):
        group = match.group("group")
        parsed = parse_group_name(group)
        if parsed is None:
            continue
        starts.append(
            {
                "suite": parsed[0],
                "libc": parsed[1],
                "group": group,
                "start": match.start(),
                "start_line": offset_to_line(offsets, match.start()),
            }
        )

    segments: dict[tuple[str, str], dict[str, Any]] = {}
    for idx, start in enumerate(starts):
        end_match = END_RE.search(text, start["start"])
        next_start = starts[idx + 1]["start"] if idx + 1 < len(starts) else len(text)
        truncated = True
        end_pos = next_start
        end_line = None
        if end_match and end_match.group("group") == start["group"] and end_match.start() < next_start:
            end_pos = end_match.end()
            end_line = offset_to_line(offsets, end_match.start())
            truncated = False

        key = (start["suite"], start["libc"])
        segments[key] = {
            "group": start["group"],
            "source": str(source),
            "text": text[start["start"] : end_pos],
            "start_line": start["start_line"],
            "end_line": end_line,
            "truncated": truncated,
        }
    return segments


def run_parser(repo: Path, suite: str, libc: str, segment: str) -> tuple[list[dict[str, Any]] | None, str | None]:
    parser = repo / "testdata" / f"judge_{suite}-{libc}.py"
    if not parser.exists():
        return None, f"missing parser: {parser}"
    proc = subprocess.run(
        [sys.executable, str(parser)],
        input=segment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if proc.returncode != 0:
        return None, proc.stderr.strip() or f"parser exited {proc.returncode}"
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        return None, f"parser emitted invalid JSON: {exc}"
    if not isinstance(data, list):
        return None, "parser JSON is not a list"
    return data, None


def result_value(item: dict[str, Any]) -> float:
    value = item.get("res", item.get("result", 0))
    try:
        return float(value)
    except (TypeError, ValueError):
        return 0.0


def score_value(item: dict[str, Any]) -> float:
    try:
        return float(item.get("score", 0))
    except (TypeError, ValueError):
        return 0.0


def summarize_items(items: list[dict[str, Any]], top: int) -> dict[str, Any]:
    normalized = [
        {
            "name": str(item.get("name", "")),
            "result": result_value(item),
            "baseline": item.get("baseline"),
            "score": score_value(item),
        }
        for item in items
    ]
    zeros = [item for item in normalized if item["result"] <= 0]
    by_score = sorted(normalized, key=lambda item: (item["score"], item["result"], item["name"]))
    return {
        "count": len(normalized),
        "zero": len(zeros),
        "score": sum(item["score"] for item in normalized),
        "zero_items": zeros,
        "top_low": by_score[:top],
    }


def build_dashboard(repo: Path, logs: list[tuple[str, Path]], top: int) -> dict[str, Any]:
    by_arch: dict[str, dict[tuple[str, str], dict[str, Any]]] = {}
    log_inputs: dict[str, list[str]] = {}
    for arch, path in logs:
        text = read_serial_log(path)
        by_arch.setdefault(arch, {}).update(extract_segments(text, path))
        log_inputs.setdefault(arch, []).append(str(path))

    matrix: dict[str, Any] = {}
    all_required_have_json = True
    all_required_ok = True
    all_parser_nonzero = True
    for arch in sorted(by_arch):
        matrix[arch] = {}
        for suite in SUITES:
            matrix[arch][suite] = {}
            for libc in LIBCS:
                segment = by_arch[arch].get((suite, libc))
                entry: dict[str, Any] = {"status": "missing", "has_json": False}
                if segment is not None:
                    parsed, error = run_parser(repo, suite, libc, segment["text"])
                    entry = {
                        "status": "truncated" if segment["truncated"] else "ok",
                        "has_json": False,
                        "group": segment["group"],
                        "source": segment["source"],
                        "start_line": segment["start_line"],
                        "end_line": segment["end_line"],
                    }
                    if error:
                        entry["status"] = "parser_error"
                        entry["error"] = error
                    else:
                        entry["has_json"] = True
                        entry.update(summarize_items(parsed or [], top))
                if not entry["has_json"]:
                    all_required_have_json = False
                if entry["status"] != "ok":
                    all_required_ok = False
                if not entry["has_json"] or entry.get("zero", 0) != 0:
                    all_parser_nonzero = False
                matrix[arch][suite][libc] = entry

    return {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "inputs": log_inputs,
        "all_required_have_json": all_required_have_json,
        "all_required_ok": all_required_ok,
        "all_parser_nonzero": all_parser_nonzero,
        "required_suites": list(SUITES),
        "required_libcs": list(LIBCS),
        "matrix": matrix,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--log",
        action="append",
        type=parse_log_spec,
        required=True,
        help="real serial log as ARCH=PATH, where ARCH is rv/riscv64/la/loongarch64",
    )
    parser.add_argument("--top", type=int, default=8, help="number of low-scoring items to include")
    parser.add_argument("--out", type=Path, help="write dashboard JSON to this path")
    args = parser.parse_args()

    repo = Path(__file__).resolve().parents[1]
    dashboard = build_dashboard(repo, args.log, args.top)
    encoded = json.dumps(dashboard, ensure_ascii=False, indent=2, sort_keys=True)
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(encoded + "\n", encoding="utf-8")
    else:
        print(encoded)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
