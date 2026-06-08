//! The GitHub Actions adapter (`specs/ci-adapter.md` §8).
//!
//! Sources `repo`, the authoritative head `commit`, the `type` hint, deploy
//! target, run identity, and the env allowlist from the GitHub Actions
//! environment. The commit resolution handles the PR merge-vs-head pitfall (§4):
//! on `pull_request` events `GITHUB_SHA` is a synthetic merge commit, so the
//! adapter reads the PR **head** SHA from the event payload.

use std::process::Command;

use crate::adapter::{CiAdapter, EnvAllowlist, RunIdentity};
use crate::runrecord::RunRecordType;

#[derive(Clone, Debug)]
pub struct GithubAdapter {
    repo: Option<String>,
    commit: Option<String>,
    type_hint: Option<RunRecordType>,
    deploy_target: Option<String>,
    run_id: Option<String>,
    run_attempt: Option<String>,
    env_allowlist: EnvAllowlist,
}

impl GithubAdapter {
    /// Build the adapter from the live GitHub Actions environment plus the
    /// action-supplied `type` hint, deploy target, and env allowlist keys.
    pub fn from_env(
        type_hint: Option<RunRecordType>,
        deploy_target: Option<String>,
        allowlist_keys: Vec<String>,
    ) -> Self {
        let event_name = std::env::var("GITHUB_EVENT_NAME").ok();
        let github_sha = std::env::var("GITHUB_SHA").ok();
        let event_json = std::env::var("GITHUB_EVENT_PATH")
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok());

        let commit = resolve_commit(
            event_name.as_deref(),
            github_sha.as_deref(),
            event_json.as_deref(),
        )
        .or_else(|| git_head(std::env::var("GITHUB_WORKSPACE").ok().as_deref()));

        Self {
            repo: std::env::var("GITHUB_REPOSITORY").ok(),
            commit,
            type_hint,
            deploy_target: deploy_target.or_else(|| std::env::var("GITHUB_ENVIRONMENT").ok()),
            run_id: std::env::var("GITHUB_RUN_ID").ok(),
            run_attempt: std::env::var("GITHUB_RUN_ATTEMPT").ok(),
            env_allowlist: EnvAllowlist::new(allowlist_keys),
        }
    }
}

impl CiAdapter for GithubAdapter {
    fn repo(&self) -> Option<String> {
        self.repo.clone()
    }
    fn commit(&self) -> Option<String> {
        self.commit.clone()
    }
    fn type_hint(&self) -> Option<RunRecordType> {
        self.type_hint
    }
    fn deploy_target(&self) -> Option<String> {
        self.deploy_target.clone()
    }
    fn env_allowlist(&self) -> &EnvAllowlist {
        &self.env_allowlist
    }
    fn run_identity(&self) -> RunIdentity {
        RunIdentity {
            run_id: self.run_id.clone(),
            run_attempt: self.run_attempt.clone(),
        }
    }
}

/// Resolve the canonical commit (`specs/ci-adapter.md` §4). Pure for testing.
///
/// - `pull_request` / `pull_request_target`: the PR **head** SHA from the event
///   payload (`pull_request.head.sha`), never the synthetic merge `GITHUB_SHA`.
/// - everything else (push, merge_group, ...): `GITHUB_SHA`.
pub fn resolve_commit(
    event_name: Option<&str>,
    github_sha: Option<&str>,
    event_json: Option<&str>,
) -> Option<String> {
    if matches!(event_name, Some("pull_request") | Some("pull_request_target")) {
        if let Some(json) = event_json {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
                if let Some(sha) = v
                    .get("pull_request")
                    .and_then(|pr| pr.get("head"))
                    .and_then(|h| h.get("sha"))
                    .and_then(|s| s.as_str())
                {
                    if !sha.is_empty() {
                        return Some(sha.to_string());
                    }
                }
            }
        }
        // Fall through to GITHUB_SHA only if the head SHA was not resolvable.
    }
    github_sha.filter(|s| !s.is_empty()).map(|s| s.to_string())
}

/// Last-resort commit fallback: `git rev-parse HEAD` in the workspace.
fn git_head(workspace: Option<&str>) -> Option<String> {
    let mut cmd = Command::new("git");
    if let Some(ws) = workspace {
        cmd.arg("-C").arg(ws);
    }
    let out = cmd.args(["rev-parse", "HEAD"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERGE: &str = "1111111111111111111111111111111111111111";
    const HEAD: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn push_uses_github_sha() {
        assert_eq!(
            resolve_commit(Some("push"), Some(MERGE), None).as_deref(),
            Some(MERGE)
        );
    }

    #[test]
    fn pull_request_uses_head_sha_not_merge() {
        let json = format!(r#"{{"pull_request":{{"head":{{"sha":"{HEAD}"}}}}}}"#);
        assert_eq!(
            resolve_commit(Some("pull_request"), Some(MERGE), Some(&json)).as_deref(),
            Some(HEAD)
        );
    }

    #[test]
    fn pull_request_target_uses_head_sha() {
        let json = format!(r#"{{"pull_request":{{"head":{{"sha":"{HEAD}"}}}}}}"#);
        assert_eq!(
            resolve_commit(Some("pull_request_target"), Some(MERGE), Some(&json)).as_deref(),
            Some(HEAD)
        );
    }

    #[test]
    fn pull_request_without_payload_falls_back_to_github_sha() {
        assert_eq!(
            resolve_commit(Some("pull_request"), Some(MERGE), None).as_deref(),
            Some(MERGE)
        );
    }

    #[test]
    fn merge_group_uses_github_sha() {
        assert_eq!(
            resolve_commit(Some("merge_group"), Some(HEAD), None).as_deref(),
            Some(HEAD)
        );
    }
}
