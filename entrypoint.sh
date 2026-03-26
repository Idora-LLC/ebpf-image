#!/bin/bash
set -e

mkdir -p /var/log
mount -t debugfs debugfs /sys/kernel/debug 2>/dev/null || true
mount -t tracefs tracefs /sys/kernel/tracing 2>/dev/null || true

/usr/local/bin/ci-tracer &
TRACER_PID=$!

cleanup() {
    kill -TERM "$TRACER_PID" 2>/dev/null || true
    wait "$TRACER_PID" 2>/dev/null || true
    exit 0
}
trap cleanup SIGTERM SIGINT

if [ $# -gt 0 ]; then
    # Direct execution mode (local testing / docker run)
    "$@"
    RET=$?
    cleanup
    exit $RET
else
    # GitHub Actions mode: keep container alive, steps run via docker exec
    tail -f /dev/null &
    wait $!
fi
