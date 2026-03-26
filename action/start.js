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
      const token = process.env.INPUT_TOKEN || '';
      if (token) {
        execSync(`echo "${token}" | docker login ghcr.io -u github --password-stdin`, { stdio: 'ignore' });
      }
      execSync(`docker pull ${IMAGE}`, { stdio: 'inherit' });
      const cid = execSync(`docker create ${IMAGE}`).toString().trim();
      execSync(`sudo docker cp ${cid}:${BINARY} ${BINARY}`, { stdio: 'inherit' });
      execSync(`docker rm ${cid}`, { stdio: 'ignore' });
      execSync(`sudo chmod 755 ${BINARY}`);
      console.log('[ci-recorder] Binary installed');
    }

    // Mount tracefs/debugfs for eBPF
    try { execSync('sudo mount -t debugfs debugfs /sys/kernel/debug 2>/dev/null || true'); } catch {}
    try { execSync('sudo mount -t tracefs tracefs /sys/kernel/tracing 2>/dev/null || true'); } catch {}

    // Start the tracer in the background via sudo (needs root for eBPF)
    execSync(`sudo sh -c 'nohup ${BINARY} > ${LOG_FILE} 2>&1 & echo $! > ${PID_FILE}'`);
    const pid = fs.readFileSync(PID_FILE, 'utf8').trim();

    console.log(`[ci-recorder] Tracer started (PID ${pid})`);

    // Wait for eBPF probes to attach
    await new Promise(r => setTimeout(r, 2000));

  } catch (error) {
    console.log(`::warning::Failed to start CI recorder: ${error.message}`);
  }
}

run();
