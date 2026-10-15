# HydraDB operator chart design

## Goal

Ship the kube-rs operator with the smallest safe default install and make
every privilege escalation an explicit opt-in.

## Decisions

- **Namespaced default.** `installScope: namespaced` renders `Role` /
  `RoleBinding` and sets `OPERATOR_SCOPE=namespaced`. Cluster scope is one
  value away, never the default.
- **No runtime helm.** The controller owns child objects directly in Rust
  (`GraphCluster` owns lifecycle, `GraphNodePool` scales workers,
  `GraphIndexer` drives indexing). The chart only installs the operator
  itself plus CRDs, RBAC, ServiceAccount, Service, PDB and NetworkPolicy.
- **Opt-in ServiceMonitor.** Absent by default; `tests/` asserts the
  absence so a regression cannot silently add monitoring objects.
- **Pinned images.** Tag is `v<semver>`, never `:latest`; an optional
  `image.digest` appends `@sha256:`. The schema rejects `latest` and the
  Deployment suite asserts the tag shape.
- **Guards on by default.** PDB (`minAvailable: 1`) and NetworkPolicy
  (healthz ingress, DNS/apiserver egress) render unless disabled; both
  states are unit-tested.

## Layout

`templates/` holds one resource per file, `tests/` mirrors it
(`<template-name>_test.yaml`), `ci/` holds the sets helmforge CI renders,
`examples/` holds documented starting points, `docs/` holds scope and
monitoring notes, `README.md` is generated from `README.md.gotmpl` via
`helm-docs`.
