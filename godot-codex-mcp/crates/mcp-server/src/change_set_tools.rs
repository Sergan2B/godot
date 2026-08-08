use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use godot_codex_bridge_client::{
    Risk, TransactionOperation, TransactionResourceRef, WritableVariant,
};
use godot_codex_transactions::{ReportPage, ValidationError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) const CONFIRMATION_GRANT_MAX_MS: u64 = 15 * 60 * 1_000;
pub(crate) const LOW_RISK_MEMORY_ONLY_SCOPE: &str = "change_set.atomic";
const LOW_RISK_MEMORY_ONLY_OPERATIONS: [&str; 3] =
    ["attach_script", "connect_signal", "create_node"];

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub(crate) enum ResourceClassInput {
    #[serde(rename = "Gradient")]
    Gradient,
    #[serde(rename = "Curve")]
    Curve,
    #[serde(rename = "Curve2D")]
    Curve2d,
    #[serde(rename = "Curve3D")]
    Curve3d,
    #[serde(rename = "Animation")]
    Animation,
    #[serde(rename = "CanvasItemMaterial")]
    CanvasItemMaterial,
    #[serde(rename = "StandardMaterial3D")]
    StandardMaterial3d,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredPropertyInput {
    #[schemars(
        regex(pattern = r"^[A-Za-z_][A-Za-z0-9_]*$"),
        length(min = 1, max = 128)
    )]
    pub name: String,
    pub value: WritableVariant,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScriptEditInput {
    #[schemars(range(max = 9_007_199_254_740_991_u64))]
    pub start_byte: u64,
    #[schemars(range(max = 9_007_199_254_740_991_u64))]
    pub end_byte: u64,
    #[schemars(length(max = 65_536))]
    pub replacement: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum PersistenceOperationInput {
    CreateResource {
        #[schemars(regex(pattern = r"^alias:[a-z][a-z0-9_]{0,63}$"))]
        alias: String,
        #[schemars(regex(pattern = r"^res://[^\\\u0000-\u001f]{1,1018}$"))]
        path: String,
        resource_class: ResourceClassInput,
        #[schemars(length(max = 64))]
        properties: Vec<StoredPropertyInput>,
    },
    UpdateResource {
        #[schemars(regex(
            pattern = r"^(?:godot:resource:(?:uid|path-content):v1:[A-Za-z0-9_-]{43}|alias:[a-z][a-z0-9_]{0,63})$"
        ))]
        resource: String,
        #[schemars(regex(pattern = r"^sha256:[0-9a-f]{64}$"))]
        expected_hash: String,
        #[schemars(length(min = 1, max = 64))]
        properties: Vec<StoredPropertyInput>,
    },
    UpdateGdscript {
        #[schemars(regex(pattern = r"^res://[^\\\u0000-\u001f]{1,1015}\.gd$"))]
        path: String,
        #[schemars(regex(pattern = r"^sha256:[0-9a-f]{64}$"))]
        expected_hash: String,
        #[schemars(length(min = 1, max = 64))]
        edits: Vec<ScriptEditInput>,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum CompoundSceneOperationInput {
    CreateNode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(regex(pattern = r"^alias:[a-z][a-z0-9_]{0,63}$"))]
        alias: Option<String>,
        #[schemars(regex(pattern = r"^(?:node:[0-9a-f]{32}|alias:[a-z][a-z0-9_]{0,63})$"))]
        parent_node_id: String,
        #[schemars(regex(pattern = r"^[A-Za-z_][A-Za-z0-9_]{0,127}$"))]
        godot_type: String,
        #[schemars(length(min = 1, max = 255))]
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(range(max = 2_147_483_647_u32))]
        insertion_index: Option<u32>,
    },
    DeleteNode {
        #[schemars(regex(pattern = r"^(?:node:[0-9a-f]{32}|alias:[a-z][a-z0-9_]{0,63})$"))]
        node_id: String,
    },
    ReparentNode {
        #[schemars(regex(pattern = r"^(?:node:[0-9a-f]{32}|alias:[a-z][a-z0-9_]{0,63})$"))]
        node_id: String,
        #[schemars(regex(pattern = r"^(?:node:[0-9a-f]{32}|alias:[a-z][a-z0-9_]{0,63})$"))]
        new_parent_node_id: String,
        #[schemars(range(max = 2_147_483_647_u32))]
        insertion_index: u32,
        keep_global_transform: bool,
    },
    SetProperty {
        #[schemars(regex(pattern = r"^(?:node:[0-9a-f]{32}|alias:[a-z][a-z0-9_]{0,63})$"))]
        node_id: String,
        #[schemars(regex(pattern = r"^[A-Za-z_][A-Za-z0-9_]*$"))]
        property: String,
        value: WritableVariant,
    },
    AttachScript {
        #[schemars(regex(pattern = r"^(?:node:[0-9a-f]{32}|alias:[a-z][a-z0-9_]{0,63})$"))]
        node_id: String,
        script_ref: TransactionResourceRef,
    },
    DetachScript {
        #[schemars(regex(pattern = r"^(?:node:[0-9a-f]{32}|alias:[a-z][a-z0-9_]{0,63})$"))]
        node_id: String,
    },
    ConnectSignal {
        #[schemars(regex(pattern = r"^(?:node:[0-9a-f]{32}|alias:[a-z][a-z0-9_]{0,63})$"))]
        emitter_node_id: String,
        #[schemars(regex(pattern = r"^[A-Za-z_][A-Za-z0-9_]*$"))]
        signal: String,
        #[schemars(regex(pattern = r"^(?:node:[0-9a-f]{32}|alias:[a-z][a-z0-9_]{0,63})$"))]
        receiver_node_id: String,
        #[schemars(regex(pattern = r"^[A-Za-z_][A-Za-z0-9_]*$"))]
        method: String,
        flags: u8,
        unbinds: u16,
        #[schemars(length(max = 1_000))]
        binds: Vec<WritableVariant>,
    },
    DisconnectSignal {
        #[schemars(regex(pattern = r"^(?:node:[0-9a-f]{32}|alias:[a-z][a-z0-9_]{0,63})$"))]
        emitter_node_id: String,
        #[schemars(regex(pattern = r"^[A-Za-z_][A-Za-z0-9_]*$"))]
        signal: String,
        #[schemars(regex(pattern = r"^(?:node:[0-9a-f]{32}|alias:[a-z][a-z0-9_]{0,63})$"))]
        receiver_node_id: String,
        #[schemars(regex(pattern = r"^[A-Za-z_][A-Za-z0-9_]*$"))]
        method: String,
        flags: u8,
        unbinds: u16,
        #[schemars(length(max = 1_000))]
        binds: Vec<WritableVariant>,
    },
}

impl CompoundSceneOperationInput {
    fn validate(&self) -> Result<(), &'static str> {
        let mut value = serde_json::to_value(self).map_err(|_| "a scene operation is invalid")?;
        let object = value
            .as_object_mut()
            .ok_or("a scene operation is invalid")?;
        object.remove("alias");
        for key in [
            "node_id",
            "parent_node_id",
            "new_parent_node_id",
            "emitter_node_id",
            "receiver_node_id",
        ] {
            if object
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|reference| reference.starts_with("alias:"))
            {
                object.insert(
                    key.to_owned(),
                    Value::String("node:00000000000000000000000000000000".to_owned()),
                );
            }
        }
        let operation: TransactionOperation =
            serde_json::from_value(value).map_err(|_| "a scene operation is invalid")?;
        operation
            .validate()
            .map_err(|_| "a scene operation is invalid")
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ChangeSetOperationInput {
    Scene(CompoundSceneOperationInput),
    Persistence(PersistenceOperationInput),
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangeSetCoordinatesInput {
    #[schemars(regex(pattern = r"^editor:[0-9a-f]{32}$"))]
    pub editor_session_id: String,
    #[schemars(regex(pattern = r"^scene:[0-9a-f]{32}$"))]
    pub scene_id: String,
    #[schemars(range(max = 9_007_199_254_740_991_u64))]
    pub scene_revision: u64,
    #[schemars(range(max = 9_007_199_254_740_991_u64))]
    pub operation_seq: u64,
    #[schemars(range(max = 9_007_199_254_740_991_u64))]
    pub resource_revision: u64,
    #[schemars(range(max = 9_007_199_254_740_991_u64))]
    pub script_graph_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SaveScopeInput {
    #[schemars(length(max = 18))]
    pub paths: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RollbackPolicyInput {
    Never,
    OnRequiredFailure,
    OnAnyFailure,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WarningPolicyInput {
    Allow,
    FailOnIntroduced,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeValidationInput {
    Skip,
    RunCurrentScene,
    RunProject,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ValidationPolicyInput {
    pub rollback: RollbackPolicyInput,
    pub warnings: WarningPolicyInput,
    pub runtime: RuntimeValidationInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 250, max = 30_000))]
    pub runtime_timeout_ms: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrepareChangeSetInput {
    #[schemars(regex(pattern = r"^project:sha256:[0-9a-f]{64}$"))]
    pub project_id: String,
    #[schemars(regex(pattern = r"^idempotency:[0-9a-f]{32}$"))]
    pub idempotency_key: String,
    pub coordinates: ChangeSetCoordinatesInput,
    #[schemars(length(min = 1, max = 16))]
    pub operations: Vec<ChangeSetOperationInput>,
    pub save_scope: SaveScopeInput,
    pub validation_policy: ValidationPolicyInput,
}

impl PrepareChangeSetInput {
    pub(crate) fn index_requirements(&self) -> (bool, bool, bool) {
        let has_scene = self
            .operations
            .iter()
            .any(|operation| matches!(operation, ChangeSetOperationInput::Scene(_)));
        let scene_persisted = has_scene
            && self
                .save_scope
                .paths
                .iter()
                .any(|path| path.ends_with(".tscn"));
        let resource = self.operations.iter().any(|operation| {
            matches!(
                operation,
                ChangeSetOperationInput::Persistence(
                    PersistenceOperationInput::CreateResource { .. }
                        | PersistenceOperationInput::UpdateResource { .. }
                )
            )
        });
        let script = self.operations.iter().any(|operation| {
            matches!(
                operation,
                ChangeSetOperationInput::Persistence(
                    PersistenceOperationInput::UpdateGdscript { .. }
                )
            )
        });
        (scene_persisted, resource, script)
    }

    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.operations.is_empty() || self.operations.len() > 16 {
            return Err("operations must contain between 1 and 16 entries");
        }
        if self.save_scope.paths.len() > 18 {
            return Err("save_scope.paths cannot contain more than 18 entries");
        }
        let mut paths = BTreeSet::new();
        for path in &self.save_scope.paths {
            if !valid_project_path(path) || !paths.insert(path) {
                return Err("save_scope contains an invalid or duplicate project path");
            }
        }
        for operation in &self.operations {
            match operation {
                ChangeSetOperationInput::Scene(operation) => {
                    operation
                        .validate()
                        .map_err(|_| "a scene operation is invalid")?;
                }
                ChangeSetOperationInput::Persistence(operation) => {
                    validate_persistence_operation(operation)?;
                }
            }
        }
        let has_persistence = self
            .operations
            .iter()
            .any(|operation| matches!(operation, ChangeSetOperationInput::Persistence(_)));
        let has_scene = self
            .operations
            .iter()
            .any(|operation| matches!(operation, ChangeSetOperationInput::Scene(_)));
        let scene_paths = self
            .save_scope
            .paths
            .iter()
            .filter(|path| path.ends_with(".tscn"))
            .count();
        const SCOPE_ERROR: &str = "save_scope must contain only affected persistence targets and at most one saved open scene";
        if scene_paths > usize::from(has_scene) {
            return Err(SCOPE_ERROR);
        }
        if has_persistence && self.save_scope.paths.is_empty() {
            return Err(SCOPE_ERROR);
        }
        if !has_persistence
            && !self.save_scope.paths.is_empty()
            && (!has_scene || scene_paths != self.save_scope.paths.len())
        {
            return Err(SCOPE_ERROR);
        }
        Ok(())
    }

    pub(crate) fn bridge_params(&self) -> Value {
        json!({
            "idempotency_key": self.idempotency_key,
            "coordinates": self.coordinates,
            "operations": self.operations,
            "save_scope": self.save_scope,
            "validation_policy": self.validation_policy,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ValidationReportInput {
    #[schemars(regex(pattern = r"^validation-report:[0-9a-f]{32}$"))]
    pub report_id: String,
    #[serde(default)]
    #[schemars(range(max = 3))]
    pub page: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfirmationPolicyInput {}

fn valid_project_path(path: &str) -> bool {
    path.starts_with("res://")
        && (7..=1_024).contains(&path.len())
        && !path.contains('\\')
        && !path.contains('\0')
        && !path.split('/').any(|segment| segment == "..")
        && !path.chars().any(char::is_control)
}

fn validate_persistence_operation(
    operation: &PersistenceOperationInput,
) -> Result<(), &'static str> {
    match operation {
        PersistenceOperationInput::CreateResource {
            path, properties, ..
        } => {
            if !valid_project_path(path) || properties.len() > 64 {
                return Err("a resource creation is invalid");
            }
        }
        PersistenceOperationInput::UpdateResource { properties, .. } => {
            if properties.is_empty() || properties.len() > 64 {
                return Err("a resource update is invalid");
            }
        }
        PersistenceOperationInput::UpdateGdscript { path, edits, .. } => {
            if !valid_project_path(path)
                || !path.ends_with(".gd")
                || edits.is_empty()
                || edits.len() > 64
                || edits
                    .iter()
                    .any(|edit| edit.start_byte > edit.end_byte || edit.replacement.len() > 65_536)
            {
                return Err("a GDScript update is invalid");
            }
        }
    }
    Ok(())
}

pub(crate) fn session_grant_eligible(preview: &Value, risk: Risk) -> bool {
    if risk != Risk::Low
        || preview.get("schema_version").and_then(Value::as_str)
            != Some("canonical-change-set-preview/1.0")
        || preview.get("risk").and_then(Value::as_str) != Some("low")
        || !preview
            .get("save_scope")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        || preview
            .pointer("/validation_policy/runtime")
            .and_then(Value::as_str)
            != Some("skip")
    {
        return false;
    }
    let Some(operations) = preview.get("operations").and_then(Value::as_array) else {
        return false;
    };
    !operations.is_empty()
        && operations.iter().all(|operation| {
            operation
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| LOW_RISK_MEMORY_ONLY_OPERATIONS.contains(&kind))
        })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConfirmationPolicyMode {
    AlwaysAsk,
    AllowLowRiskForSession,
}

#[derive(Clone, Debug)]
struct ConfirmationGrant {
    project_id: String,
    editor_session_id: String,
    expires_at_ms: u64,
    scopes: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct ConfirmationPolicyState {
    grant: Option<ConfirmationGrant>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ConfirmationPolicyStore {
    inner: Arc<Mutex<ConfirmationPolicyState>>,
}

impl ConfirmationPolicyStore {
    #[allow(
        dead_code,
        reason = "called by the trusted compound-apply elicitation path"
    )]
    pub(crate) fn grant_low_risk_for_session(
        &self,
        project_id: &str,
        editor_session_id: &str,
        now_ms: u64,
        requested_lifetime_ms: u64,
    ) -> bool {
        if requested_lifetime_ms == 0 || requested_lifetime_ms > CONFIRMATION_GRANT_MAX_MS {
            return false;
        }
        let Ok(mut state) = self.inner.lock() else {
            return false;
        };
        state.grant = Some(ConfirmationGrant {
            project_id: project_id.to_owned(),
            editor_session_id: editor_session_id.to_owned(),
            expires_at_ms: now_ms.saturating_add(requested_lifetime_ms),
            scopes: BTreeSet::from([LOW_RISK_MEMORY_ONLY_SCOPE.to_owned()]),
        });
        true
    }

    #[allow(
        dead_code,
        reason = "called by the trusted compound-apply elicitation path"
    )]
    pub(crate) fn allows_low_risk_memory_only(
        &self,
        project_id: &str,
        editor_session_id: &str,
        scope: &str,
        now_ms: u64,
    ) -> bool {
        let Ok(mut state) = self.inner.lock() else {
            return false;
        };
        if state
            .grant
            .as_ref()
            .is_some_and(|grant| grant.expires_at_ms <= now_ms)
        {
            state.grant = None;
        }
        state.grant.as_ref().is_some_and(|grant| {
            grant.project_id == project_id
                && grant.editor_session_id == editor_session_id
                && grant.scopes.contains(scope)
        })
    }

    pub(crate) fn snapshot(
        &self,
        project_id: Option<&str>,
        editor_session_id: Option<&str>,
        now_ms: u64,
    ) -> Value {
        let Ok(mut state) = self.inner.lock() else {
            return json!({
                "mode": ConfirmationPolicyMode::AlwaysAsk,
                "grant_active": false,
                "reason": "policy_store_unavailable",
                "max_grant_lifetime_ms": CONFIRMATION_GRANT_MAX_MS,
            });
        };
        if state
            .grant
            .as_ref()
            .is_some_and(|grant| grant.expires_at_ms <= now_ms)
        {
            state.grant = None;
        }
        let matching = state.grant.as_ref().filter(|grant| {
            project_id == Some(grant.project_id.as_str())
                && editor_session_id == Some(grant.editor_session_id.as_str())
        });
        json!({
            "mode": if matching.is_some() {
                ConfirmationPolicyMode::AllowLowRiskForSession
            } else {
                ConfirmationPolicyMode::AlwaysAsk
            },
            "grant_active": matching.is_some(),
            "expires_at_ms": matching.map(|grant| grant.expires_at_ms),
            "allowed_scopes": matching.map(|grant| grant.scopes.iter().cloned().collect::<Vec<_>>()).unwrap_or_default(),
            "max_grant_lifetime_ms": CONFIRMATION_GRANT_MAX_MS,
            "restrictions": {
                "risk": "low",
                "memory_only": true,
                "persistent_content": false,
                "runtime_launch": false,
                "delete": false,
            }
        })
    }

    pub(crate) fn reset(&self) -> bool {
        let Ok(mut state) = self.inner.lock() else {
            return false;
        };
        state.grant.take().is_some()
    }
}

pub(crate) fn validation_report_error(error: ValidationError) -> Value {
    let (code, message, retryable) = match error {
        ValidationError::NotFound => (
            "validation_report_not_found",
            "The bounded validation report was not found.",
            false,
        ),
        ValidationError::InvalidPage => (
            "invalid_validation_report_page",
            "The requested validation report page is invalid.",
            false,
        ),
        _ => (
            "validation_report_unavailable",
            "The validation report cannot be read safely.",
            true,
        ),
    };
    json!({"error": {"code": code, "message": message, "retryable": retryable}})
}

pub(crate) fn report_page_value(page: ReportPage) -> Value {
    json!({
        "schema_version": "validation-report-page/1.0",
        "report_id": page.report_id,
        "report_digest": page.report_digest,
        "page": page.page,
        "page_count": page.page_count,
        "content": page.content,
        "content_bytes": page.content_bytes,
        "limits_applied": {
            "page_bytes": godot_codex_transactions::MAX_REPORT_PAGE_BYTES,
            "pages": godot_codex_transactions::MAX_REPORT_PAGES,
            "retained_report_bytes": godot_codex_transactions::MAX_RETAINED_REPORT_BYTES,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_is_exact_bounded_and_revocable() {
        let store = ConfirmationPolicyStore::default();
        assert!(!store.grant_low_risk_for_session(
            "project:a",
            "editor:a",
            1_000,
            CONFIRMATION_GRANT_MAX_MS + 1,
        ));
        assert!(store.grant_low_risk_for_session("project:a", "editor:a", 1_000, 60_000,));
        assert!(store.allows_low_risk_memory_only(
            "project:a",
            "editor:a",
            LOW_RISK_MEMORY_ONLY_SCOPE,
            2_000,
        ));
        assert!(!store.allows_low_risk_memory_only(
            "project:a",
            "editor:b",
            LOW_RISK_MEMORY_ONLY_SCOPE,
            2_000,
        ));
        assert!(!store.allows_low_risk_memory_only(
            "project:a",
            "editor:a",
            "scene.property.set",
            2_000,
        ));
        assert!(store.reset());
        assert!(!store.allows_low_risk_memory_only(
            "project:a",
            "editor:a",
            LOW_RISK_MEMORY_ONLY_SCOPE,
            2_000,
        ));
    }

    #[test]
    fn grant_eligibility_is_a_closed_memory_only_low_risk_profile() {
        let eligible = json!({
            "schema_version": "canonical-change-set-preview/1.0",
            "risk": "low",
            "operations": [
                {"kind": "create_node"},
                {"kind": "attach_script"},
                {"kind": "connect_signal"},
            ],
            "save_scope": [],
            "validation_policy": {"runtime": "skip"},
        });
        assert!(session_grant_eligible(&eligible, Risk::Low));

        for mutation in [
            json!({"risk": "destructive"}),
            json!({"operations": [{"kind": "delete_node"}]}),
            json!({"operations": [{"kind": "set_property"}]}),
            json!({"operations": [{"kind": "create_resource"}]}),
            json!({"operations": []}),
            json!({"save_scope": ["res://main.tscn"]}),
            json!({"validation_policy": {"runtime": "run_current_scene"}}),
            json!({"schema_version": "canonical-change-set-preview/2.0"}),
        ] {
            let mut candidate = eligible.clone();
            for (key, value) in mutation.as_object().unwrap() {
                candidate
                    .as_object_mut()
                    .unwrap()
                    .insert(key.clone(), value.clone());
            }
            assert!(
                !session_grant_eligible(&candidate, Risk::Low),
                "unexpected eligible preview: {candidate}"
            );
        }
        assert!(!session_grant_eligible(&eligible, Risk::Write));
        assert!(!session_grant_eligible(&eligible, Risk::Destructive));
    }

    #[test]
    fn grant_expires_without_exposing_opaque_material() {
        let store = ConfirmationPolicyStore::default();
        assert!(store.grant_low_risk_for_session("project:a", "editor:a", 1_000, 1_000,));
        let snapshot = store.snapshot(Some("project:a"), Some("editor:a"), 2_000);
        assert_eq!(snapshot["mode"], "always_ask");
        assert_eq!(snapshot["grant_active"], false);
        assert!(snapshot.get("nonce").is_none());
        assert!(snapshot.get("receipt").is_none());
        assert!(snapshot.get("receipt_hash").is_none());
        assert!(snapshot.get("mac").is_none());
    }

    #[test]
    fn change_set_index_requirements_follow_persisted_domains() {
        let saved_scene: PrepareChangeSetInput = serde_json::from_value(json!({
            "project_id": format!("project:sha256:{}", "0".repeat(64)),
            "idempotency_key": format!("idempotency:{}", "1".repeat(32)),
            "coordinates": {
                "editor_session_id": format!("editor:{}", "2".repeat(32)),
                "scene_id": format!("scene:{}", "3".repeat(32)),
                "scene_revision": 1,
                "operation_seq": 1,
                "resource_revision": 1,
                "script_graph_revision": 1
            },
            "operations": [{
                "kind": "create_node",
                "parent_node_id": format!("node:{}", "4".repeat(32)),
                "godot_type": "Node2D",
                "name": "Saved"
            }],
            "save_scope": {"paths": ["res://main.tscn"]},
            "validation_policy": {
                "rollback": "on_required_failure",
                "warnings": "allow",
                "runtime": "skip"
            }
        }))
        .unwrap();

        assert_eq!(saved_scene.index_requirements(), (true, false, false));
    }
}
