#!/bin/sh
# Public final-2026 task semantics; ordinary guest commands only.
set +e
bb="${CAGENT_BUSYBOX:-/musl/busybox}"
lua="${CAGENT_LUA:-/glibc/lua}"

factorial=1
i=1
while [ "$i" -le 10 ]; do
    factorial=$((factorial * i))
    i=$((i + 1))
done
if [ "$factorial" -eq 3628800 ]; then
    echo "CASE_RESULT name=factorial status=OK value=$factorial"
else
    echo "CASE_RESULT name=factorial status=FAIL value=$factorial"
fi

cores="$("$bb" nproc 2>/dev/null)"
case "$cores" in
    ''|*[!0-9]*) echo "CASE_RESULT name=cpu status=FAIL value=missing" ;;
    *) echo "CASE_RESULT name=cpu status=OK value=$cores" ;;
esac

release="$("$bb" uname -r 2>/dev/null)"
case "$release" in
    *[0-9]*.*[0-9]*) echo "CASE_RESULT name=kernel status=OK value=$release" ;;
    *) echo "CASE_RESULT name=kernel status=FAIL value=missing" ;;
esac

tcp_count="$("$bb" awk 'FNR > 1 { n++ } END { print n + 0 }' /proc/net/tcp /proc/net/tcp6 2>/dev/null)"
case "$tcp_count" in
    ''|*[!0-9]*) echo "CASE_RESULT name=network status=FAIL value=missing" ;;
    *) echo "CASE_RESULT name=network status=OK value=$tcp_count" ;;
esac

work=/var/tmp/wll-cagent-smoke
"$bb" rm -rf "$work"
"$bb" mkdir -p "$work"
printf 'wll-cagent-create\n' > "$work/created.txt"
if "$bb" grep -qx 'wll-cagent-create' "$work/created.txt"; then
    echo 'CASE_RESULT name=fs-create status=OK value=content-match'
else
    echo 'CASE_RESULT name=fs-create status=FAIL value=content-mismatch'
fi

printf '1\n2\n3\n4\n5\n' > "$work/numbers.txt"
sum="$("$bb" awk '{ total += $1 } END { print total + 0 }' "$work/numbers.txt")"
if [ "$sum" = 15 ]; then
    echo 'CASE_RESULT name=fs-readwrite status=OK value=15'
else
    echo "CASE_RESULT name=fs-readwrite status=FAIL value=${sum:-missing}"
fi

"$bb" mkdir -p "$work/directory"
: > "$work/directory/a"
: > "$work/directory/b"
: > "$work/directory/c"
entry_count="$("$bb" find "$work/directory" -maxdepth 1 -type f | "$bb" wc -l)"
if [ "$entry_count" -ge 3 ] 2>/dev/null; then
    echo "CASE_RESULT name=fs-directory status=OK value=$entry_count"
else
    echo 'CASE_RESULT name=fs-directory status=FAIL value=missing'
fi

usage="$("$bb" df -h | "$bb" awk 'END { print $3 "/" $2 }')"
case "$usage" in
    *[0-9]*[KMGTPkmgpt]*) echo "CASE_RESULT name=fs-usage status=OK value=$usage" ;;
    *) echo "CASE_RESULT name=fs-usage status=FAIL value=${usage:-missing}" ;;
esac

search_count="$("$bb" find / -type f -name '*.sh' 2>/dev/null | "$bb" wc -l)"
case "$search_count" in
    ''|*[!0-9]*) echo 'CASE_RESULT name=fs-search status=FAIL value=missing' ;;
    *) echo "CASE_RESULT name=fs-search status=OK value=$search_count" ;;
esac

weekday="$("$lua" -e 'print(os.date("%A", os.time() - 100 * 24 * 60 * 60))' 2>/dev/null)"
case "$weekday" in
    Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday)
        echo "CASE_RESULT name=date status=OK value=$weekday" ;;
    *) echo 'CASE_RESULT name=date status=FAIL value=missing' ;;
esac

"$bb" rm -rf "$work"
echo CAGENT_PUBLIC_EQUIVALENT_DONE
