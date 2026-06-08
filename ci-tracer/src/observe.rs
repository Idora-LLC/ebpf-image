//! Observation: process-tree reconstruction and per-operation accumulation
//! (`specs/observation.md` §4-5).
//!
//! An **operation** is the process subtree rooted at one whitelisted command
//! invocation; it produces exactly one execution RunRecord. This module is pure
//! (no kernel or `/proc` access of its own) so it is unit-testable: it consumes
//! decoded [`crate::events::Event`]s and, when an operation's root is first
//! identified, captures its command line and working directory through the
//! injected [`ProcSource`].

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use ci_tracer_common::COMM_LEN;

use crate::events::{Access, ExecEvent, ExitEvent, FileEvent};
use crate::runrecord::RunRecordType;

/// Reads process metadata that only the live process exposes. Implemented over
/// `/proc` at runtime ([`crate::resolve::ProcFs`]); stubbed in tests.
pub trait ProcSource {
    /// Full argv joined with spaces, from `/proc/<pid>/cmdline`.
    fn cmdline(&self, pid: u32) -> Option<String>;
    /// Absolute working directory, from `/proc/<pid>/cwd`.
    fn cwd(&self, pid: u32) -> Option<String>;
}

/// One whitelist entry: a command prefix to match and an optional `type` tag
/// (`specs/observation.md` §5 precedence step 2).
#[derive(Clone, Debug)]
pub struct WhitelistEntry {
    pub pattern: String,
    pub type_tag: Option<RunRecordType>,
}

impl WhitelistEntry {
    pub fn new(pattern: impl Into<String>, type_tag: Option<RunRecordType>) -> Self {
        Self {
            pattern: pattern.into(),
            type_tag,
        }
    }
}

/// Per-path accumulated access state within an operation.
#[derive(Clone, Copy, Debug, Default)]
struct PathState {
    read: bool,
    wrote: bool,
    deleted: bool,
}

/// A finalized (or in-progress) operation: everything observed for one
/// whitelisted root and its descendants.
#[derive(Clone, Debug)]
pub struct Operation {
    pub root_pid: u32,
    pub root_comm: String,
    /// `type` from the matched whitelist entry; `None` falls back to adapter
    /// hint / heuristic at assembly.
    pub whitelist_type: Option<RunRecordType>,
    pub command: Option<String>,
    pub working_directory: Option<String>,
    pub exit_code: Option<i32>,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pids: std::collections::HashSet<u32>,
    paths: HashMap<String, PathState>,
    // #region agent log
    pub dbg_recorded: u64,
    pub dbg_relative: u64,
    pub dbg_samples: Vec<String>,
    // #endregion
}

impl Operation {
    fn new(root_pid: u32, root_comm: String, whitelist_type: Option<RunRecordType>) -> Self {
        let mut pids = std::collections::HashSet::new();
        pids.insert(root_pid);
        Self {
            root_pid,
            root_comm,
            whitelist_type,
            command: None,
            working_directory: None,
            exit_code: None,
            start_time: Utc::now(),
            end_time: None,
            pids,
            paths: HashMap::new(),
            // #region agent log
            dbg_recorded: 0,
            dbg_relative: 0,
            dbg_samples: Vec::new(),
            // #endregion
        }
    }

    fn record(&mut self, path: &str, access: Access) {
        // #region agent log
        self.dbg_recorded += 1;
        if !path.starts_with('/') {
            self.dbg_relative += 1;
        }
        if self.dbg_samples.len() < 12 {
            self.dbg_samples.push(format!("{access:?}:{path}"));
        }
        // #endregion
        let st = self.paths.entry(path.to_string()).or_default();
        match access {
            Access::Read => st.read = true,
            Access::Write | Access::Create | Access::Truncate => {
                st.wrote = true;
                st.deleted = false;
            }
            Access::Delete => st.deleted = true,
        }
    }

    /// Paths classified as inputs (read, never written, not deleted), filtered
    /// to the operation's repo-root scope. Sorted for determinism.
    pub fn scoped_inputs(&self) -> Vec<String> {
        self.scoped(|st| st.read && !st.wrote && !st.deleted)
    }

    /// Paths classified as outputs (written/created and not finally deleted).
    pub fn scoped_outputs(&self) -> Vec<String> {
        self.scoped(|st| st.wrote && !st.deleted)
    }

    fn scoped(&self, pred: impl Fn(&PathState) -> bool) -> Vec<String> {
        let wd = self.working_directory.as_deref().unwrap_or("");
        let mut out: Vec<String> = self
            .paths
            .iter()
            .filter(|(_, st)| pred(st))
            .map(|(p, _)| p.clone())
            .filter(|p| crate::scope::in_scope(p, wd))
            .collect();
        out.sort();
        out
    }
}

/// Information attributing a tracked PID to its operation root.
#[derive(Clone)]
struct Membership {
    root_pid: u32,
}

/// Reconstructs process trees and accumulates operations.
pub struct ProcessTree {
    whitelist: Vec<WhitelistEntry>,
    /// PID -> parent PID (from fork/exec).
    parents: HashMap<u32, u32>,
    /// PID -> membership for every tracked process.
    tracked: HashMap<u32, Membership>,
    /// root PID -> open operation.
    operations: HashMap<u32, Operation>,
    // #region agent log
    dbg_file_total: u64,
    dbg_attributed: u64,
    dbg_unattributed: u64,
    dbg_unattr_samples: Vec<String>,
    dbg_attr_samples: Vec<String>,
    dbg_exec_samples: Vec<String>,
    // #endregion
}

impl ProcessTree {
    pub fn new(whitelist: Vec<WhitelistEntry>) -> Self {
        Self {
            whitelist,
            parents: HashMap::new(),
            tracked: HashMap::new(),
            operations: HashMap::new(),
            // #region agent log
            dbg_file_total: 0,
            dbg_attributed: 0,
            dbg_unattributed: 0,
            dbg_unattr_samples: Vec::new(),
            dbg_attr_samples: Vec::new(),
            dbg_exec_samples: Vec::new(),
            // #endregion
        }
    }

    // #region agent log
    /// Whether a path is part of the (e2e) repo build I/O rather than system noise.
    fn dbg_interesting(path: &str) -> bool {
        !path.starts_with('/') || path.contains("/repo/")
    }

    /// Walk the parent chain for `pid`, marking which entries are tracked,
    /// so we can see exactly where attribution breaks.
    fn dbg_chain(&self, pid: u32) -> String {
        let mut out = String::new();
        let mut cursor = pid;
        for _ in 0..16 {
            let t = if self.tracked.contains_key(&cursor) { "T" } else { "-" };
            out.push_str(&format!("{cursor}{t} "));
            match self.parents.get(&cursor) {
                Some(&p) => cursor = p,
                None => {
                    out.push_str("|noparent");
                    break;
                }
            }
        }
        out
    }

    /// Emit a one-shot attribution summary (debug session 5d16d6) to stderr,
    /// which is captured in the CI job log.
    pub fn debug_summary(&self) {
        eprintln!(
            "[dbg 5d16d6 HYP=BCD] file_events_total={} attributed={} unattributed={} parents_map={} tracked_map={}",
            self.dbg_file_total, self.dbg_attributed, self.dbg_unattributed,
            self.parents.len(), self.tracked.len()
        );
        for s in &self.dbg_exec_samples {
            eprintln!("[dbg 5d16d6 HYP=BC exec] {s}");
        }
        for s in &self.dbg_attr_samples {
            eprintln!("[dbg 5d16d6 HYP=AE attr_sample] {s}");
        }
        for s in &self.dbg_unattr_samples {
            eprintln!("[dbg 5d16d6 HYP=BC unattr_sample] {s}");
        }
    }
    // #endregion

    /// Number of currently open (un-finalized) operations.
    pub fn open_count(&self) -> usize {
        self.operations.len()
    }

    fn set_parent(&mut self, pid: u32, ppid: u32) {
        if ppid != 0 {
            self.parents.insert(pid, ppid);
        }
    }

    /// Match the kernel 16-byte `comm` against a whitelist pattern (truncated,
    /// since `comm` is capped at `TASK_COMM_LEN`).
    fn match_whitelist(&self, comm: &str) -> Option<WhitelistEntry> {
        self.whitelist
            .iter()
            .find(|e| {
                let pat = &e.pattern;
                if pat.len() <= COMM_LEN {
                    comm == pat
                } else {
                    comm == &pat[..COMM_LEN]
                }
            })
            .cloned()
    }

    /// Establish (or look up) membership for `pid` with the given `comm`.
    /// Creating a new operation eagerly captures the root's command + cwd, since
    /// `/proc` for the root may be gone by the time it exits.
    fn membership(&mut self, pid: u32, comm: &str, proc: &dyn ProcSource) -> Option<u32> {
        if let Some(m) = self.tracked.get(&pid) {
            return Some(m.root_pid);
        }

        // Direct whitelist root match (comm may only be set after exec, so this
        // is also reached from file/exit events).
        if let Some(entry) = self.match_whitelist(comm) {
            self.start_operation(pid, comm, entry, proc);
            return Some(pid);
        }

        // Inherit from a tracked ancestor by walking the parent chain.
        let mut cursor = pid;
        for _ in 0..64 {
            let ppid = *self.parents.get(&cursor)?;
            if let Some(m) = self.tracked.get(&ppid).cloned() {
                self.tracked.insert(pid, m.clone());
                if let Some(op) = self.operations.get_mut(&m.root_pid) {
                    op.pids.insert(pid);
                }
                return Some(m.root_pid);
            }
            cursor = ppid;
        }
        None
    }

    fn start_operation(
        &mut self,
        pid: u32,
        comm: &str,
        entry: WhitelistEntry,
        proc: &dyn ProcSource,
    ) {
        self.tracked.insert(pid, Membership { root_pid: pid });
        let mut op = Operation::new(pid, comm.to_string(), entry.type_tag);
        op.command = proc.cmdline(pid);
        op.working_directory = proc.cwd(pid);
        self.operations.insert(pid, op);
    }

    pub fn on_exec(&mut self, e: &ExecEvent, proc: &dyn ProcSource) {
        self.set_parent(e.pid, e.ppid);
        // #region agent log
        if matches!(
            e.comm.as_str(),
            "sh" | "bash" | "dash" | "cat" | "mkdir" | "node" | "nest" | "tsc" | "npm run build"
        ) && self.dbg_exec_samples.len() < 40
        {
            self.dbg_exec_samples
                .push(format!("pid={} ppid={} comm={}", e.pid, e.ppid, e.comm));
        }
        // #endregion
        self.membership(e.pid, &e.comm, proc);
        // Late cwd/command capture if the root was created from a non-root comm.
        self.refresh_root_proc(e.pid, proc);
    }

    pub fn on_file(&mut self, e: &FileEvent, proc: &dyn ProcSource) {
        // #region agent log
        self.dbg_file_total += 1;
        // #endregion
        let Some(root_pid) = self.membership(e.pid, &e.comm, proc) else {
            // #region agent log
            self.dbg_unattributed += 1;
            if Self::dbg_interesting(&e.path) && self.dbg_unattr_samples.len() < 25 {
                let chain = self.dbg_chain(e.pid);
                self.dbg_unattr_samples.push(format!(
                    "pid={} comm={} {:?}:{} chain=[{}]",
                    e.pid, e.comm, e.access, e.path, chain
                ));
            }
            // #endregion
            return;
        };
        // #region agent log
        self.dbg_attributed += 1;
        if Self::dbg_interesting(&e.path) && self.dbg_attr_samples.len() < 25 {
            self.dbg_attr_samples
                .push(format!("pid={} comm={} {:?}:{}", e.pid, e.comm, e.access, e.path));
        }
        // #endregion
        if let Some(op) = self.operations.get_mut(&root_pid) {
            op.record(&e.path, e.access);
        }
    }

    /// Re-attempt command/cwd capture for an operation root that was created
    /// before its `comm`/proc data was available.
    fn refresh_root_proc(&mut self, pid: u32, proc: &dyn ProcSource) {
        if let Some(op) = self.operations.get_mut(&pid) {
            if op.command.is_none() {
                op.command = proc.cmdline(pid);
            }
            if op.working_directory.is_none() {
                op.working_directory = proc.cwd(pid);
            }
        }
    }

    /// Handle an exit. Returns a finalized [`Operation`] when an operation root
    /// exits (the operation boundary, `specs/observation.md` §5).
    pub fn on_exit(&mut self, e: &ExitEvent, proc: &dyn ProcSource) -> Option<Operation> {
        // npm sets its title after exec, so a root may only be recognizable now.
        self.membership(e.pid, &e.comm, proc);

        self.parents.remove(&e.pid);
        let membership = self.tracked.remove(&e.pid);

        match membership {
            Some(m) if m.root_pid == e.pid => {
                let mut op = self.operations.remove(&e.pid)?;
                op.exit_code = e.exit_code;
                op.end_time = Some(Utc::now());
                Some(op)
            }
            _ => None,
        }
    }

    /// Finalize all still-open operations (agent shutdown / `post` phase).
    pub fn finalize_all(&mut self) -> Vec<Operation> {
        let mut ops: Vec<Operation> = self.operations.drain().map(|(_, mut op)| {
            op.end_time = Some(Utc::now());
            op
        }).collect();
        ops.sort_by_key(|o| o.root_pid);
        ops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubProc;
    impl ProcSource for StubProc {
        fn cmdline(&self, _pid: u32) -> Option<String> {
            Some("npm run build".to_string())
        }
        fn cwd(&self, _pid: u32) -> Option<String> {
            Some("/work/repo".to_string())
        }
    }

    fn wl() -> Vec<WhitelistEntry> {
        vec![
            WhitelistEntry::new("npm run build", Some(RunRecordType::Build)),
            WhitelistEntry::new("npm run test", Some(RunRecordType::Test)),
        ]
    }

    fn exec(pid: u32, ppid: u32, comm: &str) -> ExecEvent {
        ExecEvent {
            pid,
            ppid,
            comm: comm.to_string(),
        }
    }

    fn file(pid: u32, comm: &str, path: &str, access: Access) -> FileEvent {
        FileEvent {
            pid,
            access,
            comm: comm.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn whitelisted_root_starts_operation_and_captures_proc() {
        let mut t = ProcessTree::new(wl());
        t.on_exec(&exec(100, 1, "npm run build"), &StubProc);
        assert_eq!(t.open_count(), 1);

        let exit = ExitEvent {
            pid: 100,
            exit_code: Some(0),
            comm: "npm run build".into(),
        };
        let op = t.on_exit(&exit, &StubProc).expect("op finalized at root exit");
        assert_eq!(op.command.as_deref(), Some("npm run build"));
        assert_eq!(op.working_directory.as_deref(), Some("/work/repo"));
        assert_eq!(op.whitelist_type, Some(RunRecordType::Build));
        assert_eq!(op.exit_code, Some(0));
    }

    #[test]
    fn descendants_attributed_and_classified() {
        let mut t = ProcessTree::new(wl());
        t.on_exec(&exec(100, 1, "npm run build"), &StubProc);
        t.on_exec(&exec(101, 100, "node"), &StubProc);
        t.on_exec(&exec(102, 101, "cc1"), &StubProc);

        t.on_file(&file(101, "node", "/work/repo/src/a.ts", Access::Read), &StubProc);
        t.on_file(&file(102, "cc1", "/work/repo/dist/a.js", Access::Create), &StubProc);
        // Out-of-scope reads are dropped by scope filtering.
        t.on_file(&file(102, "cc1", "/usr/lib/libc.so", Access::Read), &StubProc);
        t.on_file(&file(102, "cc1", "/work/repo/node_modules/x.js", Access::Read), &StubProc);

        let exit = ExitEvent {
            pid: 100,
            exit_code: Some(0),
            comm: "npm run build".into(),
        };
        let op = t.on_exit(&exit, &StubProc).unwrap();
        assert_eq!(op.scoped_inputs(), vec!["/work/repo/src/a.ts".to_string()]);
        assert_eq!(op.scoped_outputs(), vec!["/work/repo/dist/a.js".to_string()]);
    }

    #[test]
    fn read_then_write_is_output_only() {
        let mut t = ProcessTree::new(wl());
        t.on_exec(&exec(100, 1, "npm run build"), &StubProc);
        t.on_file(&file(100, "npm run build", "/work/repo/x", Access::Read), &StubProc);
        t.on_file(&file(100, "npm run build", "/work/repo/x", Access::Write), &StubProc);
        let exit = ExitEvent { pid: 100, exit_code: None, comm: "npm run build".into() };
        let op = t.on_exit(&exit, &StubProc).unwrap();
        assert!(op.scoped_inputs().is_empty());
        assert_eq!(op.scoped_outputs(), vec!["/work/repo/x".to_string()]);
    }

    #[test]
    fn untracked_process_ignored() {
        let mut t = ProcessTree::new(wl());
        t.on_exec(&exec(200, 1, "bash"), &StubProc);
        t.on_file(&file(200, "bash", "/work/repo/secret", Access::Read), &StubProc);
        assert_eq!(t.open_count(), 0);
    }
}
