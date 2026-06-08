# Recorder - Deployment

This spec defines how the Recorder is delivered into a customer's CI and how it behaves across runner types. The committed delivery model is **Approach A**: an external eBPF agent started by a single composite GitHub Action step, with no change to the customer's image or entrypoint. It covers the Action lifecycle (pre/main/post), the binary pull + checksum-verify, the runner compatibility matrix, the degraded-mode detection and fallback for runners without eBPF, and the optional non-eBPF (Approach C) path reserved for constrained environments. The tradeoff analysis behind these choices is in [recorder-proposal](../docs/recorder-proposal.md) §4-5; this spec is the operational contract. Resolves the runner-detection/fallback half of #7.

**Status**: Draft

**Codebase mapping**: `action/` (`action.yml`, `start.js`, `noop.js`, `stop.js`), release tooling

**Related specs**: [architecture](architecture.md), [observation](observation.md), [content-hashing](content-hashing.md), [security](security.md), [recorder-proposal](../docs/recorder-proposal.md)

---

## 1. Delivery Model (Approach A)

The Recorder ships as a **composite GitHub Action** that drops a privileged static binary onto the runner and starts it before the build/test/deploy steps. One workflow step, no image change, no entrypoint change:

```yaml
- name: Start CI Recorder
  uses: Idora-LLC/ebpf-image@<pinned>
```

This is the recommended default (lowest adoption friction, narrowest/most-removable trust surface; see [recorder-proposal](../docs/recorder-proposal.md) §5). The non-eBPF fallback path is Section 5.

---

## 2. Action Lifecycle

Bound to the Action's `pre`/`main`/`post` hooks ([action.yml](../action.yml)):

| Phase | Hook | Action |
|-------|------|--------|
| Start | `pre` (`start.js`) | Pull the binary by **pinned digest**, **checksum-verify** it (see [security](security.md) §4), mount `tracefs`/`debugfs`, start the agent under `sudo`, wait for hooks to attach. |
| Observe | `main` (`noop.js`) | No-op; the customer's build/test/deploy steps run unchanged while the background agent observes. |
| Flush + submit | `post` (`stop.js`, `post-if: always()`) | Signal the agent to finalize open operations, hash, assemble, submit, and reconcile (see [submission](submission.md) §5) — even if earlier steps failed. |

`post-if: always()` guarantees finalization and reconciliation run on failure paths too, so a failed build still yields an observed (or explicitly unknown) record.

---

## 3. Runner Compatibility Matrix

eBPF support, not Idora, sets the ceiling here (validated in [recorder-proposal](../docs/recorder-proposal.md) §4-A).

| Runner | eBPF | Capture |
|--------|------|---------|
| `ubuntu-latest` / `22.04` / `24.04` (standard hosted) | Yes (kernel 6.x, `sudo`, `CAP_BPF`) | Full `file_access_tracking`, userspace-hashed. |
| `ubuntu-slim` (unprivileged container) | No (no low-level kernel access) | Degrade (Section 4). |
| macOS / Windows hosted | No | Unsupported for eBPF; no record (Section 4). |
| Self-hosted Linux | Operator-controlled | Full; may enable BPF-LSM for kernel-atomic hashing ([content-hashing](content-hashing.md) §4). |

BPF-LSM (the kernel-atomic hashing path) needs a GRUB change + reboot and is therefore **self-hosted-only**; it cannot be enabled on hosted runners.

---

## 4. No-eBPF Handling (#7)

The agent MUST detect at startup whether eBPF is available and behave deterministically — never silently produce a low-fidelity record that looks complete.

| Detection result | Behavior |
|------------------|----------|
| eBPF available | Full capture; `observation_mode = "file_access_tracking"`. |
| eBPF unavailable | **No-op: no record produced**, the build is not failed. Reconciliation reports the job as **unknown** coverage (see [submission](submission.md) §5), never clean. |

The inputs-blind `snapshot` fallback is **dropped**: an outputs-only record cannot identify source inputs, so it carries no join/proof value (see [observation](observation.md) §10). On a no-eBPF runner the honest signal is "unknown coverage", not a misleadingly confident outputs-only record.

> Decided (#7): on a no-eBPF runner the default is **no-op** — produce no record and do not fail the build; the gap is reconciled as unknown. (Hard-fail is available as an opt-in policy for customers who want CI to stop when capture is impossible; both no-op and hard-fail reconcile as unknown.) Snapshot remains dropped (see [observation](observation.md) §10).

---

## 5. Optional: Non-eBPF Fallbacks (Approach C)

A possible future portability layer for constrained runners (older kernels, `ubuntu-slim`) where eBPF is unavailable. Only **input-capable** mechanisms are candidates, because an outputs-only capture has no proof value (the reason `snapshot` is dropped — see [observation](observation.md) §10): fanotify (file events, still needs `CAP_SYS_ADMIN` plus a separate exec-capture path) or — only when wrapping a single command — ptrace. None is implemented today; until one exists, no-eBPF runners produce no record (Section 4). Mechanism detail is deferred; the role is fallback, not primary.

---

## 6. Release & Pinning

| Control | Decision |
|---------|----------|
| Versioning | Customers pin the Action by tag or commit/digest; `latest` is not recommended. |
| Binary identity | Pulled by pinned digest, checksum-verified, signed release (see [security](security.md) §4). |
| Compatibility | The submitted RunRecord/sidecar shape tracks the pipeline contract ([run-record](run-record.md)); breaking changes are versioned with the pipeline's schema. |
