#!/usr/bin/env bash
# Replaces host-only -I/usr/include/tirpc with in-tree libtirpc headers so
# cross GCC does not error: "unsafe header/library path used in cross-compilation".
set -euo pipefail

TESTSUIT_ROOT="${1:?usage: $0 /path/to/testsuits-for-oskernel}"

SESSION_ID="cab395"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOG_FILE="$(cd "${SCRIPT_DIR}/.." && pwd)/debug-${SESSION_ID}.log"
MK="${TESTSUIT_ROOT}/lmbench_src/src/Makefile"

if [[ ! -f "$MK" ]]; then
  echo "ERROR: not found: $MK" >&2
  exit 1
fi

RESULT="noop"
if grep -q -- '-I/usr/include/tirpc' "$MK"; then
  sed -i 's|-I/usr/include/tirpc|-I../libtirpc-1.3.6/tirpc|g' "$MK"
  RESULT="patched"
else
  RESULT="no_host_tirpc_pattern"
fi

# #region agent log
printf '%s\n' "{\"sessionId\":\"${SESSION_ID}\",\"timestamp\":$(($(date +%s) * 1000)),\"location\":\"scripts/patch-testsuits-lmbench-tirpc.sh\",\"message\":\"lmbench tirpc include path\",\"data\":{\"makefile\":\"${MK}\",\"result\":\"${RESULT}\"},\"runId\":\"pre-fix\",\"hypothesisId\":\"H1\"}" >> "${LOG_FILE}"
# #endregion

echo "OK (${RESULT}): ${MK}"
