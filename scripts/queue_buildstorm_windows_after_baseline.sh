#!/usr/bin/env bash
# Run the two comparable marker-window samples only after a long baseline ends.
set -euo pipefail

if [[ $# -ne 4 ]]; then
    echo "usage: $0 <baseline-dir> <riscv64|loongarch64> <image> <evidence-root>" >&2
    exit 2
fi

baseline=$1
arch=$2
image=$3
evidence_root=$4
repo=$(cd "$(dirname "$0")/.." && pwd)

if [[ ! -d $baseline || ! -f $image ]]; then
    echo "baseline directory or image is unavailable" >&2
    exit 2
fi

while [[ ! -e $baseline/complete.finished ]]; do
    sleep 30
done

for smp in 1 8; do
    output="$evidence_root/smp${smp}-window300"
    if [[ -e $output/summary.json ]]; then
        echo "existing window evidence: $output" >&2
        continue
    fi
    mkdir -p "$evidence_root"
    (
        cd "$repo"
        python3 scripts/run_buildstorm_window.py --arch "$arch" --image "$image" \
            --output "$output" --smp "$smp" --memory 8G --window 300 --skip-build
    ) >"$output.runner.stdout" 2>"$output.runner.stderr"
done
