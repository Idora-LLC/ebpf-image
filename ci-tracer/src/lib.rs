//! The Idora Recorder core engine (userspace).
//!
//! The platform-agnostic observe -> resolve -> hash -> assemble -> submit
//! pipeline plus the CI-adapter boundary (`specs/architecture.md` §3). Kernel
//! interaction (Aya load/attach + ring-buffer drain) lives in the binary
//! (`main.rs`); everything here is portable and unit-testable.

pub mod adapter;
pub mod assemble;
pub mod config;
pub mod detect;
pub mod diag;
pub mod events;
pub mod hash;
pub mod observe;
pub mod reconcile;
pub mod resolve;
pub mod runrecord;
pub mod scope;
pub mod submit;
