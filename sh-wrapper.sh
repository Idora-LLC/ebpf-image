#!/usr/bin/sh.real
# Auto-start the eBPF tracer on first shell invocation inside the container.
# GitHub Actions runs every step via `docker exec sh -c "..."`, so this
# wrapper is called before any CI command. The real shell is at /usr/bin/sh.real.
if [ ! -f /tmp/.ci-tracer-started ]; then
    mkdir -p /var/log
    mount -t debugfs debugfs /sys/kernel/debug 2>/dev/null || true
    mount -t tracefs tracefs /sys/kernel/tracing 2>/dev/null || true
    nohup /usr/local/bin/ci-tracer >/var/log/ci-tracer.log 2>&1 &
    touch /tmp/.ci-tracer-started
    sleep 1
fi
exec /usr/bin/sh.real "$@"
