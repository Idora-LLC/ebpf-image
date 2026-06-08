# Recorder - CI Adapter

This spec defines the CI-adapter boundary that makes the Recorder portable. The observe/hash/assemble/submit core is platform-agnostic; everything that differs between GitHub Actions, GitLab CI, Jenkins, and self-hosted runners is isolated behind a thin adapter that the core depends on through one interface. This spec defines that interface (what the core asks the adapter for), the authoritative-commit rule including the PR merge-vs-head pitfall, how `repo`, operation `type` hints, and deploy target are sourced, the env-allowlist hand-off, and the GitHub Actions adapter as the first concrete implementation. GitHub Actions is the only supported platform today; GitLab/Jenkins are future adapters the interface is designed to accommodate. Resolves #3 (authoritative commit), the sourcing half of #4 and #9, and the env hand-off half of #8.

**Status**: Draft

**Codebase mapping**: `ci-tracer/src/adapter/`, `action/`

**Related specs**: [architecture](architecture.md), [observation](observation.md), [run-record](run-record.md), [security](security.md), [deployment](deployment.md)

---

## 1. Purpose

Keep the Recorder core unaware of any specific CI system. The core never reads `GITHUB_*` (or `CI_*`) variables directly; it asks the adapter. Supporting a new CI platform is implementing a new adapter against this interface — no core change (see [architecture](architecture.md) §3).

---

## 2. The Adapter Interface

The core depends on a small, stable interface. Conceptually:

```
trait CiAdapter {
    fn repo(&self) -> String;                 // §3
    fn commit(&self) -> Result<String>;       // §4 (canonical, ship SHA)
    fn type_hint(&self, op: &Operation) -> Option<RunRecordType>;  // §5
    fn deploy_target(&self, op: &Operation) -> Option<String>;     // §5
    fn env_allowlist(&self) -> EnvAllowlist;   // §6
    fn run_identity(&self) -> RunIdentity;     // run_id, run_attempt (sidecar)
    fn launch(&self) -> LaunchPlan;            // how the agent is started (see deployment)
}
```

| Concern | Owned by adapter | Owned by core |
|---------|------------------|---------------|
| `repo`, `commit`, `type` hint, deploy target, run identity | yes | no |
| env allowlist policy | adapter supplies the set | core applies it |
| Observe / hash / assemble / submit | no | yes |

Only the adapter-sourced fields differ across CI systems (see [run-record](run-record.md) §7).

---

## 3. `repo`

The adapter returns the repository identifier in the canonical form the verification side uses (so the `:Repo`/`:File` join holds). On GitHub Actions this is `GITHUB_REPOSITORY` (e.g. `owner/name`). The value MUST match what WIT records as `repo`; mismatches fork the `:Repo` node (see [run-record](run-record.md) §3).

---

## 4. Authoritative Commit (#3)

`commit` is half the join key, and the easiest one to get silently wrong.

| Rule | Decision |
|------|----------|
| Ship SHA, not merge SHA | On pull requests GitHub checks out a synthetic **merge commit**; `GITHUB_SHA` is that merge SHA, **not** the head that shipped. The adapter MUST resolve the head SHA (e.g. `github.event.pull_request.head.sha` / the PR head ref), not the merge SHA. |
| Push builds | `GITHUB_SHA` is correct. |
| Fallback | `git rev-parse HEAD` in `working_directory` only when no CI variable is authoritative. |
| Same form as verification | The string MUST match what WIT records as `commit`. |

> Decided (#3): the canonical commit is the **head SHA that shipped**. On `pull_request` the adapter resolves the PR **head** SHA (`github.event.pull_request.head.sha`), never the synthetic merge SHA in `GITHUB_SHA`; on push it is `GITHUB_SHA`; on merge-queue it is the queued head SHA. The Recorder and the verification side MUST capture this identical string.

---

## 5. `type` Hint & Deploy Target (#4, #9)

### 5.1 `type` hint (#4)

The adapter is the **authoritative** source of operation `type`, ahead of the whitelist tag and heuristics (see [observation](observation.md) §5). On GitHub Actions the hint comes from an explicit action input or the workflow step name mapped to `build`/`test`/`deploy`.

### 5.2 Deploy target (#9)

The deploy destination (`prod` / `staging`) is not observable from file I/O, so the adapter supplies it (e.g. from the deployment environment name / an action input) and the core carries it as `deploy_target` on the sidecar (see [run-record](run-record.md) §5.2).

> Decided (#9, per recommendation): deploy target is **adapter-sourced**, sidecar-carried, and **optional**. How the pipeline exposes it (Receipt property / exposure view) is a pipeline-side change (see [run-record](run-record.md) §5.2).

---

## 6. Env Allowlist Hand-Off (#8)

The adapter supplies the **allowlist** of environment keys the core may capture; the core never captures env by denylist (see [security](security.md) §3). This keeps platform-specific secret/volatile key knowledge (`GITHUB_TOKEN`, `ACTIONS_*`, runner temp paths) in the adapter, and keeps the capture fail-closed: an unknown custom key is dropped, not leaked.

---

## 7. Run Identity

The adapter exposes `run_id` and `run_attempt` (on GitHub: `GITHUB_RUN_ID`, `GITHUB_RUN_ATTEMPT`) for the re-deploy event identity carried on the sidecar (see [run-record](run-record.md) §5.1). These are deliberately **not** part of the hashed record (the pipeline strips them anyway).

---

## 8. GitHub Actions Adapter (today)

The concrete adapter for the supported platform.

| Field | GitHub source |
|-------|---------------|
| `repo` | `GITHUB_REPOSITORY` |
| `commit` | PR head SHA on `pull_request`; else `GITHUB_SHA` |
| `type` hint | action input / step name mapping |
| `deploy_target` | deployment environment name / action input |
| `run_id` / `run_attempt` | `GITHUB_RUN_ID` / `GITHUB_RUN_ATTEMPT` |
| env allowlist | GitHub-specific deny set excluded; project keys allowed |
| launch | composite Action `pre`/`post` (see [deployment](deployment.md)) |

---

## 9. Future Adapters

| Platform | Commit var | Repo var | Notes |
|----------|-----------|----------|-------|
| GitLab CI | `CI_COMMIT_SHA` (mind MR vs. pipeline SHA) | `CI_PROJECT_PATH` | Same merge-vs-head class of pitfall as #3. |
| Jenkins | `GIT_COMMIT` | job/SCM config | Launch via pipeline step / agent setup. |
| Self-hosted | per-runner | per-runner | Enables higher-fidelity capture (BPF-LSM) — see [content-hashing](content-hashing.md) §4, [deployment](deployment.md). |

The interface (Section 2) is fixed; each future platform is a new implementation only.
