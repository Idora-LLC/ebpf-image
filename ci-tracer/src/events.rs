//! Decoding of raw ring-buffer bytes into typed, owned events.
//!
//! The kernel writes the `#[repr(C)]` structs from `ci-tracer-common`; this
//! module reads the leading `event_type` discriminator and reinterprets the
//! bytes. Owned `String`s are produced here so the rest of the agent never
//! touches raw buffers.

use ci_tracer_common::*;

/// A decoded kernel event.
#[derive(Clone, Debug)]
pub enum Event {
    Exec(ExecEvent),
    Exit(ExitEvent),
    File(FileEvent),
}

#[derive(Clone, Debug)]
pub struct ExecEvent {
    pub pid: u32,
    pub ppid: u32,
    pub comm: String,
}

#[derive(Clone, Debug)]
pub struct ExitEvent {
    pub pid: u32,
    pub exit_code: Option<i32>,
    pub comm: String,
}

/// How a path was accessed, derived from the kernel `ACCESS_*` tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Create,
    Delete,
    Truncate,
}

impl Access {
    fn from_raw(raw: u32) -> Option<Access> {
        match raw {
            ACCESS_READ => Some(Access::Read),
            ACCESS_WRITE => Some(Access::Write),
            ACCESS_CREATE => Some(Access::Create),
            ACCESS_DELETE => Some(Access::Delete),
            ACCESS_TRUNCATE => Some(Access::Truncate),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FileEvent {
    pub pid: u32,
    pub access: Access,
    pub comm: String,
    pub path: String,
}

/// Decode one ring-buffer record. Returns `None` for short/unknown records.
pub fn decode(data: &[u8]) -> Option<Event> {
    if data.len() < 4 {
        return None;
    }
    let event_type = u32::from_ne_bytes(data[0..4].try_into().ok()?);
    match event_type {
        EVENT_PROCESS_EXEC => decode_exec(data),
        EVENT_PROCESS_EXIT => decode_exit(data),
        EVENT_FILE_ACCESS => decode_file(data),
        _ => None,
    }
}

fn decode_exec(data: &[u8]) -> Option<Event> {
    if data.len() < core::mem::size_of::<ProcessExecEvent>() {
        return None;
    }
    // SAFETY: length checked; the kernel wrote a `#[repr(C)]` ProcessExecEvent.
    // `read_unaligned` because the ring-buffer slice is not guaranteed aligned.
    let e = unsafe { core::ptr::read_unaligned(data.as_ptr() as *const ProcessExecEvent) };
    Some(Event::Exec(ExecEvent {
        pid: e.pid,
        ppid: e.ppid,
        comm: cstr(&e.comm),
    }))
}

fn decode_exit(data: &[u8]) -> Option<Event> {
    if data.len() < core::mem::size_of::<ProcessExitEvent>() {
        return None;
    }
    // SAFETY: length checked; the kernel wrote a `#[repr(C)]` ProcessExitEvent.
    let e = unsafe { core::ptr::read_unaligned(data.as_ptr() as *const ProcessExitEvent) };
    Some(Event::Exit(ExitEvent {
        pid: e.pid,
        exit_code: if e.code_observed != 0 {
            Some(e.exit_code)
        } else {
            None
        },
        comm: cstr(&e.comm),
    }))
}

fn decode_file(data: &[u8]) -> Option<Event> {
    if data.len() < core::mem::size_of::<FileAccessEvent>() {
        return None;
    }
    // SAFETY: length checked; the kernel wrote a `#[repr(C)]` FileAccessEvent.
    let e = unsafe { core::ptr::read_unaligned(data.as_ptr() as *const FileAccessEvent) };
    Some(Event::File(FileEvent {
        pid: e.pid,
        access: Access::from_raw(e.access)?,
        comm: cstr(&e.comm),
        path: cstr(&e.filename),
    }))
}

/// Convert a NUL-terminated kernel byte field into an owned `String`.
fn cstr(bytes: &[u8]) -> String {
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..len]).into_owned()
}
