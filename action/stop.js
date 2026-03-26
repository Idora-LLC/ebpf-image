const { execSync } = require('child_process');
const fs = require('fs');

const TRACE_FILE = '/var/log/ci-trace.jsonl';
const PID_FILE = '/tmp/.ci-tracer.pid';

async function run() {
  try {
    // Stop the tracer
    if (fs.existsSync(PID_FILE)) {
      const pid = fs.readFileSync(PID_FILE, 'utf8').trim();
      try {
        process.kill(parseInt(pid), 'SIGTERM');
        // Wait for clean shutdown
        await new Promise(r => setTimeout(r, 2000));
      } catch {}
    }

    // Print summary
    if (fs.existsSync(TRACE_FILE)) {
      const stat = fs.statSync(TRACE_FILE);
      const lines = fs.readFileSync(TRACE_FILE, 'utf8').split('\n').filter(l => l.length > 0);
      console.log(`[ci-recorder] Trace complete: ${lines.length} events, ${(stat.size / 1024).toFixed(1)} KB`);

      // Upload artifact if requested
      const uploadArtifact = (process.env.INPUT_UPLOAD_ARTIFACT || 'true') === 'true';
      const artifactName = process.env.INPUT_ARTIFACT_NAME || 'ci-trace';

      if (uploadArtifact) {
        // Use the artifact upload CLI (available in the runner)
        try {
          // Write the file path for @actions/artifact
          console.log(`::set-output name=trace-file::${TRACE_FILE}`);

          // Use the actions/upload-artifact approach via the toolkit
          // Since we can't import @actions/artifact without node_modules,
          // we'll just indicate where the file is. The user can add the
          // upload step, or we document that the post step leaves the file
          // at a known path.
          console.log(`[ci-recorder] Trace file: ${TRACE_FILE}`);
          console.log(`[ci-recorder] Add 'uses: actions/upload-artifact@v4' with path '${TRACE_FILE}' to upload`);
        } catch (e) {
          console.log(`::warning::Could not upload artifact: ${e.message}`);
        }
      }
    } else {
      console.log('[ci-recorder] No trace file found');
    }
  } catch (error) {
    console.log(`::warning::Failed to stop CI recorder: ${error.message}`);
  }
}

run();
