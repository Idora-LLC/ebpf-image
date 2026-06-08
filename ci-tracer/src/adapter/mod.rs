//! The CI-adapter boundary that makes the Recorder portable
//! (`specs/ci-adapter.md`).
//!
//! The observe/hash/assemble/submit core never reads `GITHUB_*` (or `CI_*`)
//! variables directly; it asks an adapter. Supporting a new CI platform is
//! implementing this trait against a new system. GitHub Actions is the only
//! concrete adapter today ([`github::GithubAdapter`]).

use std::collections::{BTreeMap, BTreeSet};

use crate::runrecord::RunRecordType;

pub mod github;

/// Deploy-event identity carried on the sidecar (`specs/ci-adapter.md` §7).
/// Deliberately not part of the hashed record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunIdentity {
    pub run_id: Option<String>,
    pub run_attempt: Option<String>,
}

/// The allowlist of environment keys the core may capture. Allowlist-only, never
/// denylist, so an unknown custom secret is dropped not leaked
/// (`specs/security.md` §3, `specs/ci-adapter.md` §6).
#[derive(Clone, Debug, Default)]
pub struct EnvAllowlist {
    allowed: BTreeSet<String>,
}

impl EnvAllowlist {
    pub fn new<I, S>(keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            allowed: keys.into_iter().map(Into::into).collect(),
        }
    }

    /// Apply the allowlist to a set of environment variables. Returns `None`
    /// when the allowlist is empty (the default minimal posture: capture
    /// nothing, emit `environment: null`).
    pub fn capture<I>(&self, vars: I) -> Option<BTreeMap<String, String>>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        if self.allowed.is_empty() {
            return None;
        }
        let captured: BTreeMap<String, String> = vars
            .into_iter()
            .filter(|(k, _)| self.allowed.contains(k))
            .collect();
        if captured.is_empty() {
            None
        } else {
            Some(captured)
        }
    }
}

/// The interface the core depends on (`specs/ci-adapter.md` §2). The `type` hint
/// and deploy target are job-level here (sourced from action inputs) rather than
/// per-operation; the trait can be widened per-operation without touching the
/// core.
pub trait CiAdapter {
    /// Repository identifier in the verification side's canonical form (§3).
    fn repo(&self) -> Option<String>;
    /// Authoritative commit: the head SHA that shipped (§4).
    fn commit(&self) -> Option<String>;
    /// Operation `type` hint, authoritative over whitelist tag/heuristic (§5.1).
    fn type_hint(&self) -> Option<RunRecordType>;
    /// Deploy destination, e.g. `prod`/`staging` (§5.2).
    fn deploy_target(&self) -> Option<String>;
    /// Environment-capture allowlist (§6).
    fn env_allowlist(&self) -> &EnvAllowlist;
    /// `run_id` / `run_attempt` for re-deploy identity (§7).
    fn run_identity(&self) -> RunIdentity;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allowlist_captures_nothing() {
        let al = EnvAllowlist::default();
        assert!(al
            .capture([("FOO".to_string(), "bar".to_string())])
            .is_none());
    }

    #[test]
    fn allowlist_is_fail_closed() {
        let al = EnvAllowlist::new(["NODE_ENV"]);
        let captured = al
            .capture([
                ("NODE_ENV".to_string(), "production".to_string()),
                ("SECRET_TOKEN".to_string(), "leak".to_string()),
            ])
            .unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured.get("NODE_ENV").unwrap(), "production");
        assert!(captured.get("SECRET_TOKEN").is_none());
    }
}
