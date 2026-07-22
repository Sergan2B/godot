use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::json::parse_strict;

type HmacSha256 = Hmac<Sha256>;

fn contract_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/approval_boundary_contract")
}

fn strict_json(path: &Path) -> Value {
    parse_strict(&fs::read(path).unwrap_or_else(|error| {
        panic!("cannot read approval contract {}: {error}", path.display())
    }))
    .unwrap_or_else(|error| panic!("approval contract {} is invalid: {error}", path.display()))
}

fn validator(schema: &Value) -> jsonschema::Validator {
    jsonschema::draft202012::meta::validate(schema)
        .unwrap_or_else(|error| panic!("approval schema violates Draft 2020-12: {error}"));
    jsonschema::draft202012::options()
        .build(schema)
        .unwrap_or_else(|error| panic!("cannot compile approval schema: {error}"))
}

fn string<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string {pointer}"))
}

fn integer(value: &Value, pointer: &str) -> u64 {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("missing integer {pointer}"))
}

fn lp(output: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    let length = u32::try_from(bytes.len()).expect("test field length");
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
}

fn canonical_receipt_input(vector: &Value) -> Vec<u8> {
    let mut output = b"godot-codex/approval-receipt/v1\0".to_vec();
    for pointer in [
        "/coordinates/project_id",
        "/coordinates/editor_session_id",
        "/coordinates/scene_id",
        "/coordinates/transaction_id",
        "/coordinates/preview_digest",
        "/coordinates/scope",
        "/coordinates/risk",
    ] {
        lp(&mut output, string(vector, pointer));
    }
    for pointer in [
        "/coordinates/scene_revision",
        "/coordinates/operation_seq",
        "/receipt/issued_at_ms",
        "/receipt/expires_at_ms",
    ] {
        output.extend_from_slice(&integer(vector, pointer).to_be_bytes());
    }
    output.extend_from_slice(
        &URL_SAFE_NO_PAD
            .decode(string(vector, "/receipt/nonce"))
            .expect("decode nonce"),
    );
    output
}

fn receipt_mac(vector: &Value) -> String {
    let token = hex_decode(string(vector, "/session_token_hex"));
    let mut key_mac = HmacSha256::new_from_slice(&token).expect("HMAC token");
    key_mac.update(b"godot-codex/approval-key/v1");
    let key = key_mac.finalize().into_bytes();
    let mut receipt_mac = HmacSha256::new_from_slice(&key).expect("HMAC approval key");
    receipt_mac.update(&canonical_receipt_input(vector));
    URL_SAFE_NO_PAD.encode(receipt_mac.finalize().into_bytes())
}

fn hex_decode(value: &str) -> Vec<u8> {
    assert!(value.len().is_multiple_of(2), "hex length");
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).expect("ASCII hex");
            u8::from_str_radix(text, 16).expect("valid hex")
        })
        .collect()
}

#[test]
fn approval_boundary_contract_is_strict_draft_2020_12() {
    let root = contract_root();
    let schema = strict_json(&root.join("receipt-vectors.schema.json"));
    let vectors = strict_json(&root.join("receipt-vectors.json"));
    let validator = validator(&schema);
    let errors: Vec<_> = validator.iter_errors(&vectors).collect();
    assert!(errors.is_empty(), "approval vectors failed schema: {errors:?}");
}

#[test]
fn approval_boundary_receipt_vector_reproduces_exactly() {
    let root = contract_root();
    let vectors = strict_json(&root.join("receipt-vectors.json"));
    let positive = &vectors["positive"];
    let canonical = canonical_receipt_input(positive);
    assert_eq!(
        canonical.len() as u64,
        integer(positive, "/canonical_length")
    );
    assert_eq!(
        format!("sha256:{:x}", Sha256::digest(&canonical)),
        string(positive, "/canonical_sha256")
    );
    assert_eq!(receipt_mac(positive), string(positive, "/receipt/mac"));
    assert_eq!(
        vectors["negative_cases"].as_array().expect("negative cases").len(),
        10
    );
}

#[test]
fn approval_boundary_schema_rejects_named_negatives() {
    let root = contract_root();
    let schema = strict_json(&root.join("receipt-vectors.schema.json"));
    let vectors = strict_json(&root.join("receipt-vectors.json"));
    let validator = validator(&schema);

    let mut extra = vectors.clone();
    extra["positive"]["receipt"]["approved"] = Value::Bool(true);
    assert!(validator.validate(&extra).is_err());

    let mut raw_token = vectors;
    raw_token["positive"]["receipt"]["session_token"] = Value::String("secret".to_owned());
    assert!(validator.validate(&raw_token).is_err());
}
