#!/usr/bin/env bash
# Capture one production BuildStorm run without modifying the official image or suite.
set -euo pipefail

if [[ $# -ne 3 ]]; then
    echo "usage: $0 <riscv64|loongarch64> <image> <evidence-dir>" >&2
    exit 2
fi

arch=$1
image=$2
evidence=$3
repo=$(cd "$(dirname "$0")/.." && pwd)
suite=${BUILDSTORM_SUITE_DIR:-"$repo/../testsuits-for-oskernel"}

if [[ $arch != riscv64 && $arch != loongarch64 ]]; then
    echo "unsupported architecture: $arch" >&2
    exit 2
fi
if [[ ! -f $image || ! -d $suite/.git ]]; then
    echo "image or official suite is unavailable" >&2
    exit 2
fi

mkdir -p "$evidence"
if [[ -e $evidence/launch.started ]]; then
    echo "evidence directory already contains a run" >&2
    exit 2
fi
date --iso-8601=seconds >"$evidence/launch.started"
git -C "$repo" status --short --branch >"$evidence/kernel-source-status.txt"
git -C "$repo" rev-parse HEAD >"$evidence/kernel-source-commit.txt"
git -C "$repo" diff --binary HEAD >"$evidence/kernel-source-dirty.diff"
sha256sum "$image" >"$evidence/image.sha256"
git -C "$suite" status --short --branch >"$evidence/official-suite-status.txt"
git -C "$suite" rev-parse HEAD >"$evidence/official-suite-commit.txt"
sha256sum "$suite/scripts/buildstorm_testcode.sh" "$suite/judge/judge_buildstorm-glibc.py" >"$evidence/official-suite-files.sha256"
{
    uname -a
    lscpu
    free -h
    df -h
    qemu="qemu-system-$arch"
    "$qemu" --version | head -1
} >"$evidence/host-environment.txt"

pids=()
start_sampler() {
    local output=$1
    shift
    if command -v "$1" >/dev/null 2>&1; then
        "$@" >"$output" 2>&1 &
        pids+=("$!")
    else
        printf 'unavailable: %s\n' "$1" >"$output"
    fi
}
stop_samplers() {
    local pid
    for pid in "${pids[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    wait "${pids[@]}" 2>/dev/null || true
}
trap stop_samplers EXIT

start_sampler "$evidence/host-pidstat.log" pidstat -d -r -u -w -h -C "qemu-system-$arch" 1
start_sampler "$evidence/host-iostat.log" iostat -dx 1
start_sampler "$evidence/host-vmstat.log" vmstat 1

set +e
(
    cd "$repo"
    /usr/bin/time -v -o "$evidence/host-time.txt" \
        python3 scripts/run_buildstorm.py --arch "$arch" --image "$image" \
        --stage complete --timeout 15000 --memory 8G --smp 8
) >"$evidence/runner.stdout" 2>"$evidence/runner.stderr"
runner_status=$?
set -e

cp "$repo/_tmp/buildstorm-$arch-complete.log" "$evidence/serial.log"
cp "$repo/_tmp/buildstorm-$arch-complete.json" "$evidence/runner.json"
if [[ -f $repo/_tmp/release-build-$arch.log ]]; then
    cp "$repo/_tmp/release-build-$arch.log" "$evidence/kernel-build.log"
fi
set +e
python3 "$suite/judge/judge_buildstorm-glibc.py" "$evidence/serial.log" \
    >"$evidence/official-judge.stdout" 2>"$evidence/official-judge.stderr"
judge_status=$?
set -e
printf '%s\n' "$runner_status" >"$evidence/runner.exit"
printf '%s\n' "$judge_status" >"$evidence/official-judge.exit"
date --iso-8601=seconds >"$evidence/complete.finished"
exit "$runner_status"
