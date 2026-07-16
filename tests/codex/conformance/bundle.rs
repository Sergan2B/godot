use std::fs;
use std::path::{Path, PathBuf};

use jsonschema::Registry;
use serde_json::Value;
use sha2::Digest;

use crate::error::{ConformanceError, Result, fail, require};
use crate::json::{parse_strict, parse_strict_object};
use crate::protocol::{build_handshake_transcript, handshake_proof, project_id_for_root};

#[cfg(test)]
mod resource_graph_contract {
    include!("resource_graph_contract.rs");
}

const SCHEMA_BASE_URI: &str = "https://godot-codex.local/schema/v1/";
const SCHEMA_FILES: [&str; 8] = [
    "common.schema.json",
    "discovery.schema.json",
    "fixture-manifest.schema.json",
    "handshake.schema.json",
    "rpc.schema.json",
    "lifecycle.schema.json",
    "sync.schema.json",
    "resource.schema.json",
];

fn bundle_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/codex_bridge/v1")
}

fn read_json(path: &Path) -> Result<Value> {
    parse_strict(&fs::read(path).map_err(|error| {
        ConformanceError(format!(
            "cannot read canonical fixture {}: {error}",
            path.display()
        ))
    })?)
}

fn load_schemas(root: &Path) -> Result<Vec<(&'static str, Value)>> {
    SCHEMA_FILES
        .iter()
        .map(|name| Ok((*name, read_json(&root.join(name))?)))
        .collect()
}

fn registry_for<'a>(schemas: &'a [(&str, Value)]) -> Result<Registry<'a>> {
    let mut registry = Registry::new();
    for (name, schema) in schemas {
        let uri = format!("{SCHEMA_BASE_URI}{name}");
        registry = registry.add(uri.as_str(), schema).map_err(|error| {
            ConformanceError(format!("invalid schema resource {name}: {error}"))
        })?;
    }
    registry
        .prepare()
        .map_err(|error| ConformanceError(format!("cannot prepare schema registry: {error}")))
}

fn validate_schema_case(
    root: &Path,
    schemas: &[(&str, Value)],
    registry: &Registry<'_>,
    case: &Value,
) -> Result<()> {
    let id = required_string(case, "id")?;
    let schema_reference = required_string(case, "schema_ref")?;
    let fixture_name = required_string(case, "fixture")?;
    let (schema_name, _) = schema_reference
        .split_once('#')
        .map_or((schema_reference, ""), |(name, pointer)| (name, pointer));
    require(
        schemas.iter().any(|(name, _)| *name == schema_name),
        format!("{id}: unknown schema {schema_name}"),
    )?;
    let fixture_path = root.join("fixtures").join(fixture_name);
    let fixture = parse_strict_object(&fs::read(&fixture_path).map_err(|error| {
        ConformanceError(format!(
            "{id}: cannot read {}: {error}",
            fixture_path.display()
        ))
    })?)?;
    let instance = match case.get("instance_pointer").and_then(Value::as_str) {
        Some(pointer) => fixture
            .pointer(pointer)
            .ok_or_else(|| ConformanceError(format!("{id}: missing instance pointer {pointer}")))?,
        None => &fixture,
    };
    let entry_schema = serde_json::json!({
        "$ref": format!("{SCHEMA_BASE_URI}{schema_reference}"),
    });
    let validator = jsonschema::draft202012::options()
        .with_registry(registry)
        .with_base_uri(format!("{SCHEMA_BASE_URI}conformance-entry.json").as_str())
        .build(&entry_schema)
        .map_err(|error| ConformanceError(format!("{id}: schema compile failed: {error}")))?;
    let actual_valid = validator.is_valid(instance);
    let expected_valid = required_string(case, "expect")? == "valid";
    require(
        actual_valid == expected_valid,
        format!("{id}: schema outcome differed; expected valid={expected_valid}"),
    )
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ConformanceError(format!("missing string field {key}")))
}

fn validate_manifest(
    root: &Path,
    schemas: &[(&str, Value)],
    registry: &Registry<'_>,
) -> Result<usize> {
    let manifest = read_json(&root.join("fixtures/manifest.json"))?;
    let manifest_schema = schemas
        .iter()
        .find_map(|(name, schema)| (*name == "fixture-manifest.schema.json").then_some(schema))
        .ok_or_else(|| ConformanceError("fixture manifest schema is missing".to_owned()))?;
    let validator = jsonschema::draft202012::options()
        .with_registry(registry)
        .with_base_uri(format!("{SCHEMA_BASE_URI}fixture-manifest.schema.json").as_str())
        .build(manifest_schema)
        .map_err(|error| ConformanceError(format!("manifest schema compile failed: {error}")))?;
    require(
        validator.is_valid(&manifest),
        "canonical fixture manifest does not match its schema",
    )?;

    let cases = manifest
        .get("cases")
        .and_then(Value::as_array)
        .ok_or_else(|| ConformanceError("fixture manifest has no cases".to_owned()))?;
    for case in cases {
        match required_string(case, "kind")? {
            "schema" => validate_schema_case(root, schemas, registry, case)?,
            "project_id_vector" | "handshake_vector" => {
                let fixture = required_string(case, "fixture")?;
                read_json(&root.join("fixtures").join(fixture))?;
            }
            "sequence" => {
                if let Some(sequence) = case.get("sequence").and_then(Value::as_array) {
                    for fixture in sequence {
                        let fixture = fixture.as_str().ok_or_else(|| {
                            ConformanceError("sequence fixture path must be a string".to_owned())
                        })?;
                        read_json(&root.join("fixtures").join(fixture))?;
                    }
                }
            }
            "framing" => {}
            other => return fail(format!("unknown fixture case kind: {other}")),
        }
    }
    Ok(cases.len())
}

fn decode_hex_32(value: &str) -> Result<[u8; 32]> {
    require(value.len() == 64, "expected a 32-byte hex value")?;
    let mut output = [0_u8; 32];
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|error| ConformanceError(format!("invalid hex vector: {error}")))?;
    }
    Ok(output)
}

fn validate_project_id_vector(root: &Path) -> Result<()> {
    let vector = read_json(&root.join("fixtures/test-vectors/project-id.json"))?;
    let canonical_root = required_string(&vector, "canonical_root")?;
    let expected = required_string(&vector, "expected_project_id")?;
    require(
        project_id_for_root(canonical_root.as_bytes()) == expected,
        "canonical project ID vector mismatch",
    )
}

fn validate_handshake_vector(root: &Path) -> Result<()> {
    let vector = read_json(&root.join("fixtures/test-vectors/handshake.json"))?;
    let token = decode_hex_32(required_string(&vector, "token_hex")?)?;
    let client_nonce: [u8; 32] = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        required_string(&vector, "client_nonce_base64url")?,
    )
    .map_err(|error| ConformanceError(format!("invalid client nonce vector: {error}")))?
    .try_into()
    .map_err(|_| ConformanceError("client nonce vector is not 32 bytes".to_owned()))?;
    let server_nonce: [u8; 32] = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        required_string(&vector, "server_nonce_base64url")?,
    )
    .map_err(|error| ConformanceError(format!("invalid server nonce vector: {error}")))?
    .try_into()
    .map_err(|_| ConformanceError("server nonce vector is not 32 bytes".to_owned()))?;
    let versions = vector
        .get("supported_protocol_versions")
        .and_then(Value::as_array)
        .ok_or_else(|| ConformanceError("handshake vector versions are missing".to_owned()))?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                ConformanceError("handshake vector version must be a string".to_owned())
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let transcript = build_handshake_transcript(
        required_string(&vector, "handshake_version")?,
        &versions,
        required_string(&vector, "selected_protocol_version")?,
        required_string(&vector, "project_id")?,
        required_string(&vector, "editor_session_id")?,
        &client_nonce,
        &server_nonce,
    );
    let digest = sha2::Sha256::digest(&transcript);
    require(
        format!("{digest:x}") == required_string(&vector, "transcript_sha256")?,
        "canonical handshake transcript hash mismatch",
    )?;
    let server_proof = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        handshake_proof(true, &token, &transcript)?,
    );
    let client_proof = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        handshake_proof(false, &token, &transcript)?,
    );
    require(
        server_proof == required_string(&vector, "server_proof_base64url")?,
        "canonical server proof mismatch",
    )?;
    require(
        client_proof == required_string(&vector, "client_proof_base64url")?,
        "canonical client proof mismatch",
    )
}

pub fn validate_canonical_bundle() -> Result<usize> {
    let root = bundle_root();
    let schemas = load_schemas(&root)?;
    for (name, schema) in &schemas {
        require(
            jsonschema::draft202012::meta::is_valid(schema),
            format!("canonical schema is invalid: {name}"),
        )?;
    }
    let registry = registry_for(&schemas)?;
    let case_count = validate_manifest(&root, &schemas, &registry)?;
    validate_project_id_vector(&root)?;
    validate_handshake_vector(&root)?;
    Ok(case_count)
}

pub fn validate_instance(schema_reference: &str, instance: &Value) -> Result<()> {
    let root = bundle_root();
    let schemas = load_schemas(&root)?;
    let registry = registry_for(&schemas)?;
    let (schema_name, _) = schema_reference
        .split_once('#')
        .map_or((schema_reference, ""), |(name, pointer)| (name, pointer));
    require(
        schemas.iter().any(|(name, _)| *name == schema_name),
        format!("unknown schema {schema_name}"),
    )?;
    let entry_schema = serde_json::json!({
        "$ref": format!("{SCHEMA_BASE_URI}{schema_reference}"),
    });
    let validator = jsonschema::draft202012::options()
        .with_registry(&registry)
        .with_base_uri(format!("{SCHEMA_BASE_URI}conformance-entry.json").as_str())
        .build(&entry_schema)
        .map_err(|error| ConformanceError(format!("schema compile failed: {error}")))?;
    if validator.is_valid(instance) {
        Ok(())
    } else {
        let details = validator.iter_errors(instance).next().map_or_else(
            || "unknown validation error".to_owned(),
            |error| error.to_string(),
        );
        fail(format!(
            "instance does not conform to {schema_reference}: {details}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_schema_fixture_bundle_is_self_consistent() {
        assert_eq!(validate_canonical_bundle().unwrap(), 67);
    }

    #[test]
    fn resource_snapshot_and_delta_checksums_bind_exact_payloads() {
        let root = bundle_root().join("fixtures/valid");
        let chunk = read_json(&root.join("resource-snapshot-chunk.json")).unwrap();
        let payload_json = required_string(&chunk, "payload_json").unwrap();
        let parsed_payload: Value = serde_json::from_str(payload_json).unwrap();
        assert_eq!(parsed_payload, chunk["payload"]);
        let chunk_checksum = format!("{:x}", sha2::Sha256::digest(payload_json.as_bytes()));
        assert_eq!(chunk_checksum, required_string(&chunk, "checksum").unwrap());

        let end = read_json(&root.join("resource-snapshot-end.json")).unwrap();
        let snapshot_checksum = format!("{:x}", sha2::Sha256::digest(chunk_checksum.as_bytes()));
        assert_eq!(snapshot_checksum, end["params"]["checksum"]);

        let delta = read_json(&root.join("resource-delta-batch-response.json")).unwrap();
        let operations = &delta["result"]["batch"]["operations"];
        let operations_json = serde_json::to_string(operations).unwrap();
        let batch_checksum = format!("{:x}", sha2::Sha256::digest(operations_json.as_bytes()));
        assert_eq!(batch_checksum, delta["result"]["batch"]["checksum"]);
        assert_eq!(delta["result"]["batch"]["previous_resource_revision"], 1);
        assert_eq!(delta["result"]["batch"]["resource_revision"], 2);

        let mut oversized_chunk = chunk;
        oversized_chunk["payload_json"] = Value::String("x".repeat(262_145));
        assert!(
            validate_instance(
                "resource.schema.json#/$defs/resourceSnapshotChunk",
                &oversized_chunk,
            )
            .is_err()
        );
    }
}
