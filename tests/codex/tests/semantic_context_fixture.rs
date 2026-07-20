use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

fn oracle_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/semantic_context_oracle")
}

fn read(name: &str) -> Value {
    serde_json::from_slice(
        &fs::read(oracle_root().join(name)).unwrap_or_else(|error| panic!("cannot read {name}: {error}")),
    )
    .unwrap_or_else(|error| panic!("cannot parse {name}: {error}"))
}

fn validator(schema: &Value) -> jsonschema::Validator {
    jsonschema::draft202012::meta::validate(schema)
        .unwrap_or_else(|error| panic!("schema violates Draft 2020-12: {error}"));
    jsonschema::draft202012::options()
        .build(schema)
        .unwrap_or_else(|error| panic!("cannot compile schema: {error}"))
}

#[test]
fn semantic_context_fixture_matches_draft_2020_12_schemas() {
    for (schema_name, instance_name) in [
        ("fixture-manifest.schema.json", "fixture-manifest.json"),
        ("golden-usages.schema.json", "golden-usages.json"),
    ] {
        let schema = read(schema_name);
        let instance = read(instance_name);
        validator(&schema)
            .validate(&instance)
            .unwrap_or_else(|error| panic!("{instance_name} does not match {schema_name}: {error}"));
    }
}

#[test]
fn semantic_context_fixture_schemas_reject_named_negatives() {
    let manifest_schema = read("fixture-manifest.schema.json");
    let golden_schema = read("golden-usages.schema.json");
    let mut manifest = read("fixture-manifest.json");
    let mut golden = read("golden-usages.json");

    manifest["files"][0]["path"] = json!("../secret.gd");
    assert!(!validator(&manifest_schema).is_valid(&manifest));
    golden["queries"][0]["expected"][0]["confidence"] = json!("certain");
    assert!(!validator(&golden_schema).is_valid(&golden));
    golden["unexpected"] = json!(true);
    assert!(!validator(&golden_schema).is_valid(&golden));
}
