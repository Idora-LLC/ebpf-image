const { execSync } = require('child_process');
const fs = require('fs');

const TRACE_FILE = '/var/log/ci-trace.jsonl';
const PID_FILE = '/tmp/.ci-tracer.pid';

async function run() {
  try {
    if (fs.existsSync(PID_FILE)) {
      const pid = fs.readFileSync(PID_FILE, 'utf8').trim();
      try {
        // The tracer runs under sudo, so we need sudo to kill it
        execSync(`sudo kill -TERM ${pid}`);
        await new Promise(r => setTimeout(r, 2000));
      } catch {}
    }

    if (fs.existsSync(TRACE_FILE)) {
      const stat = fs.statSync(TRACE_FILE);
      const content = fs.readFileSync(TRACE_FILE, 'utf8');
      const lines = content.split('\n').filter(l => l.length > 0);
      console.log(`[ci-recorder] Trace complete: ${lines.length} events, ${(stat.size / 1024).toFixed(1)} KB`);
    } else {
      console.log('[ci-recorder] No trace file generated');
    }
  } catch (error) {
    console.log(`::warning::CI recorder cleanup failed: ${error.message}`);
  }
}

run();
