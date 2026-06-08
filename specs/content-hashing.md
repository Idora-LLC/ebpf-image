# Recorder - Content Hashing

This spec defines how the Recorder computes the content hashes that populate `inputs[].hash` and `outputs[].hash` on the execution RunRecord. It covers the hash format, *when* a file is hashed relative to the operation (the TOCTOU window), the fidelity tiers exposed via `observation_mode`, and the "same bytes, same way" requirement that the content-hash join depends on. This is **Condition 1 of the proof goal** (the recorder-side hash); it is necessary but not sufficient, because the cross-stream proof join also needs the verification-side hash to be persisted (Section 6). This spec resolves founder issue #2, the tier half of #7, and the content-join half of #1.

**Status**: Draft

**Codebase mapping**: `ci-tracer/` (hashing)

**Related specs**: [observation](observation.md), [run-record](run-record.md), [architecture](architecture.md), [idora-pipeline data-types](../../idora-pipeline/specs/data-types.md), [idora-pipeline receipt-generation](../../idora-pipeline/specs/steps/receipt-generation/receipt-generation.md)

---

## 1. Purpose

Produce a `sha256:` content digest for every in-scope input and output file, so that two records describing the *same bytes* carry the *same* hash. This is the spine of the Idora "proof of what shipped" goal: identity-by-bytes, not identity-by-name (see [recorder-proposal](../docs/recorder-proposal.md) §2). The current PoC captures paths but not content hashes; closing that is the central hashing work.

---

## 2. Scope

### 2.1 Owns

| Responsibility |
|----------------|
| Reading the bytes of each in-scope file and computing its `sha256` digest. |
| Choosing the hash *timing* to minimize the TOCTOU window (Section 3). |
| Selecting and reporting the fidelity tier (Section 4). |

### 2.2 Does Not Own

| Excluded responsibility | Owner |
|-------------------------|-------|
| Deciding which files are in scope (read vs. write, repo-root scoping). | [observation](observation.md) §6, §8 |
| Path relativization / canonical form. | idora-pipeline Normalize ([run-record](run-record.md) §3) |
| The receipt-id hash (hash of the *record*, not the file). | idora-pipeline Receipt Generation |

### 2.3 Critical Constraints

- **Always hash content.** The Recorder ALWAYS computes a real content digest for every in-scope file; there is no non-hashing or outputs-only mode (snapshot is dropped, see [observation](observation.md) §10). The only variation is the TOCTOU grade (Section 4).
- **Hash while the workspace exists.** Content cannot be recovered after the CI job tears down; hashing MUST happen during the run (at operation boundaries / `post`), never deferred to an offline artifact (see [architecture](architecture.md) §6).
- **Git-canonical content for tracked inputs.** For git-tracked source inputs the Recorder hashes the **git-blob-normalized content** (the content as git stores it, after eol/clean normalization), not the raw on-disk bytes — so checkout-time CRLF/LFS differences do not break the join (Section 5). Generated outputs (not in git) are hashed as raw bytes.
- **Format is fixed.** `sha256:<64 lowercase hex>` exactly, matching the pipeline's `ReceiptID` format (see [idora-pipeline data-types](../../idora-pipeline/specs/data-types.md) §2.1, §7).

---

## 3. Hash Timing & the TOCTOU Window (#2)

Between observing an access and reading the bytes there is a Time-Of-Check-To-Time-Of-Use window: a file read early and modified later could be hashed in either state.

| Case | Timing rule |
|------|-------------|
| Input (read) | Hash the content as read by the operation. Prefer hashing at the read/close nearest the access; the practical hosted approach is hash-on-first-read or at operation boundary, accepting a narrow window. |
| Output (write/create) | Hash at **`CLOSE_WRITE` / process-exit**, i.e. the finalized content — that is what "shipped". |

On hosted runners the window can be **narrowed** (hash on close / process-exit) but not closed; a hosted hash is therefore "content shortly after access", a slightly weaker grade than a kernel-atomic hash (Section 4). The grade is recorded, not hidden.

---

## 4. Fidelity Tiers (#2, #7)

Hash fidelity is exposed as a tier so the consuming surface reads it as an evidence grade rather than assuming uniform quality.

| Tier | Mechanism | Where | TOCTOU |
|------|-----------|-------|--------|
| Kernel-atomic | `bpf_ima_file_hash()` / BPF-LSM, hashed in-kernel at access | **Self-hosted only** (needs BPF-LSM: GRUB change + reboot) | Closed |
| Userspace-hashed | eBPF observes the access; userspace reads + `sha256` | **Hosted default** | Narrowed |

Both tiers hash real content of both inputs and outputs; they differ only in the TOCTOU grade. There is no outputs-only (snapshot) floor — that mode is dropped (see [observation](observation.md) §10).

> Decided (#13): the Recorder **always hashes** real content — every in-scope file gets a digest, on every runner where it produces a record; there is no non-hashing mode. (#2/#7): the **hosted default is userspace-hashed**; kernel-atomic is a **self-hosted-only** tier (#12: BPF-LSM needs a GRUB change + reboot, so it cannot run on hosted runners). The tier sets the hash's TOCTOU *grade*, never whether the join is content-based — content hashing is not a blocker. (Whether kernel-atomic is packaged as a paid self-hosted tier is a commercial decision, not an engineering gate.)

---

## 5. Same Bytes, Same Way (#1, content join)

The content-hash join only works if the Recorder and the verification side (WIT) hash the **identical byte stream**. `sha256` is OS-independent, so "same OS" is not the relevant axis — identical bytes give identical digests anywhere.

The real risk is that the bytes on disk differ **before** the Recorder reads them, due to factors outside the Recorder:

- `git` line-ending normalization at checkout (`core.autocrlf`, `.gitattributes`): CRLF<->LF differences make the on-disk bytes diverge from another checkout's, despite an identical git blob.
- Git LFS smudge filters and `clean`/`smudge` filters that rewrite content on checkout.

Hashing raw on-disk bytes would therefore be fragile across two differently-configured checkouts.

> Decided (#1, content side): the Recorder hashes the **git-blob-normalized content** of tracked source inputs (the content as git stores it, independent of checkout-time `core.autocrlf` / `.gitattributes` / LFS smudge), so the input hash matches regardless of how the working tree was materialized. Generated outputs, which are not git objects, are hashed as raw bytes. **Cross-repo dependency:** the verification side (WIT) currently hashes raw checked-out bytes (`sha256(readFileSync(...))`); for the content join to hold, WIT MUST hash the **same** git-blob-normalized form. This is owned by idora/wit-core (see [architecture](architecture.md) §8) and MUST be validated, not assumed.

---

## 6. Necessary but Not Sufficient

The recorder-side hash is one of the two hashes the proof join needs. The other is the verification-side source hash. Per the pipeline audit, WIT **already computes** `sha256` of the files it evaluated into the verification RunRecord's `inputs[].hash`, but the pipeline's verification writer currently **discards** it (it writes `:File{path}` only, no `:Artifact{hash}`). So:

- Output hashes identify *what shipped*.
- Input (source) hashes are what *join to the verdict* — but only once the pipeline **persists** the verification-side hash as an `:Artifact` node.

This persistence is a cross-repo dependency owned by idora-pipeline (see [architecture](architecture.md) §8). Until it lands, the cross-stream join stays path-based correlation even when the Recorder hashes perfectly (see [run-record](run-record.md) §3, founder #1).

---

## 7. Error Codes

| Code | Condition |
|------|-----------|
| `HASH_R_001` | A file in scope could not be read for hashing (permissions / vanished); entry flagged, not silently dropped. |
| `HASH_R_002` | Hashing did not complete before workspace teardown (operation finalized too late). |
