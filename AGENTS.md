# AGENTS.md — HydraDB Operator

Source of truth for agents working in this repo.

## Repo layout

Five-crate Rust workspace:

- `crates/api` — lib `hydradb-operator-api`: CRD types (`GraphCluster`, `GraphNodePool`, `GraphIndexer`).
- `crates/controller` — bin `hydradb-operator`: the kube-rs reconciliation binary.
- `crates/cli` — bin `hydradb`: operator CLI.
- `crates/xtask` — bin `xtask`: codegen and repo tasks.
- `crates/e2e` — lib `hydradb-operator-e2e`: kind-based e2e harness.

## Commands

Run via mise tasks:

- `mise run fmt` — format.
- `mise run lint` — clippy with warnings denied.
- `mise run test` — unit tests.
- `mise run build` — build all crates.
- `mise run check-drift` — verify generated code is current.
- `mise run e2e` — end-to-end suite (needs a cluster).
- `cargo xtask gen-all` — regenerate all generated artifacts.

## Three-CRD model

API group `hydradb.helmforge.dev`, version `v1alpha1`. Three kinds only:

1. `GraphCluster` — owns the deployment lifecycle.
2. `GraphNodePool` — scales worker pools.
3. `GraphIndexer` — drives indexing.

Do not add kinds without a plan change.

## Controller rule (hard)

The controller owns child objects directly and NEVER runs `helm template` inside the controller. No shelling out to helm, no `:latest` image resolution at runtime. All child manifests are built in Rust from the CRD spec.

## Image tag policy

Tags are `v<semver>` plus `<sha>`. Never `:latest`. Example: `v0.1.0` with digest pin, never `hydradb-operator:latest`.
