// pre hook (specs/deployment.md §2): download the recorder binary by pinned
// release, checksum-verify it (specs/security.md §4), mount tracefs/debugfs, and
// start the agent under sudo before the build/test/deploy steps run.

const { execSync } = require('child_process');
const crypto = require('crypto');
const fs = require('fs');
const https = require('https');
const path = require('path');
const os = require('os');

const REPO = 'Idora-LLC/ebpf-image';
const ASSET = 'ci-tracer-linux-amd64';

const stateDir = process.env.RUNNER_TEMP || os.tmpdir();
const binPath = path.join(stateDir, 'ci-tracer');
const logPath = path.join(stateDir, 'ci-recorder.log');
const pidPath = path.join(stateDir, 'ci-recorder.pid');

// Env the agent needs (underscored names, matching its INPUT_<NAME> lookups).
// The token is preserved through sudo by NAME only (never placed in argv) so it
// does not appear in the process listing.
const PASSTHROUGH = [
  'INPUT_PIPELINE_URL', 'INPUT_PIPELINE_TOKEN', 'INPUT_WHITELIST', 'INPUT_TYPE',
  'INPUT_DEPLOY_TARGET', 'INPUT_ENV_ALLOWLIST', 'INPUT_HARD_FAIL', 'INPUT_DEBUG',
  'GITHUB_REPOSITORY', 'GITHUB_SHA', 'GITHUB_EVENT_NAME', 'GITHUB_EVENT_PATH',
  'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT', 'GITHUB_WORKSPACE', 'GITHUB_ENVIRONMENT',
  'RUNNER_TEMP',
];

function download(url, dest) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(dest);
    const get = (u) => https.get(u, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        return get(res.headers.location); // follow redirect (GitHub release CDN)
      }
      if (res.statusCode !== 200) {
        return reject(new Error(`GET ${u} -> ${res.statusCode}`));
      }
      res.pipe(file);
      file.on('finish', () => file.close(resolve));
    }).on('error', reject);
    get(url);
  });
}

function sha256(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

// GitHub maps input `pipeline-url` to INPUT_PIPELINE-URL (keeps the dash,
// uppercased). Normalize so the agent's INPUT_<NAME> lookups (underscores) work.
function normalizeInputEnv() {
  for (const key of Object.keys(process.env)) {
    if (key.startsWith('INPUT_') && key.includes('-')) {
      process.env[key.replace(/-/g, '_')] = process.env[key];
    }
  }
}

async function run() {
  try {
    normalizeInputEnv();

    const tag = process.env['INPUT_VERSION'] || process.env.GITHUB_ACTION_REF || 'latest';
    const base = `https://github.com/${REPO}/releases/download/${tag}`;

    console.log(`[ci-recorder] downloading ${ASSET} @ ${tag}`);
    await download(`${base}/${ASSET}`, binPath);
    await download(`${base}/${ASSET}.sha256`, `${binPath}.sha256`);

    // Verify the checksum before executing the privileged binary.
    const expected = fs.readFileSync(`${binPath}.sha256`, 'utf8').trim().split(/\s+/)[0];
    const actual = sha256(binPath);
    if (expected.toLowerCase() !== actual.toLowerCase()) {
      throw new Error(`checksum mismatch: expected ${expected}, got ${actual}`);
    }
    console.log('[ci-recorder] checksum verified');
    fs.chmodSync(binPath, 0o755);

    // Mount the tracing filesystems eBPF tracepoints need.
    try { execSync('sudo mount -t debugfs debugfs /sys/kernel/debug 2>/dev/null || true'); } catch {}
    try { execSync('sudo mount -t tracefs tracefs /sys/kernel/tracing 2>/dev/null || true'); } catch {}

    // Preserve only the named env (token by name, not value) through sudo.
    const preserve = PASSTHROUGH.join(',');
    const cmd = `sudo --preserve-env=${preserve} sh -c 'nohup ${binPath} > ${logPath} 2>&1 & echo $! > ${pidPath}'`;
    execSync(cmd, { stdio: 'inherit' });

    await new Promise((r) => setTimeout(r, 2000)); // let hooks attach
    const pid = fs.existsSync(pidPath) ? fs.readFileSync(pidPath, 'utf8').trim() : '?';
    console.log(`[ci-recorder] agent started (pid ${pid})`);
  } catch (err) {
    // Fail-open: a recorder start failure must never break the customer build.
    console.log(`::warning::CI recorder failed to start: ${err.message}`);
  }
}

run();
