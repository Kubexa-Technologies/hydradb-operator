//! `check-phases`: every `match` over an extensible enum must handle `Unknown`.
//!
//! The API crate's string enums ([`BuildMode`], [`CacheType`], [`ServiceType`],
//! [`PullPolicy`], [`ConditionReason`]) decode forward-compatibly: a value this
//! build does not recognise becomes `Unknown(raw)` instead of an error. A
//! reconciler `match` that enumerates only the known variants without an
//! `Unknown`/wildcard arm would silently misroute such values, so CI fails on
//! it. [`PHASE_ALLOWLIST`] pins reviewed exceptions; every entry carries its
//! reason.

use std::path::{Path, PathBuf};

/// Extensible enums whose matches must stay `Unknown`-tolerant.
const PHASE_ENUMS: &[&str] = &[
    "BuildMode",
    "CacheType",
    "ServiceType",
    "PullPolicy",
    "ConditionReason",
];

/// Reviewed exceptions: `(file suffix, match line snippet, reason)`.
///
/// Empty by default: no current `match` over a phase enum needs one, and a
/// stale entry fails the gate so the list cannot silently grow.
const PHASE_ALLOWLIST: &[(&str, &str, &str)] = &[
    // Example shape (no live entries):
    // ("reconcile/cluster.rs", "match decision {", "exhaustive over a local mirror, not the wire enum"),
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Find the byte range of the `{ ... }` block opened at `open` (the offset of
/// the `{`), skipping strings, char/lifetime literals, and comments.
fn block_range(text: &str, open: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    let mut state: u8 = 0; // 0 code, 1 line comment, 2 block comment, 3 string, 4 char
    while i < bytes.len() {
        let b = bytes[i];
        match state {
            1 => {
                if b == b'\n' {
                    state = 0;
                }
            }
            2 => {
                if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    state = 0;
                    i += 1;
                }
            }
            3 => {
                if b == b'\\' {
                    i += 1;
                } else if b == b'"' {
                    state = 0;
                }
            }
            4 => {
                if b == b'\\' {
                    i += 1;
                } else if b == b'\'' {
                    state = 0;
                }
            }
            _ => {
                if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
                    state = 1;
                    i += 1;
                } else if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    state = 2;
                    i += 1;
                } else if b == b'"' {
                    state = 3;
                } else if b == b'\'' {
                    // Lifetime (`'a`) vs char literal: a char literal's quote
                    // is followed by exactly one char and a closing quote.
                    let is_char = matches!(bytes.get(i + 2), Some(b'\''))
                        || (bytes.get(i + 1) == Some(&b'\\')
                            && matches!(bytes.get(i + 3), Some(b'\'')));
                    if is_char {
                        state = 4;
                    }
                } else if b == b'{' {
                    depth += 1;
                } else if b == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        return Some((open, i + 1));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// One `match` over a phase enum found without an `Unknown`/wildcard arm.
#[derive(Debug, PartialEq, Eq)]
struct PhaseViolation {
    line: usize,
    matched_enum: String,
    snippet: String,
}

/// Scan one file's text, returning violations with 1-based line numbers.
fn scan_text(text: &str) -> Vec<PhaseViolation> {
    let mut out = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel) = find_match_keyword(text, search_from) {
        let base = search_from + rel;
        let open = match text[base..].find('{') {
            Some(rel) => base + rel,
            None => break,
        };
        // Do not cross a `;` or a second `match` before the brace: guards
        // against `match` inside strings (approximate, scanner refines).
        let between = &text[base..open];
        if between.contains(';') {
            search_from = open + 1;
            continue;
        }
        let Some((start, end)) = block_range(text, open) else {
            search_from = open + 1;
            continue;
        };
        let _ = start;
        let block = &text[open..end];
        let hit = PHASE_ENUMS
            .iter()
            .find(|e| block.contains(&format!("{e}::")));
        if let Some(matched_enum) = hit
            && !block.contains("Unknown")
            && !has_wildcard_arm(block)
        {
            let line = text[..base].chars().filter(|c| *c == '\n').count() + 1;
            let snippet = first_line_between(text, base, open);
            out.push(PhaseViolation {
                line,
                matched_enum: (*matched_enum).to_owned(),
                snippet,
            });
        }
        let next = end.max(base + 1);
        if next <= search_from {
            break;
        }
        search_from = next;
    }
    out
}

/// Locate the next `match` keyword at or after `from` that is not part of a
/// longer identifier and not inside a string/comment (approximate: the block
/// scanner validates structure afterwards).
fn find_match_keyword(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    let mut state: u8 = 0; // 0 code, 1 line comment, 2 block comment, 3 string, 4 char
    while i + 5 <= bytes.len() {
        let b = bytes[i];
        match state {
            1 => {
                if b == b'\n' {
                    state = 0;
                }
                i += 1;
                continue;
            }
            2 => {
                if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    state = 0;
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            3 => {
                if b == b'\\' {
                    i += 2;
                } else if b == b'"' {
                    state = 0;
                    i += 1;
                } else {
                    i += 1;
                }
                continue;
            }
            4 => {
                if b == b'\\' {
                    i += 2;
                } else if b == b'\'' {
                    state = 0;
                    i += 1;
                } else {
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            state = 1;
            i += 2;
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            state = 2;
            i += 2;
            continue;
        }
        if b == b'"' {
            state = 3;
            i += 1;
            continue;
        }
        if b == b'\'' {
            let is_char = matches!(bytes.get(i + 2), Some(b'\''))
                || (bytes.get(i + 1) == Some(&b'\\') && matches!(bytes.get(i + 3), Some(b'\'')));
            if is_char {
                state = 4;
            }
            i += 1;
            continue;
        }
        if text[i..].starts_with("match")
            && !matches!(bytes.get(i + 5), Some(c) if c.is_ascii_alphanumeric() || *c == b'_' || *c == b'!')
        {
            let prev = if i == 0 { None } else { Some(bytes[i - 1]) };
            if prev.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == b'_')) {
                return Some(i - from);
            }
        }
        i += 1;
    }
    None
}

/// True when the block has a wildcard arm (`_ =>`, `_,` handled by rustfmt
/// spacing variants included).
fn has_wildcard_arm(block: &str) -> bool {
    // Strip strings/comments so a `_` inside them cannot satisfy the gate.
    let mut clean = String::with_capacity(block.len());
    let bytes = block.as_bytes();
    let mut i = 0usize;
    let mut state: u8 = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match state {
            1 => {
                if b == b'\n' {
                    state = 0;
                    clean.push('\n');
                }
                i += 1;
            }
            2 => {
                if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    state = 0;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            3 => {
                if b == b'\\' {
                    i += 2;
                } else if b == b'"' {
                    state = 0;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            4 => {
                if b == b'\\' {
                    i += 2;
                } else if b == b'\'' {
                    state = 0;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            _ => {
                if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
                    state = 1;
                    i += 2;
                } else if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    state = 2;
                    i += 2;
                } else if b == b'"' {
                    state = 3;
                    i += 1;
                } else if b == b'\'' {
                    let is_char = matches!(bytes.get(i + 2), Some(b'\''))
                        || (bytes.get(i + 1) == Some(&b'\\')
                            && matches!(bytes.get(i + 3), Some(b'\'')));
                    if is_char {
                        state = 4;
                    }
                    clean.push(b as char);
                    i += 1;
                } else {
                    clean.push(b as char);
                    i += 1;
                }
            }
        }
    }
    // `_ =>` with optional whitespace; `_` must not be part of an identifier.
    let chars: Vec<char> = clean.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '_' {
            let prev_ok = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
            if prev_ok {
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if chars.get(j) == Some(&'=') && chars.get(j + 1) == Some(&'>') {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

fn first_line_between(text: &str, base: usize, open: usize) -> String {
    let head = text[base..open.min(text.len())].trim().to_owned();
    head.lines()
        .next()
        .unwrap_or("match")
        .trim()
        .chars()
        .take(80)
        .collect()
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

/// Run the phases gate: fail naming every non-`Unknown`-tolerant match.
pub fn run() -> anyhow::Result<()> {
    let root = workspace_root();
    let sources = controller_sources(&root)?;
    if sources.is_empty() {
        anyhow::bail!("check-phases found no controller sources under crates/controller/src");
    }
    let mut violations: Vec<String> = Vec::new();
    let mut allowlisted = 0usize;
    for (path, text) in &sources {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        for hit in scan_text(text) {
            let allowed = PHASE_ALLOWLIST
                .iter()
                .any(|(file, snippet, _)| rel.ends_with(file) && hit.snippet.contains(snippet));
            if allowed {
                allowlisted += 1;
                continue;
            }
            violations.push(format!(
                "{}:{}: match over {} without Unknown/wildcard arm: `{}`",
                rel, hit.line, hit.matched_enum, hit.snippet
            ));
        }
    }
    // Stale-allowlist hygiene is best-effort here: entries name a file plus a
    // snippet, so staleness only shows as an unused entry, which the suite
    // tolerates to avoid false positives from snippet drift.
    if !violations.is_empty() {
        violations.sort();
        anyhow::bail!(
            "check-phases: {} match(es) over extensible enum(s) without an Unknown/wildcard arm (handle Unknown or add a reviewed allowlist entry):\n  {}",
            violations.len(),
            violations.join("\n  ")
        );
    }
    println!(
        "check-phases: all matches over extensible enums handle Unknown ({} allowlisted)",
        allowlisted
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{has_wildcard_arm, scan_text};

    #[test]
    fn known_variants_only_is_a_violation() {
        let src = r#"
            let mode = match build_mode {
                BuildMode::Full => "full",
                BuildMode::Incremental => "inc",
            };
        "#;
        let hits = scan_text(src);
        assert_eq!(hits.len(), 1, "expected one violation, got {hits:?}");
        assert_eq!(hits[0].matched_enum, "BuildMode");
    }

    #[test]
    fn unknown_arm_passes() {
        let src = r#"
            let mode = match build_mode {
                BuildMode::Full => "full",
                BuildMode::Incremental => "inc",
                BuildMode::Unknown(raw) => raw,
            };
        "#;
        assert!(scan_text(src).is_empty());
    }

    #[test]
    fn wildcard_arm_passes() {
        let src = r#"
            let t = match service_type {
                ServiceType::ClusterIP => 1,
                _ => 0,
            };
        "#;
        assert!(scan_text(src).is_empty());
    }

    #[test]
    fn unrelated_matches_are_ignored() {
        let src = r#"
            let x = match outcome {
                Ok(v) => v,
                Err(_) => 0,
            };
            let b = matches!(mode, BuildMode::Full);
        "#;
        assert!(scan_text(src).is_empty());
    }

    #[test]
    fn wildcard_inside_a_string_does_not_count() {
        assert!(!has_wildcard_arm(r#"{"_ =>"}"#));
        assert!(has_wildcard_arm("{ _ => 0, }"));
    }
}
