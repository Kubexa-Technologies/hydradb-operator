//! Unit tests for the copy engine: plan ordering, verification and the
//! environment contract, all against an in-memory sink.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::*;

/// In-memory [`ObjectSink`] shared by source and target in a test.
#[derive(Clone, Default)]
struct MemorySink {
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
}

impl MemorySink {
    fn with(entries: &[(&str, usize)]) -> Self {
        let sink = Self::default();
        for (key, size) in entries {
            sink.objects
                .lock()
                .expect("lock")
                .insert((*key).to_owned(), vec![b'x'; *size]);
        }
        sink
    }

    fn keys(&self) -> Vec<String> {
        self.objects.lock().expect("lock").keys().cloned().collect()
    }
}

impl ObjectSink for MemorySink {
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectEntry>, BackupError> {
        let prefix = prefix.trim_matches('/');
        Ok(self
            .objects
            .lock()
            .expect("lock")
            .iter()
            .filter(|(key, _)| prefix.is_empty() || key.starts_with(prefix))
            .map(|(key, value)| ObjectEntry {
                key: key.clone(),
                size: value.len() as u64,
            })
            .collect())
    }

    async fn copy_between(
        source: &Self,
        target: &Self,
        entry: &ObjectEntry,
        target_key: &str,
    ) -> Result<(), BackupError> {
        let value = source
            .objects
            .lock()
            .expect("lock")
            .get(&entry.key)
            .cloned()
            .ok_or_else(|| BackupError::Copy {
                key: entry.key.clone(),
                message: "missing at source".to_owned(),
            })?;
        target
            .objects
            .lock()
            .expect("lock")
            .insert(target_key.to_owned(), value);
        Ok(())
    }
}

fn entry(key: &str, size: u64) -> ObjectEntry {
    ObjectEntry {
        key: key.to_owned(),
        size,
    }
}

#[test]
fn manifest_is_always_copied_last() {
    let entries = vec![
        entry("g/manifest.json", 10),
        entry("g/wal/0001", 100),
        entry("g/wal/0002", 100),
    ];
    let plan = plan_copy(&entries, "g", "backups/demo");
    assert_eq!(
        plan.last().map(|(from, _)| from.as_str()),
        Some("g/manifest.json"),
        "the manifest must land last so a partial copy is never adoptable"
    );
    assert_eq!(plan.len(), 3);
    assert_eq!(plan[0].1, "backups/demo/wal/0001");
}

#[test]
fn nested_manifest_and_prefixes_with_slashes_are_handled() {
    let entries = vec![entry("/g/nested/manifest.json", 1), entry("/g/a", 2)];
    let plan = plan_copy(&entries, "/g/", "/target/");
    assert_eq!(
        plan.last().map(|(from, _)| from.as_str()),
        Some("/g/nested/manifest.json")
    );
    assert_eq!(plan[0].1, "target/a");
}

#[test]
fn verification_detects_a_missing_object() {
    let source = vec![entry("g/manifest.json", 5), entry("g/a", 5)];
    let target = vec![entry("t/manifest.json", 5)];
    let error = verify(&source, "g", &target, "t").expect_err("a missing object must fail");
    match error {
        BackupError::Verify {
            source_objects,
            target_objects,
            ..
        } => {
            assert_eq!(source_objects, 2);
            assert_eq!(target_objects, 1);
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn verification_detects_a_size_mismatch() {
    let source = vec![entry("g/a", 5)];
    let target = vec![entry("t/a", 6)];
    assert!(verify(&source, "g", &target, "t").is_err());
}

#[test]
fn verification_accepts_an_exact_copy() {
    let source = vec![entry("g/a", 5), entry("g/manifest.json", 1)];
    let target = vec![entry("t/a", 5), entry("t/manifest.json", 1)];
    verify(&source, "g", &target, "t").expect("an exact copy verifies");
}

#[tokio::test]
async fn run_copies_everything_and_reports_a_count() {
    let source = MemorySink::with(&[("g/a", 4), ("g/manifest.json", 2)]);
    let target = MemorySink::default();
    let config = RunConfig {
        direction: Direction::Backup,
        source_bucket: "src".to_owned(),
        source_prefix: "g".to_owned(),
        target_bucket: "dst".to_owned(),
        target_prefix: "backups/demo".to_owned(),
        verify: true,
    };
    let copied = run(&source, &target, &config).await.expect("copy runs");
    assert_eq!(copied, 2);
    assert_eq!(
        target.keys(),
        vec![
            "backups/demo/a".to_owned(),
            "backups/demo/manifest.json".to_owned()
        ]
    );
}

#[tokio::test]
async fn an_empty_source_is_an_error_not_a_success() {
    let source = MemorySink::default();
    let target = MemorySink::default();
    let config = RunConfig {
        direction: Direction::Backup,
        source_bucket: "src".to_owned(),
        source_prefix: "g".to_owned(),
        target_bucket: "dst".to_owned(),
        target_prefix: "t".to_owned(),
        verify: false,
    };
    let error = run(&source, &target, &config)
        .await
        .expect_err("empty must fail");
    assert!(matches!(error, BackupError::EmptySource(_)));
}

#[test]
fn direction_parsing_is_strict() {
    assert_eq!(Direction::parse("backup").expect("ok"), Direction::Backup);
    assert_eq!(Direction::parse("restore").expect("ok"), Direction::Restore);
    assert!(Direction::parse("sync").is_err());
}

#[test]
fn key_helpers_never_double_slashes() {
    assert_eq!(join_key("", "/a"), "a");
    assert_eq!(join_key("/g/", "/a"), "g/a");
    assert_eq!(strip_prefix("g", "g/a"), "a");
    assert_eq!(strip_prefix("", "/a"), "a");
    assert_eq!(strip_prefix("other", "g/a"), "g/a");
}
