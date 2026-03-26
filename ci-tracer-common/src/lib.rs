#![no_std]

pub const EVENT_PROCESS_EXEC: u32 = 1;
pub const EVENT_PROCESS_EXIT: u32 = 2;
pub const EVENT_FILE_OPEN: u32 = 3;

pub const COMM_LEN: usize = 16;
pub const PATH_LEN: usize = 256;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessExecEvent {
    pub event_type: u32,
    pub pid: u32,
    pub tgid: u32,
    pub uid: u32,
    pub timestamp_ns: u64,
    pub comm: [u8; COMM_LEN],
    pub filename: [u8; PATH_LEN],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessExitEvent {
    pub event_type: u32,
    pub pid: u32,
    pub timestamp_ns: u64,
    pub duration_ns: u64,
    pub comm: [u8; COMM_LEN],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FileOpenEvent {
    pub event_type: u32,
    pub pid: u32,
    pub timestamp_ns: u64,
    pub flags: u32,
    pub _pad: u32,
    pub comm: [u8; COMM_LEN],
    pub filename: [u8; PATH_LEN],
}
