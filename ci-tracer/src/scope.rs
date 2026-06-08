//! Input/output inclusion scope (`specs/observation.md` §8).
//!
//! Kernel-global observation sees everything; "what counts as an input/output"
//! is a scoping decision. The rule: only paths under the operation's
//! `working_directory` / repo root are recorded; system paths and `node_modules`
//! are excluded; out-of-tree paths are excluded (they cannot be relativized and
//! so cannot join). Within the repo root we over-include rather than
//! under-include (a missed in-repo input is a false clean).

/// Directory segments that are excluded even when under the repo root.
/// System paths (`/usr`, `/lib`, toolchain caches) are already excluded by the
/// under-working-directory bound, so only in-tree noise needs listing here.
const EXCLUDED_SEGMENTS: &[&str] = &["/node_modules/", "/.git/"];

/// Returns `true` when `path` should be recorded as an input/output for an
/// operation rooted at `working_directory`.
pub fn in_scope(path: &str, working_directory: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }

    // Must live under the operation's working directory (the relativization
    // root). This bounds scope to the repo tree and excludes system paths and
    // out-of-tree reads (which cannot be relativized and so cannot join).
    if working_directory.is_empty() || !is_under(path, working_directory) {
        return false;
    }

    if EXCLUDED_SEGMENTS.iter().any(|s| path.contains(s)) {
        return false;
    }

    true
}

/// Path-aware "is `path` under `root`" that respects directory boundaries
/// (so `/work/repo2/a` is not considered under `/work/repo`).
fn is_under(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    if path == root {
        return false; // the directory itself is not a file input
    }
    match path.strip_prefix(root) {
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WD: &str = "/work/repo";

    #[test]
    fn includes_repo_relative_source() {
        assert!(in_scope("/work/repo/src/auth.ts", WD));
        assert!(in_scope("/work/repo/dist/out.js", WD));
    }

    #[test]
    fn excludes_node_modules_and_git() {
        assert!(!in_scope("/work/repo/node_modules/x/index.js", WD));
        assert!(!in_scope("/work/repo/.git/HEAD", WD));
    }

    #[test]
    fn excludes_system_paths() {
        assert!(!in_scope("/usr/lib/x.so", WD));
        assert!(!in_scope("/etc/passwd", WD));
        assert!(!in_scope("/tmp/scratch", WD));
    }

    #[test]
    fn excludes_out_of_tree() {
        assert!(!in_scope("/work/other/file.ts", WD));
        assert!(!in_scope("/home/runner/.cache/x", WD));
    }

    #[test]
    fn excludes_relative_and_directory_root() {
        assert!(!in_scope("relative/path", WD));
        assert!(!in_scope("/work/repo", WD));
    }

    #[test]
    fn respects_directory_boundary() {
        assert!(!in_scope("/work/repo2/a.ts", WD));
    }
}
