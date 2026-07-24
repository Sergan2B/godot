use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::json::parse_strict;

const MAX_OPERATIONS: usize = 16;
const MAX_REPORT_PAGE_BYTES: usize = 65_536;
const MAX_REPORT_PAGES: usize = 4;
const MAX_RETAINED_REPORT_BYTES: usize = 262_144;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/codex_bridge/v1")
}

fn vector(name: &str) -> Value {
    parse_strict(
        &fs::read(root().join("fixtures/test-vectors").join(name))
            .unwrap_or_else(|error| panic!("cannot read {name}: {error}")),
    )
    .unwrap()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityLimits {
    operations: usize,
    report_page_bytes: usize,
    report_pages: usize,
    retained_report_bytes: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capability {
    name: String,
    version: String,
    readiness: String,
    reason: String,
    limits: CapabilityLimits,
}

fn validate_edit_ranges(edits: &[Value]) -> bool {
    let mut previous_end = 0_u64;
    edits.iter().all(|edit| {
        let Some(start) = edit.get("start_byte").and_then(Value::as_u64) else {
            return false;
        };
        let Some(end) = edit.get("end_byte").and_then(Value::as_u64) else {
            return false;
        };
        let valid = start <= end && start >= previous_end;
        previous_end = end;
        valid
    })
}

fn alias_graph_is_acyclic(operations: &[Value]) -> bool {
    let mut producers = BTreeSet::new();
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for operation in operations {
        if let Some(alias) = operation.get("alias").and_then(Value::as_str) {
            if !producers.insert(alias.to_owned()) {
                return false;
            }
            edges.entry(alias.to_owned()).or_default();
        }
    }
    for operation in operations {
        let Some(produced) = operation.get("alias").and_then(Value::as_str) else {
            continue;
        };
        if let Some(reference) = operation.get("resource").and_then(Value::as_str)
            && reference.starts_with("alias:")
        {
            edges
                .entry(produced.to_owned())
                .or_default()
                .insert(reference.to_owned());
        }
    }
    let mut resolved = BTreeSet::new();
    loop {
        let ready: Vec<_> = edges
            .iter()
            .filter(|(name, dependencies)| {
                !resolved.contains(*name)
                    && dependencies
                        .iter()
                        .all(|dependency| resolved.contains(dependency))
            })
            .map(|(name, _)| name.clone())
            .collect();
        if ready.is_empty() {
            break;
        }
        resolved.extend(ready);
    }
    resolved.len() == edges.len()
}

#[test]
fn rpc_1_8_capabilities_are_closed_bounded_and_ready_after_executor_wiring() {
    let fixture = vector("change-set.json");
    let capabilities: Vec<Capability> =
        serde_json::from_value(fixture["capabilities"].clone()).unwrap();
    assert_eq!(capabilities.len(), 2);
    assert_eq!(capabilities[0].name, "transaction.change_set_v1");
    assert_eq!(capabilities[1].name, "validation.automatic_v1");
    for capability in capabilities {
        assert_eq!(capability.version, "1.0");
        assert_eq!(capability.readiness, "ready");
        assert_eq!(capability.reason, "ready");
        assert_eq!(capability.limits.operations, MAX_OPERATIONS);
        assert_eq!(
            capability.limits.report_page_bytes,
            MAX_REPORT_PAGE_BYTES
        );
        assert_eq!(capability.limits.report_pages, MAX_REPORT_PAGES);
        assert_eq!(
            capability.limits.retained_report_bytes,
            MAX_RETAINED_REPORT_BYTES
        );
    }
}

#[test]
fn compound_vectors_bind_order_aliases_and_non_overlapping_edits() {
    let fixture = vector("change-set.json");
    let operations = fixture["operations"].as_array().unwrap();
    assert!((1..=MAX_OPERATIONS).contains(&operations.len()));
    assert!(alias_graph_is_acyclic(operations));
    let edits = operations[2]["edits"].as_array().unwrap();
    assert!(validate_edit_ranges(edits));

    let mut reversed = edits.clone();
    reversed.push(serde_json::json!({
        "start_byte": 4,
        "end_byte": 12,
        "replacement": "overlap"
    }));
    assert!(!validate_edit_ranges(&reversed));
}

#[test]
fn model_controlled_confirmation_or_validation_proofs_are_not_in_dtos() {
    let invalid = vector("change-set-invalid.json");
    let policy = invalid["extra_policy_field"].as_object().unwrap();
    for forbidden in [
        "receipt",
        "approval_receipt",
        "grant",
        "validation_result",
        "rollback_proof",
    ] {
        assert!(
            forbidden != "receipt" || policy.contains_key(forbidden),
            "negative vector must exercise the receipt boundary"
        );
    }
    let valid = vector("change-set.json");
    let serialized = serde_json::to_string(&valid["prepare_request"]["params"]).unwrap();
    for forbidden in [
        "approval_receipt",
        "confirmation_grant",
        "validation_result",
        "rollback_proof",
    ] {
        assert!(!serialized.contains(forbidden));
    }
}
