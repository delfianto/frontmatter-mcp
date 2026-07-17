use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Local;
use serde_json::Value as Json;
use serde_yaml::{Mapping, Value as Yaml};

// ── Frontmatter splitting ─────────────────────────────────────────────────────

/// Match a YAML separator line (`---` + optional trailing spaces + newline or EOF).
/// Returns the slice that follows the separator, or `None` if `s` doesn't start with one.
fn strip_sep(s: &str) -> Option<&str> {
    let s = s.strip_prefix("---")?;
    let s = s.trim_start_matches([' ', '\t']);
    // Accept \n, \r\n, or bare EOF
    s.strip_prefix('\n')
        .or_else(|| s.strip_prefix("\r\n"))
        .or(if s.is_empty() { Some("") } else { None })
}

/// Split a markdown file into `(Option<frontmatter_text>, body_text)`.
///
/// The body is returned byte-for-byte unchanged — it is never given to any parser.
pub fn split(text: &str) -> (Option<String>, String) {
    let after_open = match strip_sep(text) {
        Some(s) => s,
        None => return (None, text.to_owned()),
    };

    // Empty frontmatter: closing --- immediately follows opening ---
    if let Some(body) = strip_sep(after_open) {
        return (Some(String::new()), body.to_owned());
    }

    // Scan for the closing --- (must appear at the start of a line)
    let mut offset = 0;
    loop {
        match after_open[offset..].find('\n') {
            None => return (None, text.to_owned()), // malformed — no closing ---
            Some(rel) => {
                let nl = offset + rel;
                let candidate = &after_open[nl + 1..];
                if let Some(body) = strip_sep(candidate) {
                    // fm_text is everything before the \n that precedes ---
                    return (Some(after_open[..nl].to_owned()), body.to_owned());
                }
                offset = nl + 1;
            }
        }
    }
}

/// Reassemble a file from its frontmatter and body.
pub fn join(fm: &str, body: &str) -> String {
    if body.is_empty() {
        format!("---\n{fm}\n---")
    } else {
        format!("---\n{fm}\n---\n{body}")
    }
}

// ── YAML round-trip ───────────────────────────────────────────────────────────

fn parse_yaml(fm_text: &str) -> Result<Mapping> {
    // Normalise CRLF before handing to the parser
    let src = fm_text.replace("\r\n", "\n");
    if src.trim().is_empty() {
        return Ok(Mapping::new());
    }
    match serde_yaml::from_str::<Yaml>(&src).context("invalid YAML frontmatter")? {
        Yaml::Mapping(m) => Ok(m),
        _ => Ok(Mapping::new()),
    }
}

fn dump_yaml(mapping: &Mapping) -> Result<String> {
    let s = serde_yaml::to_string(&Yaml::Mapping(mapping.clone()))
        .context("failed to serialise YAML")?;
    // serde_yaml may prepend a `---\n` document marker — strip it
    let s = s.strip_prefix("---\n").unwrap_or(&s);
    Ok(s.trim_end_matches('\n').to_owned())
}

// ── Value conversions ─────────────────────────────────────────────────────────

fn json_to_yaml(v: Json) -> Yaml {
    match v {
        Json::Null => Yaml::Null,
        Json::Bool(b) => Yaml::Bool(b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Yaml::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                Yaml::Number(f.into())
            } else {
                Yaml::String(n.to_string())
            }
        }
        Json::String(s) => Yaml::String(s),
        Json::Array(a) => Yaml::Sequence(a.into_iter().map(json_to_yaml).collect()),
        Json::Object(o) => {
            let mut m = Mapping::new();
            for (k, v) in o {
                m.insert(Yaml::String(k), json_to_yaml(v));
            }
            Yaml::Mapping(m)
        }
    }
}

fn yaml_to_json(v: Yaml) -> Json {
    match v {
        Yaml::Null => Json::Null,
        Yaml::Bool(b) => Json::Bool(b),
        Yaml::Number(n) => {
            if let Some(i) = n.as_i64() {
                Json::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(Json::Number)
                    .unwrap_or(Json::Null)
            } else {
                Json::Null
            }
        }
        Yaml::String(s) => Json::String(s),
        Yaml::Sequence(a) => Json::Array(a.into_iter().map(yaml_to_json).collect()),
        Yaml::Mapping(m) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in m {
                if let Yaml::String(key) = k {
                    obj.insert(key, yaml_to_json(v));
                }
            }
            Json::Object(obj)
        }
        // Tagged values (e.g. !!binary) — unwrap the inner value
        Yaml::Tagged(t) => yaml_to_json(t.value),
    }
}

// ── Type inference ────────────────────────────────────────────────────────────

/// Returns true if `s` matches the Obsidian wikilink pattern `[[...]]`.
fn is_wikilink(s: &str) -> bool {
    s.len() > 4 && s.starts_with("[[") && s.ends_with("]]")
}

/// Returns true if `s` matches `YYYY-MM-DD`.
fn is_date(s: &str) -> bool {
    if s.len() != 10 {
        return false;
    }
    let b = s.as_bytes();
    b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(|c| c.is_ascii_digit())
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[8..10].iter().all(|c| c.is_ascii_digit())
}

/// Returns true if `s` matches `YYYY-MM-DD HH:MM:SS` or `YYYY-MM-DDTHH:MM:SS`
/// (with optional timezone suffix).
fn is_datetime(s: &str) -> bool {
    if s.len() < 19 {
        return false;
    }
    let b = s.as_bytes();
    is_date(&s[..10])
        && (b[10] == b' ' || b[10] == b'T')
        && b[..19][11..].iter().enumerate().all(|(i, &c)| {
            if i == 2 || i == 5 {
                c == b':'
            } else {
                c.is_ascii_digit()
            }
        })
}

/// Classify a scalar string into a specific type name.
fn classify_string(s: &str) -> &'static str {
    if is_wikilink(s) {
        "wikilink"
    } else if is_datetime(s) {
        "datetime"
    } else if is_date(s) {
        "date"
    } else {
        "string"
    }
}

/// Infer the common item type of a YAML sequence.
/// Returns the element type name if all items share one, or `"mixed"`.
fn classify_items(seq: &[Yaml]) -> &'static str {
    if seq.is_empty() {
        return "string";
    }
    let all = |f: &dyn Fn(&Yaml) -> bool| seq.iter().all(f);

    if all(&|v| matches!(v, Yaml::String(s) if is_wikilink(s))) {
        "wikilink"
    } else if all(&|v| matches!(v, Yaml::String(s) if is_datetime(s))) {
        "datetime"
    } else if all(&|v| matches!(v, Yaml::String(s) if is_date(s))) {
        "date"
    } else if all(&|v| matches!(v, Yaml::String(_))) {
        "string"
    } else if all(&|v| matches!(v, Yaml::Number(n) if n.as_i64().is_some() || n.as_u64().is_some()))
    {
        "integer"
    } else if all(&|v| matches!(v, Yaml::Number(_))) {
        "float"
    } else if all(&|v| matches!(v, Yaml::Bool(_))) {
        "boolean"
    } else {
        "mixed"
    }
}

/// Returns `(type_name, items_type)` for a YAML value.
/// `items_type` is only `Some` for sequences.
pub fn infer_type(v: &Yaml) -> (&'static str, Option<&'static str>) {
    match v {
        Yaml::Null => ("null", None),
        Yaml::Bool(_) => ("boolean", None),
        Yaml::Number(n) => {
            if n.as_i64().is_some() || n.as_u64().is_some() {
                ("integer", None)
            } else {
                ("float", None)
            }
        }
        Yaml::String(s) => (classify_string(s), None),
        Yaml::Sequence(seq) => ("array", Some(classify_items(seq))),
        Yaml::Mapping(_) => ("object", None),
        Yaml::Tagged(t) => infer_type(&t.value),
    }
}

/// Wrap a YAML value into `{"value": ..., "type": "...", ["items_type": "..."]}`.
fn annotate(v: Yaml) -> Json {
    let (type_name, items_type) = infer_type(&v);
    let value = yaml_to_json(v);
    let mut obj = serde_json::Map::new();
    obj.insert("value".into(), value);
    obj.insert("type".into(), Json::String(type_name.into()));
    if let Some(it) = items_type {
        obj.insert("items_type".into(), Json::String(it.into()));
    }
    Json::Object(obj)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn now_str() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Expand a leading `~/` to `$HOME/`, then resolve to an absolute `PathBuf`.
pub fn path_of(s: &str) -> PathBuf {
    if let Some(tail) = s.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(tail);
    }
    PathBuf::from(s)
}

fn load(path: &Path) -> Result<(Option<String>, String)> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read '{}'", path.display()))?;
    Ok(split(&text))
}

fn save(path: &Path, fm: &str, body: &str) -> Result<()> {
    std::fs::write(path, join(fm, body))
        .with_context(|| format!("cannot write '{}'", path.display()))
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Read YAML frontmatter as JSON.
///
/// Returns the full metadata object when `key` is `None`, or a single key's
/// value.  Returns `Json::Null` for missing keys.
pub fn read_meta(path: &Path, key: Option<&str>) -> Result<Json> {
    let (fm_opt, _) = load(path)?;
    let mapping = fm_opt.map_or(Ok(Mapping::new()), |fm| parse_yaml(&fm))?;

    Ok(match key {
        Some(k) => {
            let val = mapping
                .get(Yaml::String(k.to_owned()))
                .cloned()
                .unwrap_or(Yaml::Null);
            yaml_to_json(val)
        }
        None => {
            let obj = mapping
                .into_iter()
                .filter_map(|(k, v)| {
                    if let Yaml::String(key) = k {
                        Some((key, yaml_to_json(v)))
                    } else {
                        None
                    }
                })
                .collect();
            Json::Object(obj)
        }
    })
}

/// Like `read_meta` but wraps each value as `{"value": ..., "type": "...", ["items_type": "..."]}`.
///
/// Scalar types: `null`, `boolean`, `integer`, `float`, `date`, `datetime`, `wikilink`, `string`.
/// Sequence type: `array` with `items_type` set to the common element type, or `"mixed"`.
pub fn read_meta_typed(path: &Path, key: Option<&str>) -> Result<Json> {
    let (fm_opt, _) = load(path)?;
    let mapping = fm_opt.map_or(Ok(Mapping::new()), |fm| parse_yaml(&fm))?;

    Ok(match key {
        Some(k) => {
            let val = mapping
                .get(Yaml::String(k.to_owned()))
                .cloned()
                .unwrap_or(Yaml::Null);
            annotate(val)
        }
        None => {
            let obj = mapping
                .into_iter()
                .filter_map(|(k, v)| {
                    if let Yaml::String(key) = k {
                        Some((key, annotate(v)))
                    } else {
                        None
                    }
                })
                .collect();
            Json::Object(obj)
        }
    })
}

/// Patch YAML frontmatter keys in a Markdown file.
///
/// Only the keys present in `updates` are changed.  All other keys and the
/// entire file body are left byte-for-byte unchanged.  When `touch_updated`
/// is `true` the `updated` field is set to the current local timestamp.
pub fn write_meta(
    path: &Path,
    updates: serde_json::Map<String, Json>,
    touch_updated: bool,
) -> Result<()> {
    let (fm_opt, body) = load(path)?;
    let mut mapping = fm_opt.map_or(Ok(Mapping::new()), |fm| parse_yaml(&fm))?;

    for (k, v) in updates {
        mapping.insert(Yaml::String(k), json_to_yaml(v));
    }
    if touch_updated {
        mapping.insert(Yaml::String("updated".into()), Yaml::String(now_str()));
    }

    save(path, &dump_yaml(&mapping)?, &body)
}

/// Bump the `version` semver field in a Markdown file's frontmatter.
///
/// `level` must be `"major"`, `"minor"`, or `"patch"` (default).
/// Initialises `version` to `0.1.0` before bumping if the field is absent.
/// Also sets `updated` to the current local timestamp.
pub fn bump_version(path: &Path, level: &str) -> Result<String> {
    let (fm_opt, body) = load(path)?;
    let mut mapping = fm_opt.map_or(Ok(Mapping::new()), |fm| parse_yaml(&fm))?;

    let v_str = mapping
        .get(Yaml::String("version".into()))
        .and_then(|v| v.as_str())
        .unwrap_or("0.1.0")
        .to_owned();

    let new_v = bump_semver(&v_str, level)?;
    mapping.insert(Yaml::String("version".into()), Yaml::String(new_v.clone()));
    mapping.insert(Yaml::String("updated".into()), Yaml::String(now_str()));

    save(path, &dump_yaml(&mapping)?, &body)?;
    Ok(new_v)
}

fn bump_semver(v: &str, level: &str) -> Result<String> {
    let parts: Vec<&str> = v.split('.').collect();
    anyhow::ensure!(parts.len() == 3, "invalid semver '{v}'");
    let maj: u64 = parts[0].parse().context("invalid major component")?;
    let min: u64 = parts[1].parse().context("invalid minor component")?;
    let pat: u64 = parts[2].parse().context("invalid patch component")?;
    let (maj, min, pat) = match level {
        "major" => (maj + 1, 0, 0),
        "minor" => (maj, min + 1, 0),
        "patch" => (maj, min, pat + 1),
        other => anyhow::bail!("level must be major/minor/patch, got '{other}'"),
    };
    Ok(format!("{maj}.{min}.{pat}"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── splitting ─────────────────────────────────────────────────────────────

    #[test]
    fn split_no_frontmatter() {
        let (fm, body) = split("just body\n");
        assert!(fm.is_none());
        assert_eq!(body, "just body\n");
    }

    #[test]
    fn split_basic() {
        let (fm, body) = split("---\nfoo: bar\n---\nbody\n");
        assert_eq!(fm.as_deref(), Some("foo: bar"));
        assert_eq!(body, "body\n");
    }

    #[test]
    fn split_empty_frontmatter() {
        let (fm, body) = split("---\n---\nbody\n");
        assert_eq!(fm.as_deref(), Some(""));
        assert_eq!(body, "body\n");
    }

    #[test]
    fn split_no_body() {
        let (fm, body) = split("---\nfoo: 1\n---");
        assert_eq!(fm.as_deref(), Some("foo: 1"));
        assert_eq!(body, "");
    }

    #[test]
    fn split_trailing_spaces_on_sep() {
        let (fm, body) = split("---   \nfoo: 1\n---   \nbody\n");
        assert_eq!(fm.as_deref(), Some("foo: 1"));
        assert_eq!(body, "body\n");
    }

    #[test]
    fn join_round_trip() {
        let text = "---\nfoo: bar\n---\nbody\n";
        let (fm, body) = split(text);
        assert_eq!(join(&fm.unwrap(), &body), text);
    }

    #[test]
    fn join_no_body() {
        assert_eq!(join("foo: 1", ""), "---\nfoo: 1\n---");
    }

    // ── semver ────────────────────────────────────────────────────────────────

    #[test]
    fn bump_semver_patch() {
        assert_eq!(bump_semver("1.2.3", "patch").unwrap(), "1.2.4");
    }

    #[test]
    fn bump_semver_minor() {
        assert_eq!(bump_semver("1.2.3", "minor").unwrap(), "1.3.0");
    }

    #[test]
    fn bump_semver_major() {
        assert_eq!(bump_semver("1.2.3", "major").unwrap(), "2.0.0");
    }

    #[test]
    fn bump_semver_bad_level() {
        assert!(bump_semver("1.2.3", "huge").is_err());
    }

    // ── type detection ────────────────────────────────────────────────────────

    #[test]
    fn detect_integer() {
        let v = Yaml::Number(1i64.into());
        assert_eq!(infer_type(&v), ("integer", None));
    }

    #[test]
    fn detect_float() {
        let v = Yaml::Number(1.5f64.into());
        assert_eq!(infer_type(&v), ("float", None));
    }

    #[test]
    fn detect_boolean() {
        assert_eq!(infer_type(&Yaml::Bool(true)), ("boolean", None));
    }

    #[test]
    fn detect_null() {
        assert_eq!(infer_type(&Yaml::Null), ("null", None));
    }

    #[test]
    fn detect_plain_string() {
        let v = Yaml::String("hello".into());
        assert_eq!(infer_type(&v), ("string", None));
    }

    #[test]
    fn detect_wikilink() {
        let v = Yaml::String("[[My Note]]".into());
        assert_eq!(infer_type(&v), ("wikilink", None));
    }

    #[test]
    fn detect_date() {
        let v = Yaml::String("2025-03-16".into());
        assert_eq!(infer_type(&v), ("date", None));
    }

    #[test]
    fn detect_datetime_space() {
        let v = Yaml::String("2025-03-16 08:30:00".into());
        assert_eq!(infer_type(&v), ("datetime", None));
    }

    #[test]
    fn detect_datetime_t() {
        let v = Yaml::String("2025-03-16T08:30:00".into());
        assert_eq!(infer_type(&v), ("datetime", None));
    }

    #[test]
    fn detect_wikilink_array() {
        let v = Yaml::Sequence(vec![
            Yaml::String("[[Char1]]".into()),
            Yaml::String("[[Char2]]".into()),
        ]);
        assert_eq!(infer_type(&v), ("array", Some("wikilink")));
    }

    #[test]
    fn detect_string_array() {
        let v = Yaml::Sequence(vec![
            Yaml::String("Mood1".into()),
            Yaml::String("Mood2".into()),
        ]);
        assert_eq!(infer_type(&v), ("array", Some("string")));
    }

    #[test]
    fn detect_integer_array() {
        let v = Yaml::Sequence(vec![Yaml::Number(1i64.into()), Yaml::Number(2i64.into())]);
        assert_eq!(infer_type(&v), ("array", Some("integer")));
    }

    #[test]
    fn detect_mixed_array() {
        let v = Yaml::Sequence(vec![Yaml::String("foo".into()), Yaml::Number(1i64.into())]);
        assert_eq!(infer_type(&v), ("array", Some("mixed")));
    }

    // ── integration: real longform chapter frontmatter ────────────────────────

    const CHAPTER_FM: &str = r#"---
arc: 1
chapter: 1
title: WHAT
subtitle: NICE
day: Sunday
start: 2025-03-16 08:30:00
finish: 2025-03-16 11:30:00
pov: 3rd Person
characters:
  - "[[Char1]]"
  - "[[Char2]]"
  - "[[Char3]]"
mood:
  - Mood1
  - Mood2
  - Mood3
status: draft
version: 1.1.7
updated: 2026-05-23 17:24:36
context: Shorty chapter context
summary: Brief chapter summary goes here
prev: "[[The Prev Chapter]]"
next: "[[The Next Chapter]]"
tags:
  - Tag1
  - Tag1/SubTag
---

Body of the chapter goes here.
"#;

    fn chapter_typed() -> Json {
        // write to a tempfile and call read_meta_typed
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(CHAPTER_FM.as_bytes()).unwrap();
        read_meta_typed(f.path(), None).unwrap()
    }

    fn field_type(result: &Json, key: &str) -> String {
        result[key]["type"].as_str().unwrap().to_owned()
    }

    fn items_type(result: &Json, key: &str) -> String {
        result[key]["items_type"].as_str().unwrap().to_owned()
    }

    #[test]
    fn chapter_integers() {
        let r = chapter_typed();
        assert_eq!(field_type(&r, "arc"), "integer");
        assert_eq!(field_type(&r, "chapter"), "integer");
    }

    #[test]
    fn chapter_plain_strings() {
        let r = chapter_typed();
        assert_eq!(field_type(&r, "title"), "string");
        assert_eq!(field_type(&r, "status"), "string");
        assert_eq!(field_type(&r, "day"), "string");
        assert_eq!(field_type(&r, "pov"), "string");
    }

    #[test]
    fn chapter_datetimes() {
        let r = chapter_typed();
        assert_eq!(field_type(&r, "start"), "datetime");
        assert_eq!(field_type(&r, "finish"), "datetime");
        assert_eq!(field_type(&r, "updated"), "datetime");
    }

    #[test]
    fn chapter_wikilinks() {
        let r = chapter_typed();
        assert_eq!(field_type(&r, "prev"), "wikilink");
        assert_eq!(field_type(&r, "next"), "wikilink");
    }

    #[test]
    fn chapter_wikilink_array() {
        let r = chapter_typed();
        assert_eq!(field_type(&r, "characters"), "array");
        assert_eq!(items_type(&r, "characters"), "wikilink");
    }

    #[test]
    fn chapter_string_arrays() {
        let r = chapter_typed();
        assert_eq!(field_type(&r, "mood"), "array");
        assert_eq!(items_type(&r, "mood"), "string");
        assert_eq!(field_type(&r, "tags"), "array");
        assert_eq!(items_type(&r, "tags"), "string");
    }

    #[test]
    fn chapter_version_is_string() {
        // "1.1.7" should be a plain string, not mistaken for a number
        let r = chapter_typed();
        assert_eq!(field_type(&r, "version"), "string");
        assert_eq!(r["version"]["value"].as_str().unwrap(), "1.1.7");
    }

    #[test]
    fn chapter_body_untouched() {
        // write → read and check body bytes
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(CHAPTER_FM.as_bytes()).unwrap();
        let path = f.path();

        let (_, body_before) = split(CHAPTER_FM);
        write_meta(path, serde_json::Map::new(), false).unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        let (_, body_after) = split(&content);
        assert_eq!(body_before, body_after);
    }

    // ── key order preservation ────────────────────────────────────────────────

    fn key_order(text: &str) -> Vec<String> {
        let (fm, _) = split(text);
        let fm = fm.unwrap_or_default();
        // Collect only top-level keys (lines that start a mapping entry)
        fm.lines()
            .filter(|l| !l.starts_with(' ') && !l.starts_with('-') && l.contains(':'))
            .map(|l| l.split(':').next().unwrap().trim().to_owned())
            .collect()
    }

    #[test]
    fn write_preserves_key_order_for_updated_fields() {
        use std::io::Write;
        let src = "---\narc: 1\ntitle: WHAT\nstatus: draft\nversion: 1.0.0\n---\nbody\n";
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(src.as_bytes()).unwrap();

        let before = key_order(src);

        let mut updates = serde_json::Map::new();
        updates.insert("title".into(), Json::String("CHANGED".into()));
        updates.insert("status".into(), Json::String("published".into()));
        write_meta(f.path(), updates, false).unwrap();

        let after_text = std::fs::read_to_string(f.path()).unwrap();
        let after = key_order(&after_text);

        assert_eq!(
            before, after,
            "key order changed after updating existing fields"
        );
    }

    #[test]
    fn write_preserves_key_order_with_touch_updated() {
        use std::io::Write;
        // updated is in the middle — it must stay there after auto-touch
        let src =
            "---\narc: 1\ntitle: WHAT\nupdated: 2026-01-01 00:00:00\nstatus: draft\n---\nbody\n";
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(src.as_bytes()).unwrap();

        let before = key_order(src);

        let mut updates = serde_json::Map::new();
        updates.insert("status".into(), Json::String("published".into()));
        write_meta(f.path(), updates, true).unwrap(); // touch_updated = true

        let after_text = std::fs::read_to_string(f.path()).unwrap();
        let after = key_order(&after_text);

        assert_eq!(
            before, after,
            "updated field shifted position after touch_updated"
        );
    }

    #[test]
    fn bump_preserves_key_order() {
        use std::io::Write;
        let src =
            "---\narc: 1\nversion: 1.0.0\nupdated: 2026-01-01 00:00:00\nstatus: draft\n---\nbody\n";
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(src.as_bytes()).unwrap();

        let before = key_order(src);
        bump_version(f.path(), "minor").unwrap();

        let after_text = std::fs::read_to_string(f.path()).unwrap();
        let after = key_order(&after_text);

        assert_eq!(before, after, "key order changed after bump_version");
    }

    #[test]
    fn write_appends_new_keys_at_end() {
        use std::io::Write;
        let src = "---\narc: 1\ntitle: WHAT\n---\nbody\n";
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(src.as_bytes()).unwrap();

        let mut updates = serde_json::Map::new();
        updates.insert("newfield".into(), Json::String("hello".into()));
        write_meta(f.path(), updates, false).unwrap();

        let after_text = std::fs::read_to_string(f.path()).unwrap();
        let after = key_order(&after_text);

        assert_eq!(&after[..2], &["arc", "title"], "original keys moved");
        assert_eq!(
            after.last().unwrap(),
            "newfield",
            "new key not appended at end"
        );
    }

    #[test]
    fn chapter_write_preserves_full_key_order() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(CHAPTER_FM.as_bytes()).unwrap();

        let before = key_order(CHAPTER_FM);

        // Update several scattered fields
        let mut updates = serde_json::Map::new();
        updates.insert("arc".into(), Json::Number(2.into()));
        updates.insert("status".into(), Json::String("published".into()));
        updates.insert(
            "mood".into(),
            Json::Array(vec![Json::String("Hopeful".into())]),
        );
        updates.insert("prev".into(), Json::String("[[New Prev]]".into()));
        write_meta(f.path(), updates, true).unwrap();

        let after_text = std::fs::read_to_string(f.path()).unwrap();
        let after = key_order(&after_text);

        assert_eq!(
            before, after,
            "key order changed after writing chapter frontmatter"
        );
    }

    // ── path_of ───────────────────────────────────────────────────────────────

    #[test]
    fn path_of_expands_home() {
        // SAFETY: single-threaded test, no concurrent env reads
        unsafe { std::env::set_var("HOME", "/home/test") };
        let p = path_of("~/notes/foo.md");
        assert_eq!(p, PathBuf::from("/home/test/notes/foo.md"));
    }
}
