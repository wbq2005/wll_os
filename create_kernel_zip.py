"""Create a root-relative submission archive for the course evaluator.

The evaluator extracts the archive and invokes ``make all`` in that directory.
Using ``git archive`` keeps the package tied to the exact committed tree and,
unlike a hosting-site download, does not add an outer ``repo-sha/`` directory.
"""

from __future__ import annotations

import argparse
import hashlib
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path


REQUIRED_ROOTS = ("Makefile", "Cargo.toml", "os/Cargo.toml", "rust-toolchain.toml")


def git_output(repo: Path, *args: str) -> str:
    return subprocess.check_output(("git", *args), cwd=repo, text=True).strip()


def archive(repo: Path, output: Path) -> tuple[str, int, str]:
    commit = git_output(repo, "rev-parse", "HEAD")
    output = output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(prefix="wll-os-", suffix=".zip", delete=False) as tmp:
        temporary = Path(tmp.name)
    try:
        subprocess.run(
            ("git", "archive", "--format=zip", f"--output={temporary}", "HEAD"),
            cwd=repo,
            check=True,
        )
        with zipfile.ZipFile(temporary) as zf:
            names = set(zf.namelist())
            missing = [name for name in REQUIRED_ROOTS if name not in names]
            outer = sorted(name for name in names if name.startswith("wll_os-"))
            if missing:
                raise RuntimeError(f"archive missing root entries: {', '.join(missing)}")
            if outer:
                raise RuntimeError(f"archive unexpectedly has an outer directory: {outer[0]}")
            bad = zf.testzip()
            if bad is not None:
                raise RuntimeError(f"archive CRC check failed for {bad}")
            entries = len(names)
        temporary.replace(output)
    finally:
        temporary.unlink(missing_ok=True)
    digest = hashlib.sha256(output.read_bytes()).hexdigest()
    return commit, entries, digest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("-o", "--output", type=Path, help="output zip path")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent
    commit = git_output(repo, "rev-parse", "HEAD")
    output = args.output or Path.cwd() / f"wll_os-{commit[:12]}.zip"
    commit, entries, digest = archive(repo, output)
    print(f"WLL_SUBMISSION_ARCHIVE status=OK commit={commit} entries={entries}")
    print(f"WLL_SUBMISSION_ARCHIVE path={output.resolve()} sha256={digest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
