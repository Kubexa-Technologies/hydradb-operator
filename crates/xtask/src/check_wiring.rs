//! `check-wiring`: every CRD `spec` field must be referenced by a reconciler.
//!
//! The live CRD schemas (the same source `gen-crds` serializes) are flattened
//! to leaf field names; each leaf (camelCase as on the wire, plus its
//! snake_case Rust form) must occur somewhere under `crates/controller/src`.
//! A field no reconciler names is either dead surface or a missed read, and
//! CI must fail loudly. [`WIRING_ALLOWLIST`] pins the known exceptions; every
//! entry carries its reason, and a stale entry (field now wired) also fails so
//! the list cannot silently grow.

use std::path::{Path, PathBuf};

use hydradb_operator_api::crd::{GraphCluster, GraphIndexer, GraphNodePool};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::CustomResourceExt;

/// Known-unwired fields: `(wire name, why no reconciler names it)`.
///
/// Each entry needs its reason inline: the allowlist must never become a
/// dumping ground for unexamined drift.
const WIRING_ALLOWLIST: &[(&str, &str)] = &[
    // Services are built from the BOLT_PORT/HTTPS_PORT/METRICS_PORT constants
    // in reconcile/cluster.rs; the per-cluster port overrides are reserved
    // surface and not honoured yet.
    (
        "boltPort",
        "ServiceSpec override unused: Services use hardcoded port constants",
    ),
    // Same as boltPort: reserved override, Services use hardcoded constants.
    (
        "httpsPort",
        "ServiceSpec override unused: Services use hardcoded port constants",
    ),
    // TlsSpec.enabled/secretName are referenced; only the cert-manager DNS SAN
    // extension has no consumer yet.
    (
        "additionalDnsNames",
        "TlsSpec extension with no consumer: serving cert path ignores DNS SANs",
    ),
    // NodeServiceSpec overrides: the worker Service is derived from pod DNS,
    // never from these fields.
    (
        "advertisedBoltAddress",
        "NodeServiceSpec override unused: worker Service derives addresses from pod DNS",
    ),
    // Same as advertisedBoltAddress: Service type comes from the chart scope.
    (
        "boltType",
        "NodeServiceSpec override unused: Service type comes from install scope",
    ),
    // Inline S3 credentials are never mounted; the OBC path reads its own
    // provisioned Secret instead.
    (
        "credentialsSecretRef",
        "S3 inline credentials secret never mounted: only the OBC claim path is read",
    ),
    // Only CacheStorage backend selection drives volumes; no cgroup budget is
    // derived from this field yet.
    (
        "memoryLimitBytes",
        "CacheSpec RAM budget not enforced: only the storage backend selects volumes",
    ),
    // spec.affinity is cloned wholesale into the StatefulSet/Deployment pod
    // specs (pool.spec.affinity.clone()), so these k8s-openapi leaf
    // identifiers never appear by name in the controller.
    (
        "nodeAffinity",
        "passed wholesale via spec.affinity.clone(); leaf never named",
    ),
    // Same wholesale-affinity passthrough as nodeAffinity.
    (
        "nodeSelectorTerms",
        "passed wholesale via spec.affinity.clone(); leaf never named",
    ),
    // Same wholesale-affinity passthrough as nodeAffinity.
    (
        "podAffinity",
        "passed wholesale via spec.affinity.clone(); leaf never named",
    ),
    // Same wholesale-affinity passthrough as nodeAffinity.
    (
        "podAntiAffinity",
        "passed wholesale via spec.affinity.clone(); leaf never named",
    ),
    // Same wholesale-affinity passthrough as nodeAffinity.
    (
        "preferredDuringSchedulingIgnoredDuringExecution",
        "passed wholesale via spec.affinity.clone(); leaf never named",
    ),
    // Same wholesale-affinity passthrough as nodeAffinity.
    (
        "requiredDuringSchedulingIgnoredDuringExecution",
        "passed wholesale via spec.affinity.clone(); leaf never named",
    ),
];

/// Subtrees passed wholesale into child objects: every k8s-openapi leaf below
/// one of these dotted prefixes is wired through its parent, so leaf
/// identifiers never appear by name in the controller. Kept separate from
/// [`WIRING_ALLOWLIST`] because these cover only foreign (k8s-openapi) leaves:
/// a custom field can never hide below them, since the parent structs are
/// foreign types. Each entry still needs its reason inline.
const WIRING_PREFIX_ALLOWLIST: &[(&str, &str)] = &[
    // pool.spec.affinity.clone() / indexer.spec.affinity.clone() land verbatim
    // in the StatefulSet/Deployment pod specs (nodepool.rs, indexer.rs).
    (
        "spec.affinity",
        "k8s Affinity cloned wholesale into pod specs",
    ),
    // env.extend(spec.extra_env.iter().cloned()) appends the whole list to the
    // operand container env (nodepool.rs, indexer.rs).
    (
        "spec.extraEnv",
        "k8s EnvVar list extended wholesale into container env",
    ),
    // pool.spec.tolerations.clone() lands verbatim in the pod specs.
    (
        "spec.tolerations",
        "k8s Toleration list cloned wholesale into pod specs",
    ),
    // pool.spec.topology_spread_constraints.clone() lands verbatim in the
    // StatefulSet pod spec (nodepool.rs only; the indexer has no such field).
    (
        "spec.topologySpreadConstraints",
        "k8s TopologySpreadConstraint list cloned wholesale into the pod spec",
    ),
];

/// True when dotted spec path `path` falls under `prefix`: equality, a `.`
/// child, or an array-item child (`prefix[]...`).
fn under_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path.starts_with(&format!("{prefix}."))
        || path.starts_with(&format!("{prefix}["))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn camel_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.char_indices() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Collect `(dotted path, leaf name)` for every property under `spec`.
fn collect_spec_leaves(node: &serde_json::Value, prefix: &str, out: &mut Vec<(String, String)>) {
    let Some(props) = node.get("properties").and_then(|p| p.as_object()) else {
        return;
    };
    let mut keys: Vec<&str> = props.keys().map(String::as_str).collect();
    keys.sort_unstable();
    for key in keys {
        let Some(field) = props.get(key) else {
            continue;
        };
        let path = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        out.push((path.clone(), key.to_owned()));
        // Recurse into the field itself, its array items, and combiner
        // members (mirrors gen-docs flattening so the field set matches).
        collect_spec_leaves(field, &path, out);
        match field.get("items") {
            Some(serde_json::Value::Array(members)) => {
                for member in members {
                    collect_spec_leaves(member, &format!("{path}[]"), out);
                }
            }
            Some(items) => collect_spec_leaves(items, &format!("{path}[]"), out),
            None => {}
        }
        for combiner in ["anyOf", "oneOf", "allOf"] {
            if let Some(serde_json::Value::Array(members)) = field.get(combiner) {
                for member in members {
                    collect_spec_leaves(member, &path, out);
                }
            }
        }
    }
}

fn spec_schema_of(crd: &CustomResourceDefinition) -> anyhow::Result<(String, serde_json::Value)> {
    let kind = crd.spec.names.kind.clone();
    let value = serde_json::to_value(crd)?;
    let schema = value
        .get("spec")
        .and_then(|spec| spec.get("versions"))
        .and_then(|versions| versions.as_array())
        .and_then(|versions| versions.first())
        .and_then(|first| first.get("schema"))
        .and_then(|schema| schema.get("openAPIV3Schema"))
        .and_then(|schema| schema.get("properties"))
        .and_then(|props| props.get("spec"))
        .cloned();
    match schema {
        Some(schema) => Ok((kind, schema)),
        None => anyhow::bail!("CRD {kind} has no spec schema"),
    }
}

fn controller_sources(root: &Path) -> anyhow::Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    let mut dirs = vec![root.join("crates/controller/src")];
    while let Some(dir) = dirs.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|err| anyhow::anyhow!("read {}: {err}", dir.display()))?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path)?;
                out.push((path, text));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Run the wiring gate: fail naming every spec field no reconciler references.
pub fn run() -> anyhow::Result<()> {
    let crds: Vec<CustomResourceDefinition> = vec![
        GraphCluster::crd(),
        GraphNodePool::crd(),
        GraphIndexer::crd(),
    ];
    let root = workspace_root();
    let sources = controller_sources(&root)?;
    if sources.is_empty() {
        anyhow::bail!("check-wiring found no controller sources under crates/controller/src");
    }
    let haystack: Vec<String> = sources.iter().map(|(_, text)| text.clone()).collect();
    let haystack = haystack.join("\n");

    let mut unwired: Vec<String> = Vec::new();
    let mut seen_allowlisted: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    let mut seen_prefix: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for crd in &crds {
        let (kind, spec) = spec_schema_of(crd)?;
        let mut leaves = Vec::new();
        collect_spec_leaves(&spec, "spec", &mut leaves);
        if leaves.is_empty() {
            anyhow::bail!("CRD {kind} flattened to zero spec fields");
        }
        for (path, leaf) in &leaves {
            let snake = camel_to_snake(leaf);
            if haystack.contains(leaf.as_str()) || haystack.contains(snake.as_str()) {
                continue;
            }
            // Leaf entries win over subtree prefixes so a bare intermediate
            // node (e.g. spec.affinity.nodeAffinity) still counts as seen.
            if let Some((_, reason)) = WIRING_ALLOWLIST.iter().find(|(name, _)| name == leaf) {
                seen_allowlisted.insert(leaf.clone());
                println!("allowlisted unwired field {path} ({kind}): {reason}");
                continue;
            }
            if let Some(prefix) = WIRING_PREFIX_ALLOWLIST
                .iter()
                .map(|(prefix, _)| prefix)
                .find(|prefix| under_prefix(path, prefix))
            {
                seen_prefix.insert(prefix);
                continue;
            }
            unwired.push(format!("{path} ({kind})"));
        }
    }
    // Allowlist hygiene: an entry that no longer matches an unwired field is
    // stale and must be removed, not left to rot.
    let mut stale: Vec<String> = Vec::new();
    for (name, _) in WIRING_ALLOWLIST {
        if !seen_allowlisted.contains(*name) {
            stale.push((*name).to_owned());
        }
    }
    for (prefix, _) in WIRING_PREFIX_ALLOWLIST {
        if !seen_prefix.contains(prefix) {
            stale.push(format!("prefix:{prefix}"));
        }
    }
    if !stale.is_empty() {
        anyhow::bail!(
            "check-wiring: stale allowlist entries (field is now wired, remove them): {}",
            stale.join(", ")
        );
    }
    if !unwired.is_empty() {
        unwired.sort();
        unwired.dedup();
        anyhow::bail!(
            "check-wiring: {} CRD spec field(s) no reconciler references (add a consumer or an allowlist entry with reason):\n  {}",
            unwired.len(),
            unwired.join("\n  ")
        );
    }
    println!(
        "check-wiring: all CRD spec fields referenced ({} allowlisted)",
        seen_allowlisted.len()
    );
    Ok(())
}
