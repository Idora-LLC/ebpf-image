//! Startup eBPF-availability detection (`specs/deployment.md` §4).
//!
//! The agent must decide deterministically whether it can capture, and never
//! silently produce a low-fidelity record that looks complete. On a runner
//! without eBPF the default is no-op (no record, build not failed); the gap is
//! reconciled as unknown. Hard-fail is an opt-in policy.

use std::path::Path;

/// A coarse pre-flight check for eBPF capability. The authoritative check is
/// whether the programs actually load/attach (handled in `main`); this avoids
/// even trying on obviously unsupported runners.
pub fn ebpf_available() -> bool {
    has_privilege() && tracefs_present()
}

/// eBPF attach needs `CAP_BPF`/`CAP_PERFMON` (or root). We approximate by
/// checking for an effective uid of 0; the agent is started under `sudo`.
fn has_privilege() -> bool {
    // SAFETY: geteuid has no preconditions and never fails.
    unsafe { libc::geteuid() == 0 }
}

/// Tracepoints require a mounted tracefs/debugfs.
fn tracefs_present() -> bool {
    Path::new("/sys/kernel/tracing/events").exists()
        || Path::new("/sys/kernel/debug/tracing/events").exists()
}

/// Whether the self-hosted kernel-atomic hashing tier (BPF-LSM /
/// `bpf_ima_file_hash`) could be enabled here (`specs/content-hashing.md` §4).
/// Never true on hosted runners (BPF-LSM needs a GRUB change + reboot); used
/// only to decide whether to attempt the premium tier scaffolding.
pub fn bpf_lsm_available() -> bool {
    std::fs::read_to_string("/sys/kernel/security/lsm")
        .map(|s| s.split(',').any(|m| m.trim() == "bpf"))
        .unwrap_or(false)
}
