// post hook (specs/deployment.md §2, post-if: always()): signal the agent to
// finalize open operations, hash, assemble, submit, and reconcile -- even when
// earlier steps failed. Fail-open: this never fails the customer's build.

const { execSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');

const stateDir = process.env.RUNNER_TEMP || os.tmpdir();
const pidPath = path.join(stateDir, 'ci-recorder.pid');
const signalPath = path.join(stateDir, 'ci-recorder-reconciliation.json');

async function run() {
  try {
    if (fs.existsSync(pidPath)) {
      const pid = fs.readFileSync(pidPath, 'utf8').trim();
      // SIGTERM triggers finalize -> flush -> submit -> reconcile in the agent.
      try { execSync(`sudo kill -TERM ${pid}`); } catch {}
      // Give finalization + retries time within the post window.
      await new Promise((r) => setTimeout(r, 5000));
    } else {
      console.log('[ci-recorder] no agent pid found (recorder may not have started)');
    }

    // The agent runs as root, so make its outputs (log, reconciliation signal,
    // debug records JSONL, buffer) readable by the runner user for artifact upload.
    try { execSync(`sudo chmod -R a+rX ${stateDir}/ci-recorder-* 2>/dev/null || true`); } catch {}

    // Surface the reconciliation result so a gap reads as unknown, not clean.
    if (fs.existsSync(signalPath)) {
      const signal = JSON.parse(fs.readFileSync(signalPath, 'utf8'));
      console.log(
        `[ci-recorder] reconciliation: observed=${signal.observed} submitted=${signal.submitted} coverage=${signal.coverage}`
      );
      if (signal.coverage !== 'clean') {
        console.log('::warning::CI recorder coverage is unknown for this job (a dropped or unobserved record).');
      }
    } else {
      console.log('::warning::CI recorder produced no reconciliation signal (coverage unknown).');
    }
  } catch (err) {
    console.log(`::warning::CI recorder cleanup failed: ${err.message}`);
  }
}

run();
