use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::json::parse_strict;

const CASES: [(&str, &str); 3] = [
    ("fixture-manifest.schema.json", "fixture-manifest.json"),
    ("identity-vectors.schema.json", "identity-vectors.json"),
    ("golden-scene-graph.schema.json", "golden-scene-graph.json"),
];

fn oracle_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/scene_graph_oracle")
}

fn strict_json(path: &Path) -> Value {
    parse_strict(&fs::read(path).unwrap_or_else(|error| {
        panic!("cannot read scene graph fixture {}: {error}", path.display())
    }))
    .unwrap_or_else(|error| panic!("scene graph fixture {} is invalid: {error}", path.display()))
}

fn validator(schema: &Value) -> jsonschema::Validator {
    jsonschema::draft202012::meta::validate(schema)
        .unwrap_or_else(|error| panic!("scene graph schema violates Draft 2020-12: {error}"));
    jsonschema::draft202012::options()
        .build(schema)
        .unwrap_or_else(|error| panic!("cannot compile scene graph schema: {error}"))
}

#[test]
fn scene_graph_contract_bundle_is_draft_2020_12_valid() {
    let root = oracle_root();
    for (schema_name, instance_name) in CASES {
        let schema = strict_json(&root.join(schema_name));
        let instance = strict_json(&root.join(instance_name));
        if let Err(error) = validator(&schema).validate(&instance) {
            panic!("{instance_name} does not match {schema_name}: {error}");
        }
    }
}

#[test]
fn scene_graph_contract_schemas_reject_named_negatives() {
    let root = oracle_root();

    let identity_schema = strict_json(&root.join("identity-vectors.schema.json"));
    let mut identity = strict_json(&root.join("identity-vectors.json"));
    identity["vectors"][0]["kind"] = Value::String("runtime_object_id".to_owned());
    assert!(!validator(&identity_schema).is_valid(&identity));

    let manifest_schema = strict_json(&root.join("fixture-manifest.schema.json"));
    let mut manifest = strict_json(&root.join("fixture-manifest.json"));
    manifest["files"][0]["path"] = Value::String("/absolute/project.godot".to_owned());
    assert!(!validator(&manifest_schema).is_valid(&manifest));

    let golden_schema = strict_json(&root.join("golden-scene-graph.schema.json"));
    let mut golden = strict_json(&root.join("golden-scene-graph.json"));
    golden["nodes"] = Value::Array(Vec::new());
    assert!(!validator(&golden_schema).is_valid(&golden));
}
