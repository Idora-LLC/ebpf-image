//! End-to-end tests for the submission client against a real TCP server that
//! speaks just enough HTTP/1.1 to stand in for `POST /pipeline/process`. This
//! exercises the same request/retry/idempotency path the agent uses, without
//! needing the (spec-only) pipeline service.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

use ci_tracer::runrecord::{
    ExecutionMetadataSidecar, RunRecord, RunRecordType, SubmissionBody, Timestamps,
};
use ci_tracer::submit::{SubmitOutcome, Submitter};

/// Spawn a server that serves `responses` (status, body) for sequential
/// connections, then returns. Captures the first request line+headers+body and
/// sends it back over a channel.
fn serve(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        for (status, body) in responses {
            let (stream, _) = listener.accept().unwrap();
            let req = handle_conn(stream, status, body);
            let _ = tx.send(req);
        }
    });

    (base, rx)
}

fn handle_conn(mut stream: TcpStream, status: u16, body: &str) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    // Read until headers complete, then the declared body length.
    loop {
        let n = stream.read(&mut tmp).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            let headers = String::from_utf8_lossy(&buf[..pos]).to_string();
            let content_len = content_length(&headers);
            let have_body = buf.len() - (pos + 4);
            if have_body >= content_len {
                break;
            }
        }
    }

    let reason = if status == 200 { "OK" } else { "ERR" };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
    String::from_utf8_lossy(&buf).to_string()
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn content_length(headers: &str) -> usize {
    headers
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn sample_body() -> SubmissionBody {
    SubmissionBody {
        run_record: RunRecord {
            run_type: RunRecordType::Build,
            command: "npm run build".into(),
            exit_code: Some(0),
            platform: "linux".into(),
            architecture: "amd64".into(),
            working_directory: "/work/repo".into(),
            repo: "owner/name".into(),
            commit: "deadbeef".into(),
            tool_versions: None,
            inputs: None,
            outputs: None,
            environment: None,
            timestamps: Timestamps {
                start_time: "2026-04-01T15:00:00.000Z".into(),
                end_time: "2026-04-01T15:00:01.000Z".into(),
                duration_ms: 1000,
            },
        },
        metadata: ExecutionMetadataSidecar::default(),
    }
}

#[test]
fn success_returns_receipt_and_sends_auth_and_trace() {
    let receipt = r#"{"receiptId":"sha256:abc","schemaVersion":"1.0.0","type":"build","graphWritesCompleted":true,"traceId":"t"}"#;
    let (base, rx) = serve(vec![(200, receipt)]);

    let outcome = Submitter::new(&base, "secret-token".into()).submit(&sample_body());
    match outcome {
        SubmitOutcome::Success { receipt_id } => assert_eq!(receipt_id, "sha256:abc"),
        other => panic!("expected success, got {other:?}"),
    }

    let req = rx.recv().unwrap();
    assert!(req.contains("POST /pipeline/process"));
    assert!(req.contains("Authorization: Bearer secret-token"));
    assert!(req.to_lowercase().contains("x-trace-id:"));
    assert!(req.contains("\"runRecord\""));
}

#[test]
fn unauthorized_is_not_retried() {
    let (base, _rx) = serve(vec![(401, r#"{"code":"AUTH_001"}"#)]);
    let outcome = Submitter::new(&base, "bad".into()).submit(&sample_body());
    assert!(matches!(outcome, SubmitOutcome::Unauthorized { .. }));
}

#[test]
fn invalid_record_is_not_retried() {
    let (base, _rx) = serve(vec![(422, r#"{"code":"NORM_002"}"#)]);
    let outcome = Submitter::new(&base, "tok".into()).submit(&sample_body());
    assert!(matches!(outcome, SubmitOutcome::InvalidRecord { .. }));
}

#[test]
fn transient_503_is_retried_then_succeeds() {
    let receipt = r#"{"receiptId":"sha256:def","schemaVersion":"1.0.0","type":"build","graphWritesCompleted":true,"traceId":"t"}"#;
    let (base, _rx) = serve(vec![(503, r#"{"code":"STORE_005"}"#), (200, receipt)]);

    let outcome = Submitter::new(&base, "tok".into())
        .with_max_attempts(3)
        .submit(&sample_body());
    match outcome {
        SubmitOutcome::Success { receipt_id } => assert_eq!(receipt_id, "sha256:def"),
        other => panic!("expected success after retry, got {other:?}"),
    }
}
