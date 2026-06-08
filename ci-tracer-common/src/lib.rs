#![no_std]

//! Shared, `no_std` event contract between the kernel-side BPF programs
//! (`ci-tracer-ebpf`) and the userspace agent (`ci-tracer`).
//!
//! These structs are the kernel/userspace boundary referenced in
//! `specs/architecture.md` §5 and `specs/observation.md` §3. Broadening I/O
//! coverage adds variants/fields here plus attach points on the kernel side,
//! without changing the userspace consumption contract.

/// A process called `execve()`.
pub const EVENT_PROCESS_EXEC: u32 = 1;
/// A process terminated.
pub const EVENT_PROCESS_EXIT: u32 = 2;
/// A file access (open / openat2 / rename / unlink / truncate).
pub const EVENT_FILE_ACCESS: u32 = 3;

/// Length of the kernel `comm` field (`TASK_COMM_LEN`).
pub const COMM_LEN: usize = 16;
/// Maximum captured path length.
pub const PATH_LEN: usize = 256;

// ---------------------------------------------------------------------------
// File-access classification
// ---------------------------------------------------------------------------

/// File was opened for reading (a candidate **input**).
pub const ACCESS_READ: u32 = 0;
/// File was opened for writing/modification (a candidate **output**).
pub const ACCESS_WRITE: u32 = 1;
/// File was created (`O_CREAT`) (a candidate **output**).
pub const ACCESS_CREATE: u32 = 2;
/// File was removed (`unlinkat`) (an **output** removal).
pub const ACCESS_DELETE: u32 = 3;
/// File was truncated in place (`truncate`/`ftruncate`) (an **output**).
pub const ACCESS_TRUNCATE: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessExecEvent {
    pub event_type: u32,
    pub pid: u32,
    pub ppid: u32,
    pub comm: [u8; COMM_LEN],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessExitEvent {
    pub event_type: u32,
    pub pid: u32,
    /// Exit status observed from `sys_enter_exit_group`. Only meaningful when
    /// `code_observed != 0`; the agent maps "not observed" to `exit_code: null`
    /// and never defaults it to 0 (`specs/run-record.md` §2).
    pub exit_code: i32,
    /// `1` when `exit_code` was captured, `0` when unobservable.
    pub code_observed: u32,
    pub comm: [u8; COMM_LEN],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FileAccessEvent {
    pub event_type: u32,
    pub pid: u32,
    /// One of the `ACCESS_*` constants.
    pub access: u32,
    pub comm: [u8; COMM_LEN],
    pub filename: [u8; PATH_LEN],
}
