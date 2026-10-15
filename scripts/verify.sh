#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# The local verification gate for HydraDB Operator.
#
# Runs every check a reviewer is expected to run before approving a change:
# formatting, lints, the workspace test suite, generated-artifact drift, and the
# full Helm chart contract (lint, unit tests, and strict schema validation of the
# default render plus every `ci/` scenario).
#
# Deliberately requires NO cluster and NO credentials: every leg is offline-safe
# apart from the CRDs-catalog schema lookup that kubeconform performs.
#
# Usage: `mise run verify` or `bash scripts/verify.sh`
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

readonly CHART="charts/hydradb-operator"
readonly CRDS_CATALOG='https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json'

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
fail() { printf '\n\033[1;31mFAILED (exit %s): %s\033[0m\n' "$?" "$BASH_COMMAND" >&2; }
trap fail ERR

step "preflight: required tools"
for tool in cargo helm kubeconform; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf 'missing required tool: %s\n' "$tool" >&2
    exit 1
  fi
done
if ! helm plugin list 2>/dev/null | grep -q '^unittest'; then
  printf 'missing required helm plugin: unittest (helm plugin install https://github.com/helm-unittest/helm-unittest)\n' >&2
  exit 1
fi

step "cargo fmt --all --check"
cargo fmt --all --check

step "cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

step "cargo test --workspace"
cargo test --workspace

step "cargo xtask gen-all --check"
cargo xtask gen-all --check

step "helm lint --strict $CHART"
helm lint --strict "$CHART"

step "helm unittest $CHART"
helm unittest "$CHART"

step "kubeconform -strict: default render"
helm template hydradb-operator "$CHART" |
  kubeconform -strict -summary -schema-location default -schema-location "$CRDS_CATALOG" -

step "kubeconform -strict: every ci/ scenario"
shopt -s nullglob
for values_file in "$CHART"/ci/*values.yaml; do
  printf '    -> %s\n' "$values_file"
  helm template hydradb-operator "$CHART" -f "$values_file" |
    kubeconform -strict -summary -schema-location default -schema-location "$CRDS_CATALOG" -
done

printf '\n\033[1;32mAll verification gates passed.\033[0m\n'
