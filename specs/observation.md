# Recorder - Observation

This spec defines how the Recorder observes CI operations while it runs: the eBPF hook set it attaches, how it reconstructs the per-operation process tree, how it bounds an "operation" and attributes a `type` (`build`/`test`/`deploy`), what file I/O it captures and which paths count as inputs/outputs, and how it degrades when the full hook set is unavailable. Observation is the agent's continuous concern — it drains kernel events for the life of the job and, per operation, yields a process tree plus a raw set of file accesses (read vs. write/create) with absolute paths; resolving those into `command`, `exit_code`, `working_directory`, and the input/output path sets (Section 6) is what feeds [content-hashing](content-hashing.md) and [run-record](run-record.md). This spec resolves founder issues #4 (operation boundaries & `type`), #5 (I/O completeness & scope), and the capture-mode half of #7.

**Status**: Draft

**Codebase mapping**: `ci-tracer-ebpf/src/main.rs`, `ci-tracer/src/main.rs`, `ci-tracer-common/src/lib.rs`

**Related specs**: [architecture](architecture.md), [content-hashing](content-hashing.md), [run-record](run-record.md), [ci-adapter](ci-adapter.md), [deployment](deployment.md)

---

## 1. Purpose

Turn kernel-level process and file-syscall events into a per-operation record of *what ran* and *which files it read and wrote*, accurately enough that a missed input never silently reads as a clean operation. Under-capture is the dangerous direction (Section 8): a missing input hides an unverified dependency, so the operation appears to conform when it does not.

---

## 2. Scope

### 2.1 Owns

| Responsibility |
|----------------|
| Attaching the eBPF hook set and draining events without loss under normal CI load. |
| Reconstructing the process tree for each whitelisted root operation. |
| Bounding operations and attributing `type` (Section 5). |
| Classifying each file access as read vs. write/create, and applying the input-inclusion scope rule (Section 8). |
| Selecting and reporting the `observation_mode` capture tier (Section 7). |

### 2.2 Does Not Own

| Excluded responsibility | Owner |
|-------------------------|-------|
| Computing content hashes of files. | [content-hashing](content-hashing.md) |
| Sourcing `repo` / `commit` / deploy target. | [ci-adapter](ci-adapter.md) |
| Assembling or submitting the RunRecord. | [run-record](run-record.md), [submission](submission.md) |
| Deciding the delivery vehicle / runner detection. | [deployment](deployment.md) |

### 2.3 Critical Constraints

- **No process modification.** The agent attaches to kernel-global hooks; it never wraps, patches, `ptrace`s, or `LD_PRELOAD`s the observed processes.
- **Fail toward visibility, not silence.** If capture is degraded, the agent MUST mark the mode (Section 7) so a low-fidelity record is never mistaken for a complete one. It MUST NOT emit a confident, inputs-blind record unflagged.
- **Bounded, in-kernel filtering.** Filtering to the whitelisted process tree happens in-kernel where possible (Section 4) to keep event volume and overhead bounded.

---

## 3. eBPF Hook Set

The PoC attaches process-lifecycle tracepoints plus `openat`. Observation broadens this to avoid the `openat`-only blind spots (Riker's "model the whole filesystem" lesson; see [recorder-proposal](../docs/recorder-proposal.md) Research).

| Hook | Kind | Captures | Status |
|------|------|----------|--------|
| `sched_process_fork` | tracepoint | parent->child PID, for tree reconstruction | PoC |
| `sched_process_exec` | tracepoint | PID, PPID, `comm`, exec filename, start time | PoC |
| `sched_process_exit` | tracepoint | PID, exit code, duration | PoC (exit code added, see Section 6) |
| `sys_enter_openat` | tracepoint | path + open flags (read/write/create) | PoC |
| `sys_enter_openat2` | tracepoint | same, newer call convention | Add |
| `sys_enter_renameat2` | tracepoint | renames (output under a new path) | Add |
| `sys_enter_unlinkat` | tracepoint | deletions (output removed) | Add |
| `sys_enter_truncate` / `ftruncate` | tracepoint | in-place output truncation | Add |

The kernel/userspace contract is the `ci-tracer-common` event structs (`ProcessExecEvent`, `ProcessExitEvent`, `FileOpenEvent`). Added hooks add event types there.

> **Decided (#12) - hosted-runner ceiling.** The strongest coverage (LSM/VFS hooks via BPF-LSM) is **self-hosted-only**: enabling BPF-LSM needs a GRUB change + reboot, impossible on a hosted CI job (see [recorder-proposal](../docs/recorder-proposal.md) §4-A, [deployment](deployment.md) §3). On hosted runners the agent is capped at tracepoints/kprobes; the broadened tracepoint set above is the committed hosted coverage. (Whether LSM coverage is packaged as a paid self-hosted tier is a commercial decision, not an engineering gate.)

---

## 4. Process-Tree Reconstruction & Whitelist

Each operation is rooted at a **whitelisted command** ([action.yml](../action.yml) `whitelist`, default `npm run build,npm run test`; matched against the root process). Child processes are attributed to the root by walking `fork`/`exec` parentage (the PoC `PARENT_MAP` + parent walk; `comm` may be set after exec, so attribution follows the tree, not just `comm`).

| Rule | Statement |
|------|-----------|
| Root match | A process whose command matches the whitelist starts a new operation. |
| Inheritance | Every descendant of a root is part of that operation until it exits. |
| Isolation | Processes outside any whitelisted tree are ignored (kernel-global visibility, userspace scoping). |

---

## 5. Operation Boundaries & `type` Attribution (#4)

An **operation** is the process subtree rooted at one whitelisted command invocation. One operation produces exactly one execution RunRecord. This bounding (rather than per-process or per-job) is what makes a deploy a single, attributable receipt; mis-segmentation would fuse two revisions' I/O into one receipt, and a mis-typed deploy would drop out of any "what shipped" view keyed on `type:deploy`.

`type` (`build` / `test` / `deploy`) is derived in this precedence order:

1. **Explicit adapter label (authoritative).** The [ci-adapter](ci-adapter.md) maps the CI step/command to a `type` (e.g. an action input or step name). This is the recommended source.
2. **Whitelist entry tag.** Each whitelist entry MAY declare its `type` (e.g. `npm run build => build`).
3. **Heuristic (last resort).** Command-string heuristics (`build`/`test`/`deploy` keywords) only when neither above is set; a heuristic miss is flagged for reconciliation, not silently typed.

> Decided (#4, per recommendation): the operation boundary is the **process-tree root of a whitelisted command**, and `type` comes from the **adapter label first**, falling back to whitelist tag, then heuristic as a last resort (a heuristic-derived `type` is flagged `OBS_003` for reconciliation, never silently trusted).

---

## 6. Resolving the Non-I/O Fields

From the observed tree, the agent derives the non-I/O execution fields at the operation boundary:

| Field | Source |
|-------|--------|
| `command` | Full root argv via `/proc/<root_pid>/cmdline` (the exec filename alone is insufficient). |
| `exit_code` | From `sched_process_exit` of the operation's root; `null` if not observable (never defaulted to 0). |
| `working_directory` | `/proc/<root_pid>/cwd` of the operation root (absolute); CI workspace var is a coarser fallback. |
| Raw input/output paths | Absolute paths from the file-access events, classified read vs. write/create from open flags / syscall. |

Path classification mirrors the PoC: `O_CREAT` => create, `O_WRONLY`/`O_RDWR` => write (output), otherwise read (input). Outputs are best finalized on close/exit (see [content-hashing](content-hashing.md) §3).

---

## 7. Capture Mode (`observation_mode`) (#7)

The agent stamps the fidelity it achieved, carried to the pipeline on the metadata sidecar (see [run-record](run-record.md) §4) and read downstream as an evidence grade.

| Mode | Meaning | Where |
|------|---------|-------|
| `file_access_tracking` | Full eBPF I/O tracing (inputs + outputs observed). | Standard hosted/self-hosted runners with eBPF. |

There is a **single capture mode**: full eBPF I/O tracing. There is no inputs-blind degraded mode — an outputs-only capture cannot identify the source files, so it cannot serve the join that ties a deploy to a verdict and is therefore not worth emitting (see Open Questions). On a runner without eBPF the agent produces **no record** rather than a degraded one (see [deployment](deployment.md) §4); the gap is reconciled as **unknown** (see [submission](submission.md) §5), never as a clean build.

A higher-fidelity self-hosted hashing tier (kernel-atomic) is layered on top of this same mode by [content-hashing](content-hashing.md) §4; it does not introduce a second observation mode.

---

## 8. I/O Completeness & Scope (#5)

Kernel-global observation sees *everything* the operation touches, including system libraries and all of `node_modules`. "What counts as an input/output" is a scoping decision, not a capture limit.

| Rule | Decision |
|------|----------|
| Inclusion root | Only paths under the operation's `working_directory` / repo root are recorded as `inputs`/`outputs`. |
| Excludes | System paths (`/usr`, `/lib`, toolchain caches) and `node_modules` are excluded by default. |
| Out-of-tree reads | Excluded: an out-of-tree path cannot be relativized and so cannot join (see [run-record](run-record.md) §3, founder #1). |
| Completeness direction | When in doubt, **over-include within the repo root** rather than under-include: a missed in-repo input is a false clean (#5). |

> Decided (#5): **scope = repo root.** Inputs/outputs are only the paths under the operation's `working_directory` / repo root, captured via the broadened tracepoint set (Section 3); system paths and `node_modules` are excluded, and out-of-tree paths are excluded (they cannot be relativized and so cannot join). Within the repo root, over-include rather than under-include. (The precise exclusion list is an implementation detail under this rule; out-of-tree dependencies are out of scope by this decision.)

---

## 9. Error & Degradation Codes

| Code | Condition |
|------|-----------|
| `OBS_001` | eBPF unavailable on this runner; no record produced, gap reconciled as unknown (see [deployment](deployment.md) §4). |
| `OBS_002` | Event-buffer overflow / dropped events; record flagged degraded. |
| `OBS_003` | `type` could not be attributed from adapter or whitelist; heuristic used (flagged for reconciliation). |
| `OBS_004` | Operation root command did not match any whitelist entry (no record emitted). |

These flags travel with the record so downstream consumers can distinguish a complete capture from a degraded one rather than reading a gap as clean.

---

## 10. Open Questions

**Q1. Is removing the `snapshot` capture mode acceptable? (#7)** These specs **drop** the inputs-blind `snapshot` / snapshot-diff fallback entirely. Rationale: a snapshot sees only outputs, not which files were read, so it cannot supply the source inputs that join a deploy to a verdict (#1) — it carries essentially no proof value on its own, and an unflagged one is a false "clean". Consequence: on runners without eBPF the Recorder produces **no record** (the gap is reconciled as **unknown**, see [submission](submission.md) §5) instead of a degraded one. This removes the second `observation_mode` value, the snapshot hashing floor ([content-hashing](content-hashing.md) §4), and the snapshot fallback path ([deployment](deployment.md) §4). **Founder decision: confirm we drop `snapshot`, accepting no capture (unknown) on no-eBPF runners rather than an outputs-only record.**
