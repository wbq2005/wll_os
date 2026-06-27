#!/usr/bin/env bash
# Applies musl libc.so copy logic for educg vs zhou/lp64d Docker images — see fix_testsuits_musl_libc_makefile.py
exec python3 "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/fix_testsuits_musl_libc_makefile.py" "${1:?usage: $0 /path/to/testsuits-for-oskernel}"
