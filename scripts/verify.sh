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

# The CRDs live in charts/*/crds/ and Helm installs them on its own, so a plain
# `helm template` never renders them. Their SCHEMA validity is enforced by the
# envtest suite (a real apiserver rejects an unrepresentable schema); this check
# covers the packaging contract instead: exactly one manifest per kind, all five
# kinds present, in both rendered scopes.
step "CRD bundle contract: five kinds, both scopes, from the chart itself"
for scope in namespaced cluster; do
  rendered=$(helm template hydradb-operator "$CHART" --include-crds --set "installScope=$scope")
  kinds=$(printf '%s\n' "$rendered" | grep -c '^kind: CustomResourceDefinition$' || true)
  if [ "$kinds" -ne 5 ]; then
    printf 'expected 5 CustomResourceDefinitions with installScope=%s, got %s\n' "$scope" "$kinds" >&2
    exit 1
  fi
  actual=$(printf '%s\n' "$rendered" | sed -n 's/^  name: \(graph[a-z]*\.hydradb\.kubexa\.tech\)$/\1/p' | sort | tr '\n' ' ')
  expected="graphbackups.hydradb.kubexa.tech graphclusters.hydradb.kubexa.tech graphindexers.hydradb.kubexa.tech graphingestsources.hydradb.kubexa.tech graphnodepools.hydradb.kubexa.tech "
  if [ "$actual" != "$expected" ]; then
    printf 'CRD names mismatch with installScope=%s\n  expected: %s\n  actual:   %s\n' "$scope" "$expected" "$actual" >&2
    exit 1
  fi
  printf '    -> installScope=%s: 5 CRDs present\n' "$scope"
done

step "kubeconform -strict: every ci/ scenario"
shopt -s nullglob
for values_file in "$CHART"/ci/*values.yaml; do
  printf '    -> %s\n' "$values_file"
  helm template hydradb-operator "$CHART" -f "$values_file" |
    kubeconform -strict -summary -schema-location default -schema-location "$CRDS_CATALOG" -
done

printf '\n\033[1;32mAll verification gates passed.\033[0m\n'
