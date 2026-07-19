use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};

const PHASES: [&str; 10] = [
    "base",
    "body_edit",
    "line_shift",
    "symbol_rename",
    "override_change",
    "literal_dependency_change",
    "attachment_change",
    "parse_error",
    "csharp_discovery",
    "journal_gap",
];

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/script_semantics_project")
}

fn oracle_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/script_semantics_oracle")
}

fn contract_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/script_semantics_contract")
}

fn parse_strict(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = StrictValue.deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

struct StrictValue;

impl<'de> DeserializeSeed<'de> for StrictValue {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a strict JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        StrictValue.deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(StrictValue)? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object member: {key}"
                )));
            }
            values.insert(key, map.next_value_seed(StrictValue)?);
        }
        Ok(Value::Object(values))
    }
}

fn strict_json(path: &Path) -> Value {
    parse_strict(
        &fs::read(path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("strict JSON {} is invalid: {error}", path.display()))
}

fn validator(schema: &Value) -> jsonschema::Validator {
    jsonschema::draft202012::meta::validate(schema)
        .unwrap_or_else(|error| panic!("fixture schema violates Draft 2020-12: {error}"));
    jsonschema::draft202012::options()
        .build(schema)
        .unwrap_or_else(|error| panic!("cannot compile fixture schema: {error}"))
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

fn object_map<'a>(document: &'a Value, key: &str) -> BTreeMap<&'a str, &'a Value> {
    document[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} must be an array"))
        .iter()
        .map(|value| (string(value, "oracle_id"), value))
        .collect()
}

fn sha256(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn collect_files(directory: &Path, base: &Path, output: &mut Vec<String>) {
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot scan {}: {error}", directory.display()))
    {
        let path = entry.expect("fixture directory entry is invalid").path();
        if path.file_name().and_then(|name| name.to_str()) == Some(".godot") {
            continue;
        }
        if path.is_dir() {
            collect_files(&path, base, output);
        } else {
            output.push(
                path.strip_prefix(base)
                    .expect("fixture path is outside its root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
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
            .expect("golden range occurrence is missing");
        let absolute = base + relative;
        found = Some(absolute);
        base = absolute + 1;
    }
    let start = found.expect("golden range occurrence is missing");
    (start, start + needle.len())
}

fn position(value: &str, byte_offset: usize) -> [u64; 2] {
    assert!(value.is_char_boundary(byte_offset));
    let prefix = &value[..byte_offset];
    let line = prefix
        .chars()
        .filter(|character| *character == '\n')
        .count() as u64
        + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, tail)| tail)
        .chars()
        .count() as u64
        + 1;
    [line, column]
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

fn owner_coordinate(
    symbol: &Value,
    symbols: &BTreeMap<&str, &Value>,
    ranges: &BTreeMap<&str, &Value>,
) -> String {
    if let Some(qualified) = symbol.get("qualified_key").and_then(Value::as_str) {
        return qualified.to_owned();
    }
    let owner = symbols[string(symbol, "owner")];
    let base = owner_coordinate(owner, symbols, ranges);
    if string(owner, "kind") == "lambda" {
        let owner_range = ranges[string(owner, "range")];
        format!("{base}/lambda@{}", owner_range["bytes"][0])
    } else {
        base
    }
}

fn canonical_identity(
    symbol: &Value,
    symbols: &BTreeMap<&str, &Value>,
    documents: &BTreeMap<&str, &Value>,
    ranges: &BTreeMap<&str, &Value>,
    algorithms: &Value,
) -> String {
    let document = documents[string(symbol, "document")];
    let script_id = string(document, "script_id");
    let language = string(document, "language");
    let kind = string(symbol, "kind");
    if string(symbol, "identity_scope") == "persistent" {
        return format!(
            "godot:script-symbol:named:v1:{}",
            digest(
                string(algorithms, "named_symbol_domain"),
                &[script_id, language, kind, string(symbol, "qualified_key")],
            )
        );
    }
    let range = ranges[string(symbol, "range")];
    let start = range["bytes"][0]
        .as_u64()
        .expect("range start is invalid")
        .to_string();
    let end = range["bytes"][1]
        .as_u64()
        .expect("range end is invalid")
        .to_string();
    let owner = owner_coordinate(symbol, symbols, ranges);
    format!(
        "godot:script-symbol:content-revision:v1:{}",
        digest(
            string(algorithms, "content_symbol_domain"),
            &[
                script_id,
                language,
                string(document, "content_sha256"),
                &owner,
                kind,
                &start,
                &end,
            ],
        )
    )
}

#[test]
fn script_semantics_fixture_is_strict_draft_2020_12() {
    for (schema_name, instance_name) in [
        ("fixture-manifest.schema.json", "fixture-manifest.json"),
        (
            "golden-script-graph.schema.json",
            "golden-script-graph.json",
        ),
    ] {
        let schema = strict_json(&oracle_root().join(schema_name));
        let instance = strict_json(&oracle_root().join(instance_name));
        validator(&schema)
            .validate(&instance)
            .unwrap_or_else(|error| {
                panic!("{instance_name} does not match {schema_name}: {error}")
            });
    }
    assert!(parse_strict(br#"{"schema_version":1,"schema_version":1}"#).is_err());
}

#[test]
fn script_semantics_fixture_schemas_reject_named_negatives() {
    let manifest_schema = strict_json(&oracle_root().join("fixture-manifest.schema.json"));
    let golden_schema = strict_json(&oracle_root().join("golden-script-graph.schema.json"));
    let mut manifest = strict_json(&oracle_root().join("fixture-manifest.json"));
    let mut golden = strict_json(&oracle_root().join("golden-script-graph.json"));

    manifest["files"][0]["path"] = Value::String("../secret.gd".to_owned());
    assert!(!validator(&manifest_schema).is_valid(&manifest));

    golden["relations"][0]["confidence"] = Value::String("probable".to_owned());
    assert!(!validator(&golden_schema).is_valid(&golden));
    golden["unexpected"] = Value::Bool(true);
    assert!(!validator(&golden_schema).is_valid(&golden));
}

#[test]
fn script_semantics_fixture_manifest_and_ranges_close_exactly() {
    let root = fixture_root();
    let manifest = strict_json(&oracle_root().join("fixture-manifest.json"));
    let golden = strict_json(&oracle_root().join("golden-script-graph.json"));
    let mut actual_files = Vec::new();
    collect_files(&root, &root, &mut actual_files);
    actual_files.sort();
    let listed_files: Vec<&str> = manifest["files"]
        .as_array()
        .expect("manifest files must be an array")
        .iter()
        .map(|value| string(value, "path"))
        .collect();
    assert!(listed_files.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(actual_files, listed_files);
    for value in manifest["files"]
        .as_array()
        .expect("manifest files must be an array")
    {
        assert_eq!(
            sha256(&fs::read(root.join(string(value, "path"))).unwrap()),
            string(value, "sha256")
        );
    }
    let phases: Vec<&str> = manifest["phases"]
        .as_array()
        .expect("manifest phases must be an array")
        .iter()
        .map(|value| string(value, "name"))
        .collect();
    assert_eq!(phases, PHASES);

    let documents = object_map(&golden, "documents");
    let ranges = object_map(&golden, "ranges");
    assert_eq!(documents.len(), 8);
    assert_eq!(ranges.len(), 65);
    for range in ranges.values() {
        let document = documents[string(range, "document")];
        let source =
            fs::read(root.join(string(document, "path").strip_prefix("res://").unwrap())).unwrap();
        assert_eq!(
            format!("sha256:{}", sha256(&source)),
            string(document, "content_sha256")
        );
        let text = std::str::from_utf8(&source).expect("fixture source must be UTF-8");
        let (start, end) = nth_span(
            &source,
            string(range, "needle").as_bytes(),
            integer(range, "occurrence") as usize,
        );
        assert_eq!(range["bytes"], serde_json::json!([start, end]));
        assert_eq!(range["start"], serde_json::json!(position(text, start)));
        assert_eq!(range["end"], serde_json::json!(position(text, end)));
    }
}

#[test]
fn script_semantics_fixture_golden_graph_closes_relations_and_identities() {
    let golden = strict_json(&oracle_root().join("golden-script-graph.json"));
    let contract = strict_json(&contract_root().join("contract-vectors.json"));
    let documents = object_map(&golden, "documents");
    let resources = object_map(&golden, "resources");
    let ranges = object_map(&golden, "ranges");
    let symbols = object_map(&golden, "symbols");
    let relations = object_map(&golden, "relations");
    assert_eq!(symbols.len(), 48);
    assert_eq!(relations.len(), 14);

    let mut identities = BTreeMap::new();
    for (oracle_id, symbol) in &symbols {
        assert!(documents.contains_key(string(symbol, "document")));
        assert!(ranges.contains_key(string(symbol, "range")));
        if let Some(owner) = symbol.get("owner").and_then(Value::as_str) {
            assert!(symbols.contains_key(owner));
        }
        let content_scoped = matches!(string(symbol, "kind"), "parameter" | "local" | "lambda");
        assert_eq!(
            string(symbol, "identity_scope") == "content_revision",
            content_scoped
        );
        identities.insert(
            *oracle_id,
            canonical_identity(
                symbol,
                &symbols,
                &documents,
                &ranges,
                &contract["algorithms"],
            ),
        );
    }
    assert_eq!(
        identities.values().collect::<BTreeSet<_>>().len(),
        identities.len()
    );
    let mut identity_digest = Sha256::new();
    for (oracle_id, identity) in identities {
        identity_digest.update(oracle_id.as_bytes());
        identity_digest.update([0]);
        identity_digest.update(identity.as_bytes());
        identity_digest.update([0]);
    }
    assert_eq!(
        identity_digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "fb9dfb15e2e6d442baa02305b581b495a5b0f3b8375b0f7965fae27901fe214c"
    );

    let mut exact = 0;
    let mut dynamic = 0;
    for relation in relations.values() {
        assert!(symbols.contains_key(string(relation, "source_symbol")));
        assert!(ranges.contains_key(string(relation, "evidence_range")));
        let target_symbol = relation.get("target_symbol").and_then(Value::as_str);
        let target_resource = relation.get("target_resource").and_then(Value::as_str);
        if string(relation, "confidence") == "exact" {
            exact += 1;
            assert_eq!(target_symbol.is_some(), target_resource.is_none());
            assert!(target_symbol.is_none_or(|target| symbols.contains_key(target)));
            assert!(target_resource.is_none_or(|target| resources.contains_key(target)));
            assert_eq!(relation["resolvable_truth"], Value::Bool(true));
        } else {
            dynamic += 1;
            assert_eq!(string(relation, "confidence"), "dynamic");
            assert!(target_symbol.is_none() && target_resource.is_none());
            assert_eq!(relation["resolvable_truth"], Value::Bool(false));
        }
    }
    assert_eq!((exact, dynamic), (9, 5));
    assert_eq!(golden["accuracy_truth"]["false_exact_allowed"], 0);
    assert_eq!(golden["attachments"].as_array().unwrap().len(), 3);
    assert_eq!(golden["diagnostics"].as_array().unwrap().len(), 4);
}
