# Recorder - Submission

This spec defines how the Recorder submits an assembled execution RunRecord to the Idora Pipeline service and how it behaves when submission fails. It covers the request (endpoint, auth, trace id, body), idempotency and retry, and — most importantly — the failure posture: the Recorder is **fail-open** (it never breaks the customer's build), made safe by reconciliation so that a dropped record surfaces as **unknown**, not as **clean**. This spec resolves founder issue #6 (never break the build / submission durability) and defers credential handling to [authentication](authentication.md). The pipeline-side contract it calls is [idora-pipeline api-contract](../../idora-pipeline/specs/api-contract.md).

**Status**: Draft

**Codebase mapping**: `ci-tracer/` (submission client), `action/stop.js`

**Related specs**: [architecture](architecture.md), [run-record](run-record.md), [authentication](authentication.md), [idora-pipeline api-contract](../../idora-pipeline/specs/api-contract.md)

---

## 1. Purpose

Deliver each assembled RunRecord to the pipeline reliably enough to be trustworthy, without ever making the Recorder a reason a customer's CI job fails. These two goals are in tension; Section 5 resolves it.

---

## 2. The Request

The Recorder calls the pipeline's single ingestion endpoint (see [idora-pipeline api-contract](../../idora-pipeline/specs/api-contract.md) §2).

| Aspect | Value |
|--------|-------|
| Method / path | `POST /pipeline/process` |
| `Authorization` | `Bearer <token-for-recorder>` (see [authentication](authentication.md)) |
| `Content-Type` | `application/json` |
| `X-Trace-Id` | Recorder-generated per submission, for log correlation across Recorder and pipeline. |
| Body | `{ "runRecord": { ... }, "metadata": { ... } }` (see [run-record](run-record.md)). |
| Success | `200` with `{ receiptId, schemaVersion, type, graphWritesCompleted, traceId }`. |
| Timeout | Client timeout floor 30s (Graph Write is the slowest stage; per api-contract §2.5). |

---

## 3. Idempotency & Retry

`POST /pipeline/process` is idempotent for the same `runRecord` (the receipt ID is content-addressed; MERGE dedupes). The Recorder MAY therefore retry safely:

| Rule | Decision |
|------|----------|
| Retry on | Connection/read timeout, 5xx, `503` from Graph Write (the response still carries `receiptId`). |
| Do not retry on | `400`/`422` (malformed/invalid record — a Recorder bug; log and reconcile, do not loop). |
| Backoff | Bounded exponential backoff, capped attempts within the job's `post` window. |
| Identity | No idempotency key needed; the content-addressed `receiptId` is the dedupe key. |

---

## 4. When Submission Happens

Submission runs after assembly, at operation boundaries and/or in the Action `post` phase — but always **while the runner still exists**, so retries can happen in-job. The Recorder MUST NOT defer submission to an out-of-band process that runs after the workspace is gone (it could no longer prove or even re-read content; see [content-hashing](content-hashing.md) §2.3).

---

## 5. Failure Posture: Fail-Open + Reconciliation (#6)

### 5.1 Posture (product)

The Recorder is **fail-open**: if submission ultimately fails (after retries), the Recorder MUST NOT fail the CI step. A recorder defect or pipeline outage turning into a customer CI outage is unacceptable for an observability add-on.

> Committed resolution (#6): **fail-open**. (The alternative, fail-closed, trades a CI outage for guaranteed capture and is explicitly rejected as the default.)

### 5.2 The safeguard (the load-bearing half)

Fail-open is only safe if a dropped record is **detectable**. Otherwise a missing receipt is indistinguishable from "no operation happened", so missing data reads as *absence of risk* — a silent clean over an invisible gap (the same false-clean failure class as observation #5/#7).

| Mechanism | Statement |
|-----------|-----------|
| Expected-vs-submitted reconciliation | At job end the Recorder knows how many operations it observed and how many it successfully submitted. A shortfall is reported (job log + a reconciliation signal) as **unknown**, not silently swallowed. |
| Heartbeat | The Recorder emits a start/stop heartbeat so a downstream consumer can tell "Recorder ran and submitted N" from "Recorder never ran" — the latter must read as unknown coverage, not as a clean build. |
| Local durability | Assembled-but-unsubmitted records are buffered on disk during the `post` window so a transient pipeline blip does not lose them before retries are exhausted. |

> Committed resolution (#6, safeguard): fail-open is paired with **reconciliation + heartbeat** so a gap is always observable as unknown. A fail-open Recorder without this safeguard is not acceptable.

---

## 6. Error Codes

| Code | Condition |
|------|-----------|
| `SUB_001` | Pipeline unreachable / timeout after retries; record buffered, gap reconciled as unknown (fail-open). |
| `SUB_002` | `401` from pipeline (bad/expired token); see [authentication](authentication.md), reconciled as unknown. |
| `SUB_003` | `400`/`422` (invalid record); not retried, logged as a Recorder defect. |
| `SUB_004` | Reconciliation shortfall: observed operations exceed submitted receipts; reported as unknown coverage. |
