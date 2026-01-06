use anyhow::Context;
use jsonc_parser::{parse_to_ast, CollectOptions, ParseOptions};
use jsonc_parser::common::Ranged;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};
use url::Url;

use crate::schema::{resolve_schema_uri, SchemaCache};
use crate::text_index::TextIndex;
use crate::yaml_json::yaml_to_json_value;

macro_rules! dprintln {
    ($($t:tt)*) => {
        if std::env::var_os("DEBUG").is_some() {
            eprintln!($($t)*);
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Json,
    Yaml,
    Other,
}

impl DocKind {
    pub fn from_uri(uri: &Url) -> Self {
        let path = uri.path().to_ascii_lowercase();
        if path.ends_with(".json") {
            DocKind::Json
        } else if path.ends_with(".yml") || path.ends_with(".yaml") {
            DocKind::Yaml
        } else {
            DocKind::Other
        }
    }
}

#[derive(Debug, Clone)]
pub struct StoredDocument {
    pub version: i32,
    pub text: String,
    pub kind: DocKind,
}

pub fn validate_document(
    uri: &Url,
    doc: &StoredDocument,
    cache: &SchemaCache,
) -> anyhow::Result<Vec<Diagnostic>> {
    match doc.kind {
        DocKind::Json => validate_json(uri, &doc.text, cache),
        DocKind::Yaml => validate_yaml(uri, &doc.text, cache),
        DocKind::Other => Ok(vec![]),
    }
}

fn validate_json(uri: &Url, text: &str, cache: &SchemaCache) -> anyhow::Result<Vec<Diagnostic>> {
    let index = TextIndex::new(text);

    // Parse with jsonc-parser to obtain node spans for JSON pointers.
    let ast = match parse_to_ast(text, &CollectOptions::default(), &ParseOptions::default()) {
        Ok(v) => v,
        Err(e) => {
            let r = e.range();
            let range = index.range_from_bytes(r.start, r.end);
            return Ok(vec![diag(range, format!("JSON parse error: {}", e))]);
        }
    };

    let root_ast = match ast.value {
        Some(v) => v,
        None => return Ok(vec![diag(Range::default(), "Empty JSON document".to_string())]),
    };

    let mut root_json: JsonValue =
        serde_json::from_str(text).with_context(|| format!("parse JSON for validation: {uri}"))?;

    // `$schema` in instance files is typically an editor directive, not part of the instance.
    // Validate a view without `$schema` to avoid false positives with `additionalProperties: false`.
    let schema_raw = match &root_json {
        JsonValue::Object(map) => map.get("$schema").and_then(|v| v.as_str()).map(str::to_string),
        _ => None,
    };

    let Some(schema_raw) = schema_raw else {
        return Ok(vec![]);
    };

    if let JsonValue::Object(map) = &mut root_json {
        map.remove("$schema");
    }

    let schema_uri = resolve_schema_uri(uri, &schema_raw)?;
    dprintln!("[DEBUG] validate_json instance={} schema={}", uri, schema_uri);

    let validator = cache.validator_for_schema_uri(&schema_uri)?;

    let mut out = Vec::new();

    for err in validator.iter_errors(&root_json) {
        let ptr = err.instance_path().to_string();
        let span = pointer_to_span(&root_ast, &ptr).unwrap_or_else(|| {
            dprintln!("[DEBUG] pointer_to_span failed for ptr={ptr}, using root");
            let r = root_ast.range();
            (r.start, r.end)
        });

        let range = index.range_from_bytes(span.0, span.1);
        out.push(diag(range, format!("{err} (instance path: {ptr})")));

        if out.len() >= cache.cfg.max_errors {
            out.push(diag(
                Range::default(),
                format!(
                    "Too many errors; stopping after {} (use --max-errors to raise)",
                    cache.cfg.max_errors
                ),
            ));
            break;
        }
    }

    Ok(out)
}

fn validate_yaml(uri: &Url, text: &str, cache: &SchemaCache) -> anyhow::Result<Vec<Diagnostic>> {
    let _index = TextIndex::new(text);

    let yaml: serde_yaml::Value = match parse_yaml_documents(text) {
        Ok(v) => v,
        Err(e) => {
            let range = if let Some(loc) = e.location() {
                // serde_yaml uses 1-based line/column.
                let line = loc.line().saturating_sub(1) as u32;
                let col = loc.column().saturating_sub(1) as u32;
                Range {
                    start: Position { line, character: col },
                    end: Position { line, character: col },
                }
            } else {
                Range::default()
            };
            return Ok(vec![diag(range, format!("YAML parse error: {e}"))]);
        }
    };

    let mut json = yaml_to_json_value(&yaml)?;

    let schema_raw = find_yaml_schema_comment(text).or_else(|| match &json {
        JsonValue::Object(map) => map.get("$schema").and_then(|v| v.as_str()).map(str::to_string),
        _ => None,
    });

    let Some(schema_raw) = schema_raw else {
        return Ok(vec![]);
    };

    if let JsonValue::Object(map) = &mut json {
        map.remove("$schema");
    }

    let schema_uri = resolve_schema_uri(uri, &schema_raw)?;
    dprintln!("[DEBUG] validate_yaml instance={} schema={}", uri, schema_uri);

    let validator = cache.validator_for_schema_uri(&schema_uri)?;

    let mut out = Vec::new();
    for err in validator.iter_errors(&json) {
        // Mapping JSON pointers back to YAML source spans is non-trivial; keep it simple.
        out.push(diag(
            Range::default(),
            format!("{err} (instance path: {})", err.instance_path()),
        ));
        if out.len() >= cache.cfg.max_errors {
            break;
        }
    }
    Ok(out)
}

fn find_yaml_schema_comment(text: &str) -> Option<String> {
    const DIRECTIVE: &str = "yaml-language-server:";
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        let comment = trimmed.trim_start_matches('#').trim_start();
        if !comment.starts_with(DIRECTIVE) {
            continue;
        }
        let rest = comment[DIRECTIVE.len()..].trim();
        for token in rest.split_whitespace() {
            if let Some(schema) = token.strip_prefix("$schema=") {
                if !schema.is_empty() {
                    return Some(schema.to_string());
                }
            }
        }
    }
    None
}

fn parse_yaml_documents(text: &str) -> Result<serde_yaml::Value, serde_yaml::Error> {
    let mut docs = Vec::new();
    let deserializer = serde_yaml::Deserializer::from_str(text);
    for doc in deserializer {
        let value = serde_yaml::Value::deserialize(doc)?;
        docs.push(value);
    }
    match docs.len() {
        0 => Ok(serde_yaml::Value::Null),
        1 => Ok(docs.remove(0)),
        _ => Ok(serde_yaml::Value::Sequence(docs)),
    }
}

fn diag(range: Range, message: String) -> Diagnostic {
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("cgpt-jsonschema-lsp".to_string()),
        message,
        ..Default::default()
    }
}

/// Convert a JSON pointer (RFC 6901) to a byte span in the original JSON source,
/// using `jsonc-parser` AST ranges.
///
/// Returns `(start_byte, end_byte)`.
fn pointer_to_span(root: &jsonc_parser::ast::Value<'_>, pointer: &str) -> Option<(usize, usize)> {
    let mut node = root;

    if pointer.is_empty() {
        let r = node.range();
        return Some((r.start, r.end));
    }

    // jsonschema uses JSON Pointer like "/a/b/0".
    let segments = pointer
        .split('/')
        .skip(1)
        .map(unescape_pointer_segment)
        .collect::<Vec<_>>();

    for seg in segments {
        node = match node {
            jsonc_parser::ast::Value::Object(obj) => {
                let mut found = None;
                for prop in &obj.properties {
                    if prop.name.as_str() == seg {
                        found = Some(&prop.value);
                        break;
                    }
                }
                found?
            }
            jsonc_parser::ast::Value::Array(arr) => {
                let idx: usize = seg.parse().ok()?;
                arr.elements.get(idx)?
            }
            _ => {
                let r = node.range();
                return Some((r.start, r.end));
            }
        };
    }

    let r = node.range();
    Some((r.start, r.end))
}

fn unescape_pointer_segment(seg: &str) -> String {
    // RFC 6901:
    // ~0 => ~
    // ~1 => /
    let mut out = String::with_capacity(seg.len());
    let mut chars = seg.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.peek().copied() {
                Some('0') => {
                    chars.next();
                    out.push('~');
                }
                Some('1') => {
                    chars.next();
                    out.push('/');
                }
                _ => out.push('~'),
            }
        } else {
            out.push(ch);
        }
    }

    out
}
