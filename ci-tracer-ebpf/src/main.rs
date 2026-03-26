#![no_std]
#![no_main]

use aya_ebpf::{
    helpers::{
        bpf_get_current_cgroup_id, bpf_get_current_comm, bpf_get_current_pid_tgid,
        bpf_get_current_uid_gid, bpf_ktime_get_ns, bpf_probe_read_kernel_str_bytes,
        bpf_probe_read_user_str_bytes,
    },
    macros::{map, tracepoint},
    maps::{Array, LruHashMap, PerCpuArray, RingBuf},
    programs::TracePointContext,
    EbpfContext,
};

use ci_tracer_common::*;

#[map]
static EVENTS: RingBuf = RingBuf::with_byte_size(256 * 1024, 0);

/// Set by userspace to the container's cgroup v2 ID.
/// Only events from this cgroup (and descendants) are emitted.
/// A value of 0 means "not configured yet" and all events are dropped.
#[map]
static TARGET_CGROUPID: Array<u64> = Array::with_max_entries(1, 0);

/// Tracks process start times for duration calculation on exit.
#[map]
static START_TIMES: LruHashMap<u32, u64> = LruHashMap::with_max_entries(10240, 0);

/// Per-CPU scratch buffer for exec events (avoids 512-byte BPF stack limit).
#[map]
static EXEC_BUF: PerCpuArray<ProcessExecEvent> = PerCpuArray::with_max_entries(1, 0);

/// Per-CPU scratch buffer for file-open events.
#[map]
static FILE_BUF: PerCpuArray<FileOpenEvent> = PerCpuArray::with_max_entries(1, 0);

/// Returns true if the current task belongs to the target container cgroup.
#[inline(always)]
unsafe fn in_target_cgroup() -> bool {
    if let Some(target) = TARGET_CGROUPID.get(0) {
        let target_id = *target;
        if target_id == 0 {
            return false;
        }
        bpf_get_current_cgroup_id() == target_id
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// sched_process_exec – fires when a process calls execve()
// ---------------------------------------------------------------------------
// Tracepoint format (x86_64):
//   offset  8: __data_loc char[] filename  (u32)
//   offset 12: pid_t pid                   (i32)
//   offset 16: pid_t old_pid               (i32)

#[tracepoint]
pub fn trace_exec(ctx: TracePointContext) -> u32 {
    match unsafe { try_trace_exec(&ctx) } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

unsafe fn try_trace_exec(ctx: &TracePointContext) -> Result<(), i64> {
    if !in_target_cgroup() {
        return Ok(());
    }

    let ts = bpf_ktime_get_ns();
    let pid_tgid = bpf_get_current_pid_tgid();
    let pid = pid_tgid as u32;
    let tgid = (pid_tgid >> 32) as u32;

    if pid == 0 {
        return Ok(());
    }

    let uid = bpf_get_current_uid_gid() as u32;

    let _ = START_TIMES.insert(&pid, &ts, 0);

    let buf = &mut *EXEC_BUF.get_ptr_mut(0).ok_or(0i64)?;
    buf.event_type = EVENT_PROCESS_EXEC;
    buf.pid = pid;
    buf.tgid = tgid;
    buf.uid = uid;
    buf.timestamp_ns = ts;
    buf.comm = bpf_get_current_comm().map_err(|e| e)?;

    buf.filename[0] = 0;
    let data_loc: u32 = ctx.read_at(8)?;
    let offset = (data_loc & 0xFFFF) as usize;
    let _ = bpf_probe_read_kernel_str_bytes(
        ctx.as_ptr().add(offset) as *const u8,
        &mut buf.filename,
    );

    EVENTS.output(buf, 0).map_err(|e| e)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// sched_process_exit – fires when a process terminates
// ---------------------------------------------------------------------------
// Tracepoint format (sched_process_template, x86_64):
//   offset  8: char comm[16]
//   offset 24: pid_t pid      (i32)
//   offset 28: int   prio     (i32)

#[tracepoint]
pub fn trace_exit(ctx: TracePointContext) -> u32 {
    match unsafe { try_trace_exit(&ctx) } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

unsafe fn try_trace_exit(ctx: &TracePointContext) -> Result<(), i64> {
    if !in_target_cgroup() {
        return Ok(());
    }

    let ts = bpf_ktime_get_ns();
    let pid_tgid = bpf_get_current_pid_tgid();
    let pid = pid_tgid as u32;

    if pid == 0 {
        return Ok(());
    }

    let duration_ns = match START_TIMES.get(&pid) {
        Some(start) => {
            let d = ts.saturating_sub(*start);
            let _ = START_TIMES.remove(&pid);
            d
        }
        None => 0,
    };

    let mut event = ProcessExitEvent {
        event_type: EVENT_PROCESS_EXIT,
        pid,
        timestamp_ns: ts,
        duration_ns,
        comm: [0u8; COMM_LEN],
    };

    let comm: [u8; 16] = ctx.read_at(8).unwrap_or([0u8; 16]);
    event.comm = comm;

    EVENTS.output(&event, 0).map_err(|e| e)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// sys_enter_openat – fires on every openat() syscall
// ---------------------------------------------------------------------------
// Tracepoint format (x86_64, args stored as unsigned long):
//   offset  8: long __syscall_nr
//   offset 16: unsigned long dfd        (args[0])
//   offset 24: unsigned long filename   (args[1], userspace pointer)
//   offset 32: unsigned long flags      (args[2])
//   offset 40: unsigned long mode       (args[3])

#[tracepoint]
pub fn trace_openat(ctx: TracePointContext) -> u32 {
    match unsafe { try_trace_openat(&ctx) } {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

unsafe fn try_trace_openat(ctx: &TracePointContext) -> Result<(), i64> {
    if !in_target_cgroup() {
        return Ok(());
    }

    let ts = bpf_ktime_get_ns();
    let pid_tgid = bpf_get_current_pid_tgid();
    let pid = pid_tgid as u32;

    if pid == 0 {
        return Ok(());
    }

    let filename_ptr: u64 = ctx.read_at(24)?;
    let flags: u64 = ctx.read_at(32)?;

    let buf = &mut *FILE_BUF.get_ptr_mut(0).ok_or(0i64)?;
    buf.event_type = EVENT_FILE_OPEN;
    buf.pid = pid;
    buf.timestamp_ns = ts;
    buf.flags = flags as u32;
    buf._pad = 0;
    buf.comm = bpf_get_current_comm().map_err(|e| e)?;

    buf.filename[0] = 0;
    let _ = bpf_probe_read_user_str_bytes(filename_ptr as *const u8, &mut buf.filename);

    EVENTS.output(buf, 0).map_err(|e| e)?;
    Ok(())
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
