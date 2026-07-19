#!/bin/sh
echo STATFS_DIAG_BEGIN
/musl/busybox cat /proc/mounts
/musl/busybox stat /
/musl/busybox stat -c 'dev=%d mode=%f' /
/musl/busybox df -h /
/musl/busybox df -h
echo STATFS_DIAG_DONE
