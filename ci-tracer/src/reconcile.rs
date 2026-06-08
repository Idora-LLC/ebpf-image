//! Fail-open reconciliation (`specs/submission.md` §5).
//!
//! The Recorder never fails the CI step. Fail-open is only safe because a
//! dropped record is *detectable*: this module tracks observed-vs-submitted
//! counts, emits a start/stop heartbeat, buffers assembled-but-unsubmitted
//! records on disk during the `post` window, and writes a reconciliation signal
//! so a shortfall surfaces as **unknown**, never as a clean build (`SUB_004`).

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::diag;
use crate::runrecord::SubmissionBody;
use crate::submit::{SubmitOutcome, Submitter};

/// Coverage status reported at job end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// Every observed operation was submitted.
    Clean,
    /// At least one observed operation was not submitted.
    Unknown,
}

impl Coverage {
    pub fn as_str(self) -> &'static str {
        match self {
            Coverage::Clean => "clean",
            Coverage::Unknown => "unknown",
        }
    }
}

pub struct Reconciler {
    observed: u64,
    submitted: u64,
    buffer_dir: PathBuf,
    signal_path: PathBuf,
    heartbeat_path: PathBuf,
    next_buffer_seq: u64,
}

impl Reconciler {
    /// `state_dir` is a writable per-job directory (e.g. `RUNNER_TEMP`).
    pub fn new(state_dir: impl AsRef<Path>) -> Self {
        let state_dir = state_dir.as_ref();
        let buffer_dir = state_dir.join("ci-recorder-buffer");
        let _ = std::fs::create_dir_all(&buffer_dir);
        Self {
            observed: 0,
            submitted: 0,
            buffer_dir,
            signal_path: state_dir.join("ci-recorder-reconciliation.json"),
            heartbeat_path: state_dir.join("ci-recorder-heartbeat.json"),
            next_buffer_seq: 0,
        }
    }

    /// Emit the start heartbeat so a downstream consumer can distinguish
    /// "Recorder ran" from "Recorder never ran".
    pub fn heartbeat_start(&self) {
        self.write_heartbeat("started");
    }

    pub fn record_observed(&mut self) {
        self.observed += 1;
    }

    pub fn record_submitted(&mut self) {
        self.submitted += 1;
    }

    /// Buffer an assembled-but-unsubmitted record to disk for later retry.
    pub fn buffer(&mut self, body: &SubmissionBody) {
        let seq = self.next_buffer_seq;
        self.next_buffer_seq += 1;
        let path = self.buffer_dir.join(format!("record-{seq:06}.json"));
        match serde_json::to_string(body) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    eprintln!("[ci-recorder] failed to buffer record: {e}");
                }
            }
            Err(e) => eprintln!("[ci-recorder] failed to serialize buffered record: {e}"),
        }
    }

    /// Attempt to resend any buffered records (called during `post`). Each
    /// successfully delivered record is counted and its buffer file removed.
    pub fn flush_buffered(&mut self, submitter: &Submitter) -> Result<()> {
        let entries = match std::fs::read_dir(&self.buffer_dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(json) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(body) = serde_json::from_str::<SubmissionBody>(&json) else {
                continue;
            };
            if let SubmitOutcome::Success { receipt_id, .. } = submitter.submit(&body) {
                eprintln!("[ci-recorder] flushed buffered record -> {receipt_id}");
                self.submitted += 1;
                let _ = std::fs::remove_file(&path);
            }
        }
        Ok(())
    }

    pub fn coverage(&self) -> Coverage {
        if self.submitted >= self.observed {
            Coverage::Clean
        } else {
            Coverage::Unknown
        }
    }

    /// Write the reconciliation signal and stop heartbeat. Logs a shortfall as
    /// unknown coverage (`SUB_004`) rather than swallowing it.
    pub fn finish(&self) {
        let coverage = self.coverage();
        if coverage == Coverage::Unknown {
            eprintln!(
                "[ci-recorder] {}: observed {} operations but submitted {}; coverage=unknown",
                diag::SUB_004, self.observed, self.submitted
            );
        } else {
            eprintln!(
                "[ci-recorder] reconciliation: observed {} submitted {}; coverage=clean",
                self.observed, self.submitted
            );
        }

        let signal = format!(
            r#"{{"observed":{},"submitted":{},"coverage":"{}"}}"#,
            self.observed,
            self.submitted,
            coverage.as_str()
        );
        let _ = std::fs::write(&self.signal_path, signal);
        self.write_heartbeat("stopped");
    }

    fn write_heartbeat(&self, state: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        let body = format!(r#"{{"state":"{state}","at":"{now}"}}"#);
        let _ = std::fs::write(&self.heartbeat_path, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_clean_when_all_submitted() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Reconciler::new(dir.path());
        r.record_observed();
        r.record_observed();
        r.record_submitted();
        r.record_submitted();
        assert_eq!(r.coverage(), Coverage::Clean);
    }

    #[test]
    fn coverage_unknown_on_shortfall() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Reconciler::new(dir.path());
        r.record_observed();
        r.record_observed();
        r.record_submitted();
        assert_eq!(r.coverage(), Coverage::Unknown);
    }

    #[test]
    fn finish_writes_signal_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Reconciler::new(dir.path());
        r.record_observed();
        r.finish();
        let signal = std::fs::read_to_string(dir.path().join("ci-recorder-reconciliation.json")).unwrap();
        assert!(signal.contains("\"coverage\":\"unknown\""));
    }
}
