//! Agent configuration, sourced from the environment the Action sets up.
//!
//! Action inputs arrive as `INPUT_*` env vars; CI context arrives as the
//! platform's own vars (read by the adapter). The whitelist parser is pure and
//! unit-tested.

use crate::observe::WhitelistEntry;
use crate::runrecord::RunRecordType;

#[derive(Clone, Debug)]
pub struct Config {
    /// Pipeline base URL (`/pipeline/process` is appended on submit).
    pub pipeline_url: Option<String>,
    /// Bearer token for the pipeline; in-memory only, never logged.
    pub token: Option<String>,
    /// Commands that root an operation, with optional `type` tags.
    pub whitelist: Vec<WhitelistEntry>,
    /// Job-level operation `type` hint (adapter-authoritative).
    pub type_hint: Option<RunRecordType>,
    /// Deploy destination, if this job deploys.
    pub deploy_target: Option<String>,
    /// Environment keys the core may capture (allowlist-only).
    pub env_allowlist: Vec<String>,
    /// If true, the build is failed when eBPF is unavailable (opt-in).
    pub hard_fail: bool,
    /// Writable per-job state directory for buffering + signals.
    pub state_dir: String,
}

impl Config {
    pub fn from_env() -> Self {
        let whitelist = parse_whitelist(
            &input("WHITELIST").unwrap_or_else(|| "npm run build=>build,npm run test=>test".into()),
        );
        Self {
            pipeline_url: input("PIPELINE_URL").or_else(|| std::env::var("IDORA_PIPELINE_URL").ok()),
            token: input("PIPELINE_TOKEN").or_else(|| std::env::var("IDORA_PIPELINE_TOKEN").ok()),
            whitelist,
            type_hint: input("TYPE").and_then(|s| parse_type(&s)),
            deploy_target: input("DEPLOY_TARGET"),
            env_allowlist: input("ENV_ALLOWLIST")
                .map(|s| parse_csv(&s))
                .unwrap_or_default(),
            hard_fail: input("HARD_FAIL")
                .map(|s| matches!(s.to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
                .unwrap_or(false),
            state_dir: std::env::var("RUNNER_TEMP")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/tmp".to_string()),
        }
    }
}

fn input(name: &str) -> Option<String> {
    std::env::var(format!("INPUT_{name}"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse a `type` string into the execution variant.
pub fn parse_type(s: &str) -> Option<RunRecordType> {
    match s.trim().to_ascii_lowercase().as_str() {
        "build" => Some(RunRecordType::Build),
        "test" => Some(RunRecordType::Test),
        "deploy" => Some(RunRecordType::Deploy),
        _ => None,
    }
}

/// Parse a comma-separated list, trimming and dropping empties.
pub fn parse_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Parse a whitelist spec: comma-separated entries, each `command` or
/// `command=>type` (e.g. `npm run build=>build,npm run test=>test`).
pub fn parse_whitelist(s: &str) -> Vec<WhitelistEntry> {
    s.split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|entry| match entry.split_once("=>") {
            Some((cmd, ty)) => WhitelistEntry::new(cmd.trim(), parse_type(ty)),
            None => WhitelistEntry::new(entry, None),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tagged_and_untagged_entries() {
        let wl = parse_whitelist("npm run build=>build, npm run test => test, make deploy");
        assert_eq!(wl.len(), 3);
        assert_eq!(wl[0].pattern, "npm run build");
        assert_eq!(wl[0].type_tag, Some(RunRecordType::Build));
        assert_eq!(wl[1].pattern, "npm run test");
        assert_eq!(wl[1].type_tag, Some(RunRecordType::Test));
        assert_eq!(wl[2].pattern, "make deploy");
        assert_eq!(wl[2].type_tag, None);
    }

    #[test]
    fn csv_trims_and_drops_empties() {
        assert_eq!(parse_csv("A, B ,,C,"), vec!["A", "B", "C"]);
    }

    #[test]
    fn type_parsing_is_case_insensitive() {
        assert_eq!(parse_type("Deploy"), Some(RunRecordType::Deploy));
        assert_eq!(parse_type("nope"), None);
    }
}
