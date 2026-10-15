# hydradb-operator - Work Plan

## TL;DR (For humans)
<!-- Fill this LAST, after the detailed plan below is written, so it summarizes the REAL plan. -->
<!-- Plain English for a non-engineer: NO file paths, NO todo numbers, NO wave/agent/tool names. -->

**Who this is for and what changes for them:** A platform engineer who today has to hand-edit HydraDB's Helm chart and hand-build the list of nodes the database should talk to will instead apply three small Kubernetes files and get a running graph database - one that maintains its own node list, reports its own health, and installs itself through the cluster's existing GitOps pipeline.

**What you'll get:** A Kubernetes operator for HydraDB, written in Rust: three custom resource types (the cluster, a pool of query nodes, and the indexer workers), a controller that creates and maintains every workload behind them, a `kubectl hydradb` helper for status/endpoints/logs/health, automatically generated CRDs, permissions and docs with drift checks that fail the build if they fall out of sync, a signed container image published to two registries, and a Helm chart written to HelmForge's published conventions - delivered through the platform's existing Gitea, Harbor, Renovate and Flux rails and mirrored to GitHub.

**Why this approach:** The operator reproduces exactly the topology HydraDB's own chart already deploys, so nothing about how the database runs changes - only who owns the objects and how they are kept correct. And it follows the engineering discipline of an existing, proven Rust operator (kube-rs, one shared watch configuration, generated CRDs/RBAC, hand-rolled status conditions) rather than inventing new conventions for a first-of-its-kind artifact.

**What it will NOT do:** No backups, snapshots or restores. No autoscaling, dashboards, alerts or UI. No multi-cluster federation or multi-cell graphs. And it does not modify HydraDB itself.

**Effort:** XL
<!-- Effort is exactly ONE band, never hours/days. Quick = single edit, minutes of agent work; Short = one focused change, a few files; Medium = multi-file feature in one session; Large = several waves, one long session; XL = multi-session or architectural work. A written duration is rewritten to a band. -->
**Risk:** Medium-High - the only current HydraDB image is private, the webhook route from GitHub into the cluster is new public surface, and HelmForge has never shipped an operator before so its conventions are being set here rather than copied.
**Decisions to sanity-check:** the API is split into three resource types rather than one; object storage is either configured explicitly or discovered from a Rook bucket claim; deleting a cluster defaults to keeping your data (`Retain`); and the GitHub side uses a personal access token now, wired so a GitHub App can replace it later.

Your next move: run the plan - `$ulw-execute hydradb-operator`. Full execution detail follows below.

---

> TL;DR (machine): effort XL | risk Medium-High | 60 implementation todos across 10 waves + F1-F4 final verification | deliverables: Rust kube-rs operator (api/controller/cli/xtask/e2e crates), three CRDs under hydradb.helmforge.dev v1alpha1, helmforge-conforming chart, dual-registry signed image, talos-stack + GitHub platform rails, kind e2e

## Scope
### Affected user and ideal state
<!-- From the draft ledger: who this touches and how they use the result, then one IS row per property with its reason, then one GAP row per difference with its reason. Every todo below closes a GAP row; ## Success criteria proves every IS row. -->
**Affected user:** ten people/roles, and how each uses the result:
1. **Platform engineer** - declares a graph. Today: hand-edits the upstream HydraDB chart's `values.yaml`, hand-builds the Bolt routing table, and owns every child object. After: applies three CRs and gets a working graph.
2. **Application developer / AI agent** - consumes the graph over Bolt 7687 or HTTPS 8443. Today: no stable endpoint contract, and the only image is private. After: a Service name, a token, and a pinned image.
3. **GitOps operator** (Flux `wait`/kstatus, Argo CD health, `kubectl wait`) - reads readiness. Today: no CRDs, so nothing to wait on. After: standard conditions on every CR.
4. **SRE / on-call** - owns `deletionPolicy`, the opt-in ServiceMonitor, and the fact that no dashboards ship. Today: undocumented. After: explicit `Retain` semantics and a documented operations guide.
5. **Operator maintainer** - upgrades CRDs and the operator. Today: n/a. After: an xtask drift gate, an ADR'd CRD upgrade path, and a `doctor` command.
6. **HelmForge gatekeeper / signing-key holder** - reviews the first operator-style artifact and verifies GPG + Cosign. Today: helmforge has zero operator artifacts and no Rust conventions. After: a conforming chart plus the conventions written down.
7. **Registry + org admin** - approves the `oss` and `kubexa` orgs and mints the PAT scopes. Today: neither exists. After: an explicit scopes list and Tofu-managed orgs, teams and webhooks.
8. **talos-stack platform owner** - installs it. Today: no exception path. After: install through the existing Gitea/Harbor/Renovate/Flux rails like any other product.
9. **GitHub-side maintainer** - sees the mirror build and the protected `main`. Today: nothing on GitHub. After: a push-to-`main` pipeline and a ruleset.
10. **End user of the graph** - queries it. Today: nothing to query. After: a graph that is up, reachable and observable.

| Row | Statement | Reason |
| --- | --- | --- |
| IS-1 | Three CRDs - `GraphCluster`, `GraphNodePool`, `GraphIndexer` - one group, v1alpha1, namespaced; invalid combinations unrepresentable in the type system | A platform engineer must not be able to express a graph the operand cannot run; compile-time exhaustiveness is cheaper than runtime failure |
| IS-2 | Applying a `GraphCluster` plus its pool/indexer CRs produces a working graph: nodes serve Bolt 7687 and HTTPS 8443, indexer workers publish CSC generations, no manual step after apply | The operand has no bootstrap ritual and needs no leader, so the operator is the only thing that can assemble the topology |
| IS-3 | Every CR exposes standard `metav1.Condition`s (`Ready`, `Reconciling`, `Stalled`) plus `status.observedGeneration` | Flux `wait`, Argo health and `kubectl wait` all key off exactly this, so GitOps works with no custom logic |
| IS-4 | The operator recomputes `GRAPH_BOLT_NODE_ADDRESSES` on every reconcile from live pool membership | Today the upstream chart hand-builds it from `replicaCount`, so a restarted or rescheduled node leaves the routing table wrong |
| IS-5 | Object storage is pluggable: any S3-compatible endpoint via explicit config, OR auto-discovered from a Rook `ObjectBucketClaim` in the operand namespace | Object storage is the durable source of truth, and every environment supplies it differently |
| IS-6 | Node data-cache storage is pluggable: `emptyDir` or a PVC on any StorageClass (`ceph-block`, `ceph-cephfs`, EBS `gp3`, local NVMe) | The cache is disposable but its latency class drives query performance, so it must be a first-class choice |
| IS-7 | The HydraDB image is user-overridable via a CR field, with a sane default pinned by digest | Users must be able to point at their own build; a private staging ECR must never be the default |
| IS-8 | The operator installs from Harbor `oss` (mirrored to GHCR) through a Flux `OCIRepository` + `HelmRelease`, with CRD upgrades handled safely | Helm's `crds/` are install-only and never upgraded, so the upgrade path has to be designed, not assumed |
| IS-9 | The repo lives at `git.kubexa.tech/oss/hydradb-operator`; CI builds, tests and pushes images on push to `main` through the existing kaniko Pipelines-as-Code, and Renovate keeps dependencies current | The platform owner must not need a bespoke pipeline; `cargo` is already an enabled Renovate manager |
| IS-10 | `kubectl hydradb` reports status, endpoints and logs, and runs a `doctor` check that exits non-zero on failure | Support and debugging must not require reading CRs by hand |
| IS-11 | One CRD surface spans environments: the same CRs deploy on private cloud (Rook), AWS (EBS + S3) and others; only values differ | A single API is what makes this portable rather than three forks |
| IS-12 | The Helm chart conforms to helmforge conventions from day one (`values.schema.json`, `tests/`, `ci/`, `examples/`, `docs/`, `DESIGN.md`, Chart.yaml annotations, GPG + Cosign) | This is helmforge's first operator; retrofitting conventions later would be a rewrite |
| GAP-1 | No CRDs, no operator and no chart exist for HydraDB - the working directory is empty | Nothing above is possible until the operator exists (closes in W1-W6) |
| GAP-2 | The only HydraDB image is a private ECR staging repo, unusable by third parties | Without a public pinned image no one outside this cluster can install it (closes in W6, W7) |
| GAP-3 | No Gitea `oss` org, no repository, no PaC webhook | The code cannot be built or tested where the platform expects it (closes in W7) |
| GAP-4 | No Harbor `oss` project, robots, retention or immutable-tag rule | Images cannot be pushed or retained (closes in W7) |
| GAP-5 | No kaniko credential secret covering Harbor + GHCR | The dual-registry push has no credentials, and no precedent exists in the repo (closes in W6, W7) |
| GAP-6 | No Renovate job covering the new org | Dependencies would drift silently (closes in W7) |
| GAP-7 | No Flux delivery (GitRepository / Kustomization / OCIRepository / HelmRelease) | Nothing reaches the cluster by GitOps (closes in W7) |
| GAP-8 | `GRAPH_BOLT_NODE_ADDRESSES` is hand-built by the upstream chart's `_helpers.tpl` from `replicaCount` | The routing table is wrong after any reschedule (closes in W3) |
| GAP-9 | No codegen, no RBAC derivation and no drift gate | CRDs, RBAC and docs would drift from the Rust types (closes in W2, W5) |
| GAP-10 | No CLI, no e2e harness and no lint gates for a Rust repo | Debugging and regression protection are absent (closes in W4, W9) |
| GAP-11 | Upstream ships no backup/restore, no upgrade procedure, no dashboards and no alert rules | Explicit non-goals, documented so nobody assumes them (closes in W10 as documentation, not features) |
| GAP-12 | No GitHub org, repository, ruleset, CI pipeline or webhook for the mirror | The GitHub mirror is part of the delivery contract (closes in W8) |
| GAP-13 | The PaC controller is cluster-internal only, so github.com cannot reach it | No GitHub webhook can ever arrive without a public route (closes in W8) |
| GAP-14 | No GitHub PAT or documented token scopes in OpenBao | Neither Tofu nor the GHCR push can authenticate (closes in W8) |
| GAP-15 | HelmForge has zero operator artifacts, so no Rust CI / CRD-lifecycle / image-publishing convention exists | The first operator sets the convention, so it must be written down (closes in W6, W10) |

**Extrinsic constraint sweep** (each axis is explored, defaulted, or asked - none are asked):
- *Budget / paid spend* - **defaulted**: reuse existing cluster storage (Rook RGW buckets + existing StorageClasses) and existing Harbor; no new paid service is introduced. GHCR is free for the mirror.
- *Expected scale / capacity* - **defaulted**: one cluster sized for a handful of graphs and up to ~10 node replicas plus ~5 indexer replicas; no HPA/VPA in this cut, so capacity is explicit.
- *Licensing* - **explored + defaulted**: operator and chart are Apache-2.0 (matching the operand, which is Apache-2.0); `cargo-deny` enforces the dependency licence set in CI (todo 3).
- *Target audience / compliance* - **defaulted**: the audience is the platform engineer plus downstream app/agent consumers; CycloneDX SBOM + `grype` + `cosign` signatures are produced (todo 41) and **no** compliance certification is claimed.

### Must have
- The Rust workspace with `crates/{api,controller,cli,xtask,e2e}`, pinned to the operator's MSRV 1.89 / edition 2024.
- The three CRDs under group `hydradb.helmforge.dev`, v1alpha1, namespaced, with Unknown-fallback enums and cross-field validation, no webhook.
- A controller that reconciles all three kinds into the full child set (ConfigMap, Secret, five Services, StatefulSet, Deployment, PDB, NetworkPolicy, opt-in ServiceMonitor), recomputes `GRAPH_BOLT_NODE_ADDRESSES` every reconcile, writes kstatus conditions + `observedGeneration`, and honours `deletionPolicy` (default `Retain`) via a finalizer.
- The `kubectl hydradb` plugin (`status`, `endpoints`, `logs`, `doctor`, `scale`, `version`) with five output modes.
- `cargo xtask gen-crds|gen-rbac|gen-docs|gen-all [--check]` writing CRDs to both `deploy/crds/` and the chart's `crds/`, plus check-wiring and check-phases drift gates.
- A multi-stage distroless Dockerfile, the kaniko PaC image pipeline (dual tag `v<semver>` + `<sha>`, dual registry Harbor + GHCR), SBOM and cosign signing.
- A helmforge-conforming chart (Chart.yaml annotations, values.schema.json, tests/, ci/, examples/, docs/, README, DESIGN.md, icon).
- A published public HydraDB operand image with the default pinned by digest.
- The talos-stack rails: `terraform/environments/harbor/oss.tf`, `terraform/environments/gitea/oss.tf`, the kaniko credential ExternalSecret, the Renovate job + config, and the Flux GitRepository/Kustomization/OCIRepository/HelmRelease.
- The GitHub rails: `terraform/environments/github/` (org adoption + import, repo, ruleset, teams, GHCR secret, webhook), the `Repository` CR, the `github-webhook-config` ExternalSecret, the `pac.kubexa.tech` public route, and the GitHub push-to-`main` image pipeline.
- The e2e harness (kind + Helm + full scenario), the mise task surface, the PaC Rust test gate, and the docs/ADR set.

### Must NOT have (guardrails, anti-slop, scope boundaries)
- No backup/restore, snapshot, or restore CRDs - the operand has no backup surface and the plan does not invent one.
- No `helm template` inside the controller - the controller owns child objects directly via SSA.
- No admission webhook in this cut.
- No multi-cell, no federation, no cross-cluster placement (single cell only, enforced by validation).
- No HPA/VPA, no Grafana dashboards, no alert rules, no `CronJob`s.
- No UI.
- No VolSync migration path.
- No modification of the upstream HydraDB repository.
- No forked or wrapper images (helmforge forbids them); the operand image is built unmodified from upstream source.
- No `:latest` tags anywhere (images, chart, or pipeline destinations).
- No Tofu management of the GitHub org or the Gitea/GitHub repository objects (both are created out-of-band).
- No GitHub App / Checks API in this cut (the PAT path is wired so an App can be added later).
- No `.github/workflows/` (so no `workflow` PAT scope).
- No org-level GitHub webhook.
- No changes to the five existing `Repository` CRs or to PaC's existing Gitea behaviour.
- No new paid service, no compliance certification claim.
- No documentation of a feature this plan excludes.

## Verification strategy
> Zero human intervention - all verification is agent-executed.
- Test decision: **TDD for pure logic** (`crates/api`, `crates/xtask`, the pure child builders and the scheduling/validation functions) and **tests-after for reconcilers**, using `cargo test` with `tower-test` + `ApiServerVerifier` unit tests and `envtest 0.3` integration tests in CI, `helm unittest` for the chart, and a `kind`-based `crates/e2e` scenario run locally via `mise run test-e2e`. Signing is verified with `cosign verify`; generated artifacts are verified with `cargo xtask gen-all --check`.
- Evidence: <attemptDir>/task-<N>-hydradb-operator.<ext> (attemptDir = currentAttemptDir from the ulw-loop status operation - on Senpi the eval SDK import, const { agentToolkit } = await import(`${env("OMO_AGENT_TOOLKIT_SDK_ROOT")}/sdk.js`) then agentToolkit.status(), on Codex the omo-agent-toolkit CLI, .omo/evidence/ulw/<session>/<goalId>/a<attempt>; outside ulw-loop use .omo/evidence/)

## Execution strategy
### Parallel execution waves
> Target 5-8 todos per wave. Fewer than 3 (except the final) means you under-split.

- **Wave 1 - workspace foundation** (todos 1-5): Cargo workspace + five crate skeletons; the pinned dependency set; toolchain/fmt/lint/deny gates + mise; repository governance; e2e skeleton.
- **Wave 2 - the API crate and the three CRDs** (todos 6-12): module layout + shared primitives; the Unknown-fallback macro; `GraphCluster`; `GraphNodePool`; `GraphIndexer`; schema attributes + validation; the api test suite.
- **Wave 3 - the controller** (todos 13-26): skeleton + shutdown; `watcher_config`; error classification; conditions; SSA/owner refs; the `GraphCluster` reconciler (config/auth/object store, then Services + routing table); `GraphNodePool` StatefulSet; `GraphIndexer` Deployment; PDB/NetworkPolicy/ServiceMonitor; finalizer + deletionPolicy; status writers; metrics/tracing; the controller test suite.
- **Wave 4 - the CLI** (todos 27-30): skeleton + clap; status/endpoints/logs; doctor; scale + output modes + tests.
- **Wave 5 - xtask codegen** (todos 31-34): gen-crds; gen-rbac; gen-docs + gen-all; check-drift/check-wiring/check-phases.
- **Wave 6 - packaging** (todos 35-41): Dockerfile; chart skeleton + schema; chart templates; installScope; chart tests/ci/examples; the kaniko dual-tag dual-registry pipeline; SBOM + cosign.
- **Wave 7 - talos-stack rails** (todos 42-48): Harbor `oss.tf`; Gitea `oss.tf`; the kaniko credential ExternalSecret; the Renovate job + config; Flux delivery; the dual-registry SPIKE; the public operand image + default digest.
- **Wave 8 - GitHub rails** (todos 49-53): the `terraform/environments/github/` root; repo + ruleset + teams + GHCR secret; the PaC webhook + `Repository` CR + config ExternalSecret; the `pac.kubexa.tech` route; the GitHub push-to-`main` pipeline.
- **Wave 9 - end-to-end verification and CI** (todos 54-57): the kind e2e scenario; the mise/verify runner; the PaC Rust test gate; the full release-path evidence run.
- **Wave 10 - documentation** (todos 58-60): docs/ guides; ADRs 0001-0005; the chart README + DESIGN.md.
- **Final verification wave** (F1-F4): plan compliance, code quality, real manual QA, ideal-state fidelity - all in parallel, all must APPROVE.

### Dependency matrix
| Todo | Depends on | Blocks | Can parallelize with |
| --- | --- | --- | --- |
| 1 | - | 2-5, 13, 27, 35, 42, 49 | - |
| 2 | 1 | 3, 6, 13 | 3-5 |
| 3 | 2 | 55, 56 | 4, 5 |
| 4 | 1 | 60 | 2, 3, 5 |
| 5 | 1 | 54 | 2-4 |
| 6 | 2 | 7-12, 31 | 27, 35 |
| 7 | 6 | 8-12 | - |
| 8 | 7 | 11, 12, 18 | 9, 10 |
| 9 | 7 | 11, 12, 20 | 8, 10 |
| 10 | 7 | 11, 12, 21 | 8, 9 |
| 11 | 8-10 | 12, 18-21 | - |
| 12 | 11 | 31, 54 | - |
| 13 | 1-2 | 14-26 | 27, 35, 42 |
| 14 | 13 | 18-24 | 15-17, 25 |
| 15 | 13 | 18-24 | 14, 16, 17, 25 |
| 16 | 13 | 18-24 | 14, 15, 17, 25 |
| 17 | 13 | 18-24 | 14-16, 25 |
| 18 | 13-17 | 19-24 | - |
| 19 | 18 | 20-24 | - |
| 20 | 18-19 | 22-24 | 21 |
| 21 | 18 | 22-24 | 20 |
| 22 | 18-21 | 24 | - |
| 23 | 13-17 | 24 | - |
| 24 | 18-23 | 26 | - |
| 25 | 13 | 26 | 14-17 |
| 26 | 13-25 | 54 | - |
| 27 | 1-2 | 28-30 | 13, 35, 42 |
| 28 | 27 | 30 | 29 |
| 29 | 27-28 | 30 | - |
| 30 | 27-29 | 56 | - |
| 31 | 6-12 | 32-34, 36 | - |
| 32 | 13 | 34, 37 | 31, 33 |
| 33 | 31-32 | 34, 58 | - |
| 34 | 31-33 | 55, 56 | - |
| 35 | 13 | 40, 41 | 36, 42, 49 |
| 36 | 31 | 37-39 | 35, 42, 49 |
| 37 | 32, 36 | 38, 39 | - |
| 38 | 13, 37 | 39 | - |
| 39 | 36-38 | 41, 55, 56 | 40 |
| 40 | 35 | 41, 47, 48 | 39 |
| 41 | 39-40 | 60 | - |
| 42 | - | 44, 46 | 43, 49 |
| 43 | - | 44, 45 | 42, 49 |
| 44 | 42-43 | 45, 47 | - |
| 45 | 43 | - | 46, 47 |
| 46 | 42, 47 | 54, 57 | 45 |
| 47 | 40, 44 | 46, 53, 59 | - |
| 48 | 40 | 46, 54, 58 | - |
| 49 | - | 50-52 | 42, 43 |
| 50 | 49 | 51-53 | - |
| 51 | 49-50 | 53 | 52 |
| 52 | 49 | 53 | 51 |
| 53 | 47, 51 | 54, 57 | - |
| 54 | 5, 26, 46, 53 | 55, 57 | - |
| 55 | 3, 34, 39 | 56, 57 | - |
| 56 | 34, 39, 55 | 57 | - |
| 57 | 46, 48, 53, 56 | 58 | - |
| 58 | 33, 48 | 59, 60 | - |
| 59 | 47, 58 | 60 | - |
| 60 | 58-59 | - | - |
| F1-F4 | ALL todos | - | each other |


## Todos
> Implementation + Test = ONE todo. Never separate.
<!-- APPEND TASK BATCHES BELOW THIS LINE WITH edit/apply_patch - never rewrite the headers above. -->
### Wave 1 - Workspace foundation (C1-C5 skeleton)

- [x] 1. Root Cargo workspace + five crate skeletons
  What to do / Must NOT do: Create at `/home/aidas/repos/HydraDB-Operator` a root `Cargo.toml` with `[workspace]` `resolver = "2"` and `members = ["crates/api", "crates/controller", "crates/cli", "crates/xtask", "crates/e2e"]`, plus `[workspace.package]` carrying `edition = "2024"`, `rust-version = "1.89"`, `version = "0.1.0"`, `license = "Apache-2.0"`, `repository = "https://git.kubexa.tech/oss/hydradb-operator"`. Create each member's own `Cargo.toml` and `src/`: `api` = library `hydradb-operator-api`; `controller` = binary `hydradb-operator`; `cli` = binary `hydradb`; `xtask` = binary `xtask`; `e2e` = library + tests. Must NOT hand-write `Cargo.lock`, must NOT add any sixth crate, must NOT create a `src/` at the repo root.
  Closes: GAP-1
  Parallelization: Wave 1 | Blocked by: none | Blocks: 2,3,4,5,6-12,W3,W4,W5
  Context (executor has NO interview context - be exhaustive): the target repo is currently EMPTY. Mirror the shape of `/home/aidas/.cache/ulw-research/kopiur/Cargo.toml` (workspace + `[workspace.package]` + `[workspace.dependencies]`) and `/home/aidas/.cache/ulw-research/kopiur/crates/`.
  Acceptance criteria (agent-executable): `cargo metadata --format-version 1 --no-deps` exits 0 and its `packages` array has exactly 5 entries with the names above; `cargo build --workspace` exits 0.
  QA scenarios (name the exact tool + invocation): happy: run `cargo metadata --no-deps` and assert 5 workspace members and the exact binary names. failure: delete `crates/cli/Cargo.toml` and assert `cargo build --workspace` exits non-zero, then restore. Evidence `<attemptDir>/task-1-hydradb-operator.txt`
  Commit: Y | chore(workspace): scaffold five-crate cargo workspace
  Recommended task executor category: quick - mechanical file creation verified by one command.

- [x] 2. Pin the workspace dependency set
  What to do / Must NOT do: In the root `Cargo.toml` `[workspace.dependencies]` add exactly these pins: `kube = { version = "4.2.0", default-features = false, features = ["client", "runtime", "derive", "rustls-tls", "ws"] }`; `k8s-openapi = { version = "0.28.0", features = ["v1_33"] }`; `schemars = "1"`; `serde = { version = "1", features = ["derive"] }`; `serde_json = "1"`; `serde_yaml = "0.9"`; `tokio = { version = "1.53", features = ["macros", "rt-multi-thread", "signal"] }`; `thiserror = "2"`; `anyhow = "1"`; `tracing = "0.1"`; `tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }`; `clap = { version = "4.5", features = ["derive", "env"] }`; `chrono = { version = "0.4", features = ["clock", "serde"] }`; `uuid = { version = "1", features = ["v4", "serde"] }`; `futures = "0.3"`; `tower = "0.5"`; `tower-test = "0.4"`; `envtest = "0.1"`; `rand = "0.9"`; `sha2 = "0.10"`. The `api` crate depends on `kube` with ONLY the `derive` feature and on `schemars`/`serde`/`serde_json` - it MUST NOT depend on `k8s-openapi`. The `controller` and `cli` crates own the `k8s-openapi` dependency. Must NOT enable `openssl-tls`, must NOT add `native-tls`, must NOT use schemars 0.8 (it breaks `kube::CustomResource` derive on kube 4.x).
  Closes: GAP-1
  Parallelization: Wave 1 | Blocked by: 1 | Blocks: 6-26
  Context (executor has NO interview context - be exhaustive): version matrix from the draft's Findings section C; kopiur pins `kube 4.0` / `k8s-openapi 0.28` (`v1_33`) / `schemars 1`. kube-rs default features are `config,client,rustls-tls,ring`; `runtime`/`derive`/`ws`/`admission`/`jsonpatch` are opt-in. Mixing schemars 0.8 with kube 4.x breaks derive.
  Acceptance criteria (agent-executable): `cargo build --workspace` exits 0; `cargo tree -p hydradb-operator-api` output contains no `k8s-openapi` line.
  QA scenarios (name the exact tool + invocation): happy: `cargo tree -p hydradb-operator-api | rg -c 'k8s-openapi'` prints `0`. failure: temporarily set the kube pin to `"99.0.0"` and assert `cargo build --workspace` fails, then restore. Evidence `<attemptDir>/task-2-hydradb-operator.txt`
  Commit: Y | chore(deps): pin the kube-rs 4.2 / k8s-openapi 0.28 / schemars 1 stack
  Recommended task executor category: unspecified-high - cross-crate dependency wiring with a correctness constraint (the api crate must stay k8s-openapi-free).

- [x] 3. Toolchain, formatting, lint and dependency gates
  What to do / Must NOT do: Add `rust-toolchain.toml` (channel `"1.89.0"`, components `rustfmt`, `clippy`), `rustfmt.toml`, `clippy.toml`, `deny.toml` (allow Apache-2.0, MIT, BSD-3-Clause, ISC, Unicode-3.0; deny wildcard dependencies), `.dockerignore` (exclude `target/`, `.git/`, `.omo/`), and `mise.toml` defining tasks `fmt` (`cargo fmt --all --check`), `lint` (`cargo clippy --workspace --all-targets -- -D warnings`), `test` (`cargo test --workspace`), `build` (`cargo build --workspace --release`), `check-drift` (`cargo xtask gen-all --check`), `e2e` (`cargo run -p hydradb-operator-e2e`). Must NOT add a `justfile` (the platform convention is `mise`), must NOT lower `-D warnings`.
  Closes: GAP-1, GAP-10
  Parallelization: Wave 1 | Blocked by: 1 | Blocks: W9
  Context (executor has NO interview context - be exhaustive): `/home/aidas/repos/talos-stack` uses `mise` (no justfile) and `lefthook`; kopiur pins its toolchain via `mise` + `.mise/locks` and ships a `justfile`-free task surface.
  Acceptance criteria (agent-executable): `mise run fmt` exits 0 and `mise run lint` exits 0 on the scaffolded workspace; `cargo deny check` exits 0.
  QA scenarios (name the exact tool + invocation): happy: `mise run lint` exits 0. failure: insert an unused `let unused = 1;` into `crates/xtask/src/main.rs` and assert `mise run lint` exits non-zero, then revert. Evidence `<attemptDir>/task-3-hydradb-operator.txt`
  Commit: Y | chore(tooling): add toolchain pin, fmt/clippy/deny gates and mise tasks
  Recommended task executor category: quick - config files verified by running the gates.

- [x] 4. Repository governance files
  What to do / Must NOT do: Create `README.md` (what the operator is, the three CRDs, a quickstart that installs from Harbor `oss`, links to `docs/`), `AGENTS.md` (source of truth for agents: layout, commands, the three-CRD model, the rule that the controller owns child objects directly and NEVER helm-templates, the tag policy `v<semver>`+`<sha>` and never `:latest`), `LICENSE` (the full Apache-2.0 text), `CONTRIBUTING.md`, `SECURITY.md`, `.gitignore` (`target/`, `.omo/evidence/`), `.editorconfig`, `.gitattributes`. Must NOT add a code of conduct, must NOT add `.github/workflows/`, must NOT copy the upstream chart's README.
  Closes: GAP-1
  Parallelization: Wave 1 | Blocked by: 1 | Blocks: W10
  Context (executor has NO interview context - be exhaustive): the operand is Apache-2.0 (`/home/aidas/.cache/ulw-research/hydradb/LICENSE`); helmforge charts are Apache-2.0. The three kinds are `GraphCluster`, `GraphNodePool`, `GraphIndexer` in group `hydradb.helmforge.dev`.
  Acceptance criteria (agent-executable): all eight files exist; `rg -q 'Apache License' LICENSE` exits 0; `rg -q 'GraphCluster' README.md` and `rg -q 'GraphNodePool' README.md` and `rg -q 'GraphIndexer' README.md` all exit 0.
  QA scenarios (name the exact tool + invocation): happy: assert every file exists and README names all three kinds. failure: assert `.github/workflows` does NOT exist. Evidence `<attemptDir>/task-4-hydradb-operator.txt`
  Commit: Y | docs(repo): add README, AGENTS.md, LICENSE and governance files
  Recommended task executor category: writing - prose and licence text.

- [x] 5. e2e harness skeleton
  What to do / Must NOT do: Create `crates/e2e` with `src/lib.rs` exposing a `Harness` that (a) resolves a kubeconfig without hardcoding a path, (b) `try_client()` returns `Ok(None)` instead of panicking when the cluster is unreachable, (c) can create a `kind` cluster named `hydradb-operator-e2e`, (d) can `helm install` the chart from a local path, and (e) tears both down; plus `crates/e2e/tests/smoke.rs` whose tests are `#[ignore]` so CI opts in explicitly. Must NOT require a pre-existing cluster, must NOT add a second test framework.
  Closes: GAP-10
  Parallelization: Wave 1 | Blocked by: 1 | Blocks: W9
  Context (executor has NO interview context - be exhaustive): kopiur's e2e crate drives a Helm-deployed operator inside an ephemeral kind cluster with an isolated kubeconfig and a 420s default timeout (> the 290s watch reconnect window).
  Acceptance criteria (agent-executable): `cargo build -p hydradb-operator-e2e` exits 0; `KUBECONFIG=/nonexistent cargo test -p hydradb-operator-e2e -- --ignored` exits 0 after printing a skip message.
  QA scenarios (name the exact tool + invocation): happy: run the ignored suite with no cluster and assert a skip message plus exit 0. failure: point `KUBECONFIG` at `/nonexistent` and assert `try_client()` returns `Ok(None)` and does not panic. Evidence `<attemptDir>/task-5-hydradb-operator.txt`
  Commit: Y | test(e2e): add kind-based e2e harness skeleton with graceful skip
  Recommended task executor category: unspecified-low - small harness with clear boundaries.

### Wave 2 - API crate: the three CRDs (C1)

- [x] 6. api crate module layout and shared primitives
  What to do / Must NOT do: Create `crates/api/src/lib.rs` plus modules `crd/`, `spec/`, `status/`, `validation.rs`, `constants.rs`. In `constants.rs` define `pub const GROUP: &str = "hydradb.helmforge.dev";` `pub const VERSION: &str = "v1alpha1";` `pub const FIELD_MANAGER: &str = "hydradb.helmforge.dev/controller";` `pub const FINALIZER: &str = "hydradb.helmforge.dev/finalizer";` and every operand env-var name as a constant (`AWS_BUCKET_NAME`, `AWS_DEFAULT_REGION`, `AWS_ENDPOINT`, `AWS_ALLOW_HTTP`, `CLOUD_PROVIDER`, `GRAPH_NAMESPACE`, `GRAPH_ID`, `GRAPH_DATABASE`, `GRAPH_CELL_ID`, `GRAPH_CELLS`, `GRAPH_BOLT_NODE_ADDRESSES`, `RUST_MIN_STACK`, `RUST_LOG`). In `spec/` define shared types `Image { repository: String, tag: Option<String>, digest: Option<String>, pull_policy: Option<String>, pull_secrets: Vec<LocalObjectReference> }`, `ObjectStore { explicit: Option<S3 { endpoint, bucket, region, allow_http, credentials_secret_ref }>, object_bucket_claim: Option<LocalObjectReference> }`, and `CacheStorage` as a discriminated union of `EmptyDir { size_limit: Option<String> }` and `PersistentVolume { size, storage_class_name: Option<String>, access_modes: Vec<String> }`. Must NOT duplicate these types per CRD, must NOT add a `Default` impl that silently picks a backend for `ObjectStore` or `CacheStorage`.
  Closes: GAP-9
  Parallelization: Wave 2 | Blocked by: 2 | Blocks: 7-12,W3
  Context (executor has NO interview context - be exhaustive): the operand's env contract and the upstream chart's `values.yaml` are the source of truth (`/home/aidas/.cache/ulw-research/hydradb/charts/hydradb/values.yaml`, `templates/configmap.yaml`, `templates/external-secret.yaml`, `templates/auth-secret.yaml`). Mirror kopiur's `crates/api` layout (`/home/aidas/.cache/ulw-research/kopiur/crates/api`).
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator-api` exits 0 with the module tree compiling; `rg -c 'GRAPH_BOLT_NODE_ADDRESSES' crates/api/src/constants.rs` is >= 1.
  QA scenarios (name the exact tool + invocation): happy: `cargo test -p hydradb-operator-api` exits 0. failure: assert that a struct literal omitting both `ObjectStore` variants fails to compile (checked by a `trybuild` case or a compile-fail doctest). Evidence `<attemptDir>/task-6-hydradb-operator.txt`
  Commit: Y | feat(api): add crate layout, constants and shared spec primitives
  Recommended task executor category: unspecified-high - foundational shared types that everything else depends on.

- [x] 7. Unknown-fallback enum macro and strict parsers
  What to do / Must NOT do: Implement a declarative macro `phase_serde!` (or an equivalent `#[derive]`-based helper) in `crates/api/src/spec/mod.rs` that generates a serde-`Deserialize` impl which maps ANY unrecognised string to an `Unknown(String)` variant instead of erroring, plus `Display`/`Serialize` round-tripping it verbatim. Use it for every externally-extensible enum: `BuildMode` (`full`, `incremental`, `Unknown`), `CacheType`, `ServiceType`, `PullPolicy`, and the condition reasons. Additionally implement strict-Option parsers so that an absent field stays `None` and NEVER defaults into a safe-looking variant. Must NOT use `#[serde(other)]` alone (it drops the original string), must NOT make `Unknown` constructible from a non-string.
  Closes: GAP-9
  Parallelization: Wave 2 | Blocked by: 6 | Blocks: 8-12,W3
  Context (executor has NO interview context - be exhaustive): kopiur's `phase_serde!` macro with an Unknown fallback and its strict-Option parsers are the pattern (`/home/aidas/.cache/ulw-research/kopiur/crates/api`). Rationale: a CR written by a newer operator must not fail to deserialize in an older one, and an unknown value must never be silently treated as a known one.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator-api unknown_variant` passes; a test asserts `serde_json::from_str::<BuildMode>("\"brand-new-mode\"")` yields `Unknown("brand-new-mode")` and re-serializes to the same string.
  QA scenarios (name the exact tool + invocation): happy: deserialize `"incremental"` and `"brand-new-mode"` and assert `Incremental` and `Unknown("brand-new-mode")`. failure: deserialize `{}` into a type whose field is `Option<BuildMode>` and assert the field is `None`, not `Some(Unknown)`. Evidence `<attemptDir>/task-7-hydradb-operator.txt`
  Commit: Y | feat(api): add Unknown-fallback enum macro and strict option parsing
  Recommended task executor category: deep-low - macro and serde semantics reasoning that must be settled from the code itself.

- [x] 8. GraphCluster custom resource type
  What to do / Must NOT do: Define `GraphCluster` in `crates/api/src/crd/cluster.rs` via `#[derive(CustomResource, ...)]` with group `hydradb.helmforge.dev`, version `v1alpha1`, kind `GraphCluster`, `#[kube(namespaced, status = "GraphClusterStatus", shortname = "hgc", printcolumn = ...)]`, `#[kube(scale)]` omitted. `GraphClusterSpec` carries: `image: Image`, `graph: { namespace: String, id: String, database: String, cell_id: String }`, `object_store: ObjectStore`, `auth: { existing_secret: Option<String>, secret_key: Option<String>, generate: bool, token: Option<String> }`, `tls: { enabled: bool, secret_name: Option<String>, additional_dns_names: Vec<String> }`, `deletion_policy: DeletionPolicy` (`Retain` default, `Delete`), `service: { bolt_port: u16 (default 7687), https_port: u16 (default 443), admin_port: u16 (default 9090) }`, `runtime: { rust_log: String, max_open_scopes: u32, slatedb_cache_bytes: u64, data_cache_max_open_files: u32, reader_wal_replay_concurrency: u32, max_concurrent_queries: u32, max_query_scan_edges: u64 }`. `GraphClusterStatus` carries `conditions: Vec<Condition>`, `observed_generation: Option<i64>`, `endpoints: { bolt: Option<String>, https: Option<String>, admin: Option<String> }`, `node_addresses: Vec<String>`, `observed_object_store: Option<String>`. Must NOT embed node or indexer replica configuration here (that belongs to the other two CRDs), must NOT add fields the operand cannot honour.
  Closes: GAP-9
  Parallelization: Wave 2 | Blocked by: 6,7 | Blocks: W3,W5,W6
  Context (executor has NO interview context - be exhaustive): the graph identity env vars (`GRAPH_NAMESPACE`, `GRAPH_ID`, `GRAPH_DATABASE`, `GRAPH_CELL_ID`) and the runtime knobs come from `/home/aidas/.cache/ulw-research/hydradb/charts/hydradb/values.yaml` (`graph.*`, `runtime.*`, `auth.*`, `tls.*`, `service.*`). Single-cell only: `GRAPH_CELLS` is always `1` and `cell_id` is user-supplied.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator-api` exits 0; a test deserializes a minimal YAML `GraphCluster` (only `image.repository` and the required `graph`/`object_store` fields) and asserts every optional field is `None`/its documented default.
  QA scenarios (name the exact tool + invocation): happy: deserialize a full `GraphCluster` YAML and assert every field round-trips. failure: omit the required `object_store` and assert serde reports a missing-field error naming `object_store`. Evidence `<attemptDir>/task-8-hydradb-operator.txt`
  Commit: Y | feat(api): add the GraphCluster custom resource type
  Recommended task executor category: unspecified-high - the primary API surface; field shapes must match the operand exactly.

- [x] 9. GraphNodePool custom resource type
  What to do / Must NOT do: Define `GraphNodePool` in `crates/api/src/crd/nodepool.rs` with `#[kube(namespaced, status = "GraphNodePoolStatus", shortname = "hgnp", printcolumn = ...)]`, a required `cluster_ref: LocalObjectReference` (`spec.clusterRef.name`), and `GraphNodePoolSpec` carrying: `replicas: i32`, `image: Option<Image>` (overrides the cluster's), `resources: { requests, limits }`, `cache: { storage: CacheStorage, memory_limit_bytes: Option<u64> }`, `tmp_size_limit: Option<String>`, `service_account: Option<String>`, `priority_class_name: Option<String>`, `termination_grace_period_seconds: Option<i64>`, `update_strategy: Option<{ type: String, rolling_update_partition: Option<i32> }>`, `affinity: Option<Affinity>`, `topology_spread_constraints: Vec<TopologySpreadConstraint>`, `tolerations: Vec<Toleration>`, `node_selector: Option<BTreeMap<String, String>>`, `extra_env: Vec<EnvVar>`, `pod_annotations: BTreeMap<String,String>`, `pod_labels: BTreeMap<String,String>`, `service: Option<{ advertised_bolt_address: Option<String>, bolt_type: Option<String> }>`. `GraphNodePoolStatus` carries `conditions`, `observed_generation`, `ready_replicas: i32`, `current_replicas: i32`, `bolt_addresses: Vec<String>`. Must NOT put object-store, auth, TLS, or graph identity fields here (they belong to `GraphCluster`).
  Closes: GAP-9
  Parallelization: Wave 2 | Blocked by: 6,7 | Blocks: W3,W5,W6
  Context (executor has NO interview context - be exhaustive): mirror the upstream chart's `node.*` block (`/home/aidas/.cache/ulw-research/hydradb/charts/hydradb/values.yaml`) and `templates/node-statefulset.yaml`. Node is a StatefulSet with a headless Service because peers address each other by stable identity.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator-api` exits 0; a test asserts that deserializing a `GraphNodePool` with `cache.storage` holding both `emptyDir` and `persistentVolume` keys fails (deny_unknown_fields on the union).
  QA scenarios (name the exact tool + invocation): happy: deserialize a pool with `cache.storage.persistentVolume` on `ceph-block` and assert the variant plus `accessModes`. failure: deserialize a pool with `clusterRef` absent and assert a missing-field error naming `clusterRef`. Evidence `<attemptDir>/task-9-hydradb-operator.txt`
  Commit: Y | feat(api): add the GraphNodePool custom resource type
  Recommended task executor category: unspecified-high - large API surface that must match the operand's StatefulSet contract.

- [x] 10. GraphIndexer custom resource type
  What to do / Must NOT do: Define `GraphIndexer` in `crates/api/src/crd/indexer.rs` with `#[kube(namespaced, status = "GraphIndexerStatus", shortname = "hgi", printcolumn = ...)]`, required `cluster_ref: LocalObjectReference`, and `GraphIndexerSpec` carrying: `enabled: bool`, `replicas: i32`, `image: Option<Image>`, `interval_ms: Option<u64>` (default 5000), `dirty_poll_interval_ms: Option<u64>` (default 250), `retain_previous_generations: Option<u32>` (default 1), `build_mode: Option<BuildMode>` (default `full`), `incremental_min_edges: Option<u64>` (default 250000), `scope_concurrency: Option<u32>` (default 8), `scopes_per_cycle: Option<u32>` (default 256), `max_open_scopes: Option<u32>` (default 128), `readiness_failure_threshold: Option<i32>` (default 3), `max_wal_tail_files: Option<u32>` (default 4096), `wal_tail_fetch_concurrency: Option<u32>` (default 16), `resources`, `service_account`, `node_selector`, `tolerations`, `affinity`, `pod_annotations`, `pod_labels`, `extra_env`. `GraphIndexerStatus` carries `conditions`, `observed_generation`, `ready_replicas`, `current_generation_label: Option<String>`. Must NOT add a schedule/cron field (indexing is continuous), must NOT add a PVC cache (the indexer is stateless).
  Closes: GAP-9
  Parallelization: Wave 2 | Blocked by: 6,7 | Blocks: W3,W5,W6
  Context (executor has NO interview context - be exhaustive): the upstream chart's `indexer.*` block and `templates/indexer-deployment.yaml` are the source of truth (`/home/aidas/.cache/ulw-research/hydradb/charts/hydradb/values.yaml`). The indexer publishes immutable `.csc` generations to the object store and is otherwise stateless, so it is a Deployment, not a StatefulSet.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator-api` exits 0; a test asserts that omitting every optional field yields the documented defaults (5000/250/1/full/250000/8/256/128/3/4096/16).
  QA scenarios (name the exact tool + invocation): happy: deserialize `{ clusterRef, enabled: true, replicas: 2 }` and assert all defaults. failure: deserialize `buildMode: "not-a-mode"` and assert it becomes `Unknown("not-a-mode")` rather than an error. Evidence `<attemptDir>/task-10-hydradb-operator.txt`
  Commit: Y | feat(api): add the GraphIndexer custom resource type
  Recommended task executor category: unspecified-high - large API surface with many documented defaults.

- [x] 11. CRD metadata, schema attributes and validation rules
  What to do / Must NOT do: Add the schemars/kube attributes that make the CRDs usable and self-documenting: `#[schemars(deny_unknown_fields)]` on every spec type; explicit `#[schemars(range(min = 1))]`-style bounds where the operand requires them (replicas >= 1, ports 1..=65535); `#[serde(default)]` ONLY where the operand has a genuine default (never for `ObjectStore` or `CacheStorage`); printer columns `Ready` (`.status.conditions[?(@.type=="Ready")].status`), `Age` (`.metadata.creationTimestamp`), `Replicas`/`Ready` for the pool and indexer; `#[kube(category = "hydradb")]` on all three; a `DeletionPolicy` enum with `Retain` as the explicit default and `Delete` opt-in. Also implement `validation.rs` with a `fn validate(&self) -> Result<(), Vec<ValidationError>>` for the cross-field rules the schema cannot express: exactly one object-store source, `data_cache_max_open_files >= max_open_scopes * 2`, `cell_id` non-empty, and TLS `additional_dns_names` only when `tls.enabled`. Must NOT add a CRD-level webhook (D17: no webhook in cut one), must NOT add a schema `default:` where absent already equals the value (it causes GitOps churn).
  Closes: GAP-9
  Parallelization: Wave 2 | Blocked by: 6,7,8,9,10 | Blocks: W3,W5,W6
  Context (executor has NO interview context - be exhaustive): the operand validates `maxOpenFiles >= maxOpenScopes*cells*2`; kopiur deliberately omits schema `default:` where absent equals the value to avoid GitOps churn, and ships no admission webhook in its first cut. `DeletionPolicy` defaults to `Retain`.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator-api` exits 0; a test asserts `validate()` rejects an `ObjectStore` with zero and with two sources, rejects `data_cache_max_open_files < max_open_scopes * 2`, and accepts a valid combination.
  QA scenarios (name the exact tool + invocation): happy: `validate()` returns `Ok` for a fully specified cluster. failure: set both `object_store.explicit` and `object_store.objectBucketClaim` and assert `validate()` returns an error naming `object_store`. Evidence `<attemptDir>/task-11-hydradb-operator.txt`
  Commit: Y | feat(api): add CRD schema attributes, printer columns and cross-field validation
  Recommended task executor category: unspecified-high - schema plus validation semantics that must match operand behaviour.

- [x] 12. api crate test suite and CRD emission smoke test
  What to do / Must NOT do: Add `crates/api/tests/` covering: serde round-trip for all three kinds; unknown-variant handling for every extensible enum; documented-default behaviour for every `Option`; `validate()` accept/reject matrices; and a smoke test that calls `GraphCluster::crd()`, `GraphNodePool::crd()` and `GraphIndexer::crd()` and asserts `spec.group == "hydradb.helmforge.dev"`, `spec.names.kind` matches, `spec.scope == Namespaced`, and that the printed columns are non-empty. Must NOT assert on generated YAML text, must NOT depend on the controller crate.
  Closes: GAP-9
  Parallelization: Wave 2 | Blocked by: 6-11 | Blocks: W5
  Context (executor has NO interview context - be exhaustive): `kube::CustomResourceExt::crd()` emits the `CustomResourceDefinition`; kopiur's `cargo xtask gen-crds` writes exactly this to `deploy/crds/`.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator-api` exits 0 with every test listed by `-- --list` present and passing.
  QA scenarios (name the exact tool + invocation): happy: `cargo test -p hydradb-operator-api` exits 0. failure: temporarily change `GROUP` to `"x"` and assert the CRD smoke test fails, then revert. Evidence `<attemptDir>/task-12-hydradb-operator.txt`
  Commit: Y | test(api): add serde, validation and CRD emission test suites
  Recommended task executor category: unspecified-high - broad test matrix across the whole api crate.

### Wave 3 - the controller (reconcilers, conditions, children)

- [x] 13. controller crate skeleton, `run()` entrypoint and graceful shutdown
  What to do / Must NOT do: Create `crates/controller/src/{main.rs,run.rs,context.rs,error.rs,watcher.rs,conditions.rs,apply.rs,metrics.rs}` and a `Ctx` holding `kube::Client`, the operator config (from `OPERATOR_NAMESPACE`/env), and the three `Api`s. `main.rs` parses no flags (env only), installs `tracing_subscriber` with the `RUST_LOG` filter, builds the client, and calls `run()`; `run()` spawns the three `Controller`s plus the health/metrics axum server on `:8080` and returns on `shutdown_signal()` (SIGTERM/SIGINT). Must NOT read CRD fields before they exist, must NOT use `unwrap()` on client calls, must NOT add a CLI to the controller binary. Context: the OPERATOR's MSRV is 1.89 and the pinned toolchain is 1.89.0 - the operand's own Rust 1.98 is irrelevant because we never build hydradb; this precedence is authoritative. kopiur's `controller` crate is the structural model.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 1-5 | Blocks: 14-26
  Context (executor has NO interview context - be exhaustive): kube 4.2.0 with `runtime`,`client`,`derive`,`rustls-tls`; `kube::Client::try_default()`; `tokio::signal`; the operator runs in its own namespace and watches all namespaces.
  Acceptance criteria (agent-executable): `cargo build -p hydradb-operator` exits 0; `cargo clippy --all-targets -- -D warnings` exits 0; running the binary with `--help` is a no-op (no clap) and with no kubeconfig exits non-zero with a clear error.
  QA scenarios: happy: `cargo build -p hydradb-operator` exits 0. failure: unset `KUBECONFIG` and run the binary; assert a non-zero exit and an error naming the client, not a panic. Evidence `<attemptDir>/task-13-hydradb-operator.txt`
  Commit: Y | feat(controller): add controller crate skeleton, entrypoint and shutdown
  Recommended task executor category: unspecified-high - multi-module crate bootstrap that every later reconciler depends on.

- [x] 14. `watcher_config()`: ListSemantic::Any, version-gated streamingLists, debounce and concurrency cap
  What to do / Must NOT do: Implement ONE shared `watcher::Config` builder used by every `.owns()`/`.watches()`/`Controller::new()` call: `list_semantic = ListSemantic::Any`; `streaming_lists` enabled only when the apiserver version probe (bounded, fails OPEN on transport failure, fails CLOSED on a pre-1.32 answer) allows it; a 250 ms trailing-edge debounce; a per-controller concurrency cap; label-filtered owned watches and metadata-only `Api<PartialObjectMeta<K>>` watches. Must NOT construct a bare `watcher::Config::default()` anywhere else (kopiur's Maintenance informer regressed exactly this way), must NOT opt into `unstable-runtime-subscribe`. Context: kube-rs defaults to `MostRecent`, so with streaming off every initial LIST is an etcd quorum read; kopiur's three cold-start availability fixes are the template. The intervals and caps are carried-from-kopiur, validate on the first load test.
  Closes: GAP-8
  Parallelization: Wave 3 | Blocked by: 13 | Blocks: 18-24
  Context (executor has NO interview context - be exhaustive): `kube::runtime::watcher::Config` has `list_semantic`/`any_semantic`/`streaming_lists`; there is a 295 s timeout cap and known issue #1844 (streamingLists resends every object on each ~290 s reconnect), which the debounce must absorb.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; a unit test asserts the builder returns `ListSemantic::Any` and that the probe fails open when the transport errors; `grep -rn "watcher::Config::default" crates/controller/src` returns nothing.
  QA scenarios: happy: unit test asserts the config. failure: point the version probe at an unreachable host and assert it still returns a streaming-capable config (fail open). Evidence `<attemptDir>/task-14-hydradb-operator.txt`
  Commit: Y | feat(controller): add shared watcher_config with Any list semantics and version-gated streaming
  Recommended task executor category: deep-low - subtle kube-runtime semantics with a documented prior regression.

- [x] 15. Error classification and requeue policy
  What to do / Must NOT do: Add an exhaustive `Error` enum and `Error::class()` returning `Transient` (30 s, FNV-1a jittered 30-60 s) | `Structural` (300 s) | `Terminal` (1800 s heartbeat), with transport-aware `Event` suppression so a flapping apiserver does not spam Events. Every reconciler returns this type via `Action`/`ErrorAction`. Must NOT use `anyhow` in the reconcile path, must NOT `DefaultHasher` (non-deterministic across runs). Context: kopiur's classification is the template; the intervals are carried-from-kopiur, validate on the first load test, and must be overridable constants.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 13 | Blocks: 18-24
  Context (executor has NO interview context - be exhaustive): `kube::runtime::controller::Action::{requeue,await_change}`; `ErrorAction::{requeue,shutdown}`.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; tests assert the jitter stays within [30,60] s over 1000 samples and that each variant maps to its documented interval.
  QA scenarios: happy: assert `Error::Transient.class()` is within the jitter band. failure: feed an apiserver 5xx and assert no `Event` is emitted while a `Structural` error still requeues at 300 s. Evidence `<attemptDir>/task-15-hydradb-operator.txt`
  Commit: Y | feat(controller): add exhaustive error classification and requeue policy
  Recommended task executor category: quick - one module plus a focused test.

- [x] 16. Conditions module (hand-rolled kstatus)
  What to do / Must NOT do: Implement order-stable `upsert_condition` (never reorders or churns `lastTransitionTime`), `upsert_gate` driven by a `STRUCTURAL_GATES` registry, `set_ready` emitting kstatus-compatible `Ready`, and a `live_conditions` re-read that compares against the in-cluster object before writing so a no-op patch never hot-loops. Emit `Ready`, `Reconciling`, `Stalled` plus `status.observedGeneration`. Must NOT import a conditions helper from kube-rs (none exists), must NOT write status when the condition set is unchanged. Context: kube-rs ships NO `kube::runtime::conditions`; only `runtime::wait::conditions` + `await_condition` exist for tests.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 13 | Blocks: 18-24
  Context (executor has NO interview context - be exhaustive): `k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition`; Flux `wait`/kstatus and Argo health and `kubectl wait --for=condition=Ready` must all work off these conditions.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; tests assert (a) upserting the same condition twice does not change `lastTransitionTime`, (b) the condition list order is stable across 100 random upsert orders.
  QA scenarios: happy: assert `set_ready(true)` yields `Ready=True`. failure: apply the same condition twice and assert the second produces no patch (compare the serialized status). Evidence `<attemptDir>/task-16-hydradb-operator.txt`
  Commit: Y | feat(controller): add hand-rolled kstatus conditions module
  Recommended task executor category: unspecified-high - correctness-critical status semantics used by every CRD.

- [x] 17. SSA apply layer, owner references and child naming/labels
  What to do / Must NOT do: Implement `apply.rs`: a generic `apply_child(api, obj)` using `Patch::Apply` with `field_manager = "hydradb.helmforge.dev/controller"` and `force = true`; `owner_ref_for(controller: true, block_owner_deletion: false)`; deterministic child names `<cluster>-<role>` / `<pool>` / `<indexer>`; and the standard label set (`app.kubernetes.io/name`, `-instance`, `-component`, `-managed-by: hydradb-operator`, `-part-of: hydradb`) plus the `hydradb.helmforge.dev/cluster` selector label every child carries so `.owns()` can label-filter. Must NOT use merge or JSON patch for spec fields, must NOT double-prefix the field manager (the pinned value is exactly `hydradb.helmforge.dev/controller`, never `hydradb.<group>/controller`), must NOT set `blockOwnerDeletion: true`.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 13 | Blocks: 18-24
  Context (executor has NO interview context - be exhaustive): kopiur uses SSA with a fixed field manager + force and `owner_ref_for(controller:true, blockOwnerDeletion:false)`.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; a unit test asserts the exact field-manager string, `force == true`, and the label set on a built child.
  QA scenarios: happy: build a child and assert the field manager and labels. failure: assert two reconciles of the same desired object produce byte-identical JSON (no churn). Evidence `<attemptDir>/task-17-hydradb-operator.txt`
  Commit: Y | feat(controller): add SSA apply layer, owner refs and child naming
  Recommended task executor category: unspecified-high - the write path every reconciler shares.

- [x] 18. GraphCluster reconciler: ConfigMap, auth Secret and object-store resolution
  What to do / Must NOT do: Reconcile `GraphCluster` into the shared config `ConfigMap` (all operand env: `GRAPH_NAMESPACE`,`GRAPH_ID`,`GRAPH_DATABASE`,`GRAPH_CELL_ID`,`GRAPH_CELLS`, `CLOUD_PROVIDER`,`AWS_BUCKET_NAME`,`AWS_DEFAULT_REGION`,`AWS_ALLOW_HTTP`,`AWS_ENDPOINT`, `RUST_LOG`, cache knobs) and the auth `Secret` (either generated once and preserved via `preserveGeneratedToken` semantics, or `existingSecret`/ESO passthrough). Resolve the object store in the documented order: explicit `spec.objectStore.explicit` -> else `spec.objectStore.objectBucketClaim` (read the OBC's generated `Secret` for `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`BUCKET_HOST`/`BUCKET_NAME`/`BUCKET_PORT`/`BUCKET_REGION` and its `ConfigMap` for the endpoint) -> else set `Stalled` with a message naming `objectStore`. Record the resolved source in `status.observedObjectStore`. Must NOT invent a bucket, must NOT write credentials into the ConfigMap, must NOT require a webhook.
  Closes: GAP-1, GAP-8
  Parallelization: Wave 3 | Blocked by: 13-17 | Blocks: 19-24
  Context (executor has NO interview context - be exhaustive): the operand is ENV-ONLY and calls `slatedb::admin::load_object_store_from_env`; `AWS_BUCKET_NAME` is required; Rook OBCs create the Secret + ConfigMap in the OBC's own namespace; upstream values use `objectStore.cloudProvider: aws` + `aws.{bucketName,region,allowHttp,endpoint}`.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; envtest applies a `GraphCluster` with an OBC and asserts the ConfigMap carries `AWS_BUCKET_NAME` from the OBC Secret and `AWS_ENDPOINT` from the OBC ConfigMap.
  QA scenarios: happy: envtest with an OBC yields a ConfigMap with the resolved bucket. failure: a `GraphCluster` with no object store sets `Stalled=True` with a message naming `objectStore` and creates no workload. Evidence `<attemptDir>/task-18-hydradb-operator.txt`
  Commit: Y | feat(controller): reconcile GraphCluster config, auth and object-store resolution
  Recommended task executor category: deep-low - cross-object credential/endpoint resolution with a documented order.

- [x] 19. GraphCluster reconciler: Services and the Bolt routing table
  What to do / Must NOT do: Reconcile the five Services (`-node-headless` with `publishNotReadyAddresses: true`, `-bolt` 7687, `-https` 443->8443, `-metrics` 9090, `-indexer-metrics` 9091) and RECOMPUTE `GRAPH_BOLT_NODE_ADDRESSES` on every reconcile from the live pool membership (ready pod IPs/DNS names of the `GraphNodePool` StatefulSet), writing it into the shared ConfigMap so a node restart re-derives the routing table. Must NOT hand-build the address list from `replicaCount` (the upstream chart's `_helpers.tpl:63-71` does exactly that and it is the defect we remove), must NOT cache the list across reconciles.
  Closes: GAP-8
  Parallelization: Wave 3 | Blocked by: 18 | Blocks: 20-24
  Context (executor has NO interview context - be exhaustive): the operand discovers peers through heartbeat objects in object storage (`_graph_nodes/v1/`, interval 5 s, timeout 15 s) but still needs `GRAPH_BOLT_NODE_ADDRESSES` as the cross-node Bolt routing table; there is no leader, no coordinator and no placement service.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; a test builds a pool of 3 and asserts the ConfigMap value lists exactly 3 Bolt addresses; deleting one pod and re-running the reconcile yields 2.
  QA scenarios: happy: envtest with a 3-replica pool asserts 3 addresses in the ConfigMap. failure: scale the pool to 0 and assert the value becomes empty (not stale). Evidence `<attemptDir>/task-19-hydradb-operator.txt`
  Commit: Y | feat(controller): reconcile cluster Services and recompute the Bolt routing table
  Recommended task executor category: deep-low - live-membership derivation replacing a known upstream defect.

- [x] 20. GraphNodePool reconciler: StatefulSet
  What to do / Must NOT do: Reconcile `GraphNodePool` into a `StatefulSet` (`serviceName` = the headless Service, `podManagementPolicy: Parallel`, `updateStrategy` from the CR) running the `graph-node` image, `command: ["graph-node"]`, all env from the shared ConfigMap + the auth Secret, probes `/livez` + `/readyz` on 9090, ports 7687/8443/9090, `securityContext` uid/gid 10001, `readOnlyRootFilesystem: true`, writable `/var/cache/slatedb` + `/tmp` volumes, `RUST_MIN_STACK=33554432`, and `terminationGracePeriodSeconds` default 60. Cache storage comes from the `CacheStorage` union: `EmptyDir {size_limit}` or `PersistentVolume {size, storage_class_name, access_modes, annotations}` via `volumeClaimTemplates`. Must NOT default the storage class (leave it unset so the cluster default applies), must NOT run as root, must NOT omit the writable scratch mounts.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 18-19 | Blocks: 22-24
  Context (executor has NO interview context - be exhaustive): upstream node resources request 2cpu/4Gi/12Gi and limit 8/16Gi/24Gi; `maxOpenFiles >= maxOpenScopes*cells*2` is validated by the operand; `ceph-block` is the fast RBD class and `ceph-cephfs` the RWX one.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; envtest asserts the StatefulSet has `replicas` from the CR, the 3 ports, the two writable mounts, uid 10001, and a `volumeClaimTemplates` entry when `cache.storage` is `persistentVolume`.
  QA scenarios: happy: `cache.storage.type: emptyDir` yields an `emptyDir` with the requested size limit. failure: `cache.storage.type: persistentVolume` with a `storageClassName` yields a PVC template carrying that class. Evidence `<attemptDir>/task-20-hydradb-operator.txt`
  Commit: Y | feat(controller): reconcile GraphNodePool into a StatefulSet
  Recommended task executor category: unspecified-high - the largest single child object with many required fields.

- [x] 21. GraphIndexer reconciler: Deployment
  What to do / Must NOT do: Reconcile `GraphIndexer` into a `Deployment` running the `graph-indexer` image with the indexer env set (`INDEXER_INTERVAL_MS`, `DIRTY_POLL_INTERVAL_MS`, `RETAIN_PREVIOUS_GENERATIONS`, build mode + `INCREMENTAL_MIN_EDGES`, `SCOPE_CONCURRENCY`, `SCOPES_PER_CYCLE`, `MAX_OPEN_SCOPES`, `READINESS_FAILURE_THRESHOLD`, `MAX_WAL_TAIL_FILES`, `WAL_TAIL_FETCH_CONCURRENCY`), probes `/livez` + `/readyz` on 9091, and the same security context as the node. `enabled: false` must scale to 0 and set `Ready=False` with reason `Disabled`. Must NOT create a PVC (the indexer is stateless), must NOT add a cron field (there is no scheduling CRD in this cut).
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 18 | Blocks: 22-24
  Context (executor has NO interview context - be exhaustive): the indexer publishes immutable `.csc` generations via CAS object-store pointers and optionally wakes via `POST /v1/changes:process`; upstream defaults are interval 5000, dirtyPoll 250, retainPreviousGenerations 1, buildMode full, incrementalMinEdges 250000, scopeConcurrency 8, scopesPerCycle 256, maxOpenScopes 128, readinessFailureThreshold 3, maxWalTailFiles 4096, walTailFetchConcurrency 16.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; envtest asserts the Deployment env contains every listed variable with the CR's values and that `enabled: false` yields `replicas: 0`.
  QA scenarios: happy: a default `GraphIndexer` yields the documented defaults. failure: `enabled: false` asserts `Ready=False` with reason `Disabled` and zero replicas. Evidence `<attemptDir>/task-21-hydradb-operator.txt`
  Commit: Y | feat(controller): reconcile GraphIndexer into a Deployment
  Recommended task executor category: unspecified-high - many env fields plus a disabled state.

- [x] 22. PDB, NetworkPolicy and ServiceMonitor children
  What to do / Must NOT do: Reconcile a `PodDisruptionBudget` (`minAvailable: 1`) per node pool and per enabled indexer, a default-on `NetworkPolicy` allowing only the documented ingress (Bolt 7687, HTTPS 8443, admin 9090/9091 from within the namespace or the configured `endpointEgressTo`/ingress CIDRs) plus object-store egress, and a `ServiceMonitor` that is OFF by default and enabled only when `spec.monitoring.serviceMonitor.enabled: true`. Must NOT create a `ServiceMonitor` by default (upstream parity, and it would force prometheus-operator CRDs to install), must NOT emit `extra-objects` escape hatches (the CRD surface is the API), must NOT create `CronJob`s.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 18-21 | Blocks: 24
  Context (executor has NO interview context - be exhaustive): upstream ships `poddisruptionbudgets.yaml`, `networkpolicy.yaml` and `servicemonitor.yaml` (off by default) - these are parity-derived, not user-quoted scope.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; envtest asserts a PDB exists per role, a NetworkPolicy exists, and NO ServiceMonitor exists unless the field is set.
  QA scenarios: happy: default CR yields a PDB + NetworkPolicy and no ServiceMonitor. failure: set `monitoring.serviceMonitor.enabled: true` and assert exactly one ServiceMonitor appears. Evidence `<attemptDir>/task-22-hydradb-operator.txt`
  Commit: Y | feat(controller): reconcile PDB, NetworkPolicy and opt-in ServiceMonitor
  Recommended task executor category: quick - three small, independent child builders.

- [x] 23. Finalizer and deletionPolicy handling
  What to do / Must NOT do: Wrap every reconcile in `kube::runtime::finalizer(&api, "hydradb.helmforge.dev/finalizer", obj, |Event::Apply|..., |Event::Cleanup|...)` as the ONLY top-level action, and implement `deletionPolicy: Retain | Delete` (default `Retain`). On `Delete`, remove operator-owned children and the object-store data prefix; on `Retain`, stamp `hydradb.helmforge.dev/pruned-by` on the object before deleting it and leave data intact. `Cleanup` must tolerate a CR that was never applied. Must NOT block deletion on an unreachable object store, must NOT delete an operator-owned bucket it did not create.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 13-17 | Blocks: 24
  Context (executor has NO interview context - be exhaustive): kopiur ties the external snapshot lifecycle to the CR with a finalizer plus `deletionPolicy`, and stamps `pruned-by` before delete; `Cleanup` must tolerate never-applied.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; envtest deletes a `Retain` cluster and asserts children are gone, the finalizer is removed, and `pruned-by` is set; the same with `Delete` and an unreachable store still terminates.
  QA scenarios: happy: `Retain` deletion removes the finalizer and sets `pruned-by`. failure: `Delete` with an unreachable endpoint still completes within the reconcile timeout. Evidence `<attemptDir>/task-23-hydradb-operator.txt`
  Commit: Y | feat(controller): add finalizer and deletionPolicy handling
  Recommended task executor category: deep-low - destructive semantics where ordering and tolerance matter.

- [ ] 24. Status writer
  What to do / Must NOT do: Write `status` for all three kinds via the `/status` subresource: `conditions`, `observedGeneration` (always copied from `metadata.generation`), `endpoints` (`bolt`, `https`, `admin`), `nodeAddresses`, `observedObjectStore` on `GraphCluster`; `readyReplicas`/`currentReplicas`/`boltAddresses` on `GraphNodePool`; `readyReplicas`/`currentReplicas`/`currentGenerationLabel` on `GraphIndexer`. Read child state with a single `Api::list` scoped by the `hydradb.helmforge.dev/cluster` label. Must NOT write status when nothing changed, must NOT write spec from the status path.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 18-23 | Blocks: 26
  Context (executor has NO interview context - be exhaustive): Flux kstatus and Argo read these; `observedGeneration` must only advance when the reconciler has actually observed that generation.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; envtest asserts `status.observedGeneration == metadata.generation` after a spec change and that `endpoints.bolt` matches the Service.
  QA scenarios: happy: after apply, `observedGeneration` equals `generation` and `Ready=True`. failure: a partially ready pool sets `Ready=False` while `observedGeneration` still tracks `generation`. Evidence `<attemptDir>/task-24-hydradb-operator.txt`
  Commit: Y | feat(controller): add status writers for all three kinds
  Recommended task executor category: unspecified-high - three status surfaces plus the generation contract.

- [x] 25. Metrics and tracing
  What to do / Must NOT do: Expose Prometheus metrics on `:8080` (`/metrics`) - reconcile counts, durations, error-class counters, watcher events - and structured `tracing` logs with the reconcile span carrying kind/namespace/name. OTLP export is compiled behind an off-by-default `otlp` feature and enabled by env. Must NOT add a hard dependency on an OTLP collector, must NOT log credentials.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 13 | Blocks: 26
  Context (executor has NO interview context - be exhaustive): the operand pairs `tracing-opentelemetry` 0.33 with `opentelemetry` 0.32 and keeps OTLP behind an off-by-default feature - mirror that discipline.
  Acceptance criteria (agent-executable): `cargo build -p hydradb-operator --features otlp` and without the feature both exit 0; `cargo test -p hydradb-operator` exits 0; a test asserts a reconcile increments the counter.
  QA scenarios: happy: start the server and assert `GET /metrics` contains the reconcile counter. failure: build WITHOUT `otlp` and assert no OTLP symbol is required at runtime. Evidence `<attemptDir>/task-25-hydradb-operator.txt`
  Commit: Y | feat(controller): add Prometheus metrics and optional OTLP tracing
  Recommended task executor category: quick - one metrics module plus a feature flag.

- [ ] 26. Controller test suite: tower-test mock and envtest
  What to do / Must NOT do: Add `crates/controller/tests/` with (a) `tower-test` + `ApiServerVerifier`/`Scenario` tests asserting the exact requests each reconciler issues, and (b) `envtest 0.3` integration tests (`Environment::default().with_crds(...).create()`) exercising apply -> children -> status -> delete for all three kinds. Must NOT require a real cluster in CI (envtest downloads its own binaries), must NOT assert on log text.
  Closes: GAP-10
  Parallelization: Wave 3 | Blocked by: 13-25, 66 | Blocks: W9
  Context (executor has NO interview context - be exhaustive): `envtest` 0.3 + `tower-test` 0.4 are the researched stack; CI runs envtest, local `kind` e2e lives in the e2e crate.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0 including the envtest suite; `cargo test -p hydradb-operator -- --list` names every scenario.
  QA scenarios: happy: envtest apply->ready->delete passes. failure: apply a `GraphCluster` missing its required `objectStore` and assert the reconciler sets `Stalled` rather than panicking. Evidence `<attemptDir>/task-26-hydradb-operator.txt`
  Commit: Y | test(controller): add tower-test and envtest suites
  Recommended task executor category: unspecified-high - the broadest test surface in the repo.

### Wave 4 - the `kubectl hydradb` CLI plugin

- [x] 27. cli crate skeleton, clap surface and global flags
  What to do / Must NOT do: Create `crates/cli/src/{main.rs,cmd/mod.rs,output.rs}` with a `clap 4.5` derive surface: `hydradb <status|endpoints|logs|doctor|scale|version>` plus global `--kubeconfig`, `--context`, `-n/--namespace`, `-A/--all-namespaces`, `-o/--output` (`table|wide|yaml|json|name`), `-v/-vv`, and `KOPIUR`-style env logging. The binary must be named `hydradb` so krew installs it as `kubectl-hydradb`. Must NOT talk to the cluster at parse time, must NOT reimplement kubeconfig handling (use `kube::Config`).
  Closes: GAP-10
  Parallelization: Wave 4 | Blocked by: 1-5 | Blocks: 28-30
  Context (executor has NO interview context - be exhaustive): kopiur's `cli` crate is the model; the plugin version must track the operator (v1alpha1 CRDs); krew makes a `kubectl-hydradb` symlink so both the standalone and the plugin can coexist.
  Acceptance criteria (agent-executable): `cargo build -p hydradb` exits 0; `./target/debug/hydradb --help` lists every subcommand and exits 0; `-o json` on a non-existent resource exits non-zero with a clear message.
  QA scenarios: happy: `hydradb version` prints the crate version. failure: `-o bogus` exits non-zero naming the valid choices. Evidence `<attemptDir>/task-27-hydradb-operator.txt`
  Commit: Y | feat(cli): add the hydradb CLI skeleton and clap surface
  Recommended task executor category: quick - mechanical clap wiring.

- [x] 28. `status`, `endpoints` and `logs` commands
  What to do / Must NOT do: `status` lists `GraphCluster`/`GraphNodePool`/`GraphIndexer` with conditions summarised as a kstatus string and honours `-o`; `endpoints` prints the Bolt/HTTPS/admin endpoints from `status.endpoints`; `logs` streams the selected role's pod logs (node or indexer) with `--follow`, `--tail`, and a container selector. Must NOT print secrets, must NOT page through the whole cluster when a namespace is given.
  Closes: GAP-10
  Parallelization: Wave 4 | Blocked by: 27 | Blocks: 30
  Context (executor has NO interview context - be exhaustive): ports are Bolt 7687, HTTPS 443, admin 9090 (node) and 9091 (indexer); `status.endpoints` is written by the controller.
  Acceptance criteria (agent-executable): `cargo test -p hydradb` exits 0; a test asserts `-o json` emits the exact `v1/List` shape for a fixture.
  QA scenarios: happy: `hydradb endpoints -n demo` prints three endpoints. failure: `hydradb logs` for a role with no pods exits non-zero with a message naming the role. Evidence `<attemptDir>/task-28-hydradb-operator.txt`
  Commit: Y | feat(cli): add status, endpoints and logs commands
  Recommended task executor category: quick - three read-only commands over the same client.

- [x] 29. `doctor` command
  What to do / Must NOT do: Implement `doctor` with an exact, documented check list, each printing PASS/FAIL and the command exits 1 on any FAIL: CRDs installed at the expected group/version; the operator Deployment ready; every `GraphCluster` Ready; every referenced object store reachable (a bounded HEAD/list against the endpoint); auth Secret present with the expected key; each Service resolvable and each pod's `/readyz` returning 200; no `Stalled` condition anywhere. Must NOT mutate anything, must NOT require cluster-admin.
  Closes: GAP-10
  Parallelization: Wave 4 | Blocked by: 27-28 | Blocks: 30
  Context (executor has NO interview context - be exhaustive): kopiur's `doctor` exits 1 on failure; this is the operator's own definition of done for support.
  Acceptance criteria (agent-executable): `cargo test -p hydradb` exits 0; a unit test asserts the exit-code mapping (any FAIL -> exit 1) and that every check has a stable name.
  QA scenarios: happy: against a healthy envtest fixture `doctor` exits 0 with all PASS. failure: remove the auth Secret and assert `doctor` exits 1 naming that check. Evidence `<attemptDir>/task-29-hydradb-operator.txt`
  Commit: Y | feat(cli): add the doctor command
  Recommended task executor category: unspecified-low - a contained command with a fixed check list.

- [x] 30. `scale` command, output modes and CLI test suite
  What to do / Must NOT do: Add `scale graphnodepool/<name> --replicas N` and `scale graphindexer/<name> --replicas N` (patching spec only), finalise the `output.rs` renderers for all five modes, and add the CLI test suite. Must NOT scale the StatefulSet directly (the CR is the API), must NOT implement `suspend`/`resume` or a VolSync migration in this cut.
  Closes: GAP-10
  Parallelization: Wave 4 | Blocked by: 27-29 | Blocks: W9
  Context (executor has NO interview context - be exhaustive): `-o name` must emit `kind/name`; `-o yaml|json` must emit the exact Kubernetes object so it can be piped to `apply`/`jq`/`yq`.
  Acceptance criteria (agent-executable): `cargo test -p hydradb` exits 0; tests assert `-o name`, `-o wide`, `-o json` and `-o yaml` for the same fixture.
  QA scenarios: happy: `scale graphnodepool/p --replicas 5` patches the CR and the CLI prints the new replica count. failure: `--replicas -1` exits non-zero before touching the cluster. Evidence `<attemptDir>/task-30-hydradb-operator.txt`
  Commit: Y | feat(cli): add scale, output modes and CLI tests
  Recommended task executor category: quick - one command plus renderer tests.

### Wave 5 - xtask codegen and drift gates

- [x] 31. `cargo xtask gen-crds`
  What to do / Must NOT do: Implement `crates/xtask/src/{main.rs,gen_crds.rs}` exposing `cargo xtask gen-crds` that writes each `CustomResourceExt::crd()` to BOTH `deploy/crds/<plural>.<group>.yaml` and `charts/hydradb-operator/crds/<plural>.<group>.yaml`, with a stable field order and a trailing newline. Must NOT hand-edit generated YAML, must NOT write to any other path.
  Closes: GAP-9
  Parallelization: Wave 5 | Blocked by: 6-12 | Blocks: 32-34, W6
  Context (executor has NO interview context - be exhaustive): kopiur writes CRDs to both `deploy/crds/` and the chart's `crds/`; Helm's `crds/` are NOT templated, are install-only and are NEVER upgraded, which is why the plan also documents the upgrade path.
  Acceptance criteria (agent-executable): `cargo xtask gen-crds` exits 0; running it twice produces byte-identical files (`git diff --exit-code` clean); both paths contain exactly three CRD files.
  QA scenarios: happy: run gen-crds and assert three files in each of the two directories. failure: mutate a field name in `crates/api/src`, re-run, and assert `cargo xtask gen-all --check` now fails. Evidence `<attemptDir>/task-31-hydradb-operator.txt`
  Commit: Y | feat(xtask): add gen-crds writing to deploy and chart crds directories
  Recommended task executor category: quick - one generator with two outputs.

- [x] 32. `cargo xtask gen-rbac` derived from the `Resource` traits
  What to do / Must NOT do: Implement `gen-rbac` that derives the operator's ClusterRole rules from the kube-rs `Resource` traits actually used (the three CRDs plus every child kind: StatefulSet, Deployment, Service, ConfigMap, Secret, PDB, NetworkPolicy, ServiceMonitor, Event, Lease, ObjectBucketClaim) and writes `deploy/rbac/operator-*.yaml` with a header comment stating it is generated and how to regenerate. Must NOT grant wildcard verbs or `*` resources, must NOT omit `patch` on the CRD status subresources.
  Closes: GAP-9
  Parallelization: Wave 5 | Blocked by: 13 | Blocks: 34, W6
  Context (executor has NO interview context - be exhaustive): kopiur's `cargo xtask gen-rbac` is the source of truth for its RBAC and the chart templates carry a header comment saying so.
  Acceptance criteria (agent-executable): `cargo xtask gen-rbac` exits 0; the emitted ClusterRole contains no `*` in `resources` or `verbs`; `cargo xtask gen-all --check` passes.
  QA scenarios: happy: generated RBAC lists the three CRDs and their `/status` subresources. failure: add a new child kind to a reconciler without regenerating and assert `--check` fails. Evidence `<attemptDir>/task-32-hydradb-operator.txt`
  Commit: Y | feat(xtask): derive RBAC from the Resource traits
  Recommended task executor category: deep-low - correctness matters and a wildcard would silently over-grant.

- [x] 33. `cargo xtask gen-docs` and `gen-all`
  What to do / Must NOT do: Implement `gen-docs` emitting `docs/crd-reference.md` from the CRD schemas (kind, field, type, required/optional, default, description) and `gen-all` running gen-crds -> gen-rbac -> gen-docs in order. Must NOT hand-write the reference, must NOT include prose the schema does not carry.
  Closes: GAP-9
  Parallelization: Wave 5 | Blocked by: 31-32 | Blocks: 34, W10
  Context (executor has NO interview context - be exhaustive): helm-docs generates chart READMEs from `# --` value comments; this xtask covers the CRD surface.
  Acceptance criteria (agent-executable): `cargo xtask gen-all` exits 0; `docs/crd-reference.md` contains one row per schema field and is byte-stable across runs.
  QA scenarios: happy: `cargo xtask gen-all` then `git diff --exit-code` is clean. failure: add a field without docs and assert the reference gains a row with an empty description (and the plan's lint step flags it). Evidence `<attemptDir>/task-33-hydradb-operator.txt`
  Commit: Y | feat(xtask): add gen-docs and gen-all
  Recommended task executor category: quick - one renderer plus orchestration.

- [x] 34. `--check` drift gates: check-wiring and check-phases
  What to do / Must NOT do: Implement `cargo xtask gen-all --check` (regenerate into a temp dir and diff, exiting non-zero on drift) plus `check-wiring` (detect a CRD field that no reconciler reads, with an allowlist) and `check-phases` (detect a non-exhaustive match over any extensible enum, with an allowlist). Wire `check-drift` into the `mise` task surface and the CI gate. Must NOT make the allowlists a dumping ground - each entry needs a comment.
  Closes: GAP-9
  Parallelization: Wave 5 | Blocked by: 31-33 | Blocks: W9
  Context (executor has NO interview context - be exhaustive): kopiur's `gen-all --check` plus check-wiring and check-phases are exactly this pattern and they catch inert fields and unhandled variants at CI time.
  Acceptance criteria (agent-executable): `cargo xtask gen-all --check` exits 0 on a clean tree; after mutating a generated artifact it exits non-zero; `mise run check-drift` exits 0.
  QA scenarios: happy: clean tree passes. failure: hand-edit a generated CRD and assert `--check` fails naming the file. Evidence `<attemptDir>/task-34-hydradb-operator.txt`
  Commit: Y | feat(xtask): add check-drift, check-wiring and check-phases gates
  Recommended task executor category: deep-low - source-analysis heuristics that must not produce false positives.

### Wave 6 - packaging: Dockerfile, image pipeline and the helmforge-conforming chart

- [x] 35. Multi-stage Dockerfile producing both binaries on a distroless base
  What to do / Must NOT do: Add `Dockerfile` (plus `.dockerignore`) with a `cargo-chef` 0.1.78 planner -> chef -> builder chain on `rust:1.92` building `--release -p hydradb-operator -p hydradb` and a final stage on `gcr.io/distroless/cc-debian13:nonroot`. Expose a swappable binary-export stage (`FROM scratch AS bin` with `--build-context bin=`) so CI can inject prebuilt binaries and skip the Rust build. Set `ENTRYPOINT ["/usr/local/bin/hydradb-operator"]`, `USER 65532:65532`, and no `HEALTHCHECK` (the Deployment probes `/healthz`). Must NOT build hydradb (the operand), must NOT run as root, must NOT use `:latest` as a base tag.
  Closes: GAP-1
  Parallelization: Wave 6 | Blocked by: 13, 66 | Blocks: 40-41
  Context (executor has NO interview context - be exhaustive): the operator MSRV is 1.89 and the toolchain is pinned to 1.92.0 (todo 66 bumps it, so build on `rust:1.92`); the operand's Dockerfile (cargo-chef on Rust 1.98, ubuntu:24.04) is a different artifact and must not be copied.
  Acceptance criteria (agent-executable): `docker build -t hydradb-operator:test .` exits 0; `docker run --rm hydradb-operator:test --version`-equivalent (the binary prints its version and exits) works; `docker inspect` shows `User` = `65532:65532`.
  QA scenarios: happy: build and run the image, assert a clean exit. failure: build with `--build-context bin=./nonexistent` and assert the build fails loudly rather than producing an empty image. Evidence `<attemptDir>/task-35-hydradb-operator.txt`
  Commit: Y | build: add multi-stage distroless Dockerfile for the operator and CLI
  Recommended task executor category: quick - one Dockerfile with a well-known pattern.

- [x] 36. Chart skeleton, Chart.yaml annotations, values.yaml and values.schema.json
  What to do / Must NOT do: Create `charts/hydradb-operator/` with `Chart.yaml` (apiVersion v2, name `hydradb-operator`, version `0.1.0`, appVersion `0.1.0`, `kubeVersion: ">=1.26.0-0"`, `home`/`sources` pointing at the Gitea repo and helmforge, the official icon, `artifacthub.io/*` annotations, `helmforge.dev/maturity`, `helmforge.dev/signed: gpg+cosign`, `helmforge.dev/repository`, and `signKey` fingerprint+url placeholders), `values.yaml` (image repository/tag/digest, replicaCount, resources, nodeSelector, tolerations, affinity, `installScope: namespaced`, `monitoring.serviceMonitor.enabled: false`, `crds.install: true`, log level), and `values.schema.json` (draft-07) describing every value with `# --` doc comments in `values.yaml`. Must NOT hand-edit the chart version (Conventional Commits drives it), must NOT use `:latest`, must NOT leave any value undocumented.
  Closes: GAP-1, GAP-15
  Parallelization: Wave 6 | Blocked by: 31 | Blocks: 37-39
  Context (executor has NO interview context - be exhaustive): helmforge requires Chart.yaml + values.yaml + values.schema.json + templates/ + tests/ + ci/ + examples/ + docs/ + README.md + DESIGN.md + icon, and its CI runs `helm lint --strict`, `helm unittest`, `kubeconform -strict`, `ah lint`, `kubescape scan`.
  Acceptance criteria (agent-executable): `helm lint --strict charts/hydradb-operator` exits 0; `helm template charts/hydradb-operator` exits 0; every key in `values.yaml` appears in `values.schema.json`.
  QA scenarios: happy: lint + template with defaults exit 0. failure: set `installScope: bogus` and assert schema validation fails. Evidence `<attemptDir>/task-36-hydradb-operator.txt`
  Commit: Y | feat(chart): add the hydradb-operator chart skeleton with schema and annotations
  Recommended task executor category: unspecified-high - helmforge conformance is a hard external contract.

- [x] 37. Chart templates: operator Deployment, RBAC, ServiceAccount and CRDs
  What to do / Must NOT do: Add `templates/{_helpers.tpl,deployment.yaml,serviceaccount.yaml,rbac.yaml,service.yaml,servicemonitor.yaml,NOTES.txt}` plus the three CRDs copied into `crds/`. RBAC renders a Role/RoleBinding when `installScope: namespaced` and a ClusterRole/ClusterRoleBinding when `cluster`, sourced from the xtask-generated `deploy/rbac/`. The Deployment runs one replica, mounts no secrets, sets `securityContext` (non-root, read-only rootfs, no privilege escalation), resources, and probes `/healthz` on 8080. Must NOT template the CRDs (they live in `crds/` verbatim), must NOT ship a webhook, must NOT hardcode a namespace.
  Closes: GAP-1
  Parallelization: Wave 6 | Blocked by: 32, 36 | Blocks: 38-39
  Context (executor has NO interview context - be exhaustive): Helm's `crds/` are install-only and never upgraded - the chart must therefore also document the CRD upgrade path in NOTES/README.
  Acceptance criteria (agent-executable): `helm template charts/hydradb-operator` exits 0 and `helm template --set installScope=cluster` renders a ClusterRole; `kubeconform -strict` passes on the rendered output; `crds/` contains exactly three CRD files identical to `deploy/crds/`.
  QA scenarios: happy: render in both scopes and validate with kubeconform. failure: `helm template --set installScope=bogus` fails schema validation. Evidence `<attemptDir>/task-37-hydradb-operator.txt`
  Commit: Y | feat(chart): add operator deployment, RBAC and CRD templates
  Recommended task executor category: unspecified-high - RBAC scope switching plus CRD lifecycle correctness.

- [ ] 38. `installScope` namespaced/cluster parity with the operator config
  What to do / Must NOT do: Make `installScope` change BOTH the RBAC and the operator's own config: `namespaced` passes the operator's namespace and a watch scope limited to it; `cluster` grants cluster-wide watches. The controller must read the scope from env (`OPERATOR_SCOPE`) and build its `Api`s accordingly. Must NOT require two different images, must NOT silently ignore `installScope` in the controller.
  Closes: GAP-1
  Parallelization: Wave 6 | Blocked by: 13, 37 | Blocks: 39
  Context (executor has NO interview context - be exhaustive): kopiur's `installScope` (namespaced default, cluster opt-in) is the model, with namespaced being the safer default.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; a test asserts `OPERATOR_SCOPE=namespaced` builds a namespaced `Api` and `cluster` builds an all-namespaces one.
  QA scenarios: happy: both scopes build the expected `Api`. failure: an invalid `OPERATOR_SCOPE` exits non-zero at startup naming the valid values. Evidence `<attemptDir>/task-38-hydradb-operator.txt`
  Commit: Y | feat(controller): honour installScope from the chart
  Recommended task executor category: quick - one env-driven branch plus a test.

- [ ] 39. Chart tests, ci/ values and examples/ (helmforge conformance)
  What to do / Must NOT do: Add `tests/` (`helm unittest` suites asserting the Deployment image, replicas, RBAC kind per scope, and the absence of a ServiceMonitor by default), `ci/*-values.yaml` (at least a default, a cluster-scope and a ServiceMonitor-enabled set), `examples/{values-development.yaml,values-production.yaml}`, `docs/`, `README.md` (helm-docs) and `DESIGN.md`. Must NOT put tests in `templates/`, must NOT reference upstream HydraDB values that the operator does not expose.
  Closes: GAP-15
  Parallelization: Wave 6 | Blocked by: 36-38 | Blocks: 41, W9
  Context (executor has NO interview context - be exhaustive): helmforge CI runs `helm unittest` only when `tests/` exists and `helm template` against every `ci/*.yaml`.
  Acceptance criteria (agent-executable): `helm unittest charts/hydradb-operator` exits 0; `helm template` succeeds for every file in `ci/`; `helm-docs` regenerates `README.md` with no diff.
  QA scenarios: happy: unittest + template-all-ci exit 0. failure: remove the ServiceMonitor guard and assert a unittest case fails. Evidence `<attemptDir>/task-39-hydradb-operator.txt`
  Commit: Y | test(chart): add helm unittest suites, ci values and examples
  Recommended task executor category: quick - mechanical test fixtures.

- [ ] 40. Kaniko PaC image pipeline with dual tag and dual registry
  What to do / Must NOT do: Add `.tekton/hydradb-operator-image-build.yaml` as a PipelineRun mirroring the existing kaniko pattern (on-event `[push]`, on-target-branch `[main]`, on-path-change `[crates/**, charts/**, Dockerfile, .tekton/hydradb-operator-image-build.yaml]`, `nodeSelector {site: svt}`, SA `default`, `repo` workspace 1 Gi RWX `ceph-cephfs`, `basic-auth` = `{{ git_auth_secret }}`, `docker-credentials` = `docker-config-kaniko-oss`), with a `fetch-source` step (alpine/git 2.54.0 + the sed remap) and a `kaniko` step pushing BOTH `harbor.kubexa.tech/oss/hydradb-operator:<tag>` and `ghcr.io/kubexa/hydradb-operator:<tag>`, where `<tag>` is `v<semver>` from a committed `.version` plus the short SHA. Tail taskRef to the shared `eso-force-sync` Task. Must NOT push `:latest`, must NOT run the Rust build inside kaniko (kaniko builds the Dockerfile which uses the `bin=` export stage or builds in-stage).
  Closes: GAP-2, GAP-5
  Parallelization: Wave 6 | Blocked by: 35 | Blocks: 41, 54
  Context (executor has NO interview context - be exhaustive): the existing pattern uses `ghcr.io/osscontainertools/kaniko:v1.28.5`, `--cache=true --cache-repo=...`, `--skip-tls-verify-registry=harbor.kubexa.tech`, `--snapshot-mode=redo`, ca-bundle from ConfigMap `kaniko-ca-bundle` at `/kaniko/ca/ca-certificates.crt`. Dual-registry has NO precedent in the repo - hence todo 47's spike.
  Acceptance criteria (agent-executable): `kubectl apply --dry-run=server -f .tekton/hydradb-operator-image-build.yaml` exits 0; the file contains exactly two `--destination=` arguments and neither ends in `:latest`.
  QA scenarios: happy: dry-run server-side validation passes. failure: assert a grep for `:latest` in the file returns nothing. Evidence `<attemptDir>/task-40-hydradb-operator.txt`
  Commit: Y | ci: add the kaniko image pipeline with dual tag and dual registry
  Recommended task executor category: unspecified-low - one manifest closely mirroring an existing one.

- [ ] 41. SBOM, cosign signing and Artifact Hub metadata
  What to do / Must NOT do: Add the signing/SBOM steps: generate a CycloneDX SBOM for the image (`syft`) and the chart, run `grype` as a non-blocking report in the first cut, sign the image and the packaged chart with `cosign sign`, and add `artifacthub-repo.yml`-compatible metadata plus the `artifacthub.io/images` / `artifacthub.io/crds` annotations. Document in `docs/signing.md` who holds the GPG fingerprint and the cosign key. Must NOT claim SOC2 or any compliance certification, must NOT make `grype` failures block the first release.
  Closes: GAP-15
  Parallelization: Wave 6 | Blocked by: 39-40 | Blocks: W10
  Context (executor has NO interview context - be exhaustive): helmforge requires GPG + Cosign signatures; the keys are human-held, so the agent's only assertion is that `cosign verify` succeeds against the published artifact.
  Acceptance criteria (agent-executable): `cosign verify --key <pub> ghcr.io/kubexa/hydradb-operator:<tag>` exits 0 for a published tag; the SBOM file exists and `syft --version` is recorded.
  QA scenarios: happy: `cosign verify` on the published image succeeds. failure: `cosign verify` against a deliberately unsigned tag fails. Evidence `<attemptDir>/task-41-hydradb-operator.txt`
  Commit: Y | ci: add SBOM generation and cosign signing
  Recommended task executor category: unspecified-low - pipeline steps plus documentation.

### Wave 7 - talos-stack rails: Harbor, Gitea, credentials, Renovate, Flux, and the operand image

- [x] 42. `terraform/environments/harbor/oss.tf`
  What to do / Must NOT do: Create `terraform/environments/harbor/oss.tf` mirroring `webapps.tf`: `harbor_project.oss` (name `oss`, public, vulnerability_scanning with Trivy, `auto_sbom_generation: true`, a storage quota), `harbor_retention_policy` (Daily; always-retain `latest`; keep 10 most recently pushed), `harbor_immutable_tag_rule` (tag_matching `v*`, repo_matching `**`), `random_password` + `harbor_robot_account.oss_cicd` (project level, push+pull+list+read artifact+list tag) and a `pull_robot`, plus `vault_kv_secret_v2.oss_cicd` writing `kv/apps/harbor/oss-cicd/{username,password}`. Must NOT use uppercase `OSS` (Harbor rejects uppercase project names), must NOT modify the existing project files.
  Closes: GAP-4
  Parallelization: Wave 7 | Blocked by: none | Blocks: 44
  Context (executor has NO interview context - be exhaustive): one .tf file per project is the established convention; the shared `terraform_bot` system robot also gains a project-level permission block for `oss`.
  Acceptance criteria (agent-executable): `tofu fmt -check -recursive terraform/environments/harbor` exits 0 and `tofu validate` exits 0 in that directory; the file contains `harbor_project.oss` with `name = "oss"`.
  QA scenarios: happy: `tofu validate` passes. failure: set the name to `OSS` and assert `tofu plan` (or a unit assertion) rejects uppercase. Evidence `<attemptDir>/task-42-hydradb-operator.txt`
  Commit: Y | feat(harbor): add the oss project, retention, robots and vault writeback
  Recommended task executor category: quick - one file closely mirroring an existing one.

- [x] 43. `terraform/environments/gitea/oss.tf`
  What to do / Must NOT do: Create `terraform/environments/gitea/oss.tf` with `gitea_org.oss` (private), `gitea_team.oss_admins` (permission admin, include_all_repositories true) and `gitea_team.oss_devs` (write + units_map), a `gitea_repository_webhook.pac_hydradb_operator` pointing at `http://pipelines-as-code-controller.tekton-pipelines:8080` with the 9 standard events and `secret = var.gitea_webhook_secret`, and `gitea_repository_collaborator.renovate_hydradb_operator` (permission write) plus `gitea_team_membership` for the Renovate bot. The webhook must reference the repository by LITERAL name string `hydradb-operator` (the repo is created out-of-band, not by Tofu). Must NOT create a `gitea_repository` resource (user rule: repositories are not Tofu-managed), must NOT use the default provider for the bot.
  Closes: GAP-3
  Parallelization: Wave 7 | Blocked by: none | Blocks: 44, 45
  Context (executor has NO interview context - be exhaustive): PaC does NOT auto-register Gitea webhooks, so Tofu owns it; team membership alone reports permission `none`, so the collaborator grant is mandatory.
  Acceptance criteria (agent-executable): `tofu fmt -check -recursive terraform/environments/gitea` and `tofu validate` exit 0; the file contains no `gitea_repository.` resource block.
  QA scenarios: happy: `tofu validate` passes. failure: assert that removing the collaborator makes a documented note in the file about PaC requiring repo write. Evidence `<attemptDir>/task-43-hydradb-operator.txt`
  Commit: Y | feat(gitea): add the oss org, teams, PaC webhook and renovate access
  Recommended task executor category: quick - one file mirroring an existing one.

- [ ] 44. Kaniko credential ExternalSecret covering Harbor and GHCR
  What to do / Must NOT do: Add `apps/secrets/external/tekton-tasks/docker-config-kaniko-oss.yaml` (ns `tekton-tasks`, refreshInterval 1h, ClusterSecretStore `vault`, `template.engineVersion: v2`) templating `config.json` with TWO `auths` entries: `harbor.kubexa.tech` from `kv/apps/harbor/oss-cicd/{username,password}` and `ghcr.io` from the GitHub PAT path; register the file in `apps/secrets/external/tekton-tasks/kustomization.yaml`. Must NOT inline a credential, must NOT reuse the `home-ops` robot.
  Closes: GAP-5
  Parallelization: Wave 7 | Blocked by: 42-43 | Blocks: 45
  Context (executor has NO interview context - be exhaustive): the templated variant in the repo assembles `auth` as `base64(username:password)`; the GHCR PAT path must be decided in todo 49 and documented here.
  Acceptance criteria (agent-executable): `kubectl apply --dry-run=server -f apps/secrets/external/tekton-tasks/docker-config-kaniko-oss.yaml` exits 0; the kustomization lists the new file; the rendered template contains both `harbor.kubexa.tech` and `ghcr.io`.
  QA scenarios: happy: `eso-force-sync` then assert the Secret has a `config.json` key with two auths. failure: with the PAT missing the ExternalSecret reports `SecretSyncedError` and the pipeline fails loudly. Evidence `<attemptDir>/task-44-hydradb-operator.txt`
  Commit: Y | feat(secrets): add the oss kaniko credential covering Harbor and GHCR
  Recommended task executor category: quick - one ExternalSecret plus a kustomization entry.

- [ ] 45. Renovate job and config for the `oss` org
  What to do / Must NOT do: Add `infrastructure/controllers/renovate-operator/job-oss.yaml` (a `RenovateJob` for org `oss`, a distinct staggered cron slot, `discoveryFilters: ["oss/*"]`, provider gitea, secretRef `renovate-config-oss`) and `apps/secrets/external/renovate-operator/renovate-config-oss.yaml` (templating RENOVATE_TOKEN + GITHUB_COM_TOKEN + a `config.json` with `autodiscoverFilter: ["oss/*"]`), then register both in their kustomizations. Must NOT reuse another org's cron slot, must NOT add cargo-specific config (the root `renovate.json` already enables the `cargo` manager).
  Closes: GAP-6
  Parallelization: Wave 7 | Blocked by: 43 | Blocks: none
  Context (executor has NO interview context - be exhaustive): existing schedules are '20 * * * *', '35 * * * *', '50 * * * *' - pick a fourth distinct slot; RenovateJob image is `ghcr.io/renovatebot/renovate:44.151.5`.
  Acceptance criteria (agent-executable): `kubectl apply --dry-run=server` exits 0 for both files; both kustomizations list them; the cron slot differs from the existing three.
  QA scenarios: happy: dry-run passes and the slot is unique. failure: reuse an existing slot and assert the plan's own check (a documented grep) flags it. Evidence `<attemptDir>/task-45-hydradb-operator.txt`
  Commit: Y | feat(renovate): add the oss org job and config secret
  Recommended task executor category: quick - two small manifests.

- [ ] 46. Flux delivery: GitRepository, Kustomization, OCIRepository and HelmRelease
  What to do / Must NOT do: Add the product-side `flux/` directory in the operator repo (an `OCIRepository` pointing at `oci://harbor.kubexa.tech/oss/hydradb-operator` with `ref.semver`, plus a `HelmRelease` with install/upgrade remediation retries 3, `values.installScope` and the CRD upgrade handling documented), and the talos-stack side `clusters/default/hydradb-operator.yaml` holding a `GitRepository` for `http://gitea-http.gitea:3000/oss/hydradb-operator.git` and a `Kustomization {sourceRef, path: ./flux/, dependsOn: [sources, namespaces], prune: true, wait: false}`. Must NOT point the Flux source at GHCR (Harbor is the source of truth), must NOT use `ref.tag` floating.
  Closes: GAP-7
  Parallelization: Wave 7 | Blocked by: 42, 47 | Blocks: W9
  Context (executor has NO interview context - be exhaustive): the external-repo product pattern in talos-stack uses a dedicated GitRepository + a `./flux/` Kustomization; kopiur's OCIRepository+HelmRelease is the operator precedent.
  Acceptance criteria (agent-executable): `flux-schema-catalog` `grep_schema`/`get_schema` confirms every field used; `kubectl apply --dry-run=server` exits 0 for all four objects; the OCIRepository URL contains `harbor.kubexa.tech`.
  QA scenarios: happy: dry-run passes and `flux reconcile` marks the Kustomization Ready. failure: point the OCIRepository at a non-existent tag and assert the HelmRelease reports a source error rather than applying an empty chart. Evidence `<attemptDir>/task-46-hydradb-operator.txt`
  Commit: Y | feat(flux): add GitRepository, Kustomization, OCIRepository and HelmRelease delivery
  Recommended task executor category: unspecified-low - four small manifests with a strict field contract.

- [ ] 47. SPIKE: prove the dual-registry kaniko push, else split it
  What to do / Must NOT do: Run a throwaway PipelineRun (or a local `kaniko` invocation against the cluster) proving that `ghcr.io/osscontainertools/kaniko:v1.28.5` can push to BOTH `harbor.kubexa.tech/oss/...` and `ghcr.io/kubexa/...` from one step with a docker config holding two `auths`. If it cannot, split the push into a second kaniko step (or a `crane`/`skopeo` copy step) and record the decision. Write the outcome to `docs/decisions/0005-dual-registry-push.md`. Must NOT leave the spike as an unproven assumption, must NOT add a second registry that is not actually reachable.
  Closes: GAP-5
  Parallelization: Wave 7 | Blocked by: 40, 44 | Blocks: 46
  Context (executor has NO interview context - be exhaustive): GHCR is pull-only in this cluster today - there is NO dual-registry precedent, so this is the single highest-risk unproven step in the delivery path.
  Acceptance criteria (agent-executable): the spike's PipelineRun succeeds and both registries return the pushed tag (`crane manifest` or `skopeo inspect` exits 0 for both); `docs/decisions/0005-dual-registry-push.md` records the working method.
  QA scenarios: happy: both `skopeo inspect` calls succeed for the same tag. failure: force a bad GHCR credential and assert the step fails loudly (no silent single-registry success). Evidence `<attemptDir>/task-47-hydradb-operator.txt`
  Commit: Y | docs: prove and record the dual-registry push method
  Recommended task executor category: deep-low - an unproven external integration needing a real experiment and a fallback.

- [ ] 48. Publish a public operand image and pin the default digest
  What to do / Must NOT do: Resolve GAP-2 for users outside this cluster: build and publish the HydraDB operand image from the upstream source at the pinned revision to `harbor.kubexa.tech/oss/hydradb` and mirror it to `ghcr.io/kubexa/hydradb`, then set the chart/CRD default `image.repository` to that public location with a pinned digest, documenting that users may override `spec.image` to point at their own build. Must NOT modify upstream hydradb, must NOT publish a forked image (helmforge forbids forks/wrappers), must NOT leave the private ECR reference in any default.
  Closes: GAP-2
  Parallelization: Wave 7 | Blocked by: 40 | Blocks: 46, W9
  Context (executor has NO interview context - be exhaustive): the only upstream image is a private ECR staging repo (`525753068359.dkr.ecr.us-east-1.amazonaws.com/staging/hydradb`), which is unusable by third parties; the operand's Dockerfile builds both binaries with features `server-runtime,indexer-runtime,experimental-cypher-engine,otlp` on ubuntu:24.04 with libcypher-parser + libgraphblas.
  Acceptance criteria (agent-executable): `skopeo inspect docker://harbor.kubexa.tech/oss/hydradb:<tag>` exits 0 and reports a digest; `grep -rn "ecr.us-east-1" charts/ crates/ deploy/` returns nothing; the default image field carries a digest, not a tag.
  QA scenarios: happy: pull the published image in a scratch pod and run `--version`. failure: assert no default anywhere references the private ECR. Evidence `<attemptDir>/task-48-hydradb-operator.txt`
  Commit: Y | build: publish a public hydradb operand image and pin the default digest
  Recommended task executor category: unspecified-high - a real image build with licensing and provenance implications.

### Wave 8 - GitHub rails and the public PaC route

- [x] 49. `terraform/environments/github/` OpenTofu root
  What to do / Must NOT do: Create the root (`providers.tf` pinning `integrations/github ~> 6.13` plus the vault provider, an S3 backend at `github/terraform.tfstate`, `variables.tf`, `outputs.tf`, `vault.tf`) that ADOPTS the existing `kubexa` org via `github_organization_settings` + an `import` block (numeric org id, `billing_email`) and writes the GitHub PAT path into OpenBao (`vault_kv_secret_v2` for `kv/apps/tekton-tasks/github-webhook-config` with keys `provider.token` + `webhook.secret`, plus the documented PAT path). Must NOT attempt to create the org (no provider resource exists and github.com has no org-create API), must NOT put the PAT literal in any `.tf`.
  Closes: GAP-12, GAP-14
  Parallelization: Wave 8 | Blocked by: none | Blocks: 50-53
  Context (executor has NO interview context - be exhaustive): the org must be created ONCE out-of-band at github.com/organizations/new by the user; required classic PAT scopes are `admin:org, repo, write:packages, read:packages`; secret values land in Tofu state in plaintext, so they must be fed from OpenBao data sources.
  Acceptance criteria (agent-executable): `tofu fmt -check -recursive terraform/environments/github` and `tofu validate` exit 0; `grep -rnE "ghp_|github_pat_" terraform/environments/github` returns nothing; an `import` block for the org is present.
  QA scenarios: happy: `tofu validate` passes. failure: a PAT literal anywhere in the root fails the grep gate. Evidence `<attemptDir>/task-49-hydradb-operator.txt`
  Commit: Y | feat(github): add the tofu root adopting the kubexa org
  Recommended task executor category: unspecified-high - a new provider, an import, and a credential-handling contract.

- [ ] 50. GitHub repository, ruleset, teams and the GHCR secret
  What to do / Must NOT do: Add the repo-side resources: `github_repository` (`hydradb-operator`, private, no auto_init), `github_repository_ruleset` (target branch, `refs/heads/main`, active, requiring a PR + the CI status check), `github_team`/`github_team_membership`/`github_team_repository`, `github_membership` for the bot, `github_actions_secret` for the GHCR PAT, and `github_repository_vulnerability_alerts`. Must NOT use `github_branch_protection` (rulesets are the successor), must NOT create `.github/workflows/`, must NOT hardcode a secret value.
  Closes: GAP-12
  Parallelization: Wave 8 | Blocked by: 49 | Blocks: 51-53
  Context (executor has NO interview context - be exhaustive): secret values are encrypted client-side but the plaintext lands in Tofu state, so the value must come from a `data.vault_kv_secret_v2`; use `lifecycle { ignore_changes = [remote_updated_at] }`.
  Acceptance criteria (agent-executable): `tofu validate` exits 0; the ruleset resource targets `refs/heads/main` and is `enforcement = "active"`; no `.github/workflows` path exists in the repo.
  QA scenarios: happy: `tofu validate` passes and the ruleset lists the CI check. failure: assert no `github_branch_protection` resource exists anywhere in the root. Evidence `<attemptDir>/task-50-hydradb-operator.txt`
  Commit: Y | feat(github): add the repository, ruleset, teams and GHCR secret
  Recommended task executor category: unspecified-low - standard provider resources with a documented secret pattern.

- [ ] 51. GitHub PaC webhook, `Repository` CR and the webhook-config ExternalSecret
  What to do / Must NOT do: Add `github_repository_webhook` (repo-level, url the public `pac.kubexa.tech` endpoint, `content_type: json`, the secret from `var.gitea_webhook_secret`-equivalent, events push + pull_request + issue_comment + pull_request_review), `infrastructure/controllers/tekton/repositories/hydradb-operator-github.yaml` (a `Repository` CR: `spec.url: https://github.com/kubexa/hydradb-operator`, `spec.git_provider {type: github, url: https://github.com, secret {name: github-webhook-config, key: provider.token}, webhook_secret {name: github-webhook-config, key: webhook.secret}}`), and the `github-webhook-config` ExternalSecret in ns `tekton-tasks`. Must NOT rely on annotations alone (a `Repository` CR is mandatory), must NOT create an org-level webhook, must NOT modify the five existing `Repository` CRs.
  Closes: GAP-12, GAP-14
  Parallelization: Wave 8 | Blocked by: 49-50 | Blocks: 54
  Context (executor has NO interview context - be exhaustive): a mutating webhook enforces exactly ONE `Repository` per URL and it must live in the execution namespace; PaC credentials are NOT in the ConfigMap but in per-`Repository` secretRefs; the OpenBao path is `kv/apps/tekton-tasks/github-webhook-config` with keys `provider.token` + `webhook.secret`.
  Acceptance criteria (agent-executable): `kubectl apply --dry-run=server` exits 0 for all three objects; the `Repository` CR names the GitHub URL and the correct secret keys; the webhook URL contains `pac.kubexa.tech`.
  QA scenarios: happy: dry-run passes and PaC matches the CR to a push event. failure: an unset `webhook.secret` yields a `Repository` in an error state (assert the status), not a silent no-op. Evidence `<attemptDir>/task-51-hydradb-operator.txt`
  Commit: Y | feat(tekton): add the GitHub Repository CR, webhook and config secret
  Recommended task executor category: unspecified-low - three manifests with a documented shape.

- [ ] 52. Public PaC route at `pac.kubexa.tech`
  What to do / Must NOT do: Add the HTTPRoute (existing Cilium Gateway, hostname `pac.kubexa.tech`, path `/` prefix -> Service `pipelines-as-code-controller` port 8080 in `tekton-pipelines`), the cert-manager `Certificate`/issuer reference, the `external-dns` target annotation pinned to `79.76.124.104`, and a `NetworkPolicy` restricting inbound to GitHub's published webhook source ranges (fetched, not guessed). Mark the gateway name, issuer and DNS zone as needs-platform-confirm in a comment. Must NOT expose the controller beyond port 8080, must NOT add TLS passthrough, must NOT reuse the authenticated dashboard host.
  Closes: GAP-13
  Parallelization: Wave 8 | Blocked by: 49 | Blocks: 54
  Context (executor has NO interview context - be exhaustive): the PaC controller is cluster-internal only today (`http://pipelines-as-code-controller.tekton-pipelines:8080`) and the only Tekton route in the tree is the dashboard; the public entry is 79.76.124.104 -> 10.0.1.200; use `flux-schema-catalog` for the HTTPRoute/Gateway fields.
  Acceptance criteria (agent-executable): `kubectl apply --dry-run=server` exits 0 for every object; `curl -sI https://pac.kubexa.tech` returns a non-404 from the controller (an unauthenticated POST returns 401/400, not a connection error).
  QA scenarios: happy: a signed GitHub webhook delivery gets a 2xx and creates a PipelineRun. failure: a request from outside the allowed CIDRs is dropped by the NetworkPolicy. Evidence `<attemptDir>/task-52-hydradb-operator.txt`
  Commit: Y | feat(tekton): expose the PaC controller at pac.kubexa.tech
  Recommended task executor category: deep-low - new public surface with TLS, DNS and a source-range NetworkPolicy.

- [ ] 53. GitHub image pipeline firing on push to `main`
  What to do / Must NOT do: Add `.tekton/github-hydradb-operator-image-build.yaml` - a SEPARATE PipelineRun from the Gitea one - with `on-event: "[push]"`, `on-target-branch: "[main]"`, cloning `https://github.com/kubexa/hydradb-operator` (no sed remap needed), pushing to BOTH `harbor.kubexa.tech/oss/hydradb-operator:<tag>` and `ghcr.io/kubexa/hydradb-operator:<tag>` using the method proven in todo 47. Must NOT modify the Gitea pipeline, must NOT add pull-request triggers, must NOT require the Checks API.
  Closes: GAP-12
  Parallelization: Wave 8 | Blocked by: 47, 51 | Blocks: W9
  Context (executor has NO interview context - be exhaustive): PAT mode reports status as PR comments and supports no `/retest`; `{{ git_auth_secret }}` works on GitHub with token key `git-provider-token`; the trigger is push-to-main only.
  Acceptance criteria (agent-executable): `kubectl apply --dry-run=server` exits 0; the file's annotations contain `on-event: "[push]"` and `on-target-branch: "[main]"`; a real push to `main` produces a succeeded PipelineRun.
  QA scenarios: happy: push to main -> PipelineRun Succeeded and both registries have the new tag. failure: push to a branch other than main produces no PipelineRun. Evidence `<attemptDir>/task-53-hydradb-operator.txt`
  Commit: Y | ci: add the GitHub push-to-main image pipeline
  Recommended task executor category: unspecified-low - one manifest mirroring an existing pipeline.

### Wave 9 - end-to-end verification and CI gates

- [ ] 54. e2e crate: kind cluster, Helm install and the full scenario
  What to do / Must NOT do: Flesh out `crates/e2e` to (a) create an ephemeral `kind` cluster (`hydradb-operator-e2e`), (b) `helm install` the chart from the local path with a Rook-free object-store stub (a MinIO or an `s3`-compatible fake deployed by the test), (c) apply a `GraphCluster` + `GraphNodePool` + `GraphIndexer`, (d) assert `kubectl wait --for=condition=Ready graphcluster/<name> --timeout=420s`, (e) open a Bolt connection on 7687 and run `RETURN 1`, (f) compare the StatefulSet's `GRAPH_BOLT_NODE_ADDRESSES` env against the live pod list via `kubectl get ... -o jsonpath`, (g) delete with `Retain` and assert the finalizer is gone, and (h) `try_client()` returns `Ok(None)` (skip) instead of panicking when no cluster is reachable. Must NOT require the operator to be pre-installed, must NOT depend on the talos-stack cluster, must NOT assert on log text.
  Closes: GAP-10
  Parallelization: Wave 9 | Blocked by: 5, 26, 46, 53 | Blocks: 55, 57
  Context (executor has NO interview context - be exhaustive): kopiur's e2e crate drives a Helm-deployed operator in an ephemeral kind cluster with an isolated kubeconfig and a 420 s default timeout (> the 290 s watch reconnect).
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator-e2e -- --ignored --nocapture` exits 0 on a machine with Docker; the test prints each of the eight steps.
  QA scenarios: happy: the full scenario passes and a Bolt `RETURN 1` returns 1. failure: stop the object store mid-test and assert the cluster reports `Stalled` rather than hanging past the timeout. Evidence `<attemptDir>/task-54-hydradb-operator.txt`
  Commit: Y | test(e2e): add the kind end-to-end scenario
  Recommended task executor category: deep-low - a multi-step live integration test where the failure modes are environmental.

- [ ] 55. `mise` task surface and the local verification runner
  What to do / Must NOT do: Add the `mise` tasks (`fmt`, `lint`, `test`, `test-e2e`, `build`, `check-drift`, `docker-build`, `docs`) and a `scripts/verify.sh` that runs the whole local gate (fmt check, clippy `-D warnings`, `cargo test --workspace`, `cargo xtask gen-all --check`, `helm lint --strict`, `helm unittest`, `kubeconform -strict` on the rendered chart). Must NOT add a `justfile` (the platform convention is mise), must NOT make `verify.sh` require cluster credentials.
  Closes: GAP-10
  Parallelization: Wave 9 | Blocked by: 3, 34, 39 | Blocks: 57
  Context (executor has NO interview context - be exhaustive): talos-stack uses mise and has no justfile; the operand uses a justfile but that is a different repo.
  Acceptance criteria (agent-executable): `mise run verify` (or `bash scripts/verify.sh`) exits 0 on a clean checkout with no cluster; `mise tasks` lists every task.
  QA scenarios: happy: `scripts/verify.sh` exits 0. failure: introduce a formatting error and assert the script fails at the fmt step naming the file. Evidence `<attemptDir>/task-55-hydradb-operator.txt`
  Commit: Y | chore: add the mise task surface and the local verify script
  Recommended task executor category: quick - shell plus mise config.

- [ ] 56. Repo CI gate in PaC: test before build
  What to do / Must NOT do: Add `.tekton/hydradb-operator-ci.yaml` (or extend the existing `lint.yaml` pattern with an `on-path-change` gate for `crates/**`, `charts/**`, `Cargo.toml`, `Cargo.lock`) that runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `cargo xtask gen-all --check`, `helm lint --strict`, `helm unittest` and `kubeconform -strict` in a Rust toolchain image, and make the image-build pipeline depend on it. Must NOT duplicate the whole build in the test pipeline, must NOT run envtest without the `setup-envtest` binaries being fetched.
  Closes: GAP-10
  Parallelization: Wave 9 | Blocked by: 34, 39, 55 | Blocks: 57
  Context (executor has NO interview context - be exhaustive): the repo's own Rust CI is net-new for helmforge/talos-stack; `rust:1.89` plus `setup-envtest` is sufficient; the existing `.tekton/lint.yaml` shows the trigger/workspace conventions.
  Acceptance criteria (agent-executable): a push touching `crates/**` triggers the pipeline; the pipeline exits 0; a deliberately broken test makes it fail.
  QA scenarios: happy: green pipeline on a clean commit. failure: push a failing test and assert the pipeline reports failure (not a silent skip). Evidence `<attemptDir>/task-56-hydradb-operator.txt`
  Commit: Y | ci: add the Rust test and chart lint gate
  Recommended task executor category: unspecified-low - one pipeline mirroring an existing trigger pattern.

- [ ] 57. Wire the full release path end to end and record the evidence
  What to do / Must NOT do: Exercise the complete path once and capture evidence: commit to the Gitea repo -> PaC CI green -> kaniko builds and pushes both registries with `v<semver>` + `<sha>` -> `cosign verify` passes -> Flux reconciles the `OCIRepository`/`HelmRelease` -> the operator installs -> a `GraphCluster` reaches `Ready` -> `kubectl hydradb doctor` exits 0 -> the GitHub mirror pipeline also succeeds. Record each step's command and output under `.omo/evidence/`. Must NOT declare success from a partial run, must NOT skip the GHCR leg.
  Closes: GAP-2, GAP-7, GAP-12
  Parallelization: Wave 9 | Blocked by: 46, 48, 53, 56 | Blocks: W10
  Context (executor has NO interview context - be exhaustive): this is the integration proof that every wave's output composes; the cluster is NOT to be queried for design purposes but IS the target of this release exercise.
  Acceptance criteria (agent-executable): every step's command exits 0 and its output is stored under `.omo/evidence/`; `kubectl hydradb doctor` exits 0; `cosign verify` exits 0 for the published image.
  QA scenarios: happy: the whole path succeeds and the evidence files exist. failure: break the GHCR credential and assert the path fails at the mirror step with a named error. Evidence `<attemptDir>/task-57-hydradb-operator.txt`
  Commit: Y | docs: record the end-to-end release path evidence
  Recommended task executor category: unspecified-high - the integration proof spanning five systems.

### Wave 10 - documentation and decision records

- [ ] 58. `docs/`: architecture, CRD reference, install and operations guides
  What to do / Must NOT do: Write `docs/architecture.md` (the three CRDs, the object-store model, discovery, the Bolt routing table, and why the operand needs no bootstrap), `docs/install.md` (chart install via Flux and via Helm, `installScope`, CRD upgrade path), `docs/operations.md` (scaling, `deletionPolicy` semantics, Retain vs Delete, the absence of backup CRDs, the ServiceMonitor opt-in, `doctor`), `docs/crd-reference.md` (generated), and `docs/environments.md` (the private-cloud/Rook, AWS/EBS+S3, and generic S3 matrix from todo 48). Must NOT document a feature the plan excludes (backups, federation, autoscaling), must NOT claim compliance certifications.
  Closes: GAP-11
  Parallelization: Wave 10 | Blocked by: 33, 48 | Blocks: 60
  Context (executor has NO interview context - be exhaustive): IS-11 requires one CRD surface across environments; the manual matrix is the honest boundary of what an agent can prove.
  Acceptance criteria (agent-executable): every command in `docs/install.md` and `docs/operations.md` is copy-paste runnable and exits 0 against the e2e cluster; `docs/crd-reference.md` matches `cargo xtask gen-docs` output.
  QA scenarios: happy: run each documented command verbatim and assert success. failure: a documented command referencing a removed field fails, proving the docs are checked. Evidence `<attemptDir>/task-58-hydradb-operator.txt`
  Commit: Y | docs: add architecture, install, operations and environment guides
  Recommended task executor category: writing - prose grounded in verified behaviour.

- [ ] 59. ADRs 0001-0005
  What to do / Must NOT do: Write `docs/decisions/0001-rust-operator-on-kube-rs.md`, `0002-split-crd-surface.md` (why GraphCluster + GraphNodePool + GraphIndexer rather than one CR), `0003-storage-and-object-store-portability.md` (Rook RGW OBC first-class, explicit S3 equally supported), `0004-deletion-policy-and-finalizer.md` (Retain default, `pruned-by` stamp), and `0005-dual-registry-push.md` (todo 47's outcome). Must NOT write an ADR that contradicts a decision in the plan, must NOT pad with prose.
  Closes: GAP-11
  Parallelization: Wave 10 | Blocked by: 47, 58 | Blocks: 60
  Context (executor has NO interview context - be exhaustive): kopiur's ADR set (0003 the Rust operator decision, 0006 mass-deletion protection) is the model for what an ADR should capture.
  Acceptance criteria (agent-executable): five files exist under `docs/decisions/`; each states Context, Decision, Consequences, and an explicit "what we deliberately did NOT do".
  QA scenarios: happy: every ADR has the four sections. failure: an ADR contradicting decision D10 (the split API) is flagged by the reviewer. Evidence `<attemptDir>/task-59-hydradb-operator.txt`
  Commit: Y | docs: add ADRs for the operator, API split, storage and deletion policy
  Recommended task executor category: writing - decisions grounded in the plan.

- [ ] 60. Chart README via helm-docs and DESIGN.md
  What to do / Must NOT do: Add `charts/hydradb-operator/README.md` generated by `helm-docs` from the `# --` comments and `charts/hydradb-operator/DESIGN.md` documenting the chart's shape, the CRD lifecycle (install-only `crds/`, the documented upgrade procedure), the security defaults and the helmforge conformance statement. Must NOT hand-edit the generated README, must NOT restate the CRD reference (link to `docs/crd-reference.md`).
  Closes: GAP-15
  Parallelization: Wave 10 | Blocked by: 58-59 | Blocks: none
  Context (executor has NO interview context - be exhaustive): helmforge requires a DESIGN.md and a helm-docs README per chart; it has zero operator artifacts today, so this is the first instance of these conventions.
  Acceptance criteria (agent-executable): `helm-docs` regenerates `README.md` with no diff; `DESIGN.md` documents the CRD upgrade procedure and the security defaults.
  QA scenarios: happy: `helm-docs` produces no diff. failure: add an undocumented value and assert `helm-docs` output changes (proving the README is generated). Evidence `<attemptDir>/task-60-hydradb-operator.txt`
  Commit: Y | docs(chart): add the generated README and DESIGN.md
  Recommended task executor category: writing - chart documentation against helmforge conventions.

- [x] 61. Harden the health/metrics server on axum
  - **What to do**: Add `axum = { version = "0.8", default-features = false, features = ["tokio", "http1"] }` to `[workspace.dependencies]` in the root `Cargo.toml` and consume it via `axum = { workspace = true }` in `crates/controller/Cargo.toml`. Replace the hand-rolled `std::net::TcpListener` HTTP loop in `crates/controller/src/run.rs` with a real axum server: a `Router` serving `GET /healthz` and `GET /readyz` (200, `ok\n`, text/plain), `GET /metrics` (200, the minimal Prometheus exposition, text/plain; version=0.0.4), and 404 for anything else. Keep `pub const HEALTH_ADDR = "0.0.0.0:8080"` and the EXACT shutdown contract from todo 13: await SIGTERM/SIGINT, then stop the server gracefully, then return `Ok(())`. Keep `Error::Client` and `Error::Server` as the error surface.
  - **Must NOT do**: Do NOT change the endpoints, the address, the shutdown contract or the metrics placeholder body (todo 25 owns real metrics). Do NOT touch `crates/api/**`. Do NOT remove or rename `Error::Client`/`Error::Server`. Do NOT add any dependency other than axum.
  - **Closes**: GAP-1 (completes the todo-13 deliverable to the plan's stated shape)
  - **Parallelization**: Wave 3 | Blocked by: 13 | Blocks: 25
  - **Context**: todo 13 shipped a `std::net::TcpListener` server because axum was missing from the todo-2 pin list. Two defects: (a) it deviates from the plan, which names an axum server; (b) accepted sockets are BLOCKING on Linux (non-blocking is set on the listener, not the accepted stream) and the code sets no read timeout, so one idle client stalls the single-threaded accept loop and every subsequent kubelet probe fails.
  - **Acceptance criteria**: `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo fmt --all --check`, `cargo deny check` all exit 0; `cargo tree --workspace -e features -i k8s-openapi | rg 'v1_'` prints exactly one line (`v1_33`); a test binds an ephemeral port, starts the axum server, and asserts `/healthz` and `/readyz` return 200 with body `ok\n`, `/metrics` returns 200 with `hydradb_operator_up 1`, and an unknown path returns 404.
  - **QA scenarios**: happy = the in-test axum server answers all three endpoints on an ephemeral port and the process still exits 1 with the client error when run with `KUBECONFIG=/nonexistent`; failure = request an unknown path and assert 404 (not 200, not a hang).
  - **Commit**: `fix(controller): serve health and metrics on axum with a bounded server`
  - **Recommended task executor category**: `unspecified-high` — a contained rewrite across two manifests plus one source file with a real HTTP test.

- [x] 62. Declare the `time` tokio feature explicitly
  What to do / Must NOT do: Add `"time"` to the `tokio` feature list in `[workspace.dependencies]` in the root `Cargo.toml`. `crates/controller/src/watcher.rs` calls `tokio::time::timeout`, but the workspace tokio pin declares only `["macros", "rt-multi-thread", "signal", "net", "io-util"]`; `time` currently arrives only through kube-runtime feature unification, which is a latent build fragility. Must NOT change any other dependency version or feature. Folded into todo 25's dispatch because both edit the root `Cargo.toml` and a parallel edit would race.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 14 | Blocks: none
  Context (executor has NO interview context - be exhaustive): found by the orchestrator during the todo 12/14/15/61 verification gate run.
  Acceptance criteria (agent-executable): `rg -n 'time' Cargo.toml` shows `time` in the tokio feature list; `cargo build --workspace` exits 0; `cargo tree -p hydradb-operator -e features -i tokio | rg 'tokio feature "time"'` shows the feature enabled.
  QA scenarios: happy: the build is green with the feature declared. failure: temporarily remove `time` and confirm `cargo build -p hydradb-operator` still succeeds only because of unification (documents why the declaration matters), then restore it. Evidence `<attemptDir>/task-62-hydradb-operator.txt`
  Commit: Y | chore(deps): declare the tokio time feature explicitly
  Recommended task executor category: quick - one-line dependency feature change.

- [x] 63. Wire the OTLP exporter behind the `otlp` feature
  What to do / Must NOT do: Add `opentelemetry-otlp` 0.32 (paired with `opentelemetry` 0.32) as an optional dependency of `crates/controller` and include it in the existing `otlp` feature (`otlp = ["dep:opentelemetry", "dep:tracing-opentelemetry", "dep:opentelemetry-otlp"]`); make `init_otlp()` actually build an OTLP span exporter and dial `OTEL_EXPORTER_OTLP_ENDPOINT`, installing it as the global tracer provider before the subscriber is built (`main.rs` is in scope for this todo only for that wiring). Must NOT add a hard dependency on a collector (the feature stays off by default), must NOT make a missing or unreachable collector fail startup, must NOT log credentials.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 25 | Blocks: 26
  Context (executor has NO interview context - be exhaustive): todo 25 shipped the `otlp` feature with only `opentelemetry` + `tracing-opentelemetry` (the span-collecting LAYER); without `opentelemetry-otlp` the spans are collected and silently dropped, so the plan's "OTLP export ... enabled by env" is only half-delivered. The operand pairs `tracing-opentelemetry` 0.33 with `opentelemetry` 0.32; `opentelemetry-otlp` 0.32 is the matching exporter.
  Acceptance criteria (agent-executable): `cargo build -p hydradb-operator --features otlp` exits 0 and `cargo build -p hydradb-operator` (no feature) exits 0; `cargo tree -p hydradb-operator --features otlp -e features -i opentelemetry-otlp` shows the crate present only with the feature; with the feature and an unreachable `OTEL_EXPORTER_OTLP_ENDPOINT` the binary still starts and serves `/healthz` 200.
  QA scenarios: happy: build with `--features otlp`, set `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317`, start the binary and assert it starts and serves `/healthz` 200 without panicking. failure: build WITHOUT the feature and assert `opentelemetry-otlp` is absent from the dependency tree and the binary starts unchanged. Evidence `<attemptDir>/task-63-hydradb-operator.txt`
  Commit: Y | feat(controller): export OTLP spans behind the off-by-default otlp feature
  Recommended task executor category: unspecified-low - one optional dependency plus the exporter init.

- [x] 64. Fix the rustls CryptoProvider panic: enable the kube `ring` feature
  What to do / Must NOT do: add `"ring"` to the `kube` feature list in `[workspace.dependencies]` of the root `Cargo.toml` (currently `["client", "runtime", "derive", "rustls-tls", "ws"]`). kube 4.x splits the TLS crypto provider out of `rustls-tls`, so with `default-features = false` and no provider feature rustls 0.23 has NEITHER `ring` NOR `aws-lc-rs` enabled and panics at `rustls-0.23.46/src/crypto/mod.rs:249` with "Could not automatically determine the process-level CryptoProvider" the moment a real kubeconfig builds a TLS connector (verified: the binary exits 101 with a synthetic valid kubeconfig, while a missing kubeconfig exits 1 cleanly because config inference fails before TLS). MUST NOT enable BOTH `ring` and `aws-lc-rs` (rustls then cannot auto-select either); MUST NOT call `CryptoProvider::install_default()` as a workaround while the feature graph is wrong; MUST NOT change any other version or feature.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 13 | Blocks: 26
  Context (executor has NO interview context - be exhaustive): the operator must start and serve `/healthz` 200 with a reachable apiserver; the panic happens at client construction, before any reconcile. A synthetic kubeconfig at `server: https://127.0.0.1:1` with `insecure-skip-tls-verify: true` and a fake token reproduces it deterministically without a cluster.
  Acceptance criteria (agent-executable): `cargo tree -p hydradb-operator -e features -i rustls | rg -c 'rustls feature "ring"'` >= 1; `cargo build --workspace` exits 0; running the binary with the synthetic kubeconfig NO LONGER panics (exit code != 101) and stderr contains no `Could not automatically determine the process-level CryptoProvider`; `cargo build -p hydradb-operator --features otlp` exits 0 and the same synthetic-kubeconfig run does not panic either.
  QA scenarios: happy: with the synthetic kubeconfig the binary does not panic and reports a transport/connect error instead. failure: assert the panic text `Could not automatically determine the process-level CryptoProvider` is absent from stderr. Evidence `<attemptDir>/task-64-hydradb-operator.txt`
  Commit: Y | fix(deps): enable the kube ring feature so rustls has a crypto provider
  Recommended task executor category: quick - a one-line feature change plus a binary reproduction.

- [x] 65. Wire the finalizer into the three reconcile entry points
  What to do / Must NOT do: call `finalizer::with_finalizer(...)` as the ONLY top-level action inside the GraphCluster, GraphNodePool and GraphIndexer reconcile entry points, using `constants::FINALIZER`, so `deletionPolicy` (Retain default / Delete) is actually enforced. Todo 23 shipped `finalizer.rs`, but its wiring was fenced out of that dispatch by the orchestrator, so today the finalizer is dead code and deletion semantics do not exist. Must NOT implement a second finalizer; must NOT bypass `kube::runtime::finalizer`.
  Closes: GAP-1
  Parallelization: Wave 3 | Blocked by: 18, 20, 21 | Blocks: 24
  Context (executor has NO interview context - be exhaustive): `crates/controller/src/finalizer.rs` exposes `with_finalizer`, a pure `cleanup_plan`, `execute_cleanup` (with an injected stamp hook), `is_not_found` and `is_store_unreachable`. The three reconcilers were built without it because the todo-23 dispatch was deliberately scoped to the module only.
  Acceptance criteria (agent-executable): `cargo test -p hydradb-operator` exits 0; a test asserts each reconcile entry point is wrapped so the finalizer is the only top-level action, and that deleting a CR with `deletionPolicy: Retain` stamps `hydradb.helmforge.dev/pruned-by` before the object is removed.
  QA scenarios: happy: create -> Ready -> delete with `Retain` leaves the object-store prefix intact and stamps `pruned-by`. failure: cleanup on a never-applied CR returns Ok without panicking. Evidence `<attemptDir>/task-65-hydradb-operator.txt`
  Commit: Y | feat(controller): wire the finalizer into every reconcile entry point
  Recommended task executor category: unspecified-low - one wrapper call per entry point plus tests.

- [x] 66. Bump the stale workspace pins to the current stable releases
  What to do / Must NOT do: Bring `[workspace.dependencies]` in the root `Cargo.toml` up to the current latest stable releases wherever the pin is behind: `serde-saphyr` 1.3 -> 2.0, `rand` 0.9 -> 0.10, `sha2` 0.10 -> 0.11, `opentelemetry` / `opentelemetry_sdk` / `opentelemetry-otlp` 0.32 -> 0.33, `tracing-opentelemetry` 0.33 -> 0.34, `clap` 4.5 -> 4.6, and pin `envtest` at 0.3.0 wherever the controller test suite declares it (the earlier `envtest 0.1` note is stale). `kube` 4.2.0 and the rest of the crate VERSIONS are already at latest and MUST stay, but two further axes are stale and MUST move with them: (i) select the newest Kubernetes API feature `k8s-openapi` 0.28.0 offers by switching its single `v1_33` feature to `v1_36` while keeping EXACTLY one `v1_X` feature across the workspace, and (ii) bump the pinned toolchain `rust-toolchain.toml` channel `1.89.0` -> `1.92.0` (`1.92.0` is already installed on this host, so this needs no new software install; the workspace `rust-version` MSRV stays `1.89` because `kube` 4.2.0 declares `rust-version = "1.89.0"`), and (iii) note that todo 35 - which creates the `Dockerfile` and now runs after this bump - must therefore build on `rust:1.92`, so the container never has to download a second toolchain. Migrate every call site the major bumps break (serde-saphyr 2, rand 0.10, sha2 0.11, the 0.33 OpenTelemetry stack with tracing-opentelemetry 0.34). Must NOT add a second `v1_X` k8s-openapi feature, must NOT introduce `serde_yaml`, must NOT downgrade a crate that is already at latest, must NOT relax `-D warnings` or delete/ignore a test to make a migration compile, and must NOT touch the `Dockerfile` (todo 35 creates it after this bump and owns its base tag), and must NOT re-add `envtest` if it still requires libclang - libclang is ABSENT on this host and `envtest` was deliberately removed for exactly that reason (full reason chain in `.omo/notepads/hydradb-operator/issues.md` lines 9-17); if `envtest 0.3.0` still cannot build here, record that as a documented deviation and keep every gate green instead of breaking the tree.
  Closes: GAP-1
  Parallelization: Wave 5 | Blocked by: 13 | Blocks: 26, 35, 40, 41
  Context (executor has NO interview context - be exhaustive): measured against crates.io - ALREADY LATEST as a VERSION and must not change: kube 4.2.0, k8s-openapi 0.28.0 (its crate version stays; only its FEATURE moves v1_33 -> v1_36), serde 1.0.229, serde_json 1.0.154, tokio 1.53.3, thiserror 2.0.21, anyhow 1.0.104, tracing 0.1.44, tracing-subscriber 0.3.23, chrono 0.4.45, uuid 1.28.0, futures 0.3.34, tower 0.5.3, axum 0.8.9, tower-test 0.4.0. `envtest` latest stable is 0.3.0.
  Acceptance criteria (agent-executable): `cargo tree --workspace -e features -i k8s-openapi | rg 'v1_'` prints exactly one line (`v1_36`); `rustc --version` inside the repo reports `1.92.0`; `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo fmt --all --check` and `cargo deny check` all exit 0; `cargo tree -i sha2` resolves 0.11 and `cargo tree -i rand` resolves 0.10.
  QA scenarios: happy: every gate green on the bumped tree with the same test count as before. failure: assert no resolved crate is still on the old major (`cargo tree -i serde-saphyr` must show 2.0, never 1.3). Evidence `<attemptDir>/task-66-hydradb-operator.txt`
  Commit: Y | chore(deps): bump the workspace pins to the current stable releases
  Recommended task executor category: deep-low - three major-version migrations plus an OpenTelemetry stack bump.

- [ ] 67. Adopt the helmforge charts harness as the Kubernetes validation contract
  What to do / Must NOT do: Make `/home/aidas/repos/charts` (`github.com/helmforgedev/charts`) the source of truth for Kubernetes validation. Run its `./test.sh <chart-name>` helper against `charts/hydradb-operator` so the chart passes the same gates helmforge PRs run: `helm dependency build`, `helm lint --strict`, the default render, a render of every `charts/hydradb-operator/ci/*.yaml` scenario, `kubeconform -strict` on the default render and every scenario using exactly the documented schema-location pair (`-schema-location default` plus the datreeio CRDs-catalog URL) and NEVER `--ignore-missing-schema`, `helm unittest --with-subchart=false`, `ah lint -p`, and Kubescape with the minimum score gate. Also satisfy GR-079 (zero BLOCKER/HIGH findings): SPDX license headers on every changed YAML/template/shell file, the required chart structure including `DESIGN.md`, `docs/` and `examples/`, a README Security-Scan section, declared dependencies, `className`, and the `Chart.lock` rule. Align the chart's unit tests with `docs/testing-strategy.md`: `tests/<template-name>_test.yaml` naming, `documentSelector` instead of document order, and BOTH the enabled and disabled state for every optional resource (NetworkPolicy, PodDisruptionBudget, ServiceMonitor). Must NOT invent a competing harness, must NOT use `kubeconform --ignore-missing-schema`, must NOT commit `Chart.lock`, must NOT add a `.github/workflows/` file to this repo, must NOT vendor or copy helmforge charts into this repo.
  Closes: GAP-15
  Parallelization: Wave 6 | Blocked by: 36, 37, 39 | Blocks: 55, 56, 57, 68
  Context (executor has NO interview context - be exhaustive): the helper auto-installs missing tools (`helm`, `kubectl`, `kubeconform`, `ah`, `kubescape`, plus the `helm-unittest` plugin when `tests/` exists) into `~/.local/bin` or `$HELMFORGE_TOOLS_DIR`, and `--no-install` fails fast instead. On this host `helm` is v3.20.1 but the `helm-unittest` plugin and `kubeconform` are NOT installed. GR-079 is enforced by a guard living in the private `helmforgedev/helmforge-ops` repo (`scripts/charts/standards-guard.js`) and needs `OPS_AUTOMATION_TOKEN`, so run every public gate locally and record which private guard legs could not be reproduced. The helper must be pointed at our chart (e.g. a temporary symlink of the chart into the helmforge `charts/` directory, or an equivalent documented invocation) - the chart itself stays in this repo.
  Acceptance criteria (agent-executable): the helmforge helper run against our chart exits 0 with the lint, template, unittest, kubeconform and artifacthub legs all passing; every changed YAML/template/shell file carries an SPDX header; `DESIGN.md`, `docs/`, `examples/` and the README Security-Scan section exist.
  QA scenarios: happy: the helmforge helper exits 0 for `charts/hydradb-operator`. failure: introduce a floating image tag and remove the values schema, then assert the values-quality check and the standards guard flag both. Evidence `<attemptDir>/task-67-hydradb-operator.txt`
  Commit: Y | test(chart): validate the chart with the helmforge charts harness
  Recommended task executor category: unspecified-high - an external conformance contract with several tool legs.

- [ ] 68. Record the helmforge k3d runtime evidence for the chart
  What to do / Must NOT do: Run the helmforge runtime layer for our chart: `./test.sh hydradb-operator --runtime -f charts/hydradb-operator/ci/<scenario>.yaml` against a local `k3d-` context (the default expected cluster is `k3d-helmforge-tests-wsl`), installing with `--wait --timeout 120s`. Install the three CRDs before the chart, and install the prometheus-operator CRDs as well when the ServiceMonitor scenario is the one under test. Capture exactly the runtime evidence `docs/testing-strategy.md` requires: workloads Ready or Complete as expected; Services and endpoints exist where traffic is exposed; PVCs Bound when persistence is enabled; namespace events contain no warnings or failures; pod logs contain no startup, permission, crash-loop or probe errors; and the test namespace removed after validation. Must NOT run against the talos-stack cluster, must NOT claim success without captured command output, must NOT use `--ignore-missing-schema`, must NOT skip the cleanup check.
  Closes: GAP-15
  Parallelization: Wave 9 | Blocked by: 54, 67 | Blocks: 57
  Context (executor has NO interview context - be exhaustive): this is the helmforge runtime layer, which is k3d-based and deliberately separate from the crate-level `kind` e2e in todo 54 - both are required. If no local `k3d-` context exists, install k3d and create the cluster rather than substituting another cluster.
  Acceptance criteria (agent-executable): the runtime run exits 0 and the evidence file records every documented runtime check with its command and raw output.
  QA scenarios: happy: install -> ready -> cleanup passes and the evidence is recorded. failure: point the chart at an unreachable object store and assert the run reports the failure instead of passing. Evidence `<attemptDir>/task-68-hydradb-operator.txt`
  Commit: Y | test(chart): record the helmforge k3d runtime evidence
  Recommended task executor category: deep-low - a live multi-step runtime validation where the failure modes are environmental.

## Final verification wave
> Runs in parallel after ALL todos. ALL must APPROVE. Surface results and wait for the user's explicit okay before declaring complete.
- [ ] F1. Plan compliance audit
  What to verify / Must NOT do: Build a table mapping EVERY implementation todo (1-68) to its evidence path and a PASS/FAIL, then map every GAP-1..GAP-15 row to the landed todo that closes it and every IS-1..IS-12 row to its proving QA scenario. Re-run each todo's exact acceptance command yourself. Confirm no `### Must NOT have` item was built (no backup CRDs, no webhook, no UI, no HPA/VPA, no `:latest`, no forked image, no `.github/workflows/`, no Tofu-managed repo objects). Must NOT accept a todo as done from its own summary or from a passing log alone - the command and its artifact must both be reproduced.
  Acceptance criteria (agent-executable): the mapping table covers all 68 todos and all 15 GAP rows with zero unmapped entries; every re-run command exits as the todo claims; any FAIL becomes a new `- [ ] N.` row.
  QA scenarios: happy: all 68 PASS and all GAP rows closed. failure: delete one generated artifact and assert the audit reports that todo FAIL rather than passing. Evidence `<attemptDir>/F1-hydradb-operator.md`
  Recommended task executor category: unspecified-high

- [ ] F2. Code quality review
  What to verify / Must NOT do: Read the source and confirm: `cargo clippy --all-targets -- -D warnings` exits 0; no `unwrap()`/`expect()` in any reconcile path; no `as any`-equivalent type escape (no `unsafe`, no `#[allow]` without a comment); the SSA field manager is exactly `hydradb.helmforge.dev/controller` (no double prefix); RBAC contains no `*` resource or verb; conditions are order-stable and do not churn `lastTransitionTime`; no credential is logged, embedded in a ConfigMap, or written into a committed file; generated artifacts are byte-identical to `cargo xtask gen-all` output; every child carries the standard label set and an owner reference. Must NOT restyle or refactor - report only.
  Acceptance criteria (agent-executable): a findings list where every item names file:line and a severity; zero blockers of the categories above; the clippy and gen-all --check commands re-run clean.
  QA scenarios: happy: no blocker-class findings. failure: deliberately introduce a wildcard RBAC verb in a branch and assert the review catches it. Evidence `<attemptDir>/F2-hydradb-operator.md`
  Recommended task executor category: unspecified-high

- [ ] F3. Real manual QA
  What to verify / Must NOT do: On a real cluster (the e2e kind cluster or the talos-stack cluster), install the PUBLISHED chart via Flux and via Helm, apply a `GraphCluster` + `GraphNodePool` + `GraphIndexer`, and drive it by hand: `kubectl wait --for=condition=Ready graphcluster/<name> --timeout=420s`; open a Bolt connection on 7687 and run `RETURN 1`; `curl` `/readyz` on 9090 and 9091; compare the StatefulSet's `GRAPH_BOLT_NODE_ADDRESSES` env against the live pod list via `kubectl get ... -o jsonpath`; scale the pool and re-check the routing table; delete with `Retain` and confirm the finalizer clears and `pruned-by` is stamped; run `kubectl hydradb doctor` and confirm exit 0; `cosign verify` the published image. Must NOT use a mocked client for this wave, must NOT claim success without captured command output.
  Acceptance criteria (agent-executable): every command's output captured under the evidence path; `doctor` exits 0; `cosign verify` exits 0; the routing-table comparison matches exactly; the Retain deletion leaves data intact.
  QA scenarios: happy: the full manual pass succeeds end to end. failure: kill the object store mid-run and confirm the cluster reports `Stalled` and `doctor` exits non-zero naming the object store. Evidence `<attemptDir>/F3-hydradb-operator.md`
  Recommended task executor category: unspecified-high

- [ ] F4. Ideal-state fidelity
  What to verify / Must NOT do: Walk IS-1 through IS-12 one at a time against the DELIVERED behavior (not the plan text) and mark each fully met, partially met, or unmet, citing the exact artifact or command output that proves it. Confirm each of the ten affected users' "after" experience holds. Must NOT accept a partial as met - a shortfall becomes a NEW `- [ ] N.` implementation row appended to `## Todos`, never a note or a caveat.
  Acceptance criteria (agent-executable): a 12-row verdict table with evidence per row; zero partial/unmet rows, or the new rows exist to close them; the ten affected-user statements each verified against an artifact.
  QA scenarios: happy: all 12 rows fully met with cited evidence. failure: point at any IS row lacking an artifact and assert a new todo row is created for it. Evidence `<attemptDir>/F4-hydradb-operator.md`
  Recommended task executor category: unspecified-high

## Commit strategy
- One commit per todo, Conventional Commits, scoped to the crate or area: `feat(api): ...`, `feat(controller): ...`, `feat(cli): ...`, `feat(xtask): ...`, `build: ...`, `ci: ...`, `docs: ...`, `test: ...`, `chore: ...`.
- Commit ONLY after that todo's acceptance criteria and both QA scenarios pass; never commit a red tree. If a todo cannot be completed, leave the tree green and report instead.
- Regenerate before committing any artifact change: `cargo xtask gen-all` then `cargo xtask gen-all --check` must be clean in the same commit as the source change that caused the drift.
- Never commit generated CRDs, RBAC or docs by hand - only via xtask. Never commit a credential, a kubeconfig, a PAT, or anything from `.omo/evidence/`.
- No `:latest` in any committed manifest. Release tags are `v<semver>` (operator) and `hydradb-operator-v<semver>` (chart), driven by Conventional Commits.
- Default branch is `main`. On Gitea, pushes to `main` trigger PaC directly. On GitHub, `main` is protected by a ruleset requiring a PR plus the CI status check, so mirror commits arrive through the mirroring process rather than direct pushes.
- The talos-stack changes (`terraform/environments/{harbor,gitea,github}`, `apps/secrets/external/`, `infrastructure/controllers/`, `infrastructure/secrets/external/`, `clusters/default/`) are committed to the talos-stack repository, not this one, and each carries its own PaC path gate. The three `.tekton/` PipelineRuns - `.tekton/hydradb-operator-image-build.yaml`, `.tekton/hydradb-operator-ci.yaml` and `.tekton/github-hydradb-operator-image-build.yaml` - are committed to THIS repository, because PaC reads `.tekton/` from the triggering repo and all three are path-gated on this repo's paths (`crates/**`, `charts/**`, `Dockerfile`) or clone `github.com/kubexa/hydradb-operator`; talos-stack's own `.tekton/` files are only the pattern being mirrored.

## Success criteria
> One row per IS row. The plan is complete only when every IS row has a delivering todo and a proving QA scenario; F4 checks the delivered behavior against these rows 1:1, and a shortfall becomes new `- [ ] N.` rows, never a note.
| IS | Delivering todo(s) | Proving QA scenario | Evidence |
| --- | --- | --- | --- |
| IS-1 | 6, 7, 8, 9, 10, 11, 12 | todo 12 - CRD smoke test asserts group/kind/scope/printcolumns; todo 11 - validation accept/reject matrix | <attemptDir>/task-12-hydradb-operator.txt |
| IS-2 | 18, 19, 20, 21, 24, 54 | todo 54 - apply three CRs in kind, `kubectl wait --for=condition=Ready graphcluster/<name> --timeout=420s`, Bolt `RETURN 1` on 7687 | <attemptDir>/task-54-hydradb-operator.txt |
| IS-3 | 16, 24 | todo 16 - condition order-stability and `lastTransitionTime` no-churn tests; todo 24 - `observedGeneration == generation` after a spec change | <attemptDir>/task-16-hydradb-operator.txt |
| IS-4 | 19, 54 | todo 19 - 3-replica pool yields exactly 3 Bolt addresses and 0 yields empty; todo 54 - jsonpath comparison of the env against the live pod list | <attemptDir>/task-19-hydradb-operator.txt |
| IS-5 | 18, 42, 48, 58 | todo 18 - envtest with a Rook `ObjectBucketClaim` resolves bucket + endpoint from the OBC Secret/ConfigMap; todo 58 - the documented manual matrix for AWS/MinIO | <attemptDir>/task-18-hydradb-operator.txt |
| IS-6 | 8, 20, 58 | todo 20 - `emptyDir` and `persistentVolume` both render correctly with the requested size and class | <attemptDir>/task-20-hydradb-operator.txt |
| IS-7 | 6, 8, 9, 10, 36, 48 | todo 48 - `skopeo inspect` the published public image and assert no default references the private ECR | <attemptDir>/task-48-hydradb-operator.txt |
| IS-8 | 31, 36, 39, 46, 57, 60, 67 | todo 46 - Flux reconciles the `OCIRepository`/`HelmRelease` to Ready; todo 60 - the CRD upgrade procedure documented in DESIGN.md | <attemptDir>/task-46-hydradb-operator.txt |
| IS-9 | 40, 43, 44, 45, 56, 57 | todo 57 - a commit to Gitea drives PaC CI green then both registry pushes; todo 45 - the Renovate job discovers `oss/*` | <attemptDir>/task-57-hydradb-operator.txt |
| IS-10 | 27, 28, 29, 30, 57 | todo 29 - `doctor` exit-code mapping and per-check names; todo 57 - `kubectl hydradb doctor` exits 0 on the live install | <attemptDir>/task-29-hydradb-operator.txt |
| IS-11 | 11, 20, 36, 48, 58 | todo 58 - every documented environment command runs verbatim and exits 0 | <attemptDir>/task-58-hydradb-operator.txt |
| IS-12 | 36, 37, 39, 41, 60, 67, 68 | todo 39 - `helm unittest` + `helm template` for every `ci/*.yaml`; todo 41 - `cosign verify` on the published artifact; todo 60 - `helm-docs` no-diff | <attemptDir>/task-39-hydradb-operator.txt |

