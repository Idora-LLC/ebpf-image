use std::collections::HashMap;
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

/// Commands we want to trace. When a process whose cmdline matches one of
/// these patterns is exec'd, it becomes a "root" and all its descendants
/// (children, grandchildren, ...) are tracked.
const WHITELIST: &[&str] = &["npm run build", "npm run test"];

/// Info about a tracked process: which whitelisted root it belongs to.
#[derive(Clone)]
struct TrackedRoot {
    root_pid: u32,
    root_cmd: String,
}

/// Process tree tracker. Maps PID → root info for all tracked processes.
struct ProcessTree {
    tracked: HashMap<u32, TrackedRoot>,
}

impl ProcessTree {
    fn new() -> Self {
        Self {
            tracked: HashMap::new(),
        }
    }

    /// Check if this newly exec'd process should be tracked.
    /// Returns the root info if yes.
    fn check_exec(&mut self, pid: u32, comm: &str) -> Option<TrackedRoot> {
        // Read the full cmdline from /proc to match against whitelist.
        let cmdline = read_cmdline(pid).unwrap_or_default();

        // Check if this process itself matches the whitelist.
        for pattern in WHITELIST {
            if cmdline.contains(pattern) || comm_matches(comm, pattern) {
                let root = TrackedRoot {
                    root_pid: pid,
                    root_cmd: cmdline.clone(),
                };
                self.tracked.insert(pid, root.clone());
                return Some(root);
            }
        }

        // Check if the parent is tracked (inherit the root).
        let ppid = read_ppid(pid).unwrap_or(0);
        if let Some(root) = self.tracked.get(&ppid).cloned() {
            self.tracked.insert(pid, root.clone());
            return Some(root);
        }

        None
    }

    /// Check if a PID is tracked (for file/exit events).
    fn get(&self, pid: &u32) -> Option<&TrackedRoot> {
        self.tracked.get(pid)
    }

    /// Remove a PID on exit, return its root info.
    fn remove(&mut self, pid: &u32) -> Option<TrackedRoot> {
        self.tracked.remove(pid)
    }
}

/// Read /proc/<pid>/cmdline and return as a space-separated string.
fn read_cmdline(pid: u32) -> Option<String> {
    let data = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let s: String = data
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    if s.is_empty() { None } else { Some(s) }
}

/// Read ppid from /proc/<pid>/stat.
fn read_ppid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Format: "pid (comm) state ppid ..."
    // Find the closing ')' then parse the 4th field.
    let after_comm = stat.rfind(')')? + 2;
    let fields: Vec<&str> = stat[after_comm..].split_whitespace().collect();
    // fields[0] = state, fields[1] = ppid
    fields.get(1)?.parse().ok()
}

/// Check if the 16-byte comm field (e.g. "npm run build") matches a pattern.
fn comm_matches(comm: &str, pattern: &str) -> bool {
    // comm is truncated to 16 chars by the kernel
    if pattern.len() <= COMM_LEN {
        comm == pattern
    } else {
        comm == &pattern[..COMM_LEN]
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN); }
    bump_memlock_rlimit();

    #[repr(C, align(8))]
    struct Aligned<T: ?Sized>(T);
    static BPF_OBJ: &Aligned<[u8]> = &Aligned(*include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../target/bpfel-unknown-none/release/ci-tracer-ebpf"
    )));

    let mut bpf = Ebpf::load(&BPF_OBJ.0)?;

    attach_tracepoint(&mut bpf, "trace_exec", "sched", "sched_process_exec")?;
    attach_tracepoint(&mut bpf, "trace_exit", "sched", "sched_process_exit")?;
    attach_tracepoint(&mut bpf, "trace_openat", "syscalls", "sys_enter_openat")?;

    eprintln!("[ci-recorder] Tracer started, watching for: {:?}", WHITELIST);

    let mut ring_buf = RingBuf::try_from(bpf.map_mut("EVENTS").unwrap())?;

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/var/log/ci-trace.jsonl")
        .context("failed to open /var/log/ci-trace.jsonl")?;
    let mut writer = BufWriter::new(file);

    let mut tree = ProcessTree::new();
    let mut stats = Stats::default();
    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        while let Some(item) = ring_buf.next() {
            handle_event(&item, &mut writer, &mut tree, &mut stats);
        }

        tokio::select! {
            _ = sigterm.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
    }

    while let Some(item) = ring_buf.next() {
        handle_event(&item, &mut writer, &mut tree, &mut stats);
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

fn handle_event(
    data: &[u8],
    writer: &mut BufWriter<std::fs::File>,
    tree: &mut ProcessTree,
    stats: &mut Stats,
) {
    if data.len() < 4 {
        return;
    }

    let event_type = u32::from_ne_bytes(data[0..4].try_into().unwrap());

    match event_type {
        EVENT_PROCESS_EXEC => handle_exec(data, writer, tree, stats),
        EVENT_PROCESS_EXIT => handle_exit(data, writer, tree, stats),
        EVENT_FILE_OPEN => handle_file_open(data, writer, tree, stats),
        _ => {}
    }
}

fn handle_exec(
    data: &[u8],
    writer: &mut BufWriter<std::fs::File>,
    tree: &mut ProcessTree,
    stats: &mut Stats,
) {
    if data.len() < std::mem::size_of::<ProcessExecEvent>() {
        return;
    }
    let event: &ProcessExecEvent = unsafe { &*(data.as_ptr() as *const ProcessExecEvent) };
    let comm = bytes_to_str(&event.comm);
    let filename = bytes_to_str(&event.filename);

    let Some(root) = tree.check_exec(event.pid, comm) else {
        return;
    };

    stats.process_count += 1;

    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let j = json!({
        "ts": now,
        "event": "exec",
        "pid": event.pid,
        "comm": comm,
        "filename": filename,
        "root_pid": root.root_pid,
        "root_cmd": root.root_cmd,
    });
    let _ = writeln!(writer, "{j}");

    eprintln!(
        "[ci-recorder] EXEC  pid={:<6} {:<16} {} (root: {})",
        event.pid, comm, filename, root.root_cmd
    );
}

fn handle_exit(
    data: &[u8],
    writer: &mut BufWriter<std::fs::File>,
    tree: &mut ProcessTree,
    stats: &mut Stats,
) {
    if data.len() < std::mem::size_of::<ProcessExitEvent>() {
        return;
    }
    let event: &ProcessExitEvent = unsafe { &*(data.as_ptr() as *const ProcessExitEvent) };

    let Some(root) = tree.remove(&event.pid) else {
        return;
    };

    let comm = bytes_to_str(&event.comm);
    let duration_s = event.duration_ns as f64 / 1_000_000_000.0;

    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let j = json!({
        "ts": now,
        "event": "exit",
        "pid": event.pid,
        "comm": comm,
        "duration_ms": event.duration_ns / 1_000_000,
        "root_pid": root.root_pid,
        "root_cmd": root.root_cmd,
    });
    let _ = writeln!(writer, "{j}");

    eprintln!(
        "[ci-recorder] EXIT  pid={:<6} {:<16} duration={:.3}s (root: {})",
        event.pid, comm, duration_s, root.root_cmd
    );
}

fn handle_file_open(
    data: &[u8],
    writer: &mut BufWriter<std::fs::File>,
    tree: &mut ProcessTree,
    stats: &mut Stats,
) {
    if data.len() < std::mem::size_of::<FileOpenEvent>() {
        return;
    }
    let event: &FileOpenEvent = unsafe { &*(data.as_ptr() as *const FileOpenEvent) };

    let Some(root) = tree.get(&event.pid) else {
        return;
    };

    let comm = bytes_to_str(&event.comm);
    let filename = bytes_to_str(&event.filename);

    if is_skip_path(filename) {
        return;
    }

    let access = classify_access(event.flags);

    match access {
        "read" => stats.files_read += 1,
        _ => stats.files_written += 1,
    }

    let root = root.clone();

    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let j = json!({
        "ts": now,
        "event": "open",
        "pid": event.pid,
        "comm": comm,
        "path": filename,
        "access": access,
        "root_pid": root.root_pid,
        "root_cmd": root.root_cmd,
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

/// Drop entirely -- not relevant for CI tracing.
fn is_skip_path(path: &str) -> bool {
    !path.starts_with('/')
        || path.starts_with("/proc/")
        || path.starts_with("/sys/")
        || path.starts_with("/dev/")
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
