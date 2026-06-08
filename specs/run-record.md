# Recorder - RunRecord

This spec defines the execution RunRecord the Recorder produces and the metadata sidecar it attaches. The RunRecord is the Recorder's only output contract: a flat JSON object (`type` in `build`/`test`/`deploy`) that conforms to the pipeline's `RunRecordBase` shape and is submitted to `POST /pipeline/process`. This spec is the per-field capture-source map (what the Recorder must put in each field and where it comes from), the canonical-form rules for the join keys (`path`, `commit`) that decide whether a deploy can be linked to a verdict, and the sidecar fields that carry fidelity and operational context outside the receipt-identity hash. Field-level structure mirrors the pipeline's [data-types](../../idora-pipeline/specs/data-types.md); this spec does not redefine it, it pins what the Recorder emits and how. Resolves the key-form half of #1, the commit half of #3, and the recorder side of #9 and #10.

**Status**: Draft

**Codebase mapping**: `ci-tracer/` (RunRecord assembly), `ci-tracer-common/`

**Related specs**: [observation](observation.md), [content-hashing](content-hashing.md), [ci-adapter](ci-adapter.md), [submission](submission.md), [idora-pipeline data-types](../../idora-pipeline/specs/data-types.md), [WIT runrecord](../../wit-core/specs/runrecord.md)

---

## 1. Relationship to the Pipeline Contract

The Recorder emits only the **execution variant** of the RunRecord. Verification (`verify-*`) and ingestion (`ingest`) variants are WIT Core's, not the Recorder's. The authoritative type definitions live in [idora-pipeline data-types](../../idora-pipeline/specs/data-types.md) §3; this spec restates the execution subset and adds the capture-source column the pipeline spec does not (and cannot) define.

The RunRecord is **flat** (all fields top-level, no wrapper) and uses `snake_case` on the wire.

---

## 2. Execution RunRecord Schema

Every field below is required on the wire (required-but-nullable means the key MUST be present, value may be `null`). The "Source" column is the Recorder's responsibility map.

| Field | Type | Source (how the Recorder fills it) |
|-------|------|------------------------------------|
| `type` | `"build" \| "test" \| "deploy"` | [observation](observation.md) §5 (adapter label > whitelist tag > heuristic). |
| `command` | string | Root argv via `/proc/<root_pid>/cmdline` ([observation](observation.md) §6). |
| `exit_code` | int \| null | Root `sched_process_exit`; `null` if unobservable, never defaulted to 0. |
| `platform` | string | Active probe (`uname -s` -> `linux`). |
| `architecture` | string | Active probe (`uname -m`, normalized to `amd64`/`arm64`). |
| `working_directory` | string (absolute) | `/proc/<root_pid>/cwd` ([observation](observation.md) §6); the relativization root. |
| `repo` | string | [ci-adapter](ci-adapter.md) §3. |
| `commit` | string | [ci-adapter](ci-adapter.md) §4 (canonical, PR merge-vs-head resolved). |
| `tool_versions` | object \| null | Active probe of toolchain binaries; `null` if not cheaply resolvable. |
| `inputs` | `[{path,hash}]` \| null | Files read, scoped to repo root ([observation](observation.md) §8), hashed ([content-hashing](content-hashing.md)). |
| `outputs` | `[{path,hash}]` \| null | Files created/modified, hashed at finalization ([content-hashing](content-hashing.md) §3). |
| `environment` | object \| null | Allowlisted env only ([security](security.md) §3). |
| `timestamps` | `{start_time,end_time,duration_ms}` | Process exec/exit events; stripped by Normalize, surfaced on the sidecar (§4). |

`inputs[].hash` / `outputs[].hash` are `sha256:<64 lowercase hex>` of file content (see [content-hashing](content-hashing.md)).

---

## 3. Join-Compatible Keys (#1, #3)

The whole product depends on a deploy's identifiers matching the verification side's so the two records link in the graph. The Recorder and WIT meet today only at the path-keyed `:File{path,repo}` node (path = correlation); the content-hash join (`:Artifact{hash}`) is the proof target (see [content-hashing](content-hashing.md) §5-6). Either way the Recorder MUST emit the keys in the **exact canonical form** the verification side uses, or the join silently returns nothing.

### 3.1 Path canonical form

| Rule | Decision |
|------|----------|
| Relativization | The Recorder reports **real absolute paths** plus a correct `working_directory`; the pipeline's Normalize relativizes against it. The Recorder does not pre-relativize. |
| Separator | Forward slashes (Linux). |
| Case | Exact, as-on-disk (Linux is case-sensitive). |
| Result | The relativized path MUST equal the repo-relative path WIT records as `filesEvaluated` (e.g. `src/auth.ts`). |

### 3.2 Commit (#3)

`commit` MUST be the SHA that actually shipped, in the same string form WIT records. On PRs, `GITHUB_SHA` is the synthetic **merge** commit, which may not be the head that shipped; the [ci-adapter](ci-adapter.md) §4 resolves the canonical commit. Recording the wrong one silently breaks the join for PR builds.

### 3.3 Content equivalence

The content-hash join additionally requires the Recorder and WIT to hash the **same content the same way**. Decided (#1): tracked source inputs are hashed over their **git-blob-normalized content** (not raw on-disk bytes), which removes the checkout-time CRLF/LFS divergence risk — see [content-hashing](content-hashing.md) §5. This requires WIT to hash the same normalized form (cross-repo dependency).

---

## 4. Metadata Sidecar

The sidecar is the second top-level field of `POST /pipeline/process`, **outside** the receipt-identity hash, stored on the Receipt node with last-write-wins. It mirrors the pipeline's `ExecutionMetadataSidecar` (see [idora-pipeline data-types](../../idora-pipeline/specs/data-types.md) §4.2) and is the correct home for everything the Recorder observes that is operational, not identity-bearing.

| Sidecar field | Source | Purpose |
|---------------|--------|---------|
| `start_time`, `end_time`, `duration_ms` | exec/exit events | Operational timing (stripped from the hashed record). |
| `observation_mode` | [observation](observation.md) §7 + [content-hashing](content-hashing.md) §4 | Fidelity/evidence grade (`file_access_tracking`; kernel-atomic hashing tier pending pipeline support). The `snapshot` mode is dropped (see [observation](observation.md) §10). |
| `run_id`, `run_attempt` | [ci-adapter](ci-adapter.md) | Deploy-event identity for re-deploy counting (§5, #10). |
| `deploy_target` | [ci-adapter](ci-adapter.md) §5 | Deploy destination (`prod` / `staging`), for #9. |

Because these are outside the hash, capturing them is safe and does not perturb the content-addressed receipt ID.

---

## 5. Re-Deploy Identity & Deploy Target (#9, #10)

### 5.1 Re-deploy collapse (#10)

The receipt ID is `sha256` of the canonical record with `timestamps` stripped, so two identical re-deploys collapse to one `:Receipt` node — correct for content identity, but it erases deploy frequency/count. The fix is to keep the receipt content-addressed and represent each deploy as a separate event; the Recorder's part is to **emit `run_id` + `run_attempt` on the sidecar** so a distinct deploy occurrence is identifiable. The Recorder MUST NOT put `run_id` into the hashed record (that would break content-addressing and idempotent retries).

> Decided (#10, per recommendation): the Recorder **emits `run_id` + `run_attempt` on the sidecar** so a distinct deploy occurrence is identifiable, and never puts them in the hashed record. Cross-repo dependency: turning those sidecar values into a countable timeline requires a pipeline-side deploy-event representation (e.g. a `DeployEvent` node linked to the receipt), owned by idora-pipeline (see [architecture](architecture.md) §8).

### 5.2 Deploy target first-classness (#9)

The Recorder observes file I/O, not a deploy destination; "shipped to **prod**" cannot be derived from process observation alone. The target is therefore **adapter-sourced** ([ci-adapter](ci-adapter.md) §5) and carried as `deploy_target` on the sidecar.

> Decided (#9, per recommendation): deploy target is **adapter-sourced**, carried as `deploy_target` on the sidecar, **optional**. Cross-repo dependency: making `deploy_target` queryable (e.g. a Receipt property / distinct exposure view) is a pipeline-side change.

---

## 6. `parent_ids` Discrepancy (note)

WIT's [runrecord](../../wit-core/specs/runrecord.md) `RunRecordBase` includes a `parent_ids: string[]` field (for `PARENT_OF` execution-receipt edges, see WIT [receipt-pipeline](../../wit-core/specs/receipt-pipeline.md) §2.1), but the idora-pipeline `RunRecordBase` ([data-types](../../idora-pipeline/specs/data-types.md) §3.1) does not list it. The Recorder naturally has parentage from the process tree ([observation](observation.md) §4). This spec flags the discrepancy: if `PARENT_OF` is to be populated for execution receipts, `parent_ids` must be reconciled into the idora-pipeline contract first (cross-repo). Until reconciled, the Recorder does not emit `parent_ids`.

---

## 7. Field Capture Summary

A useful split (see [recorder-proposal](../docs/recorder-proposal.md) §2): `working_directory`, `environment`, `timestamps`, `command`, `exit_code`, `inputs`, `outputs` are **observed from the process**; `repo`, `commit`, `deploy_target`, `run_id`/`run_attempt` are **adapter-sourced**; `platform`, `architecture`, `tool_versions` are **host/toolchain-probed**. Only the adapter-sourced group differs across CI systems — the reason the [ci-adapter](ci-adapter.md) boundary exists.
