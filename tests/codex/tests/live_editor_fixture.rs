use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/live_editor_oracle")
}

fn load(name: &str) -> Value {
    serde_json::from_slice(&fs::read(root().join(name)).unwrap()).unwrap()
}

fn validate(schema_name: &str, instance_name: &str) -> bool {
    let schema = load(schema_name);
    let instance = load(instance_name);
    jsonschema::draft202012::options()
        .build(&schema)
        .unwrap()
        .is_valid(&instance)
}

#[test]
fn live_editor_fixture_schemas_accept_the_committed_oracle() {
    assert!(validate(
        "fixture-manifest.schema.json",
        "fixture-manifest.json"
    ));
    assert!(validate(
        "golden-live-editor.schema.json",
        "golden-live-editor.json"
    ));
}

#[test]
fn live_editor_fixture_schemas_reject_open_or_malformed_roots() {
    let schema = load("golden-live-editor.schema.json");
    let validator = jsonschema::draft202012::options().build(&schema).unwrap();
    let mut golden = load("golden-live-editor.json");
    golden["unexpected"] = json!(true);
    assert!(!validator.is_valid(&golden));

    let schema = load("fixture-manifest.schema.json");
    let validator = jsonschema::draft202012::options().build(&schema).unwrap();
    let mut manifest = load("fixture-manifest.json");
    manifest["files"][0]["sha256"] = json!("not-a-digest");
    assert!(!validator.is_valid(&manifest));
}
