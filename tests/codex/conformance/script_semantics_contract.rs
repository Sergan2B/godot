use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::json::parse_strict;

fn contract_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/script_semantics_contract")
}

fn strict_json(path: &Path) -> Value {
    parse_strict(&fs::read(path).unwrap_or_else(|error| {
        panic!("cannot read script contract {}: {error}", path.display())
    }))
    .unwrap_or_else(|error| panic!("script contract {} is invalid: {error}", path.display()))
}

fn validator(schema: &Value) -> jsonschema::Validator {
    jsonschema::draft202012::meta::validate(schema)
        .unwrap_or_else(|error| panic!("script contract schema violates Draft 2020-12: {error}"));
    jsonschema::draft202012::options()
        .build(schema)
        .unwrap_or_else(|error| panic!("cannot compile script contract schema: {error}"))
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string field {key}"))
}

fn integer(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("missing integer field {key}"))
}

fn digest(domain: &str, parts: &[&str]) -> String {
    let mut input = Vec::new();
    input.extend_from_slice(domain.as_bytes());
    input.push(0);
    for (index, part) in parts.iter().enumerate() {
        if index != 0 {
            input.push(0);
        }
        input.extend_from_slice(part.as_bytes());
    }
    URL_SAFE_NO_PAD.encode(Sha256::digest(input))
}

fn sources(document: &Value) -> BTreeMap<&str, &Value> {
    document["sources"]
        .as_array()
        .expect("sources must be an array")
        .iter()
        .map(|source| (string(source, "name"), source))
        .collect()
}

fn compute_identity(vector: &Value, algorithms: &Value, source_map: &BTreeMap<&str, &Value>) -> String {
    let script_id = string(vector, "script_id");
    let language = string(vector, "language");
    let kind = string(vector, "kind");
    match kind {
        "named_symbol" => format!(
            "godot:script-symbol:named:v1:{}",
            digest(
                string(algorithms, "named_symbol_domain"),
                &[
                    script_id,
                    language,
                    string(vector, "symbol_kind"),
                    string(vector, "qualified_key"),
                ],
            )
        ),
        "content_symbol" => {
            let source = source_map[string(vector, "source")];
            let start = integer(vector, "start_byte").to_string();
            let end = integer(vector, "end_byte").to_string();
            format!(
                "godot:script-symbol:content-revision:v1:{}",
                digest(
                    string(algorithms, "content_symbol_domain"),
                    &[
                        script_id,
                        language,
                        string(source, "sha256"),
                        string(vector, "owner_qualified_key"),
                        string(vector, "symbol_kind"),
                        &start,
                        &end,
                    ],
                )
            )
        }
        "diagnostic" => {
            let source = source_map[string(vector, "source")];
            let start = integer(vector, "start_byte").to_string();
            let end = integer(vector, "end_byte").to_string();
            format!(
                "godot:script-diagnostic:v1:{}",
                digest(
                    string(algorithms, "diagnostic_domain"),
                    &[
                        script_id,
                        language,
                        string(source, "sha256"),
                        string(vector, "severity"),
                        string(vector, "code"),
                        &start,
                        &end,
                        string(vector, "normalized_message"),
                    ],
                )
            )
        }
        other => panic!("unsupported identity kind: {other}"),
    }
}

fn nth_span(value: &[u8], needle: &[u8], occurrence: usize) -> (usize, usize) {
    assert!(!needle.is_empty());
    let mut base = 0;
    let mut found = None;
    for _ in 0..=occurrence {
        let relative = value[base..]
            .windows(needle.len())
            .position(|candidate| candidate == needle)
            .expect("range occurrence is missing");
        let absolute = base + relative;
        found = Some(absolute);
        base = absolute + 1;
    }
    let start = found.expect("range occurrence is missing");
    (start, start + needle.len())
}

fn position(value: &str, byte_offset: usize) -> (u64, u64) {
    assert!(value.is_char_boundary(byte_offset));
    let prefix = &value[..byte_offset];
    let line = prefix.chars().filter(|character| *character == '\n').count() as u64 + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, tail)| tail)
        .chars()
        .count() as u64
        + 1;
    (line, column)
}

fn classify(vector: &Value) -> (&'static str, bool) {
    let confidence = match (string(vector, "authority"), string(vector, "resolution")) {
        ("gdscript_analyzer" | "godot_resource_loader", "resolved_unambiguous") => "exact",
        ("gdscript_parser", "dynamic_target" | "string_node_path") => "dynamic",
        other => panic!("unsupported confidence classification: {other:?}"),
    };
    (confidence, string(vector, "freshness") == "current")
}

#[test]
fn script_semantics_contract_is_strict_draft_2020_12() {
    let root = contract_root();
    let schema = strict_json(&root.join("contract-vectors.schema.json"));
    let instance = strict_json(&root.join("contract-vectors.json"));
    validator(&schema)
        .validate(&instance)
        .unwrap_or_else(|error| panic!("contract vectors do not match schema: {error}"));

    let duplicate = br#"{"schema_version":1,"schema_version":1}"#;
    assert!(parse_strict(duplicate).is_err());
}

#[test]
fn script_semantics_contract_schema_rejects_named_negatives() {
    let root = contract_root();
    let schema = strict_json(&root.join("contract-vectors.schema.json"));
    let document = strict_json(&root.join("contract-vectors.json"));
    let compiled = validator(&schema);

    let mut unknown_kind = document.clone();
    unknown_kind["identity_vectors"][0]["kind"] = Value::String("runtime_symbol".to_owned());
    assert!(!compiled.is_valid(&unknown_kind));

    let mut absolute_path = document.clone();
    absolute_path["sources"][0]["path"] = Value::String("/private/player.gd".to_owned());
    assert!(!compiled.is_valid(&absolute_path));

    let mut probable = document;
    probable["confidence_vectors"][0]["expected_confidence"] = Value::String("probable".to_owned());
    assert!(!compiled.is_valid(&probable));
}

#[test]
fn script_semantics_contract_identities_reproduce_exactly() {
    let root = contract_root();
    let document = strict_json(&root.join("contract-vectors.json"));
    let source_map = sources(&document);
    let vectors = document["identity_vectors"]
        .as_array()
        .expect("identity vectors must be an array");
    assert_eq!(vectors.len(), 9);

    let computed: BTreeMap<&str, String> = vectors
        .iter()
        .map(|vector| {
            let actual = compute_identity(vector, &document["algorithms"], &source_map);
            assert_eq!(actual, string(vector, "expected_id"));
            (string(vector, "name"), actual)
        })
        .collect();

    for vector in vectors {
        let name = string(vector, "name");
        if let Some(same) = vector.get("same_identity_as").and_then(Value::as_str) {
            assert_eq!(computed[name], computed[same]);
        }
        if let Some(different) = vector.get("different_identity_from").and_then(Value::as_str) {
            assert_ne!(computed[name], computed[different]);
        }
    }
}

#[test]
fn script_semantics_contract_ranges_reproduce_exactly() {
    let root = contract_root();
    let document = strict_json(&root.join("contract-vectors.json"));
    let source_map = sources(&document);
    let vectors = document["range_vectors"]
        .as_array()
        .expect("range vectors must be an array");
    assert_eq!(vectors.len(), 5);

    for vector in vectors {
        let content = string(source_map[string(vector, "source")], "content");
        let (start, end) = nth_span(
            content.as_bytes(),
            string(vector, "needle").as_bytes(),
            integer(vector, "occurrence") as usize,
        );
        assert_eq!(start as u64, integer(vector, "start_byte"));
        assert_eq!(end as u64, integer(vector, "end_byte"));
        assert_eq!(position(content, start).0, integer(vector, "start_line"));
        assert_eq!(position(content, start).1, integer(vector, "start_column"));
        assert_eq!(position(content, end).0, integer(vector, "end_line"));
        assert_eq!(position(content, end).1, integer(vector, "end_column"));
    }
}

#[test]
fn script_semantics_contract_confidence_reproduces_exactly() {
    let root = contract_root();
    let document = strict_json(&root.join("contract-vectors.json"));
    let vectors = document["confidence_vectors"]
        .as_array()
        .expect("confidence vectors must be an array");
    assert_eq!(vectors.len(), 6);
    for vector in vectors {
        let (confidence, servable) = classify(vector);
        assert_eq!(confidence, string(vector, "expected_confidence"));
        assert_eq!(servable, vector["servable_as_current"].as_bool().unwrap());
    }
}
