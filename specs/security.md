# Recorder - Security & Trust Surface

This spec defines the trust surface of the Recorder and the postures that keep it acceptable to a customer who is letting a startup's privileged binary run inside their CI. It covers the privilege model (what the agent needs and why), environment-variable capture (allowlist not denylist, and whether to capture env at all), binary supply-chain hardening (reproducible build, signed action, pinned digest), and the runner-privilege bounds that follow from requiring privilege. The transport credential is specified separately in [authentication](authentication.md). This spec resolves founder issue #8; it is the top adoption gate even though it is correctness-light.

**Status**: Draft

**Codebase mapping**: `ci-tracer/`, `ci-tracer-ebpf/`, `action/`, build/release tooling

**Related specs**: [authentication](authentication.md), [observation](observation.md), [run-record](run-record.md), [deployment](deployment.md), [recorder-proposal](../docs/recorder-proposal.md)

---

## 1. The Trust Statement

The Recorder runs **privileged** (`CAP_BPF`/`sudo`) inside the customer's CI and can read process environments (`/proc/<pid>/environ`), which include secrets. A leaked secret from a third-party binary in customer CI is a breach in *their* environment — contract-ending for a trust vendor, not a mere bug. The design goal of this spec is to make the trust surface **narrow, auditable, and removable**, so the privilege is justified and the failure modes are contained.

---

## 2. Privilege Model

| Aspect | Decision |
|--------|----------|
| Why privileged | eBPF hook attach requires `CAP_BPF`/`CAP_PERFMON` (or `sudo`) — kernel-global observation has no unprivileged path (see [observation](observation.md) §3). |
| What it does NOT do | It does not modify the build, replace the entrypoint, take over PID 1, or alter customer infrastructure (see [recorder-proposal](../docs/recorder-proposal.md) §5, Approach A). |
| Removability | The entire footprint is one CI step; deleting it removes the Recorder completely. |
| Auditability | The agent is a single static binary with a pinned, verifiable identity (Section 4). |

---

## 3. Environment Capture: Allowlist, Not Denylist (#8)

`environment` capture is the highest-risk data path because env carries secrets.

| Rule | Decision |
|------|----------|
| Allowlist only | The Recorder captures env via an **allowlist of known-safe keys** supplied by the [ci-adapter](ci-adapter.md) §6. A denylist of known-bad keys leaks unknown custom secrets; an allowlist fails closed. |
| Defense in depth | This is on top of the pipeline's own normalization filter — secrets should never leave the runner in the first place. |
| Token excluded | The recorder token is never in the allowlist (see [authentication](authentication.md) §3). |
| Capture-at-all is optional | Whether to capture `environment` at all is a posture call: it adds graph value but raises the secret-exposure stakes. |

> Decided (#8, per recommendation): env capture is **allowlist-only** (never denylist); the default is a **minimal allowlist (or nothing)**, widened only deliberately and per-deployment. The recorder token is never in the allowlist.

---

## 4. Binary Hardening (supply chain)

The Recorder is itself a supply-chain element in the customer's pipeline; it must be verifiable.

| Control | Decision |
|---------|----------|
| Reproducible build | The agent binary is built reproducibly so its provenance can be independently verified. |
| Signed action / artifact | The GitHub Action and the binary are signed; the release provenance is attestable. |
| Pinned digest | The Action pulls the binary by **pinned digest** and **checksum-verifies** it before execution (see [deployment](deployment.md) §2) — never a mutable `latest` tag. |
| No secret in artifact | The binary and image contain no credentials; the token is injected at runtime ([authentication](authentication.md) §3). |

> Committed resolution (#8, hardening): reproducible build + signed release + pinned-digest/checksum-verified pull. (Settled engineering, not research.)

---

## 5. Runner-Privilege Bounds

Requiring privilege bounds the addressable runners — a direct consequence, not a defect.

| Runner | Privilege | Recorder |
|--------|-----------|----------|
| `ubuntu-latest`/`22.04`/`24.04` (standard hosted) | passwordless `sudo`, `CAP_BPF` | Full eBPF capture. |
| `ubuntu-slim` (unprivileged container) | no low-level kernel access | eBPF unavailable; degrade (see [deployment](deployment.md) §4). |
| macOS / Windows hosted | no eBPF | Unsupported for eBPF capture. |
| Self-hosted | operator-controlled | Can enable BPF-LSM for kernel-atomic hashing ([content-hashing](content-hashing.md) §4). |

---

## 6. Threat Notes

| Threat | Mitigation |
|--------|------------|
| Secret exfiltration via captured env | Allowlist-only capture (Section 3); minimal/none by default. |
| Tampered Recorder binary | Pinned digest + checksum verify + signing (Section 4). |
| Token leak | In-memory only, never logged or captured; per-customer tokens for containment ([authentication](authentication.md) §3, §5). |
| Privilege misuse beyond observation | Single-purpose static binary, reproducible and auditable; no infra modification. |
