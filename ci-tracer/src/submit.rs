//! Submission of an assembled RunRecord to the pipeline (`specs/submission.md`).
//!
//! `POST /pipeline/process` with a bearer token and `X-Trace-Id`, bounded
//! exponential retry, and idempotency by the content-addressed receipt id
//! (`idora-pipeline/specs/api-contract.md` §2-3). The token is held in memory
//! and never logged. Failure handling (fail-open + buffering) lives in
//! [`crate::reconcile`]; this module just performs one record's delivery.

use std::time::Duration;

use crate::diag;
use crate::runrecord::SubmissionBody;

/// How a submission attempt's HTTP status should be treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusClass {
    /// `200`: delivered.
    Success,
    /// `400`/`422`: malformed record, a Recorder bug; do not retry (`SUB_003`).
    InvalidRecord,
    /// `401`: bad/expired token; do not retry (`SUB_002`).
    Unauthorized,
    /// `5xx`/`503`/timeout: transient; retry (`SUB_001` if exhausted).
    Retry,
}

/// Pure status mapping (`specs/submission.md` §3).
pub fn classify_status(code: u16) -> StatusClass {
    match code {
        200 => StatusClass::Success,
        400 | 422 => StatusClass::InvalidRecord,
        401 => StatusClass::Unauthorized,
        _ => StatusClass::Retry,
    }
}

/// The terminal outcome of submitting one record.
#[derive(Clone, Debug)]
pub enum SubmitOutcome {
    Success { receipt_id: String },
    /// `400`/`422` (`SUB_003`).
    InvalidRecord,
    /// `401` (`SUB_002`).
    Unauthorized,
    /// Transient failure after retries (`SUB_001`); caller should buffer.
    Failed,
}

pub struct Submitter {
    process_url: String,
    token: String,
    max_attempts: u32,
    timeout: Duration,
}

impl Submitter {
    /// `base_url` is the pipeline root (e.g. `https://pipeline.idora.dev`); the
    /// `/pipeline/process` path is appended.
    pub fn new(base_url: &str, token: String) -> Self {
        let process_url = format!("{}/pipeline/process", base_url.trim_end_matches('/'));
        Self {
            process_url,
            token,
            max_attempts: 5,
            timeout: Duration::from_secs(30), // client timeout floor (§2)
        }
    }

    pub fn with_max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n.max(1);
        self
    }

    /// Submit one record, retrying transient failures with bounded backoff.
    pub fn submit(&self, body: &SubmissionBody) -> SubmitOutcome {
        let trace_id = uuid::Uuid::new_v4().to_string();
        let json = match serde_json::to_string(body) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[ci-recorder] {}: serialize failed: {e}", diag::SUB_003);
                return SubmitOutcome::InvalidRecord;
            }
        };

        let mut backoff = Duration::from_millis(250);
        for attempt in 1..=self.max_attempts {
            match self.attempt(&json, &trace_id) {
                AttemptResult::Success(receipt_id) => {
                    return SubmitOutcome::Success { receipt_id }
                }
                AttemptResult::Class(StatusClass::InvalidRecord) => {
                    eprintln!("[ci-recorder] {}: pipeline rejected record", diag::SUB_003);
                    return SubmitOutcome::InvalidRecord;
                }
                AttemptResult::Class(StatusClass::Unauthorized) => {
                    eprintln!("[ci-recorder] {}: pipeline returned 401", diag::SUB_002);
                    return SubmitOutcome::Unauthorized;
                }
                AttemptResult::Class(_) => {
                    if attempt < self.max_attempts {
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(8));
                    }
                }
            }
        }
        eprintln!(
            "[ci-recorder] {}: submission failed after {} attempts; buffering",
            diag::SUB_001,
            self.max_attempts
        );
        SubmitOutcome::Failed
    }

    fn attempt(&self, json: &str, trace_id: &str) -> AttemptResult {
        let resp = ureq::post(&self.process_url)
            .timeout(self.timeout)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Content-Type", "application/json")
            .set("X-Trace-Id", trace_id)
            .send_string(json);

        match resp {
            Ok(r) => match parse_receipt(r) {
                Some(id) => AttemptResult::Success(id),
                None => AttemptResult::Class(StatusClass::Retry),
            },
            // Non-2xx with a status: map per the retry policy.
            Err(ureq::Error::Status(code, _resp)) => AttemptResult::Class(classify_status(code)),
            // Transport / connection / timeout (and any future variant): retry.
            Err(_) => AttemptResult::Class(StatusClass::Retry),
        }
    }
}

enum AttemptResult {
    Success(String),
    Class(StatusClass),
}

fn parse_receipt(resp: ureq::Response) -> Option<String> {
    let body = resp.into_string().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v.get("receiptId")?.as_str().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert_eq!(classify_status(200), StatusClass::Success);
        assert_eq!(classify_status(400), StatusClass::InvalidRecord);
        assert_eq!(classify_status(422), StatusClass::InvalidRecord);
        assert_eq!(classify_status(401), StatusClass::Unauthorized);
        assert_eq!(classify_status(500), StatusClass::Retry);
        assert_eq!(classify_status(503), StatusClass::Retry);
    }

    #[test]
    fn process_url_is_appended_once() {
        let s = Submitter::new("https://pipe.example/", "tok".into());
        assert_eq!(s.process_url, "https://pipe.example/pipeline/process");
    }
}
