//! Diagnostic / degradation codes that travel with the run so downstream
//! consumers can distinguish a complete capture from a degraded one rather than
//! reading a gap as clean. Mirrors the per-spec error tables:
//! `specs/observation.md` §9, `specs/content-hashing.md` §7, `specs/submission.md` §6.

/// eBPF unavailable; no record produced, gap reconciled as unknown.
pub const OBS_001: &str = "OBS_001";
/// `type` not attributable from adapter or whitelist; heuristic used.
pub const OBS_003: &str = "OBS_003";

/// A file in scope could not be read for hashing.
pub const HASH_R_001: &str = "HASH_R_001";

/// Pipeline unreachable / timeout after retries; buffered, reconciled unknown.
pub const SUB_001: &str = "SUB_001";
/// `401` from pipeline (bad/expired token).
pub const SUB_002: &str = "SUB_002";
/// `400`/`422` invalid record; not retried, logged as a Recorder defect.
pub const SUB_003: &str = "SUB_003";
/// Reconciliation shortfall: observed operations exceed submitted receipts.
pub const SUB_004: &str = "SUB_004";

/// The single supported capture fidelity (`specs/observation.md` §7).
pub const OBSERVATION_MODE: &str = "file_access_tracking";
