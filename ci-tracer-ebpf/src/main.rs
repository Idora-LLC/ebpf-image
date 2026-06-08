#![no_std]
#![no_main]

//! Kernel-side BPF programs for the Idora Recorder.
//!
//! Implements the broadened hook set from `specs/observation.md` §3: process
//! lifecycle (`fork`/`exec`/`exit` with real exit code) plus a file-access set
//! covering `openat`, `openat2`, `renameat2`, `unlinkat`, and `truncate`. The
//! kernel side stays deliberately thin: it filters nothing by policy (that is
//! userspace scoping, `specs/observation.md` §8) and emits typed events on a
//! single ring buffer.

use aya_ebpf::{
    helpers::{
        bpf_get_current_comm, bpf_get_current_pid_tgid, bpf_probe_read_user,
        bpf_probe_read_user_str_bytes,
    },
    macros::{map, tracepoint},
    maps::{Array, LruHashMap, PerCpuArray, RingBuf},
    programs::TracePointContext,
};

use ci_tracer_common::*;

#[map]
static EVENTS: RingBuf = RingBuf::with_byte_size(4 * 1024 * 1024, 0);

/// child PID -> parent PID, populated by the fork handler.
#[map]
static PARENT_MAP: LruHashMap<u32, u32> = LruHashMap::with_max_entries(10240, 0);

/// tgid -> exit status, populated by `sys_enter_exit_group`, read on exit.
#[map]
static EXIT_CODES: LruHashMap<u32, i32> = LruHashMap::with_max_entries(10240, 0);

/// Per-CPU scratch for file-access events.
#[map]
static FILE_BUF: PerCpuArray<FileAccessEvent> = PerCpuArray::with_max_entries(1, 0);

// #region agent log
/// Debug (session 5d16d6): captures candidate fork-tracepoint field values from
/// the first observed fork so userspace can identify the correct offsets.
/// Indices 0-7 = u32 read at offsets 16,20,24,28,32,36,40,44; 8 = current tgid;
/// 9 = current pid; 15 = "captured" flag.
#[map]
static FORK_PROBE: Array<u32> = Array::with_max_entries(16, 0);

#[inline(always)]
unsafe fn store_probe(i: u32, v: u32) {
    if let Some(p) = FORK_PROBE.get_ptr_mut(i) {
        *p = v;
    }
}
// #endregion

// Open flags (Linux, x86_64).
const O_WRONLY: u64 = 1;
const O_RDWR: u64 = 2;
const O_CREAT: u64 = 0o100;

#[inline(always)]
fn classify_open(flags: u64) -> u32 {
    if flags & O_CREAT != 0 {
        ACCESS_CREATE
    } else if flags & (O_WRONLY | O_RDWR) != 0 {
        ACCESS_WRITE
    } else {
        ACCESS_READ
    }
}

// ---------------------------------------------------------------------------
// sched_process_fork - parent->child PID mapping
//   offset 24: pid_t parent_pid (i32)
//   offset 44: pid_t child_pid  (i32)
// ---------------------------------------------------------------------------

#[tracepoint]
pub fn trace_fork(ctx: TracePointContext) -> u32 {
    match unsafe { try_trace_fork(&ctx) } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

unsafe fn try_trace_fork(ctx: &TracePointContext) -> Result<(), i64> {
    // #region agent log
    if let Some(done) = FORK_PROBE.get_ptr_mut(15) {
        if *done == 0 {
            *done = 1;
            store_probe(0, ctx.read_at::<u32>(4).unwrap_or(0));
            store_probe(1, ctx.read_at::<u32>(8).unwrap_or(0));
            store_probe(2, ctx.read_at::<u32>(12).unwrap_or(0));
            store_probe(3, ctx.read_at::<u32>(16).unwrap_or(0));
            store_probe(4, ctx.read_at::<u32>(20).unwrap_or(0));
            store_probe(5, ctx.read_at::<u32>(44).unwrap_or(0));
            store_probe(6, ctx.read_at::<u32>(48).unwrap_or(0));
            store_probe(7, ctx.read_at::<u32>(52).unwrap_or(0));
            let pt = bpf_get_current_pid_tgid();
            store_probe(8, (pt >> 32) as u32);
            store_probe(9, pt as u32);
        }
    }
    // #endregion
    let parent_pid: u32 = ctx.read_at::<i32>(24).map(|v| v as u32)?;
    let child_pid: u32 = ctx.read_at::<i32>(44).map(|v| v as u32)?;
    let _ = PARENT_MAP.insert(&child_pid, &parent_pid, 0);
    Ok(())
}

// ---------------------------------------------------------------------------
// sched_process_exec - execve()
// We only need pid/ppid/comm; the exec filename is unused (the command is
// resolved from /proc/<pid>/cmdline in userspace).
// ---------------------------------------------------------------------------

#[tracepoint]
pub fn trace_exec(ctx: TracePointContext) -> u32 {
    match unsafe { try_trace_exec(&ctx) } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

unsafe fn try_trace_exec(_ctx: &TracePointContext) -> Result<(), i64> {
    let pid = bpf_get_current_pid_tgid() as u32;
    if pid == 0 {
        return Ok(());
    }

    let event = ProcessExecEvent {
        event_type: EVENT_PROCESS_EXEC,
        pid,
        ppid: PARENT_MAP.get(&pid).copied().unwrap_or(0),
        comm: bpf_get_current_comm()?,
    };

    EVENTS.output(&event, 0)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// sys_enter_exit_group - capture the process exit status
//   offset 16: unsigned long error_code (args[0])
// ---------------------------------------------------------------------------

#[tracepoint]
pub fn trace_exit_group(ctx: TracePointContext) -> u32 {
    match unsafe { try_trace_exit_group(&ctx) } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

unsafe fn try_trace_exit_group(ctx: &TracePointContext) -> Result<(), i64> {
    let tgid = (bpf_get_current_pid_tgid() >> 32) as u32;
    if tgid == 0 {
        return Ok(());
    }
    let raw: u64 = ctx.read_at(16)?;
    // glibc passes the process exit status; the low byte is the exit code.
    let code = (raw & 0xff) as i32;
    let _ = EXIT_CODES.insert(&tgid, &code, 0);
    Ok(())
}

// ---------------------------------------------------------------------------
// sched_process_exit - process termination
//   offset  8: char comm[16]
//   offset 24: pid_t pid (i32)
// ---------------------------------------------------------------------------

#[tracepoint]
pub fn trace_exit(ctx: TracePointContext) -> u32 {
    match unsafe { try_trace_exit(&ctx) } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

unsafe fn try_trace_exit(ctx: &TracePointContext) -> Result<(), i64> {
    let pid_tgid = bpf_get_current_pid_tgid();
    let pid = pid_tgid as u32;
    let tgid = (pid_tgid >> 32) as u32;

    if pid == 0 {
        return Ok(());
    }

    let (exit_code, code_observed) = match EXIT_CODES.get(&tgid) {
        Some(code) => {
            let c = *code;
            let _ = EXIT_CODES.remove(&tgid);
            (c, 1)
        }
        None => (0, 0),
    };

    let event = ProcessExitEvent {
        event_type: EVENT_PROCESS_EXIT,
        pid,
        exit_code,
        code_observed,
        comm: ctx.read_at(8).unwrap_or([0u8; COMM_LEN]),
    };

    EVENTS.output(&event, 0)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// File-access helper
// ---------------------------------------------------------------------------

#[inline(always)]
unsafe fn emit_file_access(pid: u32, access: u32, user_path_ptr: u64) -> Result<(), i64> {
    let buf = &mut *FILE_BUF.get_ptr_mut(0).ok_or(0i64)?;
    buf.event_type = EVENT_FILE_ACCESS;
    buf.pid = pid;
    buf.access = access;
    buf.comm = bpf_get_current_comm()?;
    buf.filename[0] = 0;
    let _ = bpf_probe_read_user_str_bytes(user_path_ptr as *const u8, &mut buf.filename);
    EVENTS.output(buf, 0)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// sys_enter_openat
//   offset 24: filename (args[1], user ptr)
//   offset 32: flags    (args[2])
// ---------------------------------------------------------------------------

#[tracepoint]
pub fn trace_openat(ctx: TracePointContext) -> u32 {
    match unsafe { try_trace_openat(&ctx) } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

unsafe fn try_trace_openat(ctx: &TracePointContext) -> Result<(), i64> {
    let pid = bpf_get_current_pid_tgid() as u32;
    if pid == 0 {
        return Ok(());
    }
    let filename_ptr: u64 = ctx.read_at(24)?;
    let flags: u64 = ctx.read_at(32)?;
    emit_file_access(pid, classify_open(flags), filename_ptr)
}

// ---------------------------------------------------------------------------
// sys_enter_openat2
//   offset 24: filename       (args[1], user ptr)
//   offset 32: struct open_how *how (args[2], user ptr); how->flags is u64 @ 0
// ---------------------------------------------------------------------------

#[tracepoint]
pub fn trace_openat2(ctx: TracePointContext) -> u32 {
    match unsafe { try_trace_openat2(&ctx) } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

unsafe fn try_trace_openat2(ctx: &TracePointContext) -> Result<(), i64> {
    let pid = bpf_get_current_pid_tgid() as u32;
    if pid == 0 {
        return Ok(());
    }
    let filename_ptr: u64 = ctx.read_at(24)?;
    let how_ptr: u64 = ctx.read_at(32)?;
    let flags: u64 = bpf_probe_read_user(how_ptr as *const u64).unwrap_or(0);
    emit_file_access(pid, classify_open(flags), filename_ptr)
}

// ---------------------------------------------------------------------------
// sys_enter_renameat2 - output under a new path, old path removed
//   offset 24: oldname (args[1], user ptr)
//   offset 40: newname (args[3], user ptr)
// ---------------------------------------------------------------------------

#[tracepoint]
pub fn trace_renameat2(ctx: TracePointContext) -> u32 {
    match unsafe { try_trace_renameat2(&ctx) } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

unsafe fn try_trace_renameat2(ctx: &TracePointContext) -> Result<(), i64> {
    let pid = bpf_get_current_pid_tgid() as u32;
    if pid == 0 {
        return Ok(());
    }
    let oldname_ptr: u64 = ctx.read_at(24)?;
    let newname_ptr: u64 = ctx.read_at(40)?;
    let _ = emit_file_access(pid, ACCESS_DELETE, oldname_ptr);
    emit_file_access(pid, ACCESS_CREATE, newname_ptr)
}

// ---------------------------------------------------------------------------
// sys_enter_unlinkat - deletion
//   offset 24: pathname (args[1], user ptr)
// ---------------------------------------------------------------------------

#[tracepoint]
pub fn trace_unlinkat(ctx: TracePointContext) -> u32 {
    match unsafe { try_trace_unlinkat(&ctx) } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

unsafe fn try_trace_unlinkat(ctx: &TracePointContext) -> Result<(), i64> {
    let pid = bpf_get_current_pid_tgid() as u32;
    if pid == 0 {
        return Ok(());
    }
    let pathname_ptr: u64 = ctx.read_at(24)?;
    emit_file_access(pid, ACCESS_DELETE, pathname_ptr)
}

// ---------------------------------------------------------------------------
// sys_enter_truncate - in-place output truncation
//   offset 16: path (args[0], user ptr)
//
// `ftruncate` operates on a file descriptor with no path argument; resolving
// fd -> path in-kernel is deferred (see specs/observation.md §3).
// ---------------------------------------------------------------------------

#[tracepoint]
pub fn trace_truncate(ctx: TracePointContext) -> u32 {
    match unsafe { try_trace_truncate(&ctx) } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

unsafe fn try_trace_truncate(ctx: &TracePointContext) -> Result<(), i64> {
    let pid = bpf_get_current_pid_tgid() as u32;
    if pid == 0 {
        return Ok(());
    }
    let path_ptr: u64 = ctx.read_at(16)?;
    emit_file_access(pid, ACCESS_TRUNCATE, path_ptr)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
