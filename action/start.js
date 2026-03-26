const { execSync, spawn } = require('child_process');
const fs = require('fs');

const BINARY = '/usr/local/bin/ci-tracer';
const IMAGE = 'ghcr.io/idora-llc/ci-recorder:latest';
const LOG_FILE = '/var/log/ci-tracer.log';
const PID_FILE = '/tmp/.ci-tracer.pid';

async function run() {
  try {
    // Extract the pre-built binary from the published Docker image.
    if (!fs.existsSync(BINARY)) {
      console.log('[ci-recorder] Downloading tracer binary...');
      execSync(`docker pull ${IMAGE}`, { stdio: 'inherit' });
      const cid = execSync(`docker create ${IMAGE}`).toString().trim();
      execSync(`docker cp ${cid}:${BINARY} ${BINARY}`);
      execSync(`docker rm ${cid}`, { stdio: 'ignore' });
      fs.chmodSync(BINARY, 0o755);
      console.log('[ci-recorder] Binary installed');
    }

    // Mount tracefs/debugfs for eBPF
    try { execSync('sudo mount -t debugfs debugfs /sys/kernel/debug 2>/dev/null || true'); } catch {}
    try { execSync('sudo mount -t tracefs tracefs /sys/kernel/tracing 2>/dev/null || true'); } catch {}

    fs.mkdirSync('/var/log', { recursive: true });

    // Start the tracer in the background
    const logFd = fs.openSync(LOG_FILE, 'a');
    const child = spawn('sudo', [BINARY], {
      detached: true,
      stdio: ['ignore', logFd, logFd],
    });
    child.unref();
    fs.closeSync(logFd);

    fs.writeFileSync(PID_FILE, child.pid.toString());

    // Save PID for the post step via GITHUB_STATE
    const stateFile = process.env.GITHUB_STATE || '';
    if (stateFile) {
      fs.appendFileSync(stateFile, `tracer_pid=${child.pid}\n`);
    }

    console.log(`[ci-recorder] Tracer started (PID ${child.pid})`);

    // Wait for eBPF probes to attach
    await new Promise(r => setTimeout(r, 2000));

  } catch (error) {
    console.log(`::warning::Failed to start CI recorder: ${error.message}`);
  }
}

run();
