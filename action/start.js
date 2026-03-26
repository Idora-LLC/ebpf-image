const { execSync, spawn } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');

const BINARY_NAME = 'ci-tracer';
const INSTALL_DIR = '/usr/local/bin';
const LOG_FILE = '/var/log/ci-tracer.log';
const TRACE_FILE = '/var/log/ci-trace.jsonl';
const PID_FILE = '/tmp/.ci-tracer.pid';

async function run() {
  try {
    // Download the pre-built binary from the action's directory.
    // In a real release, this would download from GitHub Releases.
    // For now, the binary is built by the CI and included in the release.
    const actionDir = __dirname;
    const binaryPath = path.join(INSTALL_DIR, BINARY_NAME);

    // Check if binary exists (shipped with the action release)
    const shippedBinary = path.join(actionDir, '..', 'bin', BINARY_NAME);
    if (fs.existsSync(shippedBinary)) {
      fs.copyFileSync(shippedBinary, binaryPath);
      fs.chmodSync(binaryPath, 0o755);
    } else if (!fs.existsSync(binaryPath)) {
      console.log('::error::ci-tracer binary not found. Ensure the action release includes the binary.');
      process.exit(1);
    }

    // Mount tracefs/debugfs for eBPF
    try { execSync('mount -t debugfs debugfs /sys/kernel/debug 2>/dev/null || true'); } catch {}
    try { execSync('mount -t tracefs tracefs /sys/kernel/tracing 2>/dev/null || true'); } catch {}

    // Ensure log directory exists
    fs.mkdirSync('/var/log', { recursive: true });

    // Start the tracer in the background
    const logFd = fs.openSync(LOG_FILE, 'a');
    const child = spawn(binaryPath, [], {
      detached: true,
      stdio: ['ignore', logFd, logFd],
    });
    child.unref();

    // Save PID for the post step
    fs.writeFileSync(PID_FILE, child.pid.toString());

    // Export for other steps
    const envFile = process.env.GITHUB_STATE || '';
    if (envFile) {
      fs.appendFileSync(envFile, `tracer_pid=${child.pid}\n`);
    }

    console.log(`[ci-recorder] Tracer started (PID ${child.pid})`);
    console.log(`[ci-recorder] Trace output: ${TRACE_FILE}`);

    // Give the tracer a moment to attach eBPF probes
    await new Promise(r => setTimeout(r, 2000));

  } catch (error) {
    console.log(`::warning::Failed to start CI recorder: ${error.message}`);
  }
}

run();
