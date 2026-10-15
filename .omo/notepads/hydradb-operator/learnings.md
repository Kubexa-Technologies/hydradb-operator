# Learnings — hydradb-operator

Conventions, patterns, and successful approaches discovered during work on this plan.

_Auto-scaffolded by /ulw-execute. Append new entries below - never overwrite._

---

## 2026-10-14 — Task 1: workspace scaffold
- Toolchain used: cargo 1.92.0 / rustc 1.92.0 (system toolchain; workspace rust-version pin is 1.89, no rustup install needed since 1.92 compiles edition-2024 skeletons cleanly).
- Pattern mirrored from kopiur: root [workspace] + [workspace.package] + empty [workspace.dependencies]; crates inherit via version/edition/rust-version/license/repository .workspace = true; explicit [[bin]] stanzas in controller/cli/xtask.
- Surprise: lsp_diagnostics daemon timed out twice (30s); cargo build --workspace exit 0 used as compile verification instead. Skeleton files are single-line roots so build success is sufficient here.

## 2026-10-14 — Task 35: multi-stage distroless Dockerfile
- Pre-existing uncommitted Dockerfile was structurally correct (planner/chef/builder/bin/runtime, scratch bin stage, distroless nonroot, USER+ENTRYPOINT, no HEALTHCHECK, no :latest) but pinned `rust:1.89`; toolchain is 1.92.0 so both Rust stages moved to `rust:1.92`. Audit-before-rewrite saved full rebuild of spec.
- `docker build -t hydradb-operator:test .` exit 0; `hydradb --version` in image exit 0 (`hydradb 0.1.0`); inspect User=`65532:65532`, Entrypoint=`[/usr/local/bin/hydradb-operator]`.
- Operator binary itself has no --version flag (controller ignores unknown args incl. --help, then fails loudly without kubeconfig) — version gate exercised via co-shipped `hydradb` binary; --version on controller belongs to todo 22, not this task.
- Failure QA: `--build-context bin=./nonexistent` exits 1 with explicit "failed to get build context bin" error — COPY --from=bin uses explicit file paths so a bad context can never silently yield an empty image.
- Never pipe a docker gate through tail/head: redirect to file, then `echo $?`.

## 2026-10-14 task-4 governance files
- All eight root governance files written and verified on disk (README, AGENTS.md, LICENSE Apache-2.0 canonical, CONTRIBUTING, SECURITY, .gitignore, .editorconfig, .gitattributes).
- Lesson: upstream HydraDB LICENSE is AGPL, but helmforge policy for this operator is Apache-2.0, used canonical Apache text verbatim.
- .gitignore must cover target/ + .omo/evidence/ without ignoring Cargo.lock.

## 2026-10-14 — Task 2: workspace dependency pins
- Root [workspace.dependencies] holds exactly 20 pins (kube 4.2.0 rustls-tls/ws, k8s-openapi 0.28 v1_33, schemars 1, serde+derive, serde_json, serde_yaml 0.9, tokio macros/rt-multi-thread/signal, thiserror 2, anyhow 1, tracing, tracing-subscriber env-filter/json, clap derive/env, chrono clock/serde, uuid v4/serde, futures, tower 0.5, tower-test, envtest, rand 0.9, sha2). All crates consume via `workspace = true`.
- Plan line 216 literal claim (no k8s-openapi in api tree) unachievable: kube 4.x always pulls k8s-openapi transitively. Achievable substitute verified: no declaration + no v1_ in crates/api/Cargo.toml, absent at tree depth 1, present only under kube in `-i` output. v1_33 reaches build via controller/cli unification.
- Env gotcha: concurrent todo 3 landed rust-toolchain.toml (1.89.0) mid-task while that toolchain was half-installed; `RUSTUP_TOOLCHAIN=stable cargo ...` bypasses it for verification. Evidence in .omo/evidence/task-2-hydradb-operator.txt.

## 2026-10-14 — Task 3: toolchain pin, fmt/clippy/deny gates, mise tasks
- rust-toolchain.toml pins 1.89.0 (minimal + rustfmt/clippy); `cargo --version` in repo reports cargo/rustc 1.89.0 via override. First `rustup toolchain install` left corrupt state (cargo binary missing, "not applicable" error); fix was full `rustup toolchain uninstall 1.89.0` + reinstall. Toolchain now healthy; todo 2's RUSTUP_TOOLCHAIN workaround no longer needed.
- rustfmt.toml omits `imports_granularity`: empirically verified nightly-only on 1.89.0 stable ("unstable features are only available in nightly channel" warning). Ships edition 2024 + max_width 100 + use_field_init_shorthand.
- mise 2026.9.1 present; all six tasks (fmt/lint/test/build/check-drift/e2e) defined with exact command strings from plan. `mise run fmt` exits 0.
- QA failure scenario verified at package scope: unused `let` in xtask trips `-D warnings` (exit 101), revert returns 0. Full-workspace lint equivalent blocked (see problems.md).
- cargo-deny 0.20.2 installed via `cargo install cargo-deny --locked` (4m49s compile). deny.toml parses (v1 format, no `version = 2`); bans ok, sources ok.

## Dependency-resolution retry (2026-10-14): both RED gates fixed
- Culprit for dual v1_X: `envtest 0.1.2` enables `k8s-openapi` with feature `latest` (= v1_36) unconditionally via its `kube` default feature; workspace pins `v1_33`. Proof: `cargo tree --workspace -e features -i k8s-openapi` showed `latest <- envtest`, `v1_36 <- latest`.
- `serde-saphyr 1.3` verified as clean serde_yaml replacement: pure-Rust (granit-parser, no unsafe-libyaml), supports BOTH serialize+deserialize (default features), license MIT OR Apache-2.0, rust-version 1.89 matches toolchain. Same choice as kopiur operator. `serde_yaml_ng` rejected (still pulls unsafe-libyaml). No Rust code used serde_yaml yet (skeleton crates), so swap was manifest-only.
- deny license additions: `Zlib` (foldhash via hashbrown via kube-runtime) and `CDLA-Permissive-2.0` (webpki-root-certs via rustls), each with inline source comment.

## Follow-up learning: envtest<->k8s-openapi version coupling is exact, but envtest needs libclang
- envtest 0.1.2 `k8s-openapi` dep = `version >= 0, features = ["latest"]`; k8s-openapi 0.28.0 `latest = ["v1_36"]`. Aligning workspace to v1_36 removes the dual-feature panic cleanly (verified single v1_36 in tree, build ok).
- envtest default features also pull `r2g`/rust2go whose build runs bindgen 0.71.1 requiring libclang at COMPILE time. No libclang on this box => envtest unusable here even with correct features. Feature alignment solves cargo semantics; native toolchain solves the build. Check `ldconfig -p | grep clang` before re-adding envtest anywhere.

## task-5 e2e harness (2026-10-14)
- Harness shape: `crates/e2e/src/lib.rs` holds `Harness { cluster_name }`, consts CLUSTER_NAME=hydradb-operator-e2e, DEFAULT_RELEASE_NAMESPACE. shell-outs via run_cmd() helper carrying exact cmdline + stderr in anyhow error.
- KUBECONFIG semantics: when set and non-empty it WINS — no fallback to ~/.kube/config. first existing split_paths entry returned, else None. fallback only when unset/empty. caught during work: initial version fell back to ~/.kube/config and third smoke test skipped wrong path.
- try_client() returns Result<Option<Client>>: early Ok(None) when KUBECONFIG points at nothing; Client::try_default fail -> Ok(None); apiserver_version probe fail -> Ok(None). never panics, no unwrap.
- kind_delete/helm_uninstall tolerate absent binaries and missing releases (kind absent -> Ok; "not found" stderr -> Ok) so teardown idempotent without setup.
- smoke.rs: 3 tokio tests, all #[ignore], skip via client_or_skip helper printing SKIP + test name. KUBECONFIG=/nonexistent run exits 0 with 3 skip lines.
- env: kind absent, helm present, ~/.kube/config exists but unreachable. no cluster created during verify per plan.
- clippy --all-targets -D warnings clean; fmt clean; no manifest changes.

## 2026-10-14 — Task 6: api layout + shared primitives
- kube 4.2 does NOT re-export k8s-openapi types (verified: no `pub use k8s` in kube-4.2.0/kube-core-4.2.0). api crate declares `k8s-openapi = { workspace = true, features = ["schemars"] }` — inherits v1_33 from workspace pin, adds only the non-version `schemars` feature. `cargo tree -e features -i k8s-openapi` shows exactly one v1_X (v1_33). Same shape as kopiur api (plain workspace inherit).
- k8s-openapi JsonSchema impls are feature-gated: without `schemars` feature, `LocalObjectReference` fails `JsonSchema` bound (E0277 x3). The `schemars` feature (schemars 1, not 0.8) fixes it; no manifest carries `schemars08`.
- `compile_fail` doctest on `ObjectStore` (`ObjectStore::from("my-bucket")`) passes as the honest equivalent of the plan's "omit both variants fails to compile" QA — two-Option struct literal always compiles, so stringly-construction rejection + todo-11 validate() cover it instead.
- `#![warn(missing_docs)]` in api lib.rs survives `-D warnings` clippy; every pub field/variant documented.

## 2026-10-14 — Task 7: Unknown-fallback enums + strict parsers
- phase_serde! macro in crates/api/src/spec/mod.rs mirrors kopiur common/mod.rs shape: PhaseLabel trait (ALL/label/unknown/parse/canonical/is_unknown) + macro emitting Serialize/Deserialize/Display/JsonSchema(oneOf consts, canonical only). Deserialize goes through String::deserialize so Unknown never constructible from non-string; no #[serde(other)] anywhere.
- Enums: BuildMode full/incremental, CacheType emptyDir/persistentVolume (matches values.yaml node.cache.type), ServiceType ClusterIP/NodePort/LoadBalancer/ExternalName, PullPolicy IfNotPresent/Always/Never (matches values.yaml image.pullPolicy + prior Image doc comment), ConditionReason Reconciling/Available/Degraded/Failed.
- strict_option<T> generic: Option::<T>::deserialize under #[serde(default, deserialize_with)] — absent/null stays None, never defaults into safe variant. Wired into Image.pull_policy (Option<String>->Option<PullPolicy>, wire strings unchanged).
- TDD red ran first: tests failed to compile (unresolved imports + missing strict_option) before impl; green after with 6 unknown_variant tests.
- schemars 1.2.3 json_schema! macro confirmed same API as kopiur (schemars 1). No new deps, no Cargo.toml edits.

## 2026-10-14 — Tasks 8/9/10: GraphCluster, GraphNodePool, GraphIndexer CRD types
- TDD red ran first: new `crates/api/tests/crd_types.rs` failed to compile (unresolved `crd::GraphCluster/...` imports) before impl; green after with all 8 tests.
- kube 4.2 `CustomResource` derive needs NO `Default` on spec or status — build passed without any. `ObjectStore`/`CacheStorage` still have no `Default` (validation-owned absence).
- kube `#[kube(...)]` attributes require string literals — `GROUP`/`VERSION` constants cannot interpolate. Literals used in attributes; `crd_emission_smoke` asserts emitted group/version/scope against the constants so drift fails the test.
- serde missing-field errors report the camelCase wire key (`objectStore`), not the Rust ident. Test asserts `objectStore`; task text said `object_store` — deviation recorded, wire key wins.
- Integration tests use `serde_json::json!` (not YAML): api crate has no YAML dep and adding one is forbidden. JSON is a valid YAML 1.2 subset so wire contract identical.
- `strict_option` composed with `default = "default_build_mode"` on indexer `build_mode`: missing field -> `Some(Full)`, explicit null -> `None`, unknown string -> `Some(Unknown(..))`.
- Runtime/service defaults mirror `charts/hydradb/values.yaml`: rustLog `info,hydradb=info`, maxOpenScopes 8, slatedbCacheBytes 671088640, dataCacheMaxOpenFiles 512, readerWalReplayConcurrency 16, maxConcurrentQueries 256, maxQueryScanEdges 1000000; ports 7687/443/9090.
- DeletionPolicy default choice: explicit `impl Default` (Retain) + `#[serde(default)]` on the owning field.
- `columns.as_ref().is_some_and(...)` needed in smoke test: `additionalPrinterColumns` is `Option<Vec<..>>` borrowed from CRD, cannot move.

## 2026-10-14 — Task 13: controller skeleton, run() entrypoint, shutdown
- axum NOT in workspace deps despite task text claiming pinned — health server built on std TcpListener blocking thread (nonblocking accept + 25ms poll, AtomicBool shutdown) so zero new deps. tokio workspace features lack net/time; std thread+sleep avoids relying on transitive feature unification via kube.
- kube 4.2: no `Config::from_file`; empty-kubeconfig client error via `Config::from_custom_kubeconfig(Kubeconfig::default(), &KubeConfigOptions::default()).await` (returns Result<_, KubeconfigError> — map to kube::Error::InferKubeconfig) then `Client::try_from`. `from_custom_kubeconfig` is async. Test uses match not expect_err (Client not Debug).
- deny bans wildcards fires on `{ path = "../api" }` — needs `version = "0.1.0"` alongside path.
- `cargo fmt -p hydradb-operator` scopes formatting to own crate; never run bare `cargo fmt --all` write mode while sibling edits api.
- Ctx fields unread until todos 18/20/21 — `#[allow(dead_code)]` with reason comment keeps `-D warnings` green.
## 2026-10-14 — Task 11: schemars deny_unknown_fields vs kube CRD pipeline
- `#[schemars(deny_unknown_fields)]` works on plain structs (verified `schema_for!(AuthSpec)` ends with `"additionalProperties": false`), but kube-derive 4.2 hardcodes `.with_transform(StructuralSchemaRewriter)` in generated `crd()` (kube-derive/src/custom_resource.rs:852-858), and kube-core/src/schema.rs deletes `additionalProperties: Bool(false)` on every object with properties. So `additionalProperties: false` can NEVER appear in `GraphCluster::crd()` output — no attribute opt-out (only whole-schema `schema="disabled"` manual mode). Test `crd_schema_denies_unknown_fields` therefore asserts on typed `schemars::schema_for!(Spec).get("additionalProperties")` for all three specs (typed Schema object, never YAML text) plus a CRD-embeds-spec-object check.
- schemars_derive 1.2.3 reads deny_unknown_fields from `cont.serde_attrs` (schema_exprs.rs:81-85) via schemars→serde attr copying (attr/schemars_to_serde.rs) — so `#[schemars(deny_unknown_fields)]` affects schema ONLY, serde deserialization untouched (existing deserialize tests all still green).
- `#[schemars(range(min = 1))]` on `replicas: i32`, `#[schemars(range(min = 1, max = 65535))]` on `u16` ports compile clean under `-D warnings`.
- k8s-openapi 0.28 `LocalObjectReference { name: String }` — `name` is required String, no `..Default::default()` needed.
- Validation placement: inherent `impl ObjectStore` / `impl GraphClusterSpec` blocks live in `validation.rs` (same crate, legal) — keeps schema files untouched except attributes. `u64::from(scopes) * 2` avoids u32 overflow in rule 2.
## 2026-10-14 — Task 12: api test suites (serde_matrix.rs + emission.rs)
- New files `crates/api/tests/serde_matrix.rs` (12 tests) + `crates/api/tests/emission.rs` (2 tests); suite now 37 passing (was 23), all pre-existing untouched.
- Fully-populated round-trip pattern: `to_string` -> `from_str` -> `to_string` byte-compare; use NON-default values (e.g. intervalMs 7000) so assertions prove wire survival, not default echo.
- kube pluralization verified live: `graphclusters`/`graphnodepools`/`graphindexers`; short names hgc/hgnp/hgi; `spec.names.categories` carries `hydradb`; `spec.scope` is a plain String ("Namespaced").
- k8s-openapi 0.28 inline JSON works in fixtures: `ResourceRequirements` requests/limits as `{"cpu": "2"}`, `Toleration` operator/effect as plain strings, `TopologySpreadConstraint` needs maxSkew/topologyKey/whenUnsatisfiable, `Affinity {}` round-trips as `{}`.
- Nested schemars deny check: `schemars::schema_for!(T).get("additionalProperties") == Some(json!(false))` holds for all 13 nested spec structs (GraphIdentity/AuthSpec/TlsSpec/ServiceSpec/RuntimeSpec/Image/S3/ObjectStore/EmptyDirCache/PersistentVolumeCache/CacheSpec/UpdateStrategy/NodeServiceSpec).

## Task 14 — watcher_config (2026-10-14)
- kube 4.2 splits watch tuning: `watcher::Config` owns list_semantic/initial_list_strategy/labels only; debounce+concurrency live on `controller::Config`. Brief's `.debounce`/`.concurrency` on watcher::Config don't exist — shipped `watcher_config()` + `controller_config()` pair, documented in module docs.
- `watcher::Config::streaming_lists()` takes NO bool; gate via `initial_list_strategy` field. Built struct literal field-by-field so no `Config::default()` string appears anywhere (rg gate).
- `controller::Config` fields private so `let base: controller::Config = Default::default();` used — dodges the `Config::default()` rg while staying idiomatic.
- `Api::all` needs `K::DynamicType: Default` bound too, not just `K: Resource` — required on `metadata_api`.
- `char::is_numeric` can't pass directly to `take_while` (takes char by value, iter yields &char) — closure needed.
- kube `apiserver_version()` returns k8s-openapi `version::Info` with String major/minor; real clusters report minor like "32+" — parse leading digit run.
- tokio `time` feature arrives via kube-runtime dep (feature unification), so `tokio::time::timeout` compiles without Cargo.toml change; workspace tokio features lack "time" — flagged for todo 61.

## Task 61 — axum health/metrics server (2026-10-14)
- `axum 0.8` with `default-features = false, features = ["tokio", "http1"]` is enough for `Router` + `routing::get` + `axum::serve(...).with_graceful_shutdown(...)`; no hyper/tower-http direct dep needed (axum pulls hyper internally, licences already allowed — `cargo deny` stays green).
- `axum::serve` takes `tokio::net::TcpListener`, so the workspace `tokio` dep needed `net` (listener) + `io-util` (`AsyncReadExt`/`AsyncWriteExt` in the socket test) features added to the existing pin — feature-only change, no version disturbance.
- Fallback route preserves the old hand-rolled 404 exactly (`404`, body `not found\n`, `text/plain`); axum's default 404 has an empty body, so `.fallback()` was required to keep behaviour byte-identical.
- Socket-level test pattern that works without any HTTP client dep: bind `127.0.0.1:0`, `tokio::spawn(axum::serve(...))`, write raw `GET <path> HTTP/1.1` bytes over `TcpStream`, `read_to_end`, split on `\r\n\r\n`. Note axum lowercases response header names (`content-type:`), so assertions must match lowercase.
- Removed the `.expect()` on the SIGTERM handler without behaviour change: on install failure log a warn and await SIGINT only — server still never panics.

## Task 15 — error classification + requeue policy (error.rs)
- `kube::Error::Api` wraps `Box<kube_core::Status>` with a plain `code: u16` field; `Status` derives `Default`, so tests build status errors as `kube::Error::Api(Box::new(Status { code, ..Default::default() }))`.
- kube 4.2.0 with features client/runtime/derive/rustls-tls/ws enables these `kube::Error` variants: Api, HyperError, Service, ProxyProtocolUnsupported/Disabled, FromUtf8, LinesCodecMaxLineLengthExceeded, ReadEvents, HttpError, SerdeError, BuildRequest, InferConfig, Discovery, RustlsTls, TlsRequired, UpgradeConnection (ws), Auth, InferKubeconfig. No OpensslTls/Gcp without those features — `classify_kube_error` matches exactly this set.
- Hand-rolled FNV-1a 64 (offset 14695981039346656037, prime 1099511628211) + `% 31` gives deterministic [30s,60s] jitter; same key always draws same delay, distinct keys spread (verified 200 keys, >1 distinct value).
- Precedent for future-todo items in a bin crate: targeted `#[allow(dead_code)]` with "consumed by reconcilers landing in todos 18/20/21" comment (same style as `Ctx` in context.rs); otherwise `cargo clippy -- -D warnings` fails on unused pub items.
- `cargo fmt` formats only the named file via `rustfmt --edition 2024 <file>` — safe under concurrent siblings; never run workspace `cargo fmt` (would rewrite siblings' in-flight files).

## Task 25 + 62 — Prometheus metrics + optional OTLP + tokio time feature
- Hand-rolled registry in `crates/controller/src/metrics.rs`: process-global `Mutex<Registry>` keyed by leaked `&'static str` kinds (no alloc on hot path), counters `hydradb_operator_reconciles_total{kind}`, `hydradb_operator_reconcile_errors_total{kind,class}` (class labels transient/structural/terminal via `ErrorClass`, reused from error.rs — no second enum), `hydradb_operator_watcher_events_total{kind,event}`, plus `hydradb_operator_reconcile_duration_seconds` histogram with 11 fixed buckets (0.005–10.0) storing already-cumulative counts + sum/count; `render()` emits HELP/TYPE/samples, `reconcile_span(kind,ns,name)` returns an info_span for todos 18-24.
- `run.rs`: `/metrics` serves `metrics::render()`; `init_otlp()` duplicated under `#[cfg(feature)]`/`#[cfg(not)]` so no-feature builds link zero OTEL code; with feature it reads `OTEL_EXPORTER_OTLP_ENDPOINT` and best-effort `try_init()`s a `tracing-opentelemetry` layer (warns, never panics, if main's subscriber already installed).
- `otlp = ["dep:opentelemetry", "dep:tracing-opentelemetry"]` in controller Cargo.toml; workspace pins `opentelemetry 0.32` + `tracing-opentelemetry 0.33` (exact operand pairing from plan); `cargo deny check` passes with no allow-list change.
- Todo 62: `"time"` appended to workspace tokio features; `cargo tree -p hydradb-operator -e features -i tokio | rg 'tokio feature "time"'` shows it.
- Tests use unique kind strings per test (shared global registry, parallel harness) instead of a reset helper; endpoint test does raw `TcpStream` HTTP/1.1 with no client dep. 39/39 pass.

## Task 16 — hand-rolled kstatus conditions module (`crates/controller/src/conditions.rs`)
- `k8s_openapi::jiff` is re-exported (`pub use jiff` in k8s-openapi lib.rs), so `Time(k8s_openapi::jiff::Timestamp::now())` builds `last_transition_time` with NO new dependency; fixed test times via `Timestamp::from_second(secs).expect("...")` (expect-with-message allowed in test code).
- `Condition` (k8s-openapi 0.28) fields: `last_transition_time: Time`, `message: String`, `observed_generation: Option<i64>`, `reason: String`, `status: String`, `type_: String`; derives `PartialEq`, serializes via serde — `serde_json::to_vec` byte-compare proves no-patch.
- Order-stable upsert = in-place update on `type_` match (preserve `last_transition_time` unless `status` changed) else append; NO canonical re-sort (spec says appended index never moves; 100-round shuffle test covers update-only stability).
- `STRUCTURAL_GATES: &[(ConditionType, &str)]` with `pub type ConditionType = &'static str`; `upsert_gate` returns `bool` (false on unknown gate, never panics); registry test asserts every reason parses via `ConditionReason::parse` (canonical labels only).
- `live_conditions(live, desired) -> StatusUpdate::{NoChange, Patch(Vec<Condition>)}` with order-insensitive field-exact compare (sort clones by `type_`); caller skips `/status` write on `NoChange` (no hot-loop).
- `set_ready(true)` => Ready True/Available + Reconciling False; `set_ready(false)` => Ready False/Reconciling + Reconciling True; `set_degraded(msg)` => Ready False/Degraded; all three ensure Stalled False/Available when absent but never clobber an existing Stalled=True.
- Tests live in `#[cfg(test)] mod tests` inside `conditions.rs` (8 tests: 6 required + status-change-moves-time + set_ready_false).

## Task 17 (apply.rs) learnings
- kube `Api::patch` needs `K: Clone + Debug` bounds beyond Serialize/DeserializeOwned; E0599 tells exact missing bounds.
- kube `ResourceExt::controller_owner_ref()` pins `block_owner_deletion: true`; build `OwnerReference` manually for `controller:true, block:false`.
- `GraphCluster::api_version(&())` / `::kind(&())` via `kube::Resource` give owner-ref identity without retyping group/version.
- Module-level `#![allow(dead_code)]` (inner attr after `//!` docs) silences pre-consumer warnings; trailing `#[allow(dead_code)]` only covers next item. Matches error.rs/context.rs convention.
- `PatchParams::apply(FIELD_MANAGER).force()` exposes `field_manager: Option<String>` + `force: bool` for direct unit asserts.

## task-18: GraphCluster reconciler (config/auth/object-store)
- Pure builders (`config_data`, `desired_config_map`, `plan_auth`, `resolve_explicit`, `resolve_obc`) keep all logic unit-testable; async `reconcile_inner` only does API reads/writes + status.
- `constants.rs` pins only graph-identity/object-store/RUST_LOG env names; six `GRAPH_*` runtime keys + `GRAPH_AUTH_TOKEN_FILE` mirror upstream `configmap.yaml` verbatim and live as single-definition local consts in `cluster.rs` (incl. `GRAPH_SLATE_DB_CACHE_BYTES` underscore spelling).
- OBC convention: Secret+ConfigMap share the claim name in the cluster namespace; endpoint from `ENDPOINT|AWS_ENDPOINT|BUCKET_ENDPOINT|endpoint` else `http://host:port`; creds required-present but never copied (todo 20 wires via secretKeyRef).
- Token preserve: `uuid::Uuid::new_v4().simple()` mint (no new dep); `AuthPlan::Create(Box<Secret>)` silences `large_enum_variant`.
- Status writes guarded by `live_conditions` no-change check + scalar compare; Merge patch only touches conditions/observedGeneration/observedObjectStore (todo 24 owns the rest).
- kube 4.2 `Controller::owns(api, config)` takes Api+Config; version probe once in `spawn_graph_cluster_controller`, handle aborted after server stops (shutdown contract kept).
- Task 23 finalizer: kube 4.2 `kube::runtime::finalizer(api, name, obj, |event| async move {...})` takes ONE closure over `Event::Apply/Cleanup` (not separate apply/cleanup fns); wrapper `with_finalizer` splits into two `FnOnce` closures and unwraps `ApplyFailed`/`CleanupFailed` into the inner crate `Error`, maps add/remove-finalizer failures to `Error::Api`. `Action` lives at `kube::runtime::controller::Action`, not `kube::runtime::Action`.
- Purity pattern: `cleanup_plan(&DeletionPolicy, children_exist, store_reachable, cluster_name) -> Vec<CleanupAction>` with `StampPrunedBy` always first; `execute_cleanup` takes injected `FnOnce` hooks so all tolerance paths (404 children, unreachable store) test without a cluster. Duplicate-step guard via `Option<hook>` take so repeated plan steps reuse first outcome.
- `pruned-by` stored as `metadata.annotations["hydradb.helmforge.dev/pruned-by"]` with value `{Reason}@{RFC3339-UTC}` via `Patch::Merge`; data prefix `hydradb/clusters/{name}/` — bucket itself never touched.
- Binary-crate `pub` API for pending consumers needs file-level `#![allow(dead_code)]` (precedent: apply.rs, conditions.rs) or clippy `-D warnings` fails before todos 18/20/21 land.
- Task 63 OTLP: `opentelemetry-otlp 0.32` minimal feature set is `["grpc-tonic", "trace"]` (default-features=false) — matches plan QA on `:4317`, tonic client connects lazily so startup never blocks; needs companion `opentelemetry_sdk 0.32` with `["trace", "rt-tokio"]` for `SdkTracerProvider::with_batch_exporter` (plan omitted it — added as stated deviation). `with_endpoint` needs `use opentelemetry_otlp::WithExportConfig as _`; `provider.tracer()` needs `use opentelemetry::trace::TracerProvider as _`. Boxed `Box<dyn Layer<Registry>>` must wrap the bare registry FIRST (`.with(otlp_layer)` before filter/fmt), else `Layer<Layered<..>>` bound fails. Returning `Option<Box<dyn Layer<Registry>>>` from both cfg variants keeps `main.rs` cfg-free; no-feature variant links zero OTLP symbols (tree confirms absent). Endpoint value never logged (auth may hide in userinfo/query).

## 2026-10-14 — Task 64: kube ring feature fixes rustls CryptoProvider panic
- Root cause: kube 4.x splits TLS crypto provider into own `ring` feature; `rustls-tls` alone leaves rustls 0.23 with no provider (cargo tree showed only log/logging/std/tls12).
- Fix: one-line Cargo.toml change — kube features `["client","runtime","derive","rustls-tls","ws","ring"]`. After: `cargo tree -p hydradb-operator -e features -i rustls | rg 'rustls feature "ring"'` matches 3 lines (hyper-rustls + rustls).
- Proof: synthetic kubeconfig (https://127.0.0.1:1, insecure-skip-tls-verify, token fake) no longer panics; binary starts, logs `client error (Connect)`, retries reconcile streams until `timeout 20` kills it (exit 124, NOT 101). Same for `--features otlp` build.
- Never enable both `ring` + `aws-lc-rs` — rustls auto-select then fails differently.

## task-19: GraphCluster Services + Bolt routing table (cluster.rs)
- Five pure Service builders in `crates/controller/src/reconcile/cluster.rs`: `desired_headless_service` (clusterIP None, publishNotReady true, ports 7687/8443/9090), `desired_bolt_service` (7687->7687), `desired_https_service` (443->8443), `desired_metrics_service` (9090->9090), `desired_indexer_metrics_service` (9091->9091, indexer selector).
- Headless name `<cluster>-node-headless` does NOT match `cluster_child_name(name, ROLE_HEADLESS)` (`<cluster>-headless`), so headless + indexer-metrics use explicit names via `service_object_meta` (name + standard labels + owner references; bolt/https/metrics reuse `cluster_child_name` with apply.rs ROLE consts.
- `node_pod_selector` mirrors upstream `componentLabels` (name=hydradb, instance=cluster, component=node); todo 20 must label StatefulSet pods with exactly this set.
- `routing_table_value(cluster, ns, pod_names)` mirrors `_helpers.tpl` `nodeAddresses` entry shape `pod=pod.headless.ns.svc.cluster.local:7687`, sorts names for determinism, empty in -> empty out. Reconcile lists live Pods by label selector each time; never replicaCount, never cached.
- `config_data`/`desired_config_map` gained a `bolt_node_addresses: &str` param; existing test updated with `""`.

## task-21: GraphIndexer Deployment reconciler (`reconcile/indexer.rs`, new ~700 lines)
- Env-name authority is chart `configmap.yaml`, NOT plan prose: `GRAPH_INDEXER_INTERVAL_MS`, `GRAPH_INDEXER_DIRTY_POLL_INTERVAL_MS`, `GRAPH_INDEXER_RETAIN_PREVIOUS` (shortened, no GENERATIONS), `GRAPH_INDEXER_BUILD_MODE`, `GRAPH_INDEXER_INCREMENTAL_MIN_EDGES`, `GRAPH_INDEXER_SCOPE_CONCURRENCY`, `GRAPH_INDEXER_SCOPES_PER_CYCLE`, `GRAPH_INDEXER_MAX_OPEN_SCOPES`, `GRAPH_INDEXER_READINESS_FAILURE_THRESHOLD`, `GRAPH_INDEXER_MAX_TAIL_FILES`, `GRAPH_WAL_TAIL_FETCH_CONCURRENCY` (no INDEXER infix). Module docs record this deviation from plan's bare names.
- `ConditionReason` has no `Disabled` variant (only Reconciling/Available/Degraded/Failed/Unknown): `Ready=False`/`Disabled` carried via `ConditionReason::Unknown("Disabled")` which labels verbatim.
- `Image` resolution mirrors chart `hydradb.image` helper: digest beats tag beats bare repo; `spec.image` overrides cluster image else inherit. No shared helper existed; `resolve_image` lives in indexer.rs.
- k8s-openapi 0.28 shapes that bit: `LocalObjectReference.name: String` (not Option), `ConfigMapEnvSource.name: String` (not Option), `Quantity(pub String)` tuple struct.
- Pure builder `desired_deployment(&GraphIndexer, &GraphCluster) -> Result<Deployment, Error>`: command `["graph-indexer"]`, envFrom `<cluster>-config`, auth Secret volume with key/path = cluster auth secret_key (default from `super::cluster::DEFAULT_AUTH_KEY`), `SUCCESS_REQUEUE_SECS` reused from `super::cluster`. Owner ref built manually for GraphIndexer (`owner_ref_for` in apply.rs is GraphCluster-only).
- Volumes are emptyDir (cache, tmp 1Gi) + auth Secret only; both writable mounts asserted plus no-PVC test. Probes: liveness /livez + readiness /readyz on 9091; container port 9091 named admin.
- Registration mirrors cluster controller: `spawn_graph_indexer_controller` owns `Deployment` label-filtered by `CLUSTER_SELECTOR_LABEL`; both handles aborted after health server stops.
- Gates: build ok, clippy `-D warnings` ok, 75 tests pass (7 new), fmt clean, deny ok, single `v1_33` line. Evidence `.omo/evidence/task-21-hydradb-operator.txt`.

## task-27: hydradb CLI skeleton
- `crates/cli/{src/main.rs,src/cmd/mod.rs,src/output.rs,tests/cli_surface.rs}`: clap 4.5 derive surface with global `--kubeconfig/--context/-n/-A/-o/-v` + `HYDRADB_LOG`->`RUST_LOG`->verbosity logging; `version` prints `env!("CARGO_PKG_VERSION")`; other five cmds return honest `CliError::NotImplemented{command, owning_todo}` (28/29/30).
- kube-client 4.2.0 API used: `Kubeconfig::read_from(path)` + `Config::from_custom_kubeconfig(k, &KubeConfigOptions{context,..})`, `Config::from_kubeconfig(&opts)` for `--context`, `Config::infer()` fallback; `Client::try_from(config)` error type is `kube::Error` so plain `?` works.
- `OutputFormat` ValueEnum gives clap the `-o bogus` error + valid choices for free; `Printer` implements json (pretty) + name (`kind/name` lowercase, handles `items` lists); table/wide/yaml return NotImplemented(30).
- `ScaleArgs.replicas: u32` so `--replicas -1` fails at parse time pre-cluster (todo 30 failure case needs no extra code).
- Integration tests drive `env!("CARGO_BIN_EXE_hydradb")` (no assert_cmd dep needed).
- Gates: `cargo build -p hydradb` 0, `cargo build --workspace` 0, `cargo test -p hydradb` 0 (3 unit + 4 integration), `cargo fmt --check` 0, `cargo deny check` 0, single `v1_33` tree line. Workspace clippy blocked by sibling (see issues). Evidence `.omo/evidence/task-27-hydradb-operator.txt`.

## task-20: GraphNodePool StatefulSet reconciler (`reconcile/nodepool.rs`, new ~1240 lines)
- Selector contract honored: `spec.selector.matchLabels` is exactly `super::cluster::node_pod_selector(cluster)` (the 3-label set todo 19's Services select); template labels = `standard_labels(cluster, "node")` + `spec.pod_labels`. Test asserts both.
- `CacheStorage` mapping: `EmptyDir{size_limit}` -> `emptyDir` volume (sizeLimit Quantity when set); `PersistentVolume{size,storage_class_name,access_modes}` -> single `volumeClaimTemplates` entry named `cache` (storage request + accessModes, `storageClassName` ONLY when user set it). `/tmp` always `emptyDir` (from `spec.tmp_size_limit` when set); auth Secret mounted at `/var/run/secrets/hydradb` like the indexer.
- Auth Secret NAME resolution: `spec.auth.existing_secret` passthrough when non-empty else `<cluster>-auth`; KEY from `secret_key` or `DEFAULT_AUTH_KEY`. Container gets `GRAPH_AUTH_TOKEN` via `secretKeyRef` (new local const — no shared constant pins the name; deviation documented in module docs) + `RUST_MIN_STACK=33554432`, then `extra_env` last so user duplicates win (K8s last-entry-wins).
- `updateStrategy` passthrough: `type` verbatim; `partition` only under `RollingUpdate` (dropped under `OnDelete` to avoid an invalid block); absent spec leaves field unset (K8s default). Own `UpdateStrategy` test covers all three.
- Resources: `spec.resources` when set else upstream node defaults (req 2/4Gi/12Gi, lim 8/16Gi/24Gi). Grace default 60. Ports bolt 7687/https 8443/admin 9090; probes /livez + /readyz on 9090; pod+container security mirrors indexer (uid/gid 10001, non-root, readOnlyRootFilesystem, drop ALL).
- Status mirrors indexer (conditions via set_ready + observedGeneration + ready/current replicas from live StatefulSet; live boltAddresses preserved, never recomputed) with `// TODO(todo-24)`; finalizer hook is `// TODO(todo-65)` only. `resolve_image` duplicated from indexer (no shared helper existed; api untouched).
- k8s-openapi 0.28 notes: `Quantity(pub String)` has NO Display — compare via `.0`; `ResourceRequirements{claims, limits, requests}`; `StatefulSetSpec{service_name, pod_management_policy, volume_claim_templates}` all Option; `RollingUpdateStatefulSetStrategy{partition, max_unavailable}`.
- DEVIATION: `PersistentVolumeCache` has no annotations field in frozen api (only size/storage_class_name/access_modes) — PVC template carries standard labels, no extra annotations; api untouched per constraints.
- DEVIATION (process): TDD red step collapsed — tests spec-authored before impl but verified green only (no transient `todo!` stubs; forbidden in lib code).
- Gates: build 0, clippy `-D warnings` 0, 88 controller tests pass (75 pre-existing + 13 new), fmt clean, deny ok, single `v1_33` line. Evidence `.omo/evidence/task-20-hydradb-operator.txt`.

## task-20 FIX: invented `GRAPH_AUTH_TOKEN` env removed (orchestrator defect)
- The operand NEVER reads a bare token env: verified zero hits for it in operand `src/`+`charts/`+`examples/`; ONLY `GRAPH_AUTH_TOKEN_FILE` is real (chart `configmap.yaml:63`, `graph_node/config.rs:288`, `graph-indexer.rs:3372`, min 32 non-placeholder chars). Deleted the const + the `secretKeyRef` env entry from `nodepool.rs`; container env is now just `RUST_MIN_STACK` + `extra_env`. Token reaches operand ONLY via the mounted Secret file at `/var/run/secrets/hydradb/<key>` (mount kept) + the `GRAPH_AUTH_TOKEN_FILE` value already in the shared ConfigMap via `envFrom` (kept).
- `node_env` signature shrank to `(spec)`; `EnvVarSource`/`SecretKeySelector` imports dropped. No test asserted the removed var, so zero test changes — all 13 nodepool tests still green, nothing weakened.
- Lesson: every env set on an operand container must be verified readable by the operand/chart source, not just plausible. Acceptance: `rg 'GRAPH_AUTH_TOKEN\b' crates/controller/src` prints nothing.
- task-31 (xtask gen-crds): `serde_saphyr::{to_string, from_str}` mirror serde_yaml API (ser/de halves behind `serialize`/`deserialize` features, both on by default via workspace dep). `CustomResourceDefinition.spec.scope` is plain String ("Namespaced"). Integration tests in `crates/xtask/tests/` drive real binary via `env!("CARGO_BIN_EXE_xtask")` — no lib target needed. Workspace root from `env!("CARGO_MANIFEST_DIR")` ancestors(2) works for both bin and integration tests regardless of cwd.
- task-31 gates: build 0, clippy `-D warnings` 0, 4 xtask tests pass, fmt clean, deny ok, single `v1_33` line, gen-crds twice byte-identical (sha256 verified, both dirs same hashes). Evidence `.omo/evidence/task-31-hydradb-operator.txt`.

## 2026-10-14 — Task 32: gen-rbac derived from Resource traits
- `crates/xtask/src/gen_rbac.rs`: rule table derives group+plural from `kube::Resource` (`<K as Resource>::plural/group`), incl. local marker impls for ServiceMonitor (monitoring.coreos.com) + ObjectBucketClaim (objectbucket.io) absent from k8s-openapi. Verbs narrowest per call site: CRs get/list/watch/update/patch, /status get/update/patch, SSA children full lifecycle, pods/PVC/OBC read-only, events get/list/watch/create/patch, leases full (leader election).
- k8s-openapi 0.28 RBAC path is `api::rbac::v1` (no `authorization` segment); `kube::Resource` also requires `meta`/`meta_mut` on marker structs.
- Single output `deploy/rbac/operator-clusterrole.yaml` with GENERATED header; serde_saphyr emission; write-if-different; sha256-stable across runs (26ff6556…).

## 2026-10-14 — Task 28: status/endpoints/logs CLI commands
- `Printer::print` json/name paths reused as-is; table/wide/yaml for the read commands built as ADDITIONS (`render_table`, `to_yaml`/`print_yaml`, `list_value`, `with_kind`) so the todo-27 unit test pinning `NotImplemented(30)` on `Printer` stays green untouched.
- `-o yaml` without a YAML dep: hand-rolled JSON->YAML emitter (double-quoted scalars are valid YAML 1.2); sequence-of-objects uses compact `- key: value` first line. Caught a real indent bug via the new unit test (nested keys drifted right); fixed by giving block collections their own leading indent.
- CLI crate has no `serde` dep: trait bounds spelled via the `k8s-openapi` re-export (`k8s_openapi::serde::Serialize`, `pub use serde` in k8s-openapi 0.28 lib.rs:242). Workspace tokio lacks `io-std`, so `logs` uses `Api::<Pod>::logs` (returns collected `String`, per brief) instead of `log_stream` (needs `futures`, also absent).
- kube 4.2 API facts: `Api::namespaced(client, &str)` takes `&str` not `String`; `Resource::kind(&())` returns `Cow<str>`; `pods.logs()` -> `Result<String>`; `log_stream()` -> `impl AsyncBufRead`.
- Controller label contract verified by grep (not guessed): node pods `app.kubernetes.io/{name=hydradb,instance=<cluster>,component=node}` (`node_pod_selector` in `reconcile/cluster.rs`, template labels `standard_labels(cluster,"node")` in `nodepool.rs`); indexer pods `standard_labels(cluster,"indexer")` in `indexer.rs` (component=indexer).
- `is_none_or` + `is_some_and` + `let-else`-free style pass `-D warnings` clippy on 1.89; `sort_by_key(sort_key)` beats `sort_by` (clippy `unnecessary_sort_by`); single-element `vec!` used only as slice trips `useless_vec` — use arrays in tests.
- Clap UX: global flags go BEFORE subcommand (`hydradb -n demo logs worker`); `--tail -1` parses as flag, must write `--tail=-1` (integration test initially failed on this).
- Commit burst 2026-10-14: b18a354 feat(controller) nodepool StatefulSet, 75b325a feat(cli) skeleton+status/endpoints/logs, 5a46717 feat(xtask) CRD+RBAC codegen.

## 2026-10-14 — Task 29: doctor checklist wired
- Orphan `doctor.rs` (~1053 lines) needed exactly 4 compile fixes to wire: `serde::` bounds -> `k8s_openapi::serde::` (CLI crate has no serde dep; precedent from task-28), `Scope = NamespaceResourceScope` + `Debug` + `DynamicType: Default` bounds on generic `list_values` (mirrors task-14 `Api::all` lesson), `?` in `check_operator_deployment` bool-returning fn replaced with match, E0499 double-borrow in stalled block fixed by extracting `stalled_result()` helper.
- Clippy `cloned_ref_to_slice_refs`: `&[x.clone()]` in tests -> `std::slice::from_ref(&x)`.
- Wire facts verified by grep not guess: auth default `<cluster>-auth`/`auth-token` (`cluster.rs:91`, `nodepool.rs:170-190`), objectStore wire paths `/spec/objectStore/explicit/endpoint` + `/spec/objectStore/objectBucketClaim/name` (camelCase via `rename_all`, `LocalObjectReference.name`), auth wire `existingSecret`/`secretKey`, Stalled is condition TYPE `"Stalled"` (`conditions.rs:52`), ports 9090/9091, headless `<cluster>-node-headless`.
- Gates: 50 unit (33 + 17 new doctor) + 4 cli_surface + 2 cmd_read, all 0 failed; clippy `-D warnings` + fmt clean. Evidence `.omo/evidence/task-29-hydradb-operator.txt`.

## 2026-10-14 — Task 33: gen-docs + gen-all
- Orphan `gen_docs.rs` (~319 lines) was ~95% correct on read-through: schema walk (sorted keys, `[]` array suffix, anyOf/oneOf/allOf passthrough), empty-description cells, no unwrap/expect in generator code, write-if-different + trailing newline. Kept as-is; only `cargo fmt -p xtask` normalization applied.
- `main.rs` wiring: `mod gen_docs;` + `gen-docs` arm + `gen-all` running gen-crds -> gen-rbac -> gen-docs with per-step `eprintln!("gen-all failed at {step}: ...")` + exit 1; `not_implemented` removed (would trip `-D warnings` dead-code).
- All live schema fields are documented today (0 empty-description cells in output: 60/187/166 fields across the three tables), so the integration test proves the empty-cell path by exact description parity per row + empty-cell count == undocumented count, backed by the pre-existing synthetic unit test.
- Test-parser gotcha: filtering table rows with `contains("Field")` drops the 13 `matchFields` data rows — filter on exact `| Field |` / `| ---` prefixes instead.
- Gate gotcha (recurring): `cmd | tail` reports tail's exit; redirect to file + `echo $?` with no pipe (same lesson as task 7).

## task-65: finalizer wiring for all three reconcilers
- Pattern per entry point (mirrors cluster.rs template): `reconcile` builds the namespaced `Api<K>`, then `finalizer::with_finalizer(&api, obj, apply_closure, cleanup_closure)` as the ONLY top-level action. Metrics span + `record_reconcile` stay inside the `Apply` closure; `Cleanup` returns `Action::await_change()`.
- Pool/indexer specs carry NO `deletionPolicy` field (only `GraphClusterSpec` does): cleanup resolves the owning cluster's policy via `clusterRef` + `get_opt`, defaulting to `Retain` on empty/unresolvable refs so deletion never blocks. Pure helpers (`policy_from_lookup`, `pool/indexer_cleanup_plan`) keep the default + never-delete-data contracts unit-testable without a kube client (no mock-client dep in controller dev-deps, and adding one is forbidden).
- Pool/indexer cleanup hardcodes `store_reachable=false` in the plan: they own no object-store data (prefix belongs to the cluster), so `DeleteDataPrefix` is never emitted even under `Delete` — only the stamp reason reflects the policy.
- `include_str!` self-test trap: an assertion string naming the forbidden literal makes `!source.contains(literal)` unpassable. Fix: split source at `#[cfg(test)]` and scan only the production half.
- Reused `super::cluster::policy_reason` in pool/indexer instead of a second copy; `pool_child_name`/`indexer_child_name` (child name == CR name) + `get_opt` for children-existence checks.
- Commit burst 2026-10-15: 67b6d22 feat(cli): add the doctor command; 99994fe feat(xtask): add gen-docs and gen-all; 476c4e4 feat(controller): wire the finalizer into every reconcile entry point.

## 2026-10-14 — Task 36: chart skeleton (Chart.yaml/values/schema/icon)
- `helm lint --strict` passes on a templates/-free chart (helm v3.20.1): lint only needs Chart.yaml + values; empty `helm template` render exits 0. No minimal templates/ needed, none created (row 37 owns it).
- Schema-vs-values agreement proven mechanically: leaf-path walk of values.yaml (17 leaves incl. `[]` for empty arrays/objects) against schema object-node walk — zero missing. Pattern reusable for row 37/39 drift checks.
- `additionalProperties: false` at root + nested objects plus `installScope` enum is what makes `--set installScope=bogus` fail with `value must be one of 'namespaced', 'cluster'`.
- values.yaml carries 25 `# --` doc comments (one per key incl. intermediate objects) for helm-docs consumption in row 39.

## 2026-10-15 — Task 42: harbor oss project (talos-stack)
- Mirrored `webapps.tf` exactly for `oss.tf`: project (public true, Trivy, auto_sbom true, quota 50) + 2-rule Daily retention (always-retain latest; 10 most-recently-pushed) + immutable v* + cicd robot (push/pull/read-artifact/delete-repo/list-repo/list-tag, secret via random_password + ignore_changes) + pull-only robot + vault_kv_secret_v2 at kv/apps/harbor/oss-cicd with username/password keys.
- Provider quirk verified via registry MCP (goharbor/harbor immutable_tag_rule doc 13625833): `repo_matching` IS a valid optional field on harbor_immutable_tag_rule (example uses repo_matching="**") even though no existing file sets it; included repo_matching="**" per plan, validate passes.
- terraform_bot convention: project-level permission blocks live in home-ops.tf; newest blocks (entaras/trading-team) carry tag-retention + immutable-tag accesses, older home-ops block lacks them. New oss block copied the full (newer) shape.
- Write-only secret warnings (`secret` vs `secret_wo`) are pre-existing across all harbor robot files; mirrored existing `secret` shape deliberately, validate exits 0 with warnings.
- Tooling note: filesystem_* MCP tools are scoped to HydraDB-Operator only, so talos-stack writes/edits go through bash heredoc/python; evidence + learnings (HydraDB-Operator paths) can use normal file tools or bash append.

## 2026-10-14 — Task 49: github tofu root adopting kubexa org (talos-stack)
- Provider resolved via terraform MCP: `integrations/github` latest = 6.13.0, so `~> 6.13` pin matches plan verbatim. `github_organization_settings` takes required `billing_email` + optionals; import id is the NUMERIC org id (`terraform import github_organization_settings.test 123456789`), expressed as OpenTofu `import { to = ...; id = var.github_org_id }` — `tofu validate` accepts the block with zero provider network calls.
- Sibling conventions copied verbatim in spirit: S3 backend (same rook rgw bucket/endpoint, key `github/terraform.tfstate`), vault provider (`address = var.vault_address` + `token = var.vault_token`), `.gitignore` (`*.tfvars`/`!*.example.tfvars`/`.terraform*`/`*.tfstate*` — lockfile stays local like harbor).
- Secret hygiene: PAT + webhook secret are sensitive `var`s (TF_VAR-injected, never literal) written via `vault_kv_secret_v2` with `data_json_wo` + `data_json_wo_version = 1` (harbor style, keeps values out of state) at `kv/apps/tekton-tasks/github-webhook-config` with keys `provider.token`/`webhook.secret`. Plain `data_json` (gitea style) would leak into state — do not copy that half.
- Gotcha: `tofu fmt` wants full-column alignment in `main.tf` (fmt -check exits 3); fix with `tofu fmt -recursive .` scoped to `github/`, never repo-root (todos 42/43 own harbor/ + gitea/).
- Gotcha: `rg -nE 'ghp_|github_pat_'` errors under this shell (`unknown encoding` — flag rewrite); `grep -rnE` is the working equivalent, exit 1 + empty output = clean gate.
- Quirk: `tofu init -backend=false` suffices for `validate` (no S3 creds needed); no `apply` per task.

## 2026-10-14 — Task 66: workspace pin bump (serde-saphyr 2, rand 0.10, sha2 0.11, OTEL 0.33, tracing-OTEL 0.34, clap 4.6, k8s-openapi v1_36, toolchain 1.92)
- Zero API migration needed for the "major" bumps: serde-saphyr 2 keeps `to_string`/`from_str`; rand+sha2 are e2e dev-deps unused in src; OTLP 0.33 `SpanExporter::builder().with_tonic().with_endpoint().build()` + `SdkTracerProvider::builder().with_batch_exporter()` unchanged (default + `--features otlp` builds green).
- Only real breakage came from the TOOLCHAIN, not the crates: rustc 1.92 clippy fires `derivable_impls` on the hand-written `impl Default for DeletionPolicy` — fixed by `#[derive(Default)]` + `#[default]` on Retain, semantics identical.
- k8s-openapi v1_33->v1_36 rewrites generated text: deploy/crds + charts crds + docs drift (EnvFiles `fileRef`, toleration Lt/Gt operators, anti-affinity weight-subtraction wording). `cargo test` itself regenerates via write-if-different; commit the regenerated files and prove with `mise run check-drift` (exit 0).
- xtask gen_crds tests race each other: all four shell out to `xtask gen-crds` concurrently, so the first post-bump run can fail `gen_crds_is_idempotent` on a mid-write snapshot; rerun with no changes goes green. Flaky-by-design, not a code bug.
- `cargo tree -i <crate>` goes ambiguous (exit 101) when a third-party transitive keeps the old major (rand 0.9.5 via tungstenite<-kube, serde-saphyr 0.0.29 via jsonpath-rust<-kube). Disambiguate with `cargo tree -i rand@0.10.3` — ours is the direct pin, theirs is kube-internal and untouchable.
- envtest 0.3.0 still defaults to `r2g`/rust2go (bindgen->libclang): deviation STANDS, not re-added. Evidence `.omo/evidence/task-66-hydradb-operator.txt`.
## 2026-10-14 — Task 43: gitea oss.tf commit (talos-stack, commit-only)
- Gates: `tofu fmt -check -recursive .` 0, `tofu validate` 0, `rg resource "gitea_repository"` exit 1 (no repo resources). Commit `7b67687` adds exactly `terraform/environments/gitea/oss.tf` (83 lines). Evidence `.omo/evidence/task-43-hydradb-operator.txt`.
- Task 36 landed as cc08ac6 (chart skeleton: Chart.yaml, values.yaml, values.schema.json, icon.svg); all three gates exit 0, evidence `.omo/evidence/task-36-hydradb-operator.txt`.

## 2026-10-14 — Task 37: chart templates (Deployment, RBAC, SA, Service, ServiceMonitor, NOTES)
- 7 files in `charts/hydradb-operator/templates/`: `_helpers.tpl` (fullname/labels/selectorLabels/serviceAccountName/isCluster/rbacKind/bindingKind/image helpers), `deployment.yaml`, `serviceaccount.yaml`, `rbac.yaml` (2 docs: role + binding), `service.yaml`, `servicemonitor.yaml` (guarded by `monitoring.serviceMonitor.enabled`), `NOTES.txt` (scope summary + CRD upgrade path: `kubectl apply --server-side -f crds/` before `helm upgrade`).
- RBAC rules copied from `deploy/rbac/operator-clusterrole.yaml` (15 rules); `installScope` switch changes BOTH kind and binding kind (`Role`/`RoleBinding` vs `ClusterRole`/`ClusterRoleBinding`). YAML-parse compare proves semantic identity in both scopes (indent differs, content same).
- Deployment facts: replicas from `.Values.replicaCount` (default 1); NO volumes/volumeMounts (mounts no secrets); pod+container security (uid/gid 65532, non-root, readOnlyRootFilesystem, drop ALL, RuntimeDefault); probes `/healthz` on 8080 (matches controller `HEALTH_ADDR 0.0.0.0:8080` in run.rs); env `OPERATOR_NAMESPACE` (fieldRef), `OPERATOR_SCOPE` (installScope), `RUST_LOG` (log.level); image helper appends `@sha256:` digest when set; namespace always `.Release.Namespace`.
- Gates: `helm lint --strict` 0; default render 5 docs (Role path, no ServiceMonitor); cluster render 5 docs (ClusterRole path); `installScope=bogus` exits 1 via schema enum; `kubeconform -strict` exits 0 on both default renders. SM-enabled render: only failure is `could not find schema for ServiceMonitor` (catalog lacks monitoring.coreos.com) — not a manifest error; no `--ignore-missing-schema` used.
- kubeconform was absent on host; installed via `go install github.com/yannh/kubeconform/cmd/kubeconform@latest` (GOPATH/bin on PATH).
- LSP YAML diagnostics on `templates/*.yaml` are false positives (Go template syntax unparseable as YAML) — helm template/lint are the real gates.
- `crds/` already byte-identical to `deploy/crds/` (3 files); untouched, not staged.

## 2026-10-14 — Task 30: scale + output modes + CLI tests
- `Printer::render(&Value) -> Result<String>` added as pure core; `print` just prints it. Lets one fixture assert all five modes (name/wide/json/yaml + table) without capturing stdout. Pattern for future CLI output tests.
- Generic table rows: KIND/NAMESPACE/NAME/STATUS (+READY/AGE for wide) derived from `status.conditions` with same precedence family as status.rs (Failed > Ready > InProgress > Unknown); read commands keep their own richer tables and route only json/name through Printer.
- Scale: `parse_target` before any cluster contact (unknown kind/missing slash/empty name → CliError::Output naming valid kinds); `scale_patch` builds `{"spec":{"replicas":N}}` merge patch only — CR is API, StatefulSet/Deployment never touched. `ScaleArgs.replicas: u32` gives `--replicas -1` parse failure (exit 2) for free.
- `use super::*` in `pub mod scale` only brings `Cli, CliError` (what mod.rs imports at top); `build_client/build_kube_config/effective_namespace` need explicit `use crate::{...}`.
- No live cluster on box: happy-path patch proven by unit test + binary run that fails only at kubeconfig load (exit 1 kubeconfig error, not parse error). Same limitation as tasks 28/29.
- Gates: build 0, clippy `-D warnings` 0, test 0 (66 passed), fmt -p hydradb 0. Chart-YAML lsp diagnostics during work are sibling todo-37 Helm-template false positives; ignored per protocol.
- Task 22 (PDB/NetworkPolicy/ServiceMonitor): `monitoring.coreos.com` has no k8s-openapi type — applies go through `DynamicObject` + `Api::namespaced_with`, watches need a minimal `Resource<DynamicType=()>` marker because `Controller::owns` rejects `DynamicObject`. `DynamicObject.data` is `serde_json::Value` in kube 4 (not a Map). NetworkPolicy egress pinned to DNS + 80/443 TCP (no per-endpoint CIDR field on CR). New API field broke exhaustive `GraphClusterSpec` fixture in api/tests/validation.rs (E0063) — fixture needs the field too. Concurrent sibling agent refactoring xtask mid-todo: `cargo fmt -p` still touches their in-progress files, so scope fmt to own packages and never stage their paths; their `gen-all` run had already picked up my uncommitted API field (gen output deterministic, `gen-all --check` clean after). Gates: build 0, clippy 0, test -p hydradb-operator 106 passed, api 37 passed.

## 2026-10-15 — Task 34: check-drift, check-wiring, check-phases gates
- Split each gen module into `render_all()` (pure) + `run()` (write); `--check` diffs render vs disk, names every drifted file, exits 1. Never pipe gates through tail/head — pipe hides the exit code these gates exist to signal.
- check-wiring flattens live CRD `spec` schemas (not Rust source) to leaf names, checks camelCase + snake_case against controller text. k8s-openapi deep leaves (affinity/extraEnv/tolerations/topologySpreadConstraints) never named since parents cloned wholesale — covered by 4 prefix entries with reasons, leaf entries win over prefixes (else stale-entry false positive). 7 genuinely-unread custom leaves allowlisted with reasons (service ports use constants, etc.).
- check-phases brace-balances `match` blocks mentioning the 5 PhaseLabel enums, requires `Unknown` or `_ =>`; `matches!` excluded, strings/comments stripped. Controller currently has zero such matches (label() style) so gate passes vacuously + 5 unit tests guard the scanner.
- Concurrent sibling todo landed 10182b6 mid-task (spec.monitoring + regen); its uncommitted state transiently broke build/clippy/test — waited, re-verified all green on merged tree, staged only own paths. `touch` on missing lib.rs creates 0B files — deleted the stray, never commit it.
- Gates: build 0, clippy `-D warnings` 0, test 0 (233 passed), gen-all --check 0 (10 files), mise check-drift 0, QA tamper 1 naming file then 0 after revert.
