# syntax=docker/dockerfile:1
#
# HydraDB operator image: builds the `hydradb-operator` controller binary and
# the `hydradb` CLI plugin from this repo, and ships both on a Chainguard Wolfi
# base (glibc, digest-pinned, rebuilt daily upstream).
#
# Binaries built here (BOTH are this repo's own binaries and BOTH are built):
#   - hydradb-operator  (crates/controller, package `hydradb-operator`)
#   - hydradb           (crates/cli, package `hydradb` — the operator's CLI plugin)
# NOT built here: the HydraDB *operand* images (`graph-node`, `graph-indexer`);
# those are separate upstream artifacts. `crates/xtask` and `crates/e2e` are
# workspace members but are never built into this image either.
#
# Build paths:
#   1. Full build from source (default, needs network for crates.io):
#        docker build -t hydradb-operator:test .
#   2. Export just the binaries to the host (no runtime image):
#        docker build --target bin --output type=local,dest=./dist .
#   3. CI with prebuilt binaries (skips the entire Rust toolchain stages):
#        docker build --build-context bin=./dist -t hydradb-operator:test .
#      The named `bin` build context OVERRIDES the `bin` stage defined below,
#      so `planner`/`chef`/`builder` are never executed. The runtime stage does
#      `COPY --from=bin` of explicit file paths, so a missing context directory
#      or a context without the binaries FAILS LOUDLY instead of producing an
#      empty image (see the failure QA: `--build-context bin=./nonexistent`).
#
# Runtime contract (consumed by charts/hydradb-operator Deployment):
#   - ENTRYPOINT is the operator; the Deployment probes GET /healthz on :8080,
#     so no EXPOSE is declared here (the port is documented in the chart).
#   - Runs as non-root 65532:65532 (the :nonroot base default, set explicitly).

# --- planner: compute the cargo-chef dependency recipe -----------------------
FROM rust:1.92@sha256:f58923369ba295ae1f60bc49d03f2c955a5c93a0b7d49acfb2b2a65bebaf350d AS planner
WORKDIR /src
RUN cargo install cargo-chef --locked --version 0.1.78
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# --- chef: pre-build the dependency skeleton for a warm layer cache ---------
FROM rust:1.92@sha256:f58923369ba295ae1f60bc49d03f2c955a5c93a0b7d49acfb2b2a65bebaf350d AS chef
WORKDIR /src
RUN cargo install cargo-chef --locked --version 0.1.78
COPY --from=planner /src/recipe.json recipe.json
# The manifests must exist for `cargo chef cook` to resolve the workspace.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY .cargo ./.cargo
RUN cargo chef cook --release --recipe-path recipe.json

# --- builder: compile the two shipped binaries --------------------------------
FROM chef AS builder
WORKDIR /src
# Copy the real sources over the cooked skeleton, then build ONLY the two
# shipped packages (never xtask/e2e, never the HydraDB operand).
COPY . .
RUN cargo build --release -p hydradb-operator -p hydradb \
    && mkdir -p /out \
    && cp target/release/hydradb-operator target/release/hydradb /out/

# --- bin: swappable binary-export stage ---------------------------------------
# Default: re-export what `builder` produced (used by `--target bin` exports
# and by the runtime stage below). CI overrides this whole stage with
# `--build-context bin=<dir-with-prebuilt-binaries>` holding:
#   <dir>/hydradb-operator and <dir>/hydradb
FROM scratch AS bin
COPY --from=builder /out/hydradb-operator /hydradb-operator
COPY --from=builder /out/hydradb /hydradb

# --- runtime: Chainguard Wolfi, non-root, operator entrypoint -----------------
# Chainguard Wolfi base (glibc, no shell in the default profile beyond busybox,
# rebuilt daily upstream) pinned by digest: `latest` is never shipped.
FROM cgr.dev/chainguard/wolfi-base@sha256:0feab531471d2edfcca55f18883441cb7df598e724fa2dedf9985273f532eed7 AS runtime

# CA roots for the object-store and Kubernetes API TLS paths, plus tzdata for
# the log timestamps. Installed as a virtual package so the layer stays minimal.
RUN apk add --no-cache --virtual .hydradb-runtime ca-certificates-bundle tzdata \
    && update-ca-certificates
# Explicit paths: a `bin` context missing either file is a hard build error.
COPY --from=bin /hydradb-operator /usr/local/bin/hydradb-operator
COPY --from=bin /hydradb /usr/local/bin/hydradb
# Wolfi's `nonroot` account is 65532:65532, matching the chart's pod security
# context and the Chainguard-provided nonroot account.
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/hydradb-operator"]
