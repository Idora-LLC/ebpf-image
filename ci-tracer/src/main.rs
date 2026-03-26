use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::time::Duration;

use anyhow::{Context, Result};
use aya::maps::RingBuf;
use aya::programs::TracePoint;
use aya::Ebpf;
use chrono::Utc;
use serde_json::json;
use tokio::signal::unix::{signal, SignalKind};

use ci_tracer_common::*;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    bump_memlock_rlimit();

    let mut bpf = Ebpf::load(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../target/bpfel-unknown-none/release/ci-tracer-ebpf"
    )))?;

    attach_tracepoint(&mut bpf, "trace_exec", "sched", "sched_process_exec")?;
    attach_tracepoint(&mut bpf, "trace_exit", "sched", "sched_process_exit")?;
    attach_tracepoint(&mut bpf, "trace_openat", "syscalls", "sys_enter_openat")?;

    eprintln!("[ci-recorder] Tracer started, attaching eBPF probes...");

    let mut ring_buf = RingBuf::try_from(bpf.map_mut("EVENTS").unwrap())?;

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/var/log/ci-trace.jsonl")
        .context("failed to open /var/log/ci-trace.jsonl")?;
    let mut writer = BufWriter::new(file);

    let mut stats = Stats::default();
    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        while let Some(item) = ring_buf.next() {
            handle_event(&item, &mut writer, &mut stats);
        }

        tokio::select! {
            _ = sigterm.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    while let Some(item) = ring_buf.next() {
        handle_event(&item, &mut writer, &mut stats);
    }
    let _ = writer.flush();

    eprintln!(
        "[ci-recorder] --- Summary: {} processes, {} files read, {} files written ---",
        stats.process_count, stats.files_read, stats.files_written
    );

    Ok(())
}

fn attach_tracepoint(
    bpf: &mut Ebpf,
    prog_name: &str,
    category: &str,
    tp_name: &str,
) -> Result<()> {
    let prog: &mut TracePoint = bpf
        .program_mut(prog_name)
        .with_context(|| format!("program {prog_name} not found in BPF object"))?
        .try_into()?;
    prog.load()?;
    prog.attach(category, tp_name)
        .with_context(|| format!("failed to attach {category}/{tp_name}"))?;
    Ok(())
}

// ── Event handling ──────────────────────────────────────────────────────────

#[derive(Default)]
struct Stats {
    process_count: u64,
    files_read: u64,
    files_written: u64,
}

fn handle_event(data: &[u8], writer: &mut BufWriter<std::fs::File>, stats: &mut Stats) {
    if data.len() < 4 {
        return;
    }

    let event_type = u32::from_ne_bytes(data[0..4].try_into().unwrap());
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    match event_type {
        EVENT_PROCESS_EXEC => handle_exec(data, &now, writer, stats),
        EVENT_PROCESS_EXIT => handle_exit(data, &now, writer, stats),
        EVENT_FILE_OPEN => handle_file_open(data, &now, writer, stats),
        _ => {}
    }
}

fn handle_exec(data: &[u8], now: &str, writer: &mut BufWriter<std::fs::File>, stats: &mut Stats) {
    if data.len() < std::mem::size_of::<ProcessExecEvent>() {
        return;
    }
    let event: &ProcessExecEvent = unsafe { &*(data.as_ptr() as *const ProcessExecEvent) };
    let comm = bytes_to_str(&event.comm);
    let filename = bytes_to_str(&event.filename);

    stats.process_count += 1;

    eprintln!(
        "[ci-recorder] EXEC  pid={:<6} {:<16} {}",
        event.pid, comm, filename
    );

    let j = json!({
        "ts": now,
        "event": "exec",
        "pid": event.pid,
        "tgid": event.tgid,
        "uid": event.uid,
        "comm": comm,
        "filename": filename,
    });
    let _ = writeln!(writer, "{j}");
}

fn handle_exit(data: &[u8], now: &str, writer: &mut BufWriter<std::fs::File>, stats: &mut Stats) {
    if data.len() < std::mem::size_of::<ProcessExitEvent>() {
        return;
    }
    let event: &ProcessExitEvent = unsafe { &*(data.as_ptr() as *const ProcessExitEvent) };
    let comm = bytes_to_str(&event.comm);
    let duration_s = event.duration_ns as f64 / 1_000_000_000.0;

    eprintln!(
        "[ci-recorder] EXIT  pid={:<6} {:<16} duration={:.3}s",
        event.pid, comm, duration_s
    );

    let j = json!({
        "ts": now,
        "event": "exit",
        "pid": event.pid,
        "comm": comm,
        "duration_ms": event.duration_ns / 1_000_000,
    });
    let _ = writeln!(writer, "{j}");
}

fn handle_file_open(
    data: &[u8],
    now: &str,
    writer: &mut BufWriter<std::fs::File>,
    stats: &mut Stats,
) {
    if data.len() < std::mem::size_of::<FileOpenEvent>() {
        return;
    }
    let event: &FileOpenEvent = unsafe { &*(data.as_ptr() as *const FileOpenEvent) };
    let comm = bytes_to_str(&event.comm);
    let filename = bytes_to_str(&event.filename);

    if is_noise_path(filename) {
        return;
    }

    let access = classify_access(event.flags);

    match access {
        "read" => stats.files_read += 1,
        _ => stats.files_written += 1,
    }

    let label = match access {
        "read" => "READ ",
        "write" => "WRITE",
        "create" => "CREAT",
        _ => "FILE ",
    };

    eprintln!(
        "[ci-recorder] {label}  pid={:<6} {:<16} {}",
        event.pid, comm, filename
    );

    let j = json!({
        "ts": now,
        "event": "open",
        "pid": event.pid,
        "comm": comm,
        "path": filename,
        "access": access,
    });
    let _ = writeln!(writer, "{j}");
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn bytes_to_str(bytes: &[u8]) -> &str {
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..len]).unwrap_or("<invalid utf8>")
}

const O_WRONLY: u32 = 1;
const O_RDWR: u32 = 2;
const O_CREAT: u32 = 0o100;

fn classify_access(flags: u32) -> &'static str {
    if flags & O_CREAT != 0 {
        "create"
    } else if flags & (O_WRONLY | O_RDWR) != 0 {
        "write"
    } else {
        "read"
    }
}

fn is_noise_path(path: &str) -> bool {
    path.starts_with("/proc/")
        || path.starts_with("/sys/")
        || path.starts_with("/dev/")
        || path.starts_with("/etc/ld.so")
        || path.contains(".so.")
        || path.ends_with(".so")
}

fn bump_memlock_rlimit() {
    let rlim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    unsafe {
        libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim);
    }
}
