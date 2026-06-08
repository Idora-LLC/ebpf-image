//! Resolution of the non-I/O execution fields (`specs/observation.md` §6).
//!
//! Provides the runtime [`ProcSource`] implementation over `/proc` and the
//! host/toolchain probes for `platform`, `architecture`, and `tool_versions`
//! (`specs/run-record.md` §2, §7).

use std::collections::BTreeMap;
use std::process::Command;

use crate::observe::ProcSource;

/// `/proc`-backed [`ProcSource`]. Reads must happen while the process is alive,
/// which is why operation roots are captured eagerly during observation.
pub struct ProcFs;

impl ProcSource for ProcFs {
    fn cmdline(&self, pid: u32) -> Option<String> {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        if raw.is_empty() {
            return None;
        }
        // argv is NUL-separated; join with spaces into a readable command line.
        let parts: Vec<String> = raw
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        let target = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
        Some(target.to_string_lossy().into_owned())
    }
}

/// Host OS name probe (`uname -s` equivalent).
pub fn platform() -> String {
    // The agent only runs on Linux runners; report the canonical name.
    "linux".to_string()
}

/// Host CPU architecture normalized to the pipeline's vocabulary
/// (`amd64`/`arm64`).
pub fn architecture() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
    .to_string()
}

/// Best-effort toolchain version probe. Returns `None` when nothing is cheaply
/// resolvable rather than guessing (`tool_versions` is nullable).
pub fn tool_versions() -> Option<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for (tool, args) in [("node", "--version"), ("go", "version")] {
        if let Some(v) = probe(tool, args) {
            map.insert(tool.to_string(), v);
        }
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

fn probe(tool: &str, arg: &str) -> Option<String> {
    let out = Command::new(tool).arg(arg).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}
