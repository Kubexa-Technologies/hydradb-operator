//! TDD RED: failing tests for todo 7 (Unknown-fallback enums + strict parsers).
//!
//! These tests reference types that do not exist yet
//! (`BuildMode`, `CacheType`, `ServiceType`, `PullPolicy`, `ConditionReason`,
//! `strict_option`); they must FAIL to compile until the implementation
//! lands. Names all contain `unknown_variant` so the plan's
//! `cargo test -p hydradb-operator-api unknown_variant` filter selects them.

use super::{BuildMode, CacheType, ConditionReason, PullPolicy, ServiceType};
use serde::Deserialize;

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildModeHolder {
    #[serde(default, deserialize_with = "super::strict_option")]
    build_mode: Option<BuildMode>,
}

#[test]
fn unknown_variant_build_mode_known() {
    let parsed: BuildMode =
        serde_json::from_str("\"incremental\"").expect("known variant must parse");
    assert_eq!(parsed, BuildMode::Incremental);
}

#[test]
fn unknown_variant_build_mode_round_trip() {
    let parsed: BuildMode =
        serde_json::from_str("\"brand-new-mode\"").expect("unknown string must not error");
    assert_eq!(parsed, BuildMode::Unknown("brand-new-mode".to_string()));
    let back = serde_json::to_string(&parsed).expect("serialize must succeed");
    assert_eq!(back, "\"brand-new-mode\"");
}

#[test]
fn unknown_variant_other_enums_round_trip() {
    let cache: CacheType = serde_json::from_str("\"emptyDir\"").expect("known cache type");
    assert_eq!(cache, CacheType::EmptyDir);
    let cache_unknown: CacheType =
        serde_json::from_str("\"future-cache\"").expect("unknown cache type");
    assert_eq!(
        cache_unknown,
        CacheType::Unknown("future-cache".to_string())
    );
    assert_eq!(
        serde_json::to_string(&cache_unknown).expect("serialize"),
        "\"future-cache\""
    );

    let service: ServiceType = serde_json::from_str("\"ClusterIP\"").expect("known service type");
    assert_eq!(service, ServiceType::ClusterIP);
    let service_unknown: ServiceType =
        serde_json::from_str("\"HyperClusterIP\"").expect("unknown service type");
    assert_eq!(
        serde_json::to_string(&service_unknown).expect("serialize"),
        "\"HyperClusterIP\""
    );

    let policy: PullPolicy = serde_json::from_str("\"IfNotPresent\"").expect("known pull policy");
    assert_eq!(policy, PullPolicy::IfNotPresent);
    let policy_unknown: PullPolicy =
        serde_json::from_str("\"Sometimes\"").expect("unknown pull policy");
    assert_eq!(
        serde_json::to_string(&policy_unknown).expect("serialize"),
        "\"Sometimes\""
    );

    let reason: ConditionReason = serde_json::from_str("\"Reconciling\"").expect("known reason");
    assert_eq!(reason, ConditionReason::Reconciling);
    let reason_unknown: ConditionReason =
        serde_json::from_str("\"QuantumFlux\"").expect("unknown reason");
    assert_eq!(
        serde_json::to_string(&reason_unknown).expect("serialize"),
        "\"QuantumFlux\""
    );
}

#[test]
fn unknown_variant_non_string_rejected() {
    assert!(serde_json::from_str::<BuildMode>("42").is_err());
    assert!(serde_json::from_str::<BuildMode>("null").is_err());
    assert!(serde_json::from_str::<BuildMode>("{}").is_err());
    assert!(serde_json::from_str::<BuildMode>("true").is_err());
}

#[test]
fn unknown_variant_absent_option_stays_none() {
    let holder: BuildModeHolder = serde_json::from_str("{}").expect("absent field must parse");
    assert_eq!(holder.build_mode, None);
}

#[test]
fn unknown_variant_strict_option_values() {
    let explicit_null: BuildModeHolder =
        serde_json::from_str("{\"buildMode\": null}").expect("explicit null must parse");
    assert_eq!(explicit_null.build_mode, None);

    let known: BuildModeHolder =
        serde_json::from_str("{\"buildMode\": \"full\"}").expect("known value must parse");
    assert_eq!(known.build_mode, Some(BuildMode::Full));

    let unknown: BuildModeHolder =
        serde_json::from_str("{\"buildMode\": \"hyper-full\"}").expect("unknown value must parse");
    assert_eq!(
        unknown.build_mode,
        Some(BuildMode::Unknown("hyper-full".to_string()))
    );

    let non_string = serde_json::from_str::<BuildModeHolder>("{\"buildMode\": 7}");
    assert!(non_string.is_err());
}
