# Recorder - Authentication

This spec defines how the Recorder authenticates to the Idora Pipeline service: the credential it presents, where that credential lives in the customer's CI, how it is injected and rotated, and the least-privilege posture around it. The Recorder is the caller side of the pipeline's bearer-token model; the server side (allow-list, validation, `401` semantics) is defined in [idora-pipeline authentication](../../idora-pipeline/specs/authentication.md), and this spec mirrors it from the caller's perspective. Trust-surface concerns broader than the token (privileged agent, secret capture, binary hardening) live in [security](security.md).

**Status**: Draft

**Codebase mapping**: `ci-tracer/` (submission auth), `action/` (token input)

**Related specs**: [idora-pipeline authentication](../../idora-pipeline/specs/authentication.md), [submission](submission.md), [security](security.md), [deployment](deployment.md)

---

## 1. Model

The Recorder authenticates a **service-to-service** call. It is one of exactly two callers of the pipeline (the other is WIT Core); the pipeline does not distinguish callers at the auth layer — a valid token from the allow-list grants `POST /pipeline/process` (see [idora-pipeline authentication](../../idora-pipeline/specs/authentication.md) §1). There is no user identity, no OAuth, no token-issuance API. The token is an operational secret.

---

## 2. Credential

| Property | Value |
|----------|-------|
| Type | Opaque bearer token (not a JWT, not signed), `token-for-recorder`. |
| Header | `Authorization: Bearer <token>` on every `POST /pipeline/process`. |
| Allow-list | The token is one entry in the pipeline's `PIPELINE_SERVICE_TOKENS` env allow-list. |
| Lifetime | Long-lived; rotated by updating both sides (Section 4). |

The token is opaque to the Recorder too: it does not parse or inspect it, only attaches it.

---

## 3. Storage & Injection on the Recorder Side

The Recorder runs in customer CI, so the token must come from the **CI secret store**, never be baked into the image, the Action, or any log.

| Aspect | Decision |
|--------|----------|
| Source | A CI secret (e.g. GitHub Actions encrypted secret) supplied to the recorder step as an input/env var. |
| In-memory only | The Recorder holds the token in memory for the job's lifetime; it is never written to disk or the trace output. |
| Never logged | The token MUST NOT appear in logs, error envelopes, or the reconciliation signal. It is also excluded from any captured `environment` (the env allowlist must not include it — see [security](security.md) §3). |
| Not in the RunRecord | The token is transport credential only; it never enters the RunRecord or sidecar. |

> Decided: the recorder token is a **CI secret injected at runtime**, held in memory, never persisted or captured. Tenancy is **per-customer tokens** (Section 5).

---

## 4. Rotation

Rotation mirrors the pipeline's restart-based model (see [idora-pipeline authentication](../../idora-pipeline/specs/authentication.md) §5): the new token is added to the pipeline's allow-list, the CI secret is updated, and the old token is removed after the cutover. Because each call is independent and stateless, no Recorder-side session state needs draining. The pipeline allow-list holding multiple tokens lets the recorder token rotate independently of WIT Core's.

---

## 5. Least Privilege & Token Scope

| Concern | Posture |
|---------|---------|
| One capability | The token grants exactly one thing: submitting RunRecords. It is not a graph-read or admin credential. |
| Blast radius | A leaked recorder token lets an attacker submit RunRecords (write evidence), not read the graph. Still sensitive; treat as a secret. |
| Per-customer tokens | Recommended: a distinct `token-for-recorder` per customer/tenant so one leak is contained and individually revocable. |

> Decided (#8, token scope): use **per-customer recorder tokens**, so a leak is contained and individually revocable without disrupting other customers.

---

## 6. Failure Behavior

A `401` (missing/expired/unknown token) is a submission failure handled fail-open with reconciliation (see [submission](submission.md) §5): the record is not delivered, the gap is surfaced as **unknown**, and the CI step is not failed. A persistent `401` is an operational alert (token misconfiguration), not a silent drop.
