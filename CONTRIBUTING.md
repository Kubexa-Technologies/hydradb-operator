# Contributing to HydraDB Operator

## Build and test

```sh
mise run build
mise run fmt
mise run lint
mise run test
mise run check-drift
cargo xtask gen-all
```

`mise run e2e` needs a cluster (kind). See `docs/` for details.

## Commits

Conventional Commits. One commit per plan todo, e.g. `docs(repo): add README, AGENTS.md, LICENSE and governance files`.

## Branches and tags

Branch from `main` with short names (`feat/<thing>`, `fix/<thing>`, `docs/<thing>`). Releases are tags named `v<semver>` (e.g. `v0.1.0`).
