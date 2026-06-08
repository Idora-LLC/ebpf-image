//! Content hashing (`specs/content-hashing.md`).
//!
//! Produces a `sha256:<64 lowercase hex>` digest for every in-scope file
//! (`idora-pipeline/specs/data-types.md` §7). Tracked source **inputs** are
//! hashed over their **git-blob-normalized** content (eol/clean/LFS filters
//! applied) so checkout-time CRLF/LFS differences do not break the join (§5);
//! generated **outputs** are hashed as **raw bytes**. There is no non-hashing
//! mode (§2.3): the only variation is the TOCTOU grade.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// The achieved hash fidelity tier (`specs/content-hashing.md` §4). Hosted
/// runners are `userspace-hashed`; the `kernel-atomic` tier is self-hosted-only,
/// requires BPF-LSM, and is gated behind the `kernel-atomic` build feature.
/// Both tiers always hash real content; the tier sets only the TOCTOU grade.
pub fn hash_tier() -> &'static str {
    if cfg!(feature = "kernel-atomic") && crate::detect::bpf_lsm_available() {
        "kernel-atomic"
    } else {
        "userspace-hashed"
    }
}

/// Format raw bytes as the canonical `sha256:` digest string.
pub fn sha256_of(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(71);
    s.push_str("sha256:");
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Computes content hashes for one operation's files, rooted at a repo.
pub struct Hasher {
    repo_root: PathBuf,
    /// Whether `git` plumbing is usable for this repo (probed once).
    git_available: bool,
}

impl Hasher {
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        let repo_root = repo_root.into();
        let git_available = git_repo_present(&repo_root);
        Self {
            repo_root,
            git_available,
        }
    }

    /// Hash an input (read) file. Tracked source files are normalized via git;
    /// everything else falls back to raw bytes.
    pub fn hash_input(&self, abs_path: &str) -> Result<String> {
        if self.git_available && self.is_tracked(abs_path) {
            if let Ok(bytes) = self.git_blob_normalized(abs_path) {
                return Ok(sha256_of(&bytes));
            }
            // Fall through to raw bytes if git normalization fails; we still
            // produce a real digest rather than dropping the entry (HASH_R_001
            // is reserved for genuinely unreadable files).
        }
        self.hash_raw(abs_path)
    }

    /// Hash an output (written/created) file as raw, finalized bytes.
    pub fn hash_output(&self, abs_path: &str) -> Result<String> {
        self.hash_raw(abs_path)
    }

    fn hash_raw(&self, abs_path: &str) -> Result<String> {
        let bytes =
            std::fs::read(abs_path).with_context(|| format!("{abs_path}: read for hashing"))?;
        Ok(sha256_of(&bytes))
    }

    fn rel_path(&self, abs_path: &str) -> Option<String> {
        Path::new(abs_path)
            .strip_prefix(&self.repo_root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    }

    fn is_tracked(&self, abs_path: &str) -> bool {
        let Some(rel) = self.rel_path(abs_path) else {
            return false;
        };
        Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .args(["ls-files", "--error-unmatch", "--"])
            .arg(&rel)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Return the file's content as git would store it (after clean/eol/LFS
    /// normalization), independent of how the working tree was materialized.
    fn git_blob_normalized(&self, abs_path: &str) -> Result<Vec<u8>> {
        let rel = self
            .rel_path(abs_path)
            .context("path not under repo root")?;

        // `hash-object -w --path <rel>` applies the path's gitattributes filters
        // and writes the resulting blob, returning its OID.
        let oid_out = Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .args(["hash-object", "-w", "--path"])
            .arg(&rel)
            .arg("--")
            .arg(abs_path)
            .output()
            .context("git hash-object")?;
        anyhow::ensure!(oid_out.status.success(), "git hash-object failed");
        let oid = String::from_utf8(oid_out.stdout)?.trim().to_string();
        anyhow::ensure!(!oid.is_empty(), "git hash-object produced no oid");

        // `cat-file blob <oid>` yields the normalized bytes that went into the
        // blob; sha256 of these is the join-compatible content hash.
        let blob = Command::new("git")
            .arg("-C")
            .arg(&self.repo_root)
            .args(["cat-file", "blob"])
            .arg(&oid)
            .output()
            .context("git cat-file")?;
        anyhow::ensure!(blob.status.success(), "git cat-file failed");
        Ok(blob.stdout)
    }
}

fn git_repo_present(repo_root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_format_and_known_vectors() {
        assert_eq!(
            sha256_of(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_of(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_is_lowercase_hex_71_chars() {
        let h = sha256_of(b"some content");
        assert!(h.starts_with("sha256:"));
        assert_eq!(h.len(), 71);
        assert!(h["sha256:".len()..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn raw_hash_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, b"abc").unwrap();
        let h = Hasher::new(dir.path());
        assert_eq!(
            h.hash_output(p.to_str().unwrap()).unwrap(),
            sha256_of(b"abc")
        );
    }
}
