//! A minimal stand-in for the idora-pipeline service, used only to exercise the
//! Recorder's submit + reconciliation paths end-to-end (the real service is
//! spec-only). It implements just enough of
//! `idora-pipeline/specs/api-contract.md`:
//!
//! - `GET  /health`            -> `200 {"status":"ok",...}`
//! - `POST /pipeline/process`  -> `200` with a content-addressed `receiptId`,
//!                                `401` when the bearer token is missing/unknown.
//!
//! Received records are appended (as JSON lines) to `MOCK_LOG` when set, so a
//! test harness can assert what was submitted.
//!
//! Env:
//!   MOCK_ADDR   bind address (default 127.0.0.1:8787)
//!   MOCK_TOKEN  required bearer token; if unset, any non-empty Bearer is accepted
//!   MOCK_LOG    optional path to append received submission bodies

use std::io::{Read, Write};

use anyhow::Result;
use sha2::{Digest, Sha256};
use tiny_http::{Header, Method, Request, Response, Server};

fn main() -> Result<()> {
    let addr = std::env::var("MOCK_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".to_string());
    let server = Server::http(&addr).map_err(|e| anyhow::anyhow!("bind {addr}: {e}"))?;
    eprintln!("[mock-pipeline] listening on http://{addr}");

    for request in server.incoming_requests() {
        if let Err(e) = handle(request) {
            eprintln!("[mock-pipeline] handler error: {e}");
        }
    }
    Ok(())
}

fn handle(mut request: Request) -> Result<()> {
    let method = request.method().clone();
    let url = request.url().to_string();

    match (method, url.as_str()) {
        (Method::Get, "/health") => respond_json(
            request,
            200,
            r#"{"status":"ok","uptimeSeconds":0,"version":"mock"}"#,
        ),
        (Method::Post, "/pipeline/process") => handle_process(&mut request)
            .and_then(|(status, body)| respond_json(request, status, &body)),
        _ => respond_json(request, 404, r#"{"statusCode":404,"message":"not found"}"#),
    }
}

/// Returns the `(status, body)` to send; reads the request body via `&mut`
/// before the caller consumes the request to respond.
fn handle_process(request: &mut Request) -> Result<(u16, String)> {
    if !authorized(request) {
        return Ok((
            401,
            r#"{"statusCode":401,"stage":"auth","errorClass":"UnauthorizedError","code":"AUTH_001","message":"Invalid or missing bearer token"}"#.to_string(),
        ));
    }

    let trace_id = header(request, "X-Trace-Id").unwrap_or_else(|| "mock-trace".to_string());

    let mut body = String::new();
    request.as_reader().read_to_string(&mut body)?;

    if let Ok(path) = std::env::var("MOCK_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{body}");
        }
    }

    let value: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    let run_record = value
        .get("runRecord")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let run_type = run_record
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("build")
        .to_string();

    let canonical = serde_json::to_string(&run_record).unwrap_or_default();
    let receipt_id = sha256_hex(canonical.as_bytes());

    let resp = format!(
        r#"{{"receiptId":"{receipt_id}","schemaVersion":"1.0.0","type":"{run_type}","graphWritesCompleted":true,"traceId":"{trace_id}"}}"#
    );
    Ok((200, resp))
}

fn authorized(request: &Request) -> bool {
    let Some(auth) = header(request, "Authorization") else {
        return false;
    };
    let Some(token) = auth.strip_prefix("Bearer ") else {
        return false;
    };
    match std::env::var("MOCK_TOKEN") {
        Ok(expected) => token == expected,
        Err(_) => !token.is_empty(),
    }
}

fn header(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn respond_json(request: Request, status: u16, body: &str) -> Result<()> {
    let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .map_err(|_| anyhow::anyhow!("bad header"))?;
    let response = Response::from_string(body)
        .with_status_code(status)
        .with_header(header);
    // Ignore broken-pipe style errors from a client that already disconnected.
    let _ = request.respond(response);
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::from("sha256:");
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}
