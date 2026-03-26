#!/bin/bash
# Convenience script for GitHub Actions container jobs where the
# image ENTRYPOINT is overridden.  Call this as the first step:
#
#   - name: Start eBPF tracer
#     run: /usr/local/bin/start-ci-tracer
#
set -e

mkdir -p /var/log
mount -t debugfs debugfs /sys/kernel/debug 2>/dev/null || true
mount -t tracefs tracefs /sys/kernel/tracing 2>/dev/null || true

/usr/local/bin/ci-tracer &
echo "CI_TRACER_PID=$!" >> "${GITHUB_ENV:-/dev/null}"
sleep 1
