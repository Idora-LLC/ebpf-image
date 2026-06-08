//! The Idora Recorder agent: the privileged eBPF process started once per CI
//! job. It loads the kernel programs, attaches the observation hook set, drains
//! events into the userspace [`ci_tracer::observe::ProcessTree`], and at each
//! operation boundary (and at shutdown) resolves -> hashes -> assembles ->
//! submits, reconciling observed-vs-submitted at the end (fail-open).
//!
//! Lifecycle is bound to the Action's `pre`/`post` hooks (`specs/architecture.md`
//! §6): `start.js` launches this under `sudo`; `stop.js` sends SIGTERM to
//! trigger finalization.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use aya::maps::RingBuf;
use aya::programs::TracePoint;
use aya::Ebpf;
use tokio::signal::unix::{signal, SignalKind};

use ci_tracer::adapter::github::GithubAdapter;
use ci_tracer::assemble::assemble;
use ci_tracer::config::Config;
use ci_tracer::detect;
use ci_tracer::events::{decode, Event};
use ci_tracer::hash::Hasher;
use ci_tracer::observe::{Operation, ProcessTree};
use ci_tracer::reconcile::Reconciler;
use ci_tracer::resolve::ProcFs;
use ci_tracer::submit::{SubmitOutcome, Submitter};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Ignore SIGPIPE so a closed log pipe never kills the agent.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    bump_memlock_rlimit();

    let config = Config::from_env();
    let reconciler = Reconciler::new(&config.state_dir);
    reconciler.heartbeat_start();

    // No-eBPF handling (specs/deployment.md §4): no record, build not failed,
    // gap reconciled as unknown. Hard-fail is opt-in.
    if !detect::ebpf_available() {
        eprintln!(
            "[ci-recorder] {}: eBPF unavailable on this runner; no record will be produced",
            ci_tracer::diag::OBS_001
        );
        reconciler.finish();
        if config.hard_fail {
            anyhow::bail!("eBPF unavailable and hard-fail is enabled");
        }
        return Ok(());
    }

    if let Err(e) = run(config, reconciler).await {
        // Fail-open: surface the error but never propagate a non-zero exit that
        // would fail the customer's CI step.
        eprintln!("[ci-recorder] agent error (fail-open): {e:#}");
    }
    Ok(())
}

async fn run(config: Config, mut reconciler: Reconciler) -> Result<()> {
    let mut bpf = load_bpf()?;
    attach_all(&mut bpf)?;

    let adapter = GithubAdapter::from_env(
        config.type_hint,
        config.deploy_target.clone(),
        config.env_allowlist.clone(),
    );
    let submitter = match (&config.pipeline_url, &config.token) {
        (Some(url), Some(token)) => Some(Submitter::new(url, token.clone())),
        _ => {
            eprintln!("[ci-recorder] no pipeline URL/token configured; records will be buffered only");
            None
        }
    };

    // Debug mode: also dump every assembled RunRecord to a JSONL file so CI can
    // upload it as an artifact, independent of submission.
    let debug_records: Option<PathBuf> = config
        .debug
        .then(|| Path::new(&config.state_dir).join("ci-recorder-records.jsonl"));
    if let Some(p) = &debug_records {
        eprintln!("[ci-recorder] debug mode on; writing all RunRecords to {}", p.display());
    }

    let proc = ProcFs;
    let mut tree = ProcessTree::new(config.whitelist.clone());
    let mut ring_buf = RingBuf::try_from(bpf.map_mut("EVENTS").context("EVENTS map missing")?)?;

    eprintln!(
        "[ci-recorder] started; observation_mode={}, hash_tier={}, whitelist={:?}",
        ci_tracer::diag::OBSERVATION_MODE,
        ci_tracer::hash::hash_tier(),
        config.whitelist.iter().map(|e| &e.pattern).collect::<Vec<_>>()
    );
    if detect::bpf_lsm_available() {
        eprintln!("[ci-recorder] BPF-LSM present; kernel-atomic hashing tier is available (self-hosted)");
    }

    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        while let Some(item) = ring_buf.next() {
            if let Some(op) = dispatch(&item, &mut tree, &proc) {
                process_operation(op, &adapter, submitter.as_ref(), debug_records.as_deref(), &mut reconciler);
            }
        }

        tokio::select! {
            _ = sigterm.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
    }

    // Drain anything buffered in the ring after the stop signal.
    while let Some(item) = ring_buf.next() {
        if let Some(op) = dispatch(&item, &mut tree, &proc) {
            process_operation(op, &adapter, submitter.as_ref(), debug_records.as_deref(), &mut reconciler);
        }
    }

    // Finalize operations whose root never exited before job end.
    for op in tree.finalize_all() {
        process_operation(op, &adapter, submitter.as_ref(), debug_records.as_deref(), &mut reconciler);
    }

    // #region agent log
    // Inspect the kernel PARENT_MAP directly to see whether the fork tracepoint
    // is populating it (HYP=F). Drop the ring buffer first to release its &mut bpf.
    drop(ring_buf);
    match bpf.map("PARENT_MAP") {
        Some(m) => match aya::maps::HashMap::<_, u32, u32>::try_from(m) {
            Ok(pm) => {
                let mut count = 0u32;
                let mut samples: Vec<(u32, u32)> = Vec::new();
                for item in pm.iter().flatten() {
                    count += 1;
                    if samples.len() < 12 {
                        samples.push(item);
                    }
                }
                eprintln!("[dbg 5d16d6 HYP=F] PARENT_MAP child->parent entries={count} samples={samples:?}");
            }
            Err(e) => eprintln!("[dbg 5d16d6 HYP=F] PARENT_MAP try_from error: {e}"),
        },
        None => eprintln!("[dbg 5d16d6 HYP=F] PARENT_MAP not found"),
    }
    if let Some(m) = bpf.map("FORK_PROBE") {
        if let Ok(arr) = aya::maps::Array::<_, u32>::try_from(m) {
            let labels = [
                "off16", "off20", "off24", "off28", "off32", "off36", "off40", "off44",
                "cur_tgid", "cur_pid",
            ];
            for (i, label) in labels.iter().enumerate() {
                let v = arr.get(&(i as u32), 0).unwrap_or(0);
                eprintln!("[dbg 5d16d6 HYP=F probe] {label}={v}");
            }
        }
    }
    tree.debug_summary();
    // #endregion

    // Last chance to resend buffered records before the workspace disappears.
    if let Some(s) = submitter.as_ref() {
        let _ = reconciler.flush_buffered(s);
    }

    reconciler.finish();
    Ok(())
}

/// Decode one ring-buffer record and feed it to the process tree, returning a
/// finalized operation when a root exits.
fn dispatch(data: &[u8], tree: &mut ProcessTree, proc: &ProcFs) -> Option<Operation> {
    match decode(data)? {
        Event::Exec(e) => {
            tree.on_exec(&e, proc);
            None
        }
        Event::File(e) => {
            tree.on_file(&e, proc);
            None
        }
        Event::Exit(e) => tree.on_exit(&e, proc),
    }
}

fn process_operation(
    op: Operation,
    adapter: &GithubAdapter,
    submitter: Option<&Submitter>,
    debug_records: Option<&Path>,
    reconciler: &mut Reconciler,
) {
    reconciler.record_observed();

    let repo_root = op.working_directory.clone().unwrap_or_default();
    let hasher = Hasher::new(repo_root);
    let env_vars: Vec<(String, String)> = std::env::vars().collect();

    let body = match assemble(&op, adapter, &hasher, env_vars) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[ci-recorder] assembly failed for op pid={}: {e:#}", op.root_pid);
            return;
        }
    };

    eprintln!(
        "[ci-recorder] operation: type={} command={:?} inputs={} outputs={} exit={:?}",
        body.run_record.run_type.as_str(),
        body.run_record.command,
        body.run_record.inputs.as_ref().map(|v| v.len()).unwrap_or(0),
        body.run_record.outputs.as_ref().map(|v| v.len()).unwrap_or(0),
        body.run_record.exit_code,
    );
    // #region agent log
    eprintln!(
        "[dbg 5d16d6 HYP=AE op] root_pid={} wd={:?} recorded={} relative={} scoped_in={} scoped_out={}",
        op.root_pid,
        op.working_directory,
        op.dbg_recorded,
        op.dbg_relative,
        op.scoped_inputs().len(),
        op.scoped_outputs().len(),
    );
    for s in &op.dbg_samples {
        eprintln!("[dbg 5d16d6 HYP=AE op_sample] {s}");
    }
    // #endregion

    if let Some(path) = debug_records {
        if let Err(e) = append_debug_record(path, &body) {
            eprintln!("[ci-recorder] failed to write debug record: {e}");
        }
    }

    match submitter {
        Some(s) => match s.submit(&body) {
            SubmitOutcome::Success { receipt_id } => {
                eprintln!("[ci-recorder] submitted -> {receipt_id}");
                reconciler.record_submitted();
            }
            // Fail-open: buffer for retry; reconciliation surfaces the gap.
            _ => reconciler.buffer(&body),
        },
        None => reconciler.buffer(&body),
    }
}

/// Append one assembled submission to the debug JSONL file.
fn append_debug_record(path: &Path, body: &ci_tracer::runrecord::SubmissionBody) -> Result<()> {
    let mut line = serde_json::to_string(body)?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

fn load_bpf() -> Result<Ebpf> {
    #[repr(C, align(8))]
    struct Aligned<T: ?Sized>(T);
    static BPF_OBJ: &Aligned<[u8]> = &Aligned(*include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../target/bpfel-unknown-none/release/ci-tracer-ebpf"
    )));
    Ebpf::load(&BPF_OBJ.0).context("loading BPF object")
}

fn attach_all(bpf: &mut Ebpf) -> Result<()> {
    // Core set: required. Failure here is a genuine error.
    attach(bpf, "trace_fork", "sched", "sched_process_fork", true)?;
    attach(bpf, "trace_exec", "sched", "sched_process_exec", true)?;
    attach(bpf, "trace_exit", "sched", "sched_process_exit", true)?;
    attach(bpf, "trace_exit_group", "syscalls", "sys_enter_exit_group", true)?;
    attach(bpf, "trace_openat", "syscalls", "sys_enter_openat", true)?;

    // Broadened set: best-effort, so older kernels still run with core coverage.
    attach(bpf, "trace_openat2", "syscalls", "sys_enter_openat2", false)?;
    attach(bpf, "trace_renameat2", "syscalls", "sys_enter_renameat2", false)?;
    attach(bpf, "trace_unlinkat", "syscalls", "sys_enter_unlinkat", false)?;
    attach(bpf, "trace_truncate", "syscalls", "sys_enter_truncate", false)?;
    Ok(())
}

fn attach(bpf: &mut Ebpf, prog: &str, category: &str, name: &str, required: bool) -> Result<()> {
    let result = (|| -> Result<()> {
        let p: &mut TracePoint = bpf
            .program_mut(prog)
            .with_context(|| format!("program {prog} not found"))?
            .try_into()?;
        p.load()?;
        p.attach(category, name)
            .with_context(|| format!("attach {category}/{name}"))?;
        Ok(())
    })();

    match result {
        Ok(()) => Ok(()),
        Err(e) if !required => {
            eprintln!("[ci-recorder] optional hook {category}/{name} unavailable: {e}");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn bump_memlock_rlimit() {
    let rlim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // SAFETY: setrlimit with a valid resource and rlimit pointer.
    unsafe {
        libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim);
    }
}
