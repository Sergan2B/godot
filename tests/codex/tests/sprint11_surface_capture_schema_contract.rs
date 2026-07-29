use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;
use serde_json::json;

const SCHEMAS: &[&str] = &[
    "sprint11-surface-metadata.schema.json",
    "sprint11-surface-capture-artifact.schema.json",
    "sprint11-recorder-journal.schema.json",
];

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn load_schema(name: &str) -> Value {
    let path = repository_root()
        .join("godot-codex-mcp/schemas/godot_codex")
        .join(name);
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn validator_for_def(schema: &Value, definition: &str) -> jsonschema::Validator {
    let fragment = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": schema["$defs"].clone(),
        "$ref": format!("#/$defs/{definition}"),
    });
    jsonschema::draft202012::options().build(&fragment).unwrap()
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn build_fixture_documents() -> Value {
    let script = concat!(
        "import json\n",
        "from tests.codex.test_sprint11_surface_artifacts import build_fixture_bundle\n",
        "bundle = build_fixture_bundle()\n",
        "documents = {name: bundle[name] for name in ('metadata', 'capture', 'journal')}\n",
        "print(json.dumps(documents, allow_nan=False, separators=(',', ':'), sort_keys=True))\n",
    );
    let output = Command::new("python3")
        .args(["-E", "-s", "-S", "-c", script])
        .current_dir(repository_root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute the isolated Python fixture builder");
    assert!(
        output.status.success(),
        "isolated Python fixture builder failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.len() <= 16 * 1024 * 1024,
        "fixture documents exceed the schema-test byte bound"
    );
    serde_json::from_slice(&output.stdout).expect("fixture builder emitted invalid JSON")
}

#[test]
fn new_surface_capture_schemas_are_valid_closed_draft_2020_12_documents() {
    for name in SCHEMAS {
        let schema = load_schema(name);
        assert!(
            jsonschema::draft202012::meta::is_valid(&schema),
            "{name} is not a valid Draft 2020-12 schema"
        );
        assert_eq!(schema["additionalProperties"], Value::Bool(false));
        jsonschema::draft202012::options().build(&schema).unwrap();
    }
}

#[test]
fn surface_metadata_schema_freezes_measured_package_and_machine_bindings() {
    let schema = load_schema("sprint11-surface-metadata.schema.json");
    let validator = jsonschema::draft202012::options().build(&schema).unwrap();
    let tools = (0..41)
        .map(|index| format!("godot_tool_{index:02}"))
        .collect::<Vec<_>>();
    let metadata = json!({
        "schema_version": "s11-surface-metadata/1.1",
        "surface": "app",
        "package": {
            "version": "0.1.1",
            "detached_manifest_sha256": digest('1'),
            "internal_manifest_sha256": digest('2'),
            "mcp_binary_sha256": digest('3'),
        },
        "host": {
            "name": "Codex",
            "identifier": "com.openai.codex",
            "artifact_kind": "macos_application",
            "version": "1.2.3",
            "build": "123",
            "commit": null,
            "client_version": "1.2.3",
            "host_metadata_sha256": null,
            "host_code_signature": null,
            "client_code_signature": null,
            "ide_host_version": null,
            "ide_shell_identifier": null,
            "ide_shell_artifact_sha256": null,
            "ide_shell_team_id": null,
            "ide_shell_code_signature": null,
            "architecture": "arm64",
        },
        "bindings": {
            "package_source_commit": "4".repeat(40),
            "package_manifest_sha256": digest('1'),
            "compatibility_matrix_sha256": digest('5'),
            "host_coordinate_profile_sha256": digest('6'),
            "project_fixture_sha256": digest('7'),
            "registry_sha256": digest('8'),
            "prompt_pack_sha256": digest('9'),
            "host_artifact_sha256": digest('a'),
            "client_artifact_sha256": digest('b'),
            "host_provenance_sha256": digest('c'),
            "mcp_binary_sha256": digest('3'),
            "godot_artifact_sha256": digest('d'),
        },
        "machine_bindings": {
            "project_identity_sha256": digest('e'),
            "package_launcher_sha256": digest('3'),
            "project_config_sha256": digest('f'),
            "setup_receipt_sha256": digest('0'),
        },
        "registry": {
            "tools": tools,
            "fixed_resources": [
                "godot://one",
                "godot://two",
                "godot://three",
                "godot://four",
            ],
            "resource_templates": ["godot://resource/{id}"],
            "instructions_sha256": digest('a'),
        },
        "redaction": {
            "absolute_paths_absent": true,
            "account_identity_absent": true,
            "host_controls_absent": true,
            "secrets_absent": true,
        },
    });
    assert!(validator.is_valid(&metadata));

    let mut extra = metadata.clone();
    extra["machine_bindings"]["project_root"] = json!("/private/project");
    assert!(!validator.is_valid(&extra));

    let mut unredacted = metadata;
    unredacted["redaction"]["host_controls_absent"] = json!(false);
    assert!(!validator.is_valid(&unredacted));
}

#[test]
fn private_capture_event_schema_enforces_phase_class_and_tool_observation_rules() {
    let schema = load_schema("sprint11-surface-capture-artifact.schema.json");
    let validator = validator_for_def(&schema, "event");
    let base = json!({
        "sequence": 1,
        "observed_at_unix_ms": 1,
        "direction": "client_to_server",
        "class": "status",
        "phase": "request",
        "method": "tools/call",
        "tool": "godot_get_connection_status",
        "previous_event_sha256": digest('0'),
        "event_sha256": digest('1'),
    });
    assert!(validator.is_valid(&base));

    let mut unsupported_member = base.clone();
    unsupported_member["raw_message"] = json!("not permitted");
    assert!(!validator.is_valid(&unsupported_member));

    let mut missing_automatic_observation = base.clone();
    missing_automatic_observation["direction"] = json!("server_to_client");
    missing_automatic_observation["phase"] = json!("response");
    assert!(!validator.is_valid(&missing_automatic_observation));

    let mut response = missing_automatic_observation;
    response["tool_observation"] = json!({
        "request": {},
        "result": {
            "status": "ready",
        },
    });
    assert!(validator.is_valid(&response));

    let form = json!({
        "sequence": 2,
        "observed_at_unix_ms": 2,
        "direction": "client_to_server",
        "class": "form",
        "phase": "response",
        "method": "elicitation/create",
        "tool_observation": {
            "request": {
                "parent_tool": "godot_apply_transaction",
                "transaction_id": "transaction:abc",
            },
            "result": {
                "action": "accept",
                "content_recorded": false,
            },
        },
        "previous_event_sha256": digest('1'),
        "event_sha256": digest('2'),
    });
    assert!(validator.is_valid(&form));

    let mut unsafe_form = form;
    unsafe_form["tool_observation"]["result"]["content"] = json!("captured");
    assert!(!validator.is_valid(&unsafe_form));

    let projection = json!({
        "sequence": 3,
        "observed_at_unix_ms": 3,
        "direction": "server_to_client",
        "class": "semantic_projection",
        "phase": "projection",
        "semantic_projection": {
            "projection_id": "revision.initial",
            "source_event_seq": 2,
            "value": {
                "step": "initial",
            },
        },
        "previous_event_sha256": digest('2'),
        "event_sha256": digest('3'),
    });
    assert!(validator.is_valid(&projection));
    let mut wrong_projection_phase = projection;
    wrong_projection_phase["phase"] = json!("response");
    assert!(!validator.is_valid(&wrong_projection_phase));
}

#[test]
fn canonical_journal_event_schema_rejects_raw_or_misclassified_fields() {
    let schema = load_schema("sprint11-recorder-journal.schema.json");
    let validator = validator_for_def(&schema, "event");
    let status = json!({
        "seq": 0,
        "direction": "server_to_client",
        "kind": "response",
        "class": "status",
        "method": "tools/call",
        "tool": "godot_get_connection_status",
        "registry_names": [],
        "tool_observation": {
            "request": {},
            "result": {
                "status": "ready",
            },
        },
    });
    assert!(validator.is_valid(&status));

    let mut raw_timestamp = status.clone();
    raw_timestamp["observed_at_unix_ms"] = json!(1);
    assert!(!validator.is_valid(&raw_timestamp));

    let mut wrong_error = status.clone();
    wrong_error["class"] = json!("error");
    wrong_error["kind"] = json!("request");
    assert!(!validator.is_valid(&wrong_error));

    let mut misplaced_instructions = status;
    misplaced_instructions["instructions_sha256"] = json!(digest('a'));
    assert!(!validator.is_valid(&misplaced_instructions));
}

#[test]
fn canonical_projection_array_has_one_fixed_slot_for_every_transport_projection() {
    let schema = load_schema("sprint11-recorder-journal.schema.json");
    let projection_array = &schema["$defs"]["semantic_projections"];
    let slots = projection_array["prefixItems"].as_array().unwrap();
    assert_eq!(slots.len(), 23);
    assert_eq!(projection_array["minItems"], Value::from(23));
    assert_eq!(projection_array["maxItems"], Value::from(23));
    let ids = slots
        .iter()
        .map(|slot| {
            slot["properties"]["projection_id"]["const"]
                .as_str()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted);
    assert!(!ids.contains(&"assertion.host.unsupported_form"));
}

#[test]
fn canonical_schema_validates_the_real_python_journal_and_rejects_tampering() {
    let documents = build_fixture_documents();
    for (schema_name, document_name) in [
        ("sprint11-surface-metadata.schema.json", "metadata"),
        ("sprint11-surface-capture-artifact.schema.json", "capture"),
    ] {
        let schema = load_schema(schema_name);
        let validator = jsonschema::draft202012::options().build(&schema).unwrap();
        if let Err(error) = validator.validate(&documents[document_name]) {
            panic!("real {document_name} fixture does not match {schema_name}: {error}");
        }
    }

    let schema = load_schema("sprint11-recorder-journal.schema.json");
    let validator = jsonschema::draft202012::options().build(&schema).unwrap();
    let journal = documents["journal"].clone();
    if let Err(error) = validator.validate(&journal) {
        panic!("real build_fixture_bundle journal does not match its schema: {error}");
    }
    let observed = journal["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event.get("tool_observation").is_some())
        .count();
    assert!(
        observed >= 20,
        "fixture lacks representative tool observations"
    );

    let status_index = journal["events"]
        .as_array()
        .unwrap()
        .iter()
        .position(|event| {
            event["tool"] == "godot_get_connection_status"
                && event.get("tool_observation").is_some()
        })
        .unwrap();
    let mut unknown_result_member = journal.clone();
    unknown_result_member["events"][status_index]["tool_observation"]["result"]["raw_content"] =
        json!("not allowlisted");
    assert!(!validator.is_valid(&unknown_result_member));

    let mut missing_observation = journal.clone();
    missing_observation["events"][status_index]
        .as_object_mut()
        .unwrap()
        .remove("tool_observation");
    assert!(!validator.is_valid(&missing_observation));

    let mut false_redaction = journal.clone();
    false_redaction["redaction"]["tool_results_reduced_to_allowlist"] = json!(false);
    assert!(!validator.is_valid(&false_redaction));

    let mut obsolete_redaction = journal;
    obsolete_redaction["redaction"]
        .as_object_mut()
        .unwrap()
        .remove("opaque_native_handles_absent");
    obsolete_redaction["redaction"]["native_ids_absent"] = json!(true);
    assert!(!validator.is_valid(&obsolete_redaction));
}
