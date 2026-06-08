//! Assembly of a finalized [`Operation`] into an execution RunRecord + sidecar
//! (`specs/run-record.md`).
//!
//! Resolves the final operation `type` with the spec precedence (adapter hint >
//! whitelist tag > heuristic, `specs/observation.md` §5), hashes the scoped
//! inputs/outputs, probes host fields, and emits a flat snake_case record plus
//! the [`ExecutionMetadataSidecar`].

use anyhow::{Context, Result};
use chrono::SecondsFormat;

use crate::adapter::CiAdapter;
use crate::diag;
use crate::hash::Hasher;
use crate::observe::Operation;
use crate::resolve;
use crate::runrecord::{
    ArtifactEntry, ExecutionMetadataSidecar, RunRecord, RunRecordType, SubmissionBody, Timestamps,
};

/// Resolve the operation `type` per the precedence order. Returns the resolved
/// type and whether it came from the heuristic last resort (which is flagged).
pub fn resolve_type(
    adapter_hint: Option<RunRecordType>,
    whitelist_tag: Option<RunRecordType>,
    command: &str,
) -> (RunRecordType, bool) {
    if let Some(t) = adapter_hint {
        return (t, false);
    }
    if let Some(t) = whitelist_tag {
        return (t, false);
    }
    let lc = command.to_ascii_lowercase();
    let t = if lc.contains("deploy") {
        RunRecordType::Deploy
    } else if lc.contains("test") {
        RunRecordType::Test
    } else {
        RunRecordType::Build
    };
    (t, true)
}

/// Assemble one operation. `env_vars` is the process environment to filter
/// through the adapter's allowlist.
pub fn assemble<A: CiAdapter>(
    op: &Operation,
    adapter: &A,
    hasher: &Hasher,
    env_vars: Vec<(String, String)>,
) -> Result<SubmissionBody> {
    let working_directory = op
        .working_directory
        .clone()
        .context("operation has no working_directory; cannot relativize/join")?;
    let repo = adapter.repo().context("adapter could not source repo")?;
    let commit = adapter.commit().context("adapter could not source commit")?;

    let command = op
        .command
        .clone()
        .unwrap_or_else(|| op.root_comm.clone());

    let (run_type, heuristic) = resolve_type(adapter.type_hint(), op.whitelist_type, &command);
    if heuristic {
        // Flag for reconciliation; a heuristic-derived type is never silently trusted.
        eprintln!("[ci-recorder] {}: type for {command:?} derived from heuristic", diag::OBS_003);
    }

    let inputs = hash_set(&op.scoped_inputs(), |p| hasher.hash_input(p));
    let outputs = hash_set(&op.scoped_outputs(), |p| hasher.hash_output(p));

    let environment = adapter.env_allowlist().capture(env_vars);

    let end = op.end_time.unwrap_or_else(chrono::Utc::now);
    let duration_ms = (end - op.start_time).num_milliseconds().max(0) as u64;
    let start_iso = op.start_time.to_rfc3339_opts(SecondsFormat::Millis, true);
    let end_iso = end.to_rfc3339_opts(SecondsFormat::Millis, true);

    let run_record = RunRecord {
        run_type,
        command,
        exit_code: op.exit_code,
        platform: resolve::platform(),
        architecture: resolve::architecture(),
        working_directory,
        repo,
        commit,
        tool_versions: resolve::tool_versions(),
        inputs,
        outputs,
        environment,
        timestamps: Timestamps {
            start_time: start_iso.clone(),
            end_time: end_iso.clone(),
            duration_ms,
        },
    };

    let identity = adapter.run_identity();
    let metadata = ExecutionMetadataSidecar {
        start_time: Some(start_iso),
        end_time: Some(end_iso),
        duration_ms: Some(duration_ms),
        observation_mode: Some(diag::OBSERVATION_MODE.to_string()),
        run_id: identity.run_id,
        run_attempt: identity.run_attempt,
        deploy_target: adapter.deploy_target(),
    };

    Ok(SubmissionBody {
        run_record,
        metadata,
    })
}

/// Hash a set of paths into artifact entries, flagging unreadable files with
/// `HASH_R_001` instead of silently dropping them. Returns `None` for an empty
/// set so the wire carries `null` rather than `[]` where appropriate.
fn hash_set(
    paths: &[String],
    mut hash_one: impl FnMut(&str) -> Result<String>,
) -> Option<Vec<ArtifactEntry>> {
    if paths.is_empty() {
        return None;
    }
    let mut entries = Vec::new();
    for p in paths {
        match hash_one(p) {
            Ok(hash) => entries.push(ArtifactEntry {
                path: p.clone(),
                hash,
            }),
            Err(e) => {
                eprintln!("[ci-recorder] {}: {p}: {e}", diag::HASH_R_001);
            }
        }
    }
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_hint_wins() {
        let (t, h) = resolve_type(Some(RunRecordType::Deploy), Some(RunRecordType::Build), "npm run build");
        assert_eq!(t, RunRecordType::Deploy);
        assert!(!h);
    }

    #[test]
    fn whitelist_tag_used_without_hint() {
        let (t, h) = resolve_type(None, Some(RunRecordType::Test), "whatever");
        assert_eq!(t, RunRecordType::Test);
        assert!(!h);
    }

    #[test]
    fn heuristic_is_flagged() {
        let (t, h) = resolve_type(None, None, "sh ./scripts/deploy.sh");
        assert_eq!(t, RunRecordType::Deploy);
        assert!(h);

        let (t2, h2) = resolve_type(None, None, "make all");
        assert_eq!(t2, RunRecordType::Build);
        assert!(h2);
    }
}
