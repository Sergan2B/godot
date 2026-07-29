use std::fs;
use std::path::PathBuf;

use serde_json::Value;
use serde_json::json;

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

#[test]
fn sprint11_schemas_are_valid_draft_2020_12_and_closed_at_the_root() {
    for name in [
        "compatibility-matrix.schema.json",
        "sprint11-evidence.schema.json",
        "sprint11-packaged-regression-receipt.schema.json",
        "sprint11-recorder-journal.schema.json",
        "sprint11-surface-acquisition-authority.schema.json",
        "sprint11-surface-capture-artifact.schema.json",
        "sprint11-surface-metadata.schema.json",
        "sprint11-surface-trace.schema.json",
        "sprint11-multi-project-report.schema.json",
        "sprint11-multi-project-receipt.schema.json",
        "sprint11-reproducibility-receipt.schema.json",
        "sprint11-usability-report.schema.json",
        "host-coordinate-profile.schema.json",
        "sprint11-host-provenance.schema.json",
        "sprint11-host-provenance-acquisition.schema.json",
        "sprint11-human-acquisition-authority.schema.json",
        "sprint11-human-consent-receipt.schema.json",
        "sprint11-human-usability-defect-ledger.schema.json",
        "sprint11-human-usability-rubric.schema.json",
        "sprint11-human-usability-trace.schema.json",
        "doctor-report.schema.json",
    ] {
        let schema = load_schema(name);
        assert!(jsonschema::draft202012::meta::is_valid(&schema));
        assert_eq!(schema["additionalProperties"], Value::Bool(false));
        jsonschema::draft202012::options().build(&schema).unwrap();
    }
}

#[test]
fn compatibility_matrix_validates_against_its_draft_2020_12_schema() {
    let schema = load_schema("compatibility-matrix.schema.json");
    let matrix_path =
        repository_root().join("godot-codex-mcp/product/compatibility-matrix.v1.json");
    let matrix: Value = serde_json::from_slice(&fs::read(matrix_path).unwrap()).unwrap();
    let validator = jsonschema::draft202012::options().build(&schema).unwrap();
    assert!(
        validator.is_valid(&matrix),
        "product compatibility matrix differs from its schema"
    );
}

#[test]
fn sprint11_evidence_schema_freezes_the_release_projection() {
    let schema = load_schema("sprint11-evidence.schema.json");
    let projection = &schema["properties"]["release_projection"]["properties"];
    assert_eq!(projection["external_codex_beta_macos"]["const"], "passed");
    assert_eq!(projection["r1_08"]["const"], "partial");
    assert_eq!(projection["r1_10"]["const"], "partial");
    assert_eq!(projection["beta_gate"]["const"], "not_reached");
}

#[test]
fn sprint11_usability_schema_binds_complete_primary_bundles_and_authority() {
    let schema = load_schema("sprint11-usability-report.schema.json");
    let required = schema["$defs"]["participant"]["required"]
        .as_array()
        .unwrap();
    let required: Vec<&str> = required
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    for field in [
        "trace_path",
        "trace_sha256",
        "rubric_path",
        "rubric_sha256",
        "consent_path",
        "consent_sha256",
        "defect_ledger_path",
        "defect_ledger_sha256",
        "authority_path",
        "authority_sha256",
        "external_attestation_independence",
    ] {
        assert!(
            required.contains(&field),
            "missing participant binding: {field}"
        );
    }
    assert_eq!(
        schema["properties"]["attestation_trust_boundary"]["const"],
        "git_binds_exact_bytes_external_operator_attests_human_facts_without_cryptographic_identity_proof"
    );

    let evidence = load_schema("sprint11-evidence.schema.json");
    assert_eq!(
        evidence["properties"]["usability"]["properties"]["externally_attested_independent_participant"]
            ["const"],
        Value::Bool(true)
    );
    assert!(
        evidence["properties"]["usability"]["properties"]
            .get("independent_participant")
            .is_none()
    );
}

#[test]
fn sprint11_surface_evidence_requires_external_operator_authority() {
    let evidence = load_schema("sprint11-evidence.schema.json");
    let surface = &evidence["$defs"]["surface"];
    let required = surface["required"].as_array().unwrap();
    for field in ["authority_path", "authority_sha256"] {
        assert!(
            required.iter().any(|value| value.as_str() == Some(field)),
            "missing surface authority binding: {field}"
        );
    }
    let authority = load_schema("sprint11-surface-acquisition-authority.schema.json");
    assert_eq!(
        authority["properties"]["trust_boundary"]["const"],
        "git_binds_exact_surface_artifacts_external_operator_attests_actual_host_session_without_cryptographic_process_origin_proof"
    );
    let observations = authority["$defs"]["observations"]["required"]
        .as_array()
        .unwrap();
    assert!(
        observations
            .iter()
            .any(|value| { value.as_str() == Some("project_trust_reviewed_and_accepted") }),
        "surface authority must attest personal project Trust review and acceptance"
    );
    for observation in observations {
        let name = observation.as_str().unwrap();
        assert_eq!(
            authority["$defs"]["observations"]["properties"][name]["const"],
            Value::Bool(true),
            "surface authority observation is not fail-closed: {name}"
        );
    }
}

#[test]
fn sprint11_assertion_schemas_reject_empty_semantic_projections() {
    let schema = load_schema("sprint11-surface-trace.schema.json");
    let assertion_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": schema["$defs"].clone(),
        "oneOf": schema["properties"]["assertions"]["items"]["oneOf"].clone(),
    });
    let validator = jsonschema::draft202012::options()
        .build(&assertion_schema)
        .unwrap();
    let assertion_ids = schema["$defs"]["assertion_id"]["enum"].as_array().unwrap();
    assert_eq!(assertion_ids.len(), 17);
    assert!(
        !assertion_ids
            .iter()
            .any(|value| value.as_str() == Some("host.unsupported_form"))
    );
    for assertion_id in assertion_ids {
        let instance = json!({
            "assertion_id": assertion_id,
            "projection": {},
        });
        assert!(
            !validator.is_valid(&instance),
            "empty projection unexpectedly validates for {assertion_id}"
        );
    }
}

#[test]
fn sprint11_human_trace_excludes_the_global_unsupported_form_probe() {
    let schema = load_schema("sprint11-human-usability-trace.schema.json");
    let assertion_ids = schema["$defs"]["assertion_id"]["enum"].as_array().unwrap();
    assert_eq!(assertion_ids.len(), 17);
    assert!(
        !assertion_ids
            .iter()
            .any(|value| value.as_str() == Some("host.unsupported_form"))
    );
    assert_eq!(
        schema["$defs"]["task"]["properties"]["semantic_assertion_ids"]["maxItems"],
        Value::from(8)
    );
}

#[test]
fn sprint11_evidence_gates_bind_runner_definitions_not_claimed_results() {
    let schema = load_schema("sprint11-evidence.schema.json");
    let gate_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": schema["$defs"].clone(),
        "$ref": "#/$defs/gate",
    });
    let validator = jsonschema::draft202012::options()
        .build(&gate_schema)
        .unwrap();
    assert!(!validator.is_valid(&json!({
        "status": "passed",
        "duration_ms": 1,
    })));
    assert!(validator.is_valid(&json!({
        "runner": "sprint11_acceptance/1.0",
        "definition_sha256": format!("sha256:{}", "a".repeat(64)),
    })));
}

#[test]
fn sprint11_relative_path_schemas_reject_dot_and_empty_segments() {
    for name in [
        "sprint11-host-provenance-acquisition.schema.json",
        "sprint11-multi-project-receipt.schema.json",
        "sprint11-reproducibility-receipt.schema.json",
    ] {
        let schema = load_schema(name);
        let path_schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$defs": schema["$defs"].clone(),
            "$ref": "#/$defs/path",
        });
        let validator = jsonschema::draft202012::options()
            .build(&path_schema)
            .unwrap();
        assert!(
            validator.is_valid(&json!("fixture/project.godot")),
            "{name} rejects a canonical relative path"
        );
        for invalid in [
            ".",
            "..",
            "fixture/.",
            "fixture/..",
            "fixture//project.godot",
            "fixture/",
            "/fixture",
        ] {
            assert!(
                !validator.is_valid(&json!(invalid)),
                "{name} accepts unsafe relative path {invalid:?}"
            );
        }
    }
}

#[test]
fn sprint11_reproducibility_schema_requires_ordered_independent_rebuilds() {
    let schema = load_schema("sprint11-reproducibility-receipt.schema.json");
    let array_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": schema["$defs"].clone(),
        "type": "object",
        "additionalProperties": false,
        "required": ["commands", "outputs"],
        "properties": {
            "commands": schema["properties"]["commands"].clone(),
            "outputs": schema["properties"]["outputs"].clone(),
        },
    });
    let validator = jsonschema::draft202012::options()
        .build(&array_schema)
        .unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    let command_a = json!({
        "id": "clean_rebuild_a",
        "cwd": ".",
        "argv_template": [
            "{python}", "-E", "-s", "-S",
            "godot-codex-mcp/packaging/build_macos.py",
            "--repository-root", "{repository_root}",
            "--workspace", "{workspace}",
            "--output-dir", "{build_a}",
            "--source-commit", "{source_commit}",
            "--godot-prerequisite", "{godot}"
        ],
        "command_sha256": digest,
        "runner_path": "godot-codex-mcp/packaging/build_macos.py",
        "runner_sha256": digest,
        "stdout_sha256": digest,
        "stderr_sha256": digest,
        "exit_code": 0,
        "duration_ms": 1
    });
    let command_b = json!({
        "id": "clean_rebuild_b",
        "cwd": ".",
        "argv_template": [
            "{python}", "-E", "-s", "-S",
            "godot-codex-mcp/packaging/build_macos.py",
            "--repository-root", "{repository_root}",
            "--workspace", "{workspace}",
            "--output-dir", "{build_b}",
            "--source-commit", "{source_commit}",
            "--godot-prerequisite", "{godot}"
        ],
        "command_sha256": digest,
        "runner_path": "godot-codex-mcp/packaging/build_macos.py",
        "runner_sha256": digest,
        "stdout_sha256": digest,
        "stderr_sha256": digest,
        "exit_code": 0,
        "duration_ms": 1
    });
    let output_a = json!({
        "id": "a",
        "manifest_sha256": digest,
        "build_provenance_sha256": digest,
        "archive_sha256": digest,
        "archive_bytes": 1,
        "package_tree_sha256": digest
    });
    let output_b = json!({
        "id": "b",
        "manifest_sha256": digest,
        "build_provenance_sha256": digest,
        "archive_sha256": digest,
        "archive_bytes": 1,
        "package_tree_sha256": digest
    });
    let valid = json!({
        "commands": [command_a, command_b],
        "outputs": [output_a, output_b],
    });
    assert!(validator.is_valid(&valid));

    let mut duplicate_command = valid.clone();
    duplicate_command["commands"][1] = duplicate_command["commands"][0].clone();
    assert!(!validator.is_valid(&duplicate_command));
    let mut swapped_commands = valid.clone();
    swapped_commands["commands"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    assert!(!validator.is_valid(&swapped_commands));
    let mut duplicate_output = valid.clone();
    duplicate_output["outputs"][1] = duplicate_output["outputs"][0].clone();
    assert!(!validator.is_valid(&duplicate_output));
    let mut swapped_outputs = valid;
    swapped_outputs["outputs"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    assert!(!validator.is_valid(&swapped_outputs));
}
