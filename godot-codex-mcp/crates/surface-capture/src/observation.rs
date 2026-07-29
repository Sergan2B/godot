use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::journal::{
    contains_account_identity, contains_private_absolute_path, contains_private_secret_marker,
    sha256_bytes,
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_OBSERVATION_BYTES: usize = 64 * 1024;
const MAX_COLLECTION_ITEMS: usize = 256;
const MAX_DEPTH: usize = 8;
const MAX_VALIDATION_CHECKS: usize = 16;
const SUPPORTED_TOOLS: &[&str] = &[
    "godot_get_connection_status",
    "godot_get_current_scene",
    "godot_get_scene_graph",
    "godot_run_project",
    "godot_run_current_scene",
    "godot_stop_project",
    "godot_get_runtime_tree",
    "godot_get_stack_trace",
    "godot_prepare_change_set",
    "godot_apply_transaction",
    "godot_get_transaction_status",
    "godot_undo_transaction",
    "godot_get_validation_report",
];

/// Safe request/result projection for one allowlisted tool response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolObservation {
    /// Allowlisted, content-minimized request arguments.
    pub request: Value,
    /// Allowlisted, content-minimized structured result.
    pub result: Value,
}

pub(crate) fn supported_tool(tool: &str) -> bool {
    SUPPORTED_TOOLS.contains(&tool)
}

pub(crate) fn request_observation(tool: &str, arguments: Option<&Value>) -> Option<Value> {
    if !supported_tool(tool) {
        return None;
    }
    let empty = Map::new();
    let arguments = arguments.and_then(Value::as_object).unwrap_or(&empty);
    if arguments
        .keys()
        .any(|field| !request_field_allowed(tool, field))
    {
        return None;
    }
    let projected = project_object(arguments, 0, ProjectionContext::Request)?;
    bounded_value(Value::Object(projected))
}

pub(crate) fn result_observation(tool: &str, message: &Value) -> Option<Value> {
    if !supported_tool(tool) {
        return None;
    }
    let result = message.get("result");
    let structured = result
        .and_then(|value| value.get("structuredContent"))
        .or_else(|| result.and_then(|value| value.get("structured_content")))
        .or_else(|| message.pointer("/error/data"))?;
    let object = structured.as_object()?;
    let filtered = object
        .iter()
        .filter(|(field, _)| result_field_allowed(tool, field))
        .map(|(field, value)| (field.clone(), value.clone()))
        .collect::<Map<_, _>>();
    let mut projected = project_object(&filtered, 0, ProjectionContext::Result)?;
    if tool == "godot_get_validation_report"
        && object.get("page").and_then(Value::as_u64) == Some(0)
        && let Some(content) = object.get("content").and_then(Value::as_str)
        && content.len() <= MAX_OBSERVATION_BYTES
        && let Ok(report) = serde_json::from_str::<Value>(content)
        && let Some(content_projection) = validation_content_projection(&report)
    {
        projected.insert("content_projection".to_owned(), content_projection);
    }
    if let Some(is_error) = result
        .and_then(|value| value.get("isError"))
        .or_else(|| result.and_then(|value| value.get("is_error")))
        .and_then(Value::as_bool)
    {
        projected.insert("is_error".to_owned(), Value::Bool(is_error));
    }
    bounded_value(Value::Object(projected))
}

pub(crate) fn form_request_observation<'a>(
    pending_apply_requests: impl Iterator<Item = &'a Value>,
) -> Option<Value> {
    let requests: Vec<&Value> = pending_apply_requests.collect();
    if requests.len() != 1 {
        return None;
    }
    let transaction_id = requests[0].get("transaction_id")?.as_str()?;
    if !safe_token(transaction_id) {
        return None;
    }
    Some(serde_json::json!({
        "parent_tool": "godot_apply_transaction",
        "transaction_id": transaction_id,
    }))
}

pub(crate) fn form_result_observation(message: &Value) -> Option<Value> {
    let action = message.pointer("/result/action")?.as_str()?;
    if !matches!(action, "accept" | "decline" | "cancel") {
        return None;
    }
    Some(serde_json::json!({
        "action": action,
        "content_recorded": false,
    }))
}

fn bounded_value(value: Value) -> Option<Value> {
    (serde_json::to_vec(&value).ok()?.len() <= MAX_OBSERVATION_BYTES).then_some(value)
}

fn request_field_allowed(tool: &str, field: &str) -> bool {
    let allowed: &[&str] = match tool {
        "godot_get_connection_status" | "godot_run_project" | "godot_run_current_scene" => &[],
        "godot_get_current_scene" => &[
            "expected_editor_session_id",
            "expected_event_seq",
            "expected_scene_revision",
        ],
        "godot_get_scene_graph" => &["scene", "limit", "cursor"],
        "godot_stop_project" => &["runtime_session_id", "expected_runtime_event_seq"],
        "godot_get_runtime_tree" => &[
            "runtime_session_id",
            "expected_runtime_event_seq",
            "limit",
            "cursor",
        ],
        "godot_get_stack_trace" => &[
            "runtime_session_id",
            "runtime_stack_id",
            "expected_runtime_event_seq",
        ],
        "godot_prepare_change_set" => &[
            "project_id",
            "idempotency_key",
            "coordinates",
            "operations",
            "save_scope",
            "validation_policy",
        ],
        "godot_apply_transaction" => &[
            "transaction_id",
            "preview_digest",
            "expected_scene_revision",
            "expected_operation_seq",
        ],
        "godot_get_transaction_status" => &["transaction_id"],
        "godot_undo_transaction" => &[
            "transaction_id",
            "expected_transaction_seq",
            "expected_scene_revision",
            "expected_operation_seq",
        ],
        "godot_get_validation_report" => &["report_id", "page"],
        _ => return false,
    };
    allowed.contains(&field)
}

fn result_field_allowed(tool: &str, field: &str) -> bool {
    if field == "error" {
        return true;
    }
    let allowed: &[&str] = match tool {
        "godot_get_connection_status" => &[
            "schema_version",
            "status",
            "project_scope",
            "package_version",
            "compatibility",
            "bridge",
            "static_cache",
            "recovery",
            "components",
            "diagnostic",
            "remediation_id",
            "next_action",
            "limits_applied",
            "evidence",
        ],
        "godot_get_current_scene" => &[
            "schema_version",
            "project_id",
            "editor_session_id",
            "snapshot_id",
            "revision_vector",
            "status",
            "freshness",
            "truncated",
            "evidence",
            "capabilities_used",
            "partial_reasons",
            "diagnostics",
            "limits_applied",
            "entities",
            "facts",
            "scene",
            "nodes",
        ],
        "godot_get_scene_graph" => &[
            "project_id",
            "schema_version",
            "generation_id",
            "index_revision",
            "resource_revision",
            "scene_graph_revision",
            "freshness",
            "offline_cached",
            "status",
            "scene",
            "query",
            "nodes",
            "project_context",
            "project_context_truncated",
            "diagnostics",
            "partial_reasons",
            "truncated",
            "next_cursor",
            "live_overlay",
            "conflicts",
            "validated_checkpoint",
            "evidence",
        ],
        "godot_run_project" | "godot_run_current_scene" | "godot_stop_project" => &[
            "schema_version",
            "project_id",
            "editor_session_id",
            "runtime_session_id",
            "runtime_event_seq",
            "state",
            "origin",
            "target",
            "scene_path",
        ],
        "godot_get_runtime_tree" => &[
            "schema_version",
            "project_id",
            "editor_session_id",
            "runtime_session_id",
            "runtime_event_seq",
            "state",
            "snapshot_id",
            "limit",
            "offset",
            "total",
            "nodes",
            "truncated",
            "limits_applied",
            "next_cursor",
            "evidence",
        ],
        "godot_get_stack_trace" => &[
            "schema_version",
            "runtime_session_id",
            "runtime_event_seq",
            "state",
            "stack",
        ],
        "godot_prepare_change_set" => &[
            "schema_version",
            "change_set_id",
            "state",
            "transaction_seq",
            "preview_digest",
            "request_digest",
            "preview",
            "risk",
            "scope",
            "operation_count",
            "created_at_ms",
            "expires_at_ms",
            "limits_applied",
            "coordinates",
        ],
        "godot_apply_transaction" | "godot_get_transaction_status" | "godot_undo_transaction" => &[
            "schema_version",
            "transaction_id",
            "change_set_id",
            "editor_session_id",
            "scene_id",
            "state",
            "transaction_seq",
            "operation_kind",
            "risk",
            "scope",
            "preview_digest",
            "request_digest",
            "current_scene_revision",
            "current_operation_seq",
            "outcome",
            "undo_eligibility",
            "committed_entities",
            "updated_at_ms",
            "limits_applied",
            "truncated",
            "preview",
            "operation_count",
            "created_at_ms",
            "expires_at_ms",
            "coordinates",
            "postimage_digest",
            "validation_report_id",
            "terminal_error",
        ],
        "godot_get_validation_report" => &[
            "schema_version",
            "report_id",
            "report_digest",
            "page",
            "page_count",
            "content",
            "content_bytes",
            "limits_applied",
        ],
        _ => return false,
    };
    allowed.contains(&field)
}

#[derive(Clone, Copy)]
enum ProjectionContext {
    Request,
    Result,
}

fn project_object(
    object: &Map<String, Value>,
    depth: usize,
    context: ProjectionContext,
) -> Option<Map<String, Value>> {
    if depth > MAX_DEPTH || object.len() > MAX_COLLECTION_ITEMS {
        return None;
    }
    let mut projected = Map::new();
    for (field, value) in object {
        if let Some(target) = match field.as_str() {
            "property" => Some("property_name"),
            "signal" => Some("signal_name"),
            "method" => Some("method_name"),
            _ => None,
        } {
            let Some(token) = value.as_str() else {
                if matches!(context, ProjectionContext::Request) {
                    return None;
                }
                continue;
            };
            if !safe_token(token) {
                if matches!(context, ProjectionContext::Request) {
                    return None;
                }
                continue;
            }
            projected.insert(target.to_owned(), Value::String(token.to_owned()));
            continue;
        }
        if sensitive_field(field) {
            if insert_digest_pair(&mut projected, field, value).is_none()
                && matches!(context, ProjectionContext::Request)
            {
                return None;
            }
            continue;
        }
        if field == "diagnostics" {
            let Some(diagnostics) = value.as_array() else {
                if matches!(context, ProjectionContext::Request) {
                    return None;
                }
                continue;
            };
            if diagnostics.len() > MAX_COLLECTION_ITEMS {
                if matches!(context, ProjectionContext::Request) {
                    return None;
                }
                continue;
            }
            let values = diagnostics
                .iter()
                .filter_map(diagnostic_projection)
                .collect::<Vec<_>>();
            projected.insert(field.clone(), Value::Array(values));
            continue;
        }
        if field == "scene_revisions" {
            let Some(revisions) = value.as_object() else {
                if matches!(context, ProjectionContext::Request) {
                    return None;
                }
                continue;
            };
            if revisions.len() > MAX_COLLECTION_ITEMS {
                if matches!(context, ProjectionContext::Request) {
                    return None;
                }
                continue;
            }
            let mut safe_revisions = Map::new();
            for (scene_id, revision) in revisions {
                if !safe_project_path(scene_id) || !safe_integer(revision) {
                    if matches!(context, ProjectionContext::Request) {
                        return None;
                    }
                    continue;
                }
                safe_revisions.insert(scene_id.clone(), revision.clone());
            }
            projected.insert(field.clone(), Value::Object(safe_revisions));
            continue;
        }
        let Some(value) = project_field(field, value, depth + 1, context)? else {
            continue;
        };
        projected.insert(field.clone(), value);
    }
    Some(projected)
}

fn project_field(
    field: &str,
    value: &Value,
    depth: usize,
    context: ProjectionContext,
) -> Option<Option<Value>> {
    if depth > MAX_DEPTH {
        return None;
    }
    if value.is_null() {
        return Some(None);
    }
    if value.is_object() && object_field(field) {
        let object = value.as_object()?;
        return match project_object(object, depth, context) {
            Some(projected) => Some(Some(Value::Object(projected))),
            None => invalid_or_omit(context),
        };
    }
    if value.is_array() && array_field(field) {
        let values = value.as_array()?;
        if values.len() > MAX_COLLECTION_ITEMS {
            return invalid_or_omit(context);
        }
        let mut projected = Vec::with_capacity(values.len());
        for value in values {
            let safe = match value {
                Value::Object(object) => project_object(object, depth, context).map(Value::Object),
                Value::String(value) if safe_token(value) || safe_project_path(value) => {
                    Some(Value::String(value.clone()))
                }
                Value::Number(_) if safe_integer(value) => Some(value.clone()),
                _ => None,
            };
            match safe {
                Some(value) => projected.push(value),
                None if matches!(context, ProjectionContext::Request) => return None,
                None => {}
            }
        }
        return Some(Some(Value::Array(projected)));
    }
    if digest_field(field) {
        let valid = value.as_str().is_some_and(valid_digest);
        return if valid {
            Some(Some(value.clone()))
        } else {
            invalid_or_omit(context)
        };
    }
    if path_field(field) {
        let valid = value.as_str().is_some_and(safe_project_path);
        return if valid {
            Some(Some(value.clone()))
        } else {
            invalid_or_omit(context)
        };
    }
    if integer_field(field) {
        return if safe_integer(value) {
            Some(Some(value.clone()))
        } else {
            invalid_or_omit(context)
        };
    }
    if boolean_field(field) {
        return if value.is_boolean() {
            Some(Some(value.clone()))
        } else {
            invalid_or_omit(context)
        };
    }
    if token_field(field) {
        let valid = value.as_str().is_some_and(|value| {
            safe_token(value) || (field.ends_with("_id") && safe_project_path(value))
        });
        return if valid {
            Some(Some(value.clone()))
        } else {
            invalid_or_omit(context)
        };
    }
    if matches!(context, ProjectionContext::Request) {
        // A request has already passed its exact top-level field set. Unknown
        // nested data is not safe to observe.
        return None;
    }
    // Closed server fields irrelevant to the acquisition are omitted.
    Some(None)
}

fn invalid_or_omit(context: ProjectionContext) -> Option<Option<Value>> {
    match context {
        ProjectionContext::Request => None,
        ProjectionContext::Result => Some(None),
    }
}

fn diagnostic_projection(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let mut projected = Map::new();
    projected.insert(
        "fingerprint_sha256".to_owned(),
        Value::String(value_digest(value)?),
    );
    projected.insert("redacted".to_owned(), Value::Bool(true));
    for field in [
        "diagnostic_id",
        "runtime_diagnostic_id",
        "runtime_stack_id",
        "stack_frame_id",
        "code",
        "error_code",
        "severity",
        "fact_code",
    ] {
        if let Some(value) = object.get(field) {
            let token = value.as_str()?;
            if !safe_token(token) {
                return None;
            }
            projected.insert(field.to_owned(), Value::String(token.to_owned()));
        }
    }
    if let Some(source) = object.get("source")
        && let Some(source) = source.as_object()
    {
        let mut safe_source = Map::new();
        if let Some(path) = source.get("path").and_then(Value::as_str)
            && safe_project_path(path)
        {
            safe_source.insert("path".to_owned(), Value::String(path.to_owned()));
        }
        if let Some(line) = source.get("line")
            && safe_integer(line)
        {
            safe_source.insert("line".to_owned(), line.clone());
        }
        if !safe_source.is_empty() {
            projected.insert("source".to_owned(), Value::Object(safe_source));
        }
    }
    Some(Value::Object(projected))
}

fn insert_digest_pair(target: &mut Map<String, Value>, field: &str, value: &Value) -> Option<()> {
    let stem = match field {
        "property" => "property_name",
        "signal" => "signal_name",
        "method" => "method_name",
        other => other,
    };
    target.insert(format!("{stem}_redacted"), Value::Bool(true));
    target.insert(
        format!("{stem}_digest"),
        Value::String(value_digest(value)?),
    );
    if field == "value"
        && let Some(value_type) = value
            .as_object()
            .and_then(|object| object.get("type"))
            .and_then(Value::as_str)
        && safe_token(value_type)
    {
        target.insert(
            "value_type".to_owned(),
            Value::String(value_type.to_owned()),
        );
    }
    Some(())
}

fn validation_content_projection(report: &Value) -> Option<Value> {
    let report = report.as_object()?;
    let transaction_id = report.get("change_set_id")?.as_str()?;
    if !safe_token(transaction_id) {
        return None;
    }
    let outcome = report.get("outcome")?.as_str()?;
    if !matches!(outcome, "passed" | "failed" | "inconclusive" | "timed_out") {
        return None;
    }
    let checks = report.get("checks")?.as_array()?;
    if checks.len() > MAX_VALIDATION_CHECKS {
        return None;
    }
    let checks = checks
        .iter()
        .map(validation_check_projection)
        .collect::<Option<Vec<_>>>()?;
    let mut projection = Map::new();
    projection.insert(
        "transaction_id".to_owned(),
        Value::String(transaction_id.to_owned()),
    );
    projection.insert("outcome".to_owned(), Value::String(outcome.to_owned()));
    projection.insert("checks".to_owned(), Value::Array(checks));
    if let Some(diagnostics) = report
        .get("diagnostics")
        .and_then(validation_diagnostics_projection)
    {
        projection.insert("diagnostics".to_owned(), diagnostics);
    }
    bounded_value(Value::Object(projection))
}

fn validation_check_projection(check: &Value) -> Option<Value> {
    let check = check.as_object()?;
    let check_code = check.get("check")?.as_str()?;
    let authority = check.get("authority")?.as_str()?;
    let outcome = check.get("outcome")?.as_str()?;
    if !matches!(
        check_code,
        "intrinsic"
            | "persistence"
            | "reload_reparse"
            | "index_convergence"
            | "semantic_graph"
            | "diagnostics"
            | "runtime"
    ) || !matches!(authority, "required" | "optional")
        || !matches!(
            outcome,
            "pending"
                | "passed"
                | "failed"
                | "inconclusive"
                | "timed_out"
                | "skipped"
                | "stale"
                | "truncated"
        )
    {
        return None;
    }
    Some(serde_json::json!({
        "check": check_code,
        "authority": authority,
        "outcome": outcome,
    }))
}

fn validation_diagnostics_projection(diagnostics: &Value) -> Option<Value> {
    let diagnostics = diagnostics.as_object()?;
    let mut projected = Map::new();
    for field in ["pre_existing", "resolved", "introduced"] {
        let Some(values) = diagnostics.get(field).and_then(Value::as_array) else {
            continue;
        };
        if values.len() > MAX_COLLECTION_ITEMS {
            return None;
        }
        let values = values
            .iter()
            .filter_map(validation_fingerprint_projection)
            .collect::<Vec<_>>();
        projected.insert(field.to_owned(), Value::Array(values));
    }
    for field in ["introduced_errors", "introduced_warnings"] {
        if let Some(value) = diagnostics.get(field)
            && safe_integer(value)
        {
            projected.insert(field.to_owned(), value.clone());
        }
    }
    Some(Value::Object(projected))
}

fn validation_fingerprint_projection(fingerprint: &Value) -> Option<Value> {
    let fingerprint = fingerprint.as_object()?;
    let raw = Value::Object(fingerprint.clone());
    let severity = fingerprint.get("severity")?.as_str()?;
    if !matches!(severity, "warning" | "error") {
        return None;
    }
    let mut projected = Map::new();
    projected.insert(
        "fingerprint_sha256".to_owned(),
        Value::String(value_digest(&raw)?),
    );
    projected.insert("severity".to_owned(), Value::String(severity.to_owned()));
    for field in ["id", "entity_id"] {
        if let Some(value) = fingerprint.get(field).and_then(Value::as_str)
            && safe_token(value)
        {
            projected.insert(field.to_owned(), Value::String(value.to_owned()));
        }
    }
    if let Some(value) = fingerprint.get("message_digest").and_then(Value::as_str)
        && valid_digest(value)
    {
        projected.insert("message_digest".to_owned(), Value::String(value.to_owned()));
    }
    Some(Value::Object(projected))
}

fn value_digest(value: &Value) -> Option<String> {
    let bytes = serde_json::to_vec(value).ok()?;
    (bytes.len() <= MAX_OBSERVATION_BYTES).then(|| sha256_bytes(&bytes))
}

fn sensitive_field(field: &str) -> bool {
    matches!(
        field,
        "name"
            | "value"
            | "script"
            | "script_ref"
            | "binds"
            | "replacement"
            | "properties"
            | "message"
            | "safe_message"
            | "summary"
            | "query"
            | "next_action"
            | "idempotency_key"
            | "fact_value"
            | "source_text"
            | "native_id"
            | "object_id"
    )
}

fn digest_field(field: &str) -> bool {
    field.ends_with("_digest")
        || field.ends_with("_sha256")
        || matches!(field, "sha256" | "expected_hash")
}

fn path_field(field: &str) -> bool {
    field.ends_with("_path")
        || matches!(
            field,
            "path" | "scene" | "resource" | "resolved_path" | "source_path"
        )
}

fn integer_field(field: &str) -> bool {
    field.ends_with("_seq")
        || field.ends_with("_revision")
        || field.ends_with("_count")
        || field.ends_with("_bytes")
        || field.ends_with("_ms")
        || matches!(
            field,
            "line"
                | "page"
                | "page_count"
                | "limit"
                | "offset"
                | "total"
                | "flags"
                | "unbinds"
                | "insertion_index"
                | "start_byte"
                | "end_byte"
        )
}

fn boolean_field(field: &str) -> bool {
    field.starts_with("is_")
        || field.ends_with("_cached")
        || field.ends_with("_claimed")
        || field.ends_with("_observed")
        || field.ends_with("_redacted")
        || field.ends_with("_verified")
        || matches!(
            field,
            "retryable"
                | "truncated"
                | "keep_global_transform"
                | "eligible"
                | "project_context_truncated"
                | "matched"
                | "affected_closure_only"
                | "available"
                | "visible"
                | "visible_in_tree"
        )
}

fn token_field(field: &str) -> bool {
    field.ends_with("_id")
        || field.ends_with("_code")
        || matches!(
            field,
            "schema_version"
                | "status"
                | "state"
                | "outcome"
                | "condition"
                | "risk"
                | "scope"
                | "freshness"
                | "confidence"
                | "remediation_id"
                | "project_scope"
                | "recovery"
                | "editor"
                | "transactions"
                | "authority"
                | "check"
                | "code"
                | "severity"
                | "source"
                | "schema"
                | "generation"
                | "negotiated_protocol"
                | "kind"
                | "operation_kind"
                | "godot_type"
                | "property_name"
                | "signal_name"
                | "method_name"
                | "origin"
                | "target"
                | "reason"
                | "terminal_reason"
                | "cursor"
                | "next_cursor"
                | "fact_code"
                | "rollback"
                | "warnings"
                | "runtime"
                | "resource_class"
                | "alias"
        )
}

fn object_field(field: &str) -> bool {
    matches!(
        field,
        "project_scope"
            | "compatibility"
            | "bridge"
            | "static_cache"
            | "recovery"
            | "components"
            | "diagnostic"
            | "evidence"
            | "scene"
            | "revision_vector"
            | "stack"
            | "source"
            | "coordinates"
            | "preview"
            | "validation_report"
            | "outcome"
            | "error"
            | "undo_eligibility"
            | "limits_applied"
            | "live_overlay"
            | "save_scope"
            | "validation_policy"
            | "terminal_error"
    )
}

fn array_field(field: &str) -> bool {
    matches!(
        field,
        "nodes"
            | "facts"
            | "entities"
            | "frames"
            | "operations"
            | "committed_entities"
            | "partial_reasons"
            | "capabilities_used"
            | "conflicts"
            | "paths"
            | "checks"
            | "capabilities"
            | "evidence"
    )
}

fn safe_integer(value: &Value) -> bool {
    value
        .as_u64()
        .is_some_and(|value| value <= MAX_SAFE_INTEGER)
}

fn valid_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn safe_project_path(value: &str) -> bool {
    let path = value
        .strip_prefix("res://")
        .or_else(|| value.strip_prefix("scene:res://"));
    let Some(path) = path else {
        return false;
    };
    !path.is_empty()
        && value.len() <= 1_024
        && !path.starts_with('/')
        && !path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "..")
        && !path.chars().any(char::is_control)
        && !path.contains('\\')
}

fn safe_token(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    !value.is_empty()
        && value.len() <= 512
        && !contains_private_absolute_path(value)
        && !contains_private_secret_marker(value)
        && !contains_account_identity(value)
        && !value.contains("://")
        && !lower.contains("secret")
        && !lower.contains("password")
        && !lower.contains("token")
        && !lower.contains("authorization")
        && !lower.starts_with("sk-")
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/' | b'@' | b'{' | b'}')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }

    #[test]
    fn exact_tool_set_and_unknown_request_shapes_fail_closed() {
        assert_eq!(SUPPORTED_TOOLS.len(), 13);
        assert!(supported_tool("godot_get_connection_status"));
        assert!(!supported_tool("godot_capture_viewport"));
        assert!(
            request_observation(
                "godot_get_connection_status",
                Some(&serde_json::json!({"injected": true}))
            )
            .is_none()
        );
        assert!(request_observation("godot_capture_viewport", None).is_none());
    }

    #[test]
    fn compound_request_golden_redacts_raw_values() {
        let observed = request_observation(
            "godot_prepare_change_set",
            Some(&serde_json::json!({
                "project_id": format!("project:sha256:{}", "1".repeat(64)),
                "idempotency_key": format!("idempotency:{}", "2".repeat(32)),
                "coordinates": {
                    "editor_session_id": format!("editor:{}", "3".repeat(32)),
                    "scene_id": format!("scene:{}", "4".repeat(32)),
                    "scene_revision": 7,
                    "operation_seq": 8,
                    "resource_revision": 9,
                    "script_graph_revision": 10
                },
                "operations": [{
                    "kind": "create_node",
                    "alias": "alias:created",
                    "parent_node_id": format!("node:{}", "5".repeat(32)),
                    "godot_type": "Node",
                    "name": "Private Node Name"
                },{
                    "kind": "set_property",
                    "node_id": "alias:created",
                    "property": "private_property",
                    "value": {"type":"string","value":"private value"}
                },{
                    "kind": "connect_signal",
                    "emitter_node_id": "alias:created",
                    "signal": "ready",
                    "receiver_node_id": format!("node:{}", "6".repeat(32)),
                    "method": "_on_ready",
                    "flags": 0,
                    "unbinds": 0,
                    "binds": [{"type":"string","value":"private bind"}]
                }],
                "save_scope": {"paths":[]},
                "validation_policy": {
                    "rollback":"on_required_failure",
                    "warnings":"allow",
                    "runtime":"skip"
                }
            })),
        )
        .unwrap();
        assert_eq!(
            observed.pointer("/operations/0/name_redacted"),
            Some(&Value::Bool(true))
        );
        assert!(observed.pointer("/operations/0/name_digest").is_some());
        assert_eq!(
            observed.pointer("/operations/1/value_redacted"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            observed.pointer("/operations/1/value_type"),
            Some(&Value::String("string".to_owned()))
        );
        assert_eq!(
            observed.pointer("/operations/2/binds_redacted"),
            Some(&Value::Bool(true))
        );
        let encoded = serde_json::to_string(&observed).unwrap();
        for forbidden in [
            "Private Node Name",
            "private value",
            "private bind",
            "idempotency:2222",
        ] {
            assert!(!encoded.contains(forbidden), "retained {forbidden}");
        }
    }

    #[test]
    fn deterministic_result_golden_retains_only_safe_evidence() {
        let message = serde_json::json!({
            "result": {
                "isError": false,
                "structuredContent": {
                    "schema_version": "transaction-status/1.0",
                    "change_set_id": format!("change-set:{}", "1".repeat(32)),
                    "state": "committed",
                    "transaction_seq": 3,
                    "preview_digest": digest('2'),
                    "request_digest": digest('3'),
                    "preview": {
                        "operations": [{
                            "kind": "create_node",
                            "parent_node_id": format!("node:{}", "4".repeat(32)),
                            "godot_type": "Node",
                            "name": "Private Name"
                        }],
                        "risk":"low",
                        "scope":"change_set.atomic"
                    },
                    "risk": "low",
                    "scope": "change_set.atomic",
                    "operation_count": 1,
                    "created_at_ms": 10,
                    "expires_at_ms": 20,
                    "limits_applied": {},
                    "outcome": {"status":"passed"},
                    "coordinates": {
                        "editor_session_id": format!("editor:{}", "5".repeat(32)),
                        "scene_revision": 11,
                        "operation_seq": 12,
                        "event_seq": 13
                    },
                    "postimage_digest": digest('6'),
                    "validation_report_id": format!("validation-report:{}", "7".repeat(32)),
                    "terminal_error": null
                }
            }
        });
        let first = result_observation("godot_apply_transaction", &message).unwrap();
        let second = result_observation("godot_apply_transaction", &message).unwrap();
        assert_eq!(first, second);
        assert_eq!(first["state"], "committed");
        assert_eq!(first["coordinates"]["scene_revision"], 11);
        assert_eq!(
            first.pointer("/preview/operations/0/name_redacted"),
            Some(&Value::Bool(true))
        );
        let encoded = serde_json::to_string(&first).unwrap();
        assert!(!encoded.contains("Private Name"));
        assert!(!encoded.contains("/Users/"));
    }

    #[test]
    fn realistic_connection_and_current_scene_shapes_are_projected() {
        let connection = serde_json::json!({
            "result": {
                "structuredContent": {
                    "schema_version":"connection-status/1.0",
                    "status":"ready",
                    "project_scope":"project:fixture",
                    "package_version":"0.1.1",
                    "compatibility":{"status":"compatible"},
                    "bridge":{
                        "condition":"ready",
                        "negotiated_protocol":"1.8",
                        "capabilities":["transactions","runtime"]
                    },
                    "static_cache":{
                        "condition":"verified_current",
                        "schema":"semantic-cache/1.0",
                        "generation":"generation:fixture",
                        "revisions":{"index_revision":4,"scene_graph_revision":5},
                        "source_hashes_verified":true,
                        "age_seconds":1
                    },
                    "recovery":"none",
                    "components":{
                        "editor":"ready",
                        "runtime":"unavailable",
                        "transactions":"ready"
                    },
                    "diagnostic":{"code":"ready","message":"Private diagnostic detail"},
                    "remediation_id":"none",
                    "next_action":"Wait for a private project at /Users/alice/project.",
                    "limits_applied":{"max_bytes":8192,"truncated":false,"omitted_counts":{}},
                    "evidence":[{"source":"sidecar_connection_state","freshness":"current"}],
                    "future_private_field":"/Users/alice/project"
                }
            }
        });
        let observed = result_observation("godot_get_connection_status", &connection).unwrap();
        assert_eq!(observed["status"], "ready");
        assert_eq!(observed["project_scope"], "project:fixture");
        assert_eq!(observed["bridge"]["condition"], "ready");
        assert_eq!(observed["components"]["editor"], "ready");
        assert_eq!(
            observed.pointer("/next_action_redacted"),
            Some(&Value::Bool(true))
        );
        assert!(observed.pointer("/next_action_digest").is_some());
        let encoded = serde_json::to_string(&observed).unwrap();
        assert!(!encoded.contains("/Users/alice"));
        assert!(!encoded.contains("future_private_field"));

        let current_scene = serde_json::json!({
            "result": {
                "structuredContent": {
                    "schema_version":"current-scene/1.0",
                    "project_id":format!("project:{}", "1".repeat(32)),
                    "editor_session_id":format!("editor:{}", "2".repeat(32)),
                    "snapshot_id":format!("snapshot:{}", "3".repeat(32)),
                    "revision_vector":{
                        "editor_session_id":format!("editor:{}", "2".repeat(32)),
                        "event_seq":8,
                        "project_revision":6,
                        "operation_seq":9,
                        "resource_revision":10,
                        "scene_graph_revision":11,
                        "script_graph_revision":12,
                        "scene_revisions":{"scene:res://main.tscn":7}
                    },
                    "status":"ready",
                    "freshness":"current",
                    "truncated":false,
                    "evidence":[],
                    "capabilities_used":[],
                    "partial_reasons":[],
                    "diagnostics":[],
                    "limits_applied":{},
                    "entities":[],
                    "facts":[],
                    "scene":{
                        "scene_id":"scene:res://main.tscn",
                        "path":"res://main.tscn",
                        "name":"Private Main Scene"
                    },
                    "nodes":[]
                }
            }
        });
        let observed = result_observation("godot_get_current_scene", &current_scene).unwrap();
        assert_eq!(
            observed.pointer("/scene/scene_id"),
            Some(&Value::String("scene:res://main.tscn".to_owned()))
        );
        assert_eq!(
            observed.pointer("/revision_vector/scene_revisions/scene:res:~1~1main.tscn"),
            Some(&Value::from(7))
        );
        assert!(
            !serde_json::to_string(&observed)
                .unwrap()
                .contains("Private Main Scene")
        );
    }

    #[test]
    fn runtime_native_paths_are_omitted_without_losing_runtime_ids() {
        let message = serde_json::json!({
            "result": {
                "structuredContent": {
                    "schema_version":"runtime-tree/1.0",
                    "project_id":format!("project:{}", "1".repeat(32)),
                    "editor_session_id":format!("editor:{}", "2".repeat(32)),
                    "runtime_session_id":format!("runtime:{}", "3".repeat(32)),
                    "runtime_event_seq":9,
                    "state":"running",
                    "snapshot_id":format!("snapshot:{}", "4".repeat(32)),
                    "limit":100,
                    "offset":0,
                    "total":1,
                    "nodes":[{
                        "kind":"runtime_node",
                        "entity_id":format!("runtime-object:{}", "5".repeat(43)),
                        "runtime_object_id":format!("runtime-object:{}", "5".repeat(43)),
                        "parent_runtime_object_id":null,
                        "name":"Private Player",
                        "godot_type":"Node2D",
                        "runtime_node_path":"/root/PrivatePlayer",
                        "depth":1,
                        "child_count":0,
                        "visibility":{"available":true,"visible":true,"visible_in_tree":true}
                    }],
                    "truncated":false,
                    "limits_applied":{},
                    "next_cursor":null,
                    "evidence":[],
                    "private_extension":"/Users/alice/runtime"
                }
            }
        });
        let observed = result_observation("godot_get_runtime_tree", &message).unwrap();
        assert_eq!(
            observed.pointer("/nodes/0/runtime_object_id"),
            Some(&Value::String(format!("runtime-object:{}", "5".repeat(43))))
        );
        assert!(observed.pointer("/nodes/0/runtime_node_path").is_none());
        assert_eq!(
            observed.pointer("/nodes/0/name_redacted"),
            Some(&Value::Bool(true))
        );
        let encoded = serde_json::to_string(&observed).unwrap();
        assert!(!encoded.contains("/root/PrivatePlayer"));
        assert!(!encoded.contains("/Users/alice/runtime"));
    }

    #[test]
    fn embedded_private_paths_secrets_and_accounts_are_not_safe_tokens() {
        for unsafe_token in [
            "ready:/Users/alice",
            "path=/Users/alice",
            "artifact=C:/Users/alice",
            r"path=\Users\alice",
            r"\\private-server\account",
            "ready:sk-proj-abcdefghijklmnop",
            "ready:ghp_abcdefghijklmnopqrst",
            "account:alice@example.com",
        ] {
            assert!(
                !safe_token(unsafe_token),
                "unsafe token was accepted: {unsafe_token}"
            );
        }
        for safe in [
            "ready",
            "project:fixture",
            "class:Player/method:attack/local:PlayerLocal@7",
            "/usr/bin/shasum",
        ] {
            assert!(safe_token(safe), "closed safe token was rejected: {safe}");
        }

        let response = serde_json::json!({
            "result": {
                "structuredContent": {
                    "schema_version": "connection-status/1.0",
                    "status": "ready:/Users/alice"
                }
            }
        });
        let observed = result_observation("godot_get_connection_status", &response).unwrap();
        assert!(observed.get("status").is_none());
        assert!(
            !serde_json::to_string(&observed)
                .unwrap()
                .contains("/Users/alice")
        );

        assert!(
            request_observation(
                "godot_apply_transaction",
                Some(&serde_json::json!({
                    "transaction_id": "ready:sk-proj-abcdefghijklmnop"
                }))
            )
            .is_none()
        );
    }

    #[test]
    fn validation_report_page_parses_only_safe_content_projection() {
        let report = serde_json::json!({
            "report_id":format!("validation-report:{}", "1".repeat(32)),
            "report_digest":digest('2'),
            "change_set_id":format!("change-set:{}", "3".repeat(32)),
            "preview_digest":digest('4'),
            "outcome":"failed",
            "policy":{"rollback":"on_required_failure","warnings":"allow","runtime":"skip"},
            "started_at_ms":10,
            "completed_at_ms":20,
            "checks":[{
                "check":"diagnostics",
                "authority":"required",
                "outcome":"failed",
                "started_at_ms":10,
                "completed_at_ms":20,
                "summary":"Private check summary at /Users/alice/project",
                "evidence_digest":digest('5')
            }],
            "semantic":null,
            "diagnostics":{
                "pre_existing":[],
                "resolved":[],
                "introduced":[{
                    "id":"diagnostic:fixture",
                    "severity":"error",
                    "source":"gdscript",
                    "entity_id":"script:fixture",
                    "message_digest":digest('6')
                }],
                "introduced_errors":1,
                "introduced_warnings":0
            },
            "runtime_session_id":null,
            "affected_closure_only":true
        });
        let content = serde_json::to_string(&report).unwrap();
        let message = serde_json::json!({
            "result": {
                "structuredContent": {
                    "schema_version":"validation-report-page/1.0",
                    "report_id":format!("validation-report:{}", "1".repeat(32)),
                    "report_digest":digest('2'),
                    "page":0,
                    "page_count":1,
                    "content":content,
                    "content_bytes":content.len(),
                    "limits_applied":{
                        "page_bytes":65536,
                        "pages":4,
                        "retained_report_bytes":262144
                    }
                }
            }
        });
        let observed = result_observation("godot_get_validation_report", &message).unwrap();
        assert!(observed.get("content").is_none());
        assert_eq!(
            observed.pointer("/content_projection/transaction_id"),
            Some(&Value::String(format!("change-set:{}", "3".repeat(32))))
        );
        assert_eq!(
            observed.pointer("/content_projection/checks/0/check"),
            Some(&Value::String("diagnostics".to_owned()))
        );
        assert_eq!(
            observed.pointer("/content_projection/diagnostics/introduced/0/severity"),
            Some(&Value::String("error".to_owned()))
        );
        assert!(
            observed
                .pointer("/content_projection/diagnostics/introduced/0/fingerprint_sha256")
                .is_some()
        );
        let encoded = serde_json::to_string(&observed).unwrap();
        assert!(!encoded.contains("Private check summary"));
        assert!(!encoded.contains("/Users/alice"));

        let mut continuation = message;
        continuation["result"]["structuredContent"]["page"] = Value::from(1);
        assert!(
            result_observation("godot_get_validation_report", &continuation)
                .unwrap()
                .get("content_projection")
                .is_none()
        );
    }
}
