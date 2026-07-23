use std::time::Duration;

use godot_codex_bridge_client::{
    RevisionCoordinates, TransactionOperation, TransactionResourceRef, WritableVariant,
};
use godot_codex_transactions::{
    ApprovalDecision, ApprovalPrompt, ApprovalProvider, BridgeFuture, PrepareCommand,
    TransactionError,
};
use rmcp::model::{
    BooleanSchema, ElicitRequestParams, ElicitationAction, ElicitationSchema,
    PrimitiveSchemaDefinition,
};
use rmcp::service::{ElicitationMode, RequestContext, ServiceError};
use rmcp::{RoleServer, model::CallToolResult};
use serde_json::{Value, json};

macro_rules! prepare_struct {
    ($name:ident { $($(#[$meta:meta])* $field:ident : $kind:ty),* $(,)? }) => {
        #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
        #[serde(deny_unknown_fields)]
        pub(crate) struct $name {
            #[schemars(regex(pattern = r"^project:sha256:[0-9a-f]{64}$"))]
            pub project_id: String,
            #[schemars(regex(pattern = r"^editor:[0-9a-f]{32}$"))]
            pub editor_session_id: String,
            #[schemars(regex(pattern = r"^scene:[0-9a-f]{32}$"))]
            pub scene_id: String,
            #[schemars(regex(pattern = r"^history:[0-9a-f]{32}$"))]
            pub history_id: String,
            #[schemars(range(max = 9_007_199_254_740_991_u64))]
            pub scene_revision: u64,
            #[schemars(range(max = 9_007_199_254_740_991_u64))]
            pub operation_seq: u64,
            #[schemars(regex(pattern = r"^idempotency:[0-9a-f]{32}$"))]
            pub idempotency_key: String,
            $($(#[$meta])* pub $field: $kind),*
        }

        impl $name {
            fn command(self, operation: TransactionOperation) -> PrepareCommand {
                PrepareCommand {
                    project_id: self.project_id,
                    editor_session_id: self.editor_session_id,
                    idempotency_key: self.idempotency_key,
                    coordinates: RevisionCoordinates {
                        scene_id: self.scene_id,
                        history_id: self.history_id,
                        scene_revision: self.scene_revision,
                        operation_seq: self.operation_seq,
                    },
                    operation,
                }
            }
        }
    };
}

prepare_struct!(PrepareCreateNodeInput {
    #[schemars(regex(pattern = r"^node:[0-9a-f]{32}$"))]
    parent_node_id: String,
    #[schemars(regex(pattern = r"^[A-Za-z_][A-Za-z0-9_]*$"), length(min = 1, max = 256))]
    godot_type: String,
    #[schemars(length(min = 1, max = 512))]
    name: String,
    insertion_index: Option<u32>,
});

impl PrepareCreateNodeInput {
    pub(crate) fn into_command(self) -> PrepareCommand {
        let operation = TransactionOperation::CreateNode {
            parent_node_id: self.parent_node_id.clone(),
            godot_type: self.godot_type.clone(),
            name: self.name.clone(),
            insertion_index: self.insertion_index,
        };
        self.command(operation)
    }
}

prepare_struct!(PrepareDeleteNodeInput {
    #[schemars(regex(pattern = r"^node:[0-9a-f]{32}$"))]
    node_id: String
});

impl PrepareDeleteNodeInput {
    pub(crate) fn into_command(self) -> PrepareCommand {
        let operation = TransactionOperation::DeleteNode {
            node_id: self.node_id.clone(),
        };
        self.command(operation)
    }
}

prepare_struct!(PrepareReparentNodeInput {
    #[schemars(regex(pattern = r"^node:[0-9a-f]{32}$"))]
    node_id: String,
    #[schemars(regex(pattern = r"^node:[0-9a-f]{32}$"))]
    new_parent_node_id: String,
    insertion_index: u32,
    keep_global_transform: bool,
});

impl PrepareReparentNodeInput {
    pub(crate) fn into_command(self) -> PrepareCommand {
        let operation = TransactionOperation::ReparentNode {
            node_id: self.node_id.clone(),
            new_parent_node_id: self.new_parent_node_id.clone(),
            insertion_index: self.insertion_index,
            keep_global_transform: self.keep_global_transform,
        };
        self.command(operation)
    }
}

prepare_struct!(PrepareSetPropertyInput {
    #[schemars(regex(pattern = r"^node:[0-9a-f]{32}$"))]
    node_id: String,
    #[schemars(length(min = 1, max = 512))]
    property: String,
    value: WritableVariant,
});

impl PrepareSetPropertyInput {
    pub(crate) fn into_command(self) -> PrepareCommand {
        let operation = TransactionOperation::SetProperty {
            node_id: self.node_id.clone(),
            property: self.property.clone(),
            value: self.value.clone(),
        };
        self.command(operation)
    }
}

prepare_struct!(PrepareAttachScriptInput {
    #[schemars(regex(pattern = r"^node:[0-9a-f]{32}$"))]
    node_id: String,
    script_ref: TransactionResourceRef,
});

impl PrepareAttachScriptInput {
    pub(crate) fn into_command(self) -> PrepareCommand {
        let operation = TransactionOperation::AttachScript {
            node_id: self.node_id.clone(),
            script_ref: self.script_ref.clone(),
        };
        self.command(operation)
    }
}

prepare_struct!(PrepareDetachScriptInput {
    #[schemars(regex(pattern = r"^node:[0-9a-f]{32}$"))]
    node_id: String
});

impl PrepareDetachScriptInput {
    pub(crate) fn into_command(self) -> PrepareCommand {
        let operation = TransactionOperation::DetachScript {
            node_id: self.node_id.clone(),
        };
        self.command(operation)
    }
}

prepare_struct!(PrepareConnectSignalInput {
    #[schemars(regex(pattern = r"^node:[0-9a-f]{32}$"))]
    emitter_node_id: String,
    #[schemars(length(min = 1, max = 512))]
    signal: String,
    #[schemars(regex(pattern = r"^node:[0-9a-f]{32}$"))]
    receiver_node_id: String,
    #[schemars(length(min = 1, max = 512))]
    method: String,
    flags: u8,
    #[schemars(range(max = 1_000_u16))]
    unbinds: u16,
    #[schemars(length(max = 1_000))]
    binds: Vec<WritableVariant>,
});

impl PrepareConnectSignalInput {
    pub(crate) fn into_command(self) -> PrepareCommand {
        let operation = TransactionOperation::ConnectSignal {
            emitter_node_id: self.emitter_node_id.clone(),
            signal: self.signal.clone(),
            receiver_node_id: self.receiver_node_id.clone(),
            method: self.method.clone(),
            flags: self.flags,
            unbinds: self.unbinds,
            binds: self.binds.clone(),
        };
        self.command(operation)
    }
}

prepare_struct!(PrepareDisconnectSignalInput {
    #[schemars(regex(pattern = r"^node:[0-9a-f]{32}$"))]
    emitter_node_id: String,
    #[schemars(length(min = 1, max = 512))]
    signal: String,
    #[schemars(regex(pattern = r"^node:[0-9a-f]{32}$"))]
    receiver_node_id: String,
    #[schemars(length(min = 1, max = 512))]
    method: String,
    flags: u8,
    #[schemars(range(max = 1_000_u16))]
    unbinds: u16,
    #[schemars(length(max = 1_000))]
    binds: Vec<WritableVariant>,
});

impl PrepareDisconnectSignalInput {
    pub(crate) fn into_command(self) -> PrepareCommand {
        let operation = TransactionOperation::DisconnectSignal {
            emitter_node_id: self.emitter_node_id.clone(),
            signal: self.signal.clone(),
            receiver_node_id: self.receiver_node_id.clone(),
            method: self.method.clone(),
            flags: self.flags,
            unbinds: self.unbinds,
            binds: self.binds.clone(),
        };
        self.command(operation)
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplyTransactionInput {
    #[schemars(regex(pattern = r"^transaction:[0-9a-f]{32}$"))]
    pub transaction_id: String,
    #[schemars(regex(pattern = r"^sha256:[0-9a-f]{64}$"))]
    pub preview_digest: String,
    #[schemars(range(max = 9_007_199_254_740_991_u64))]
    pub expected_scene_revision: u64,
    #[schemars(range(max = 9_007_199_254_740_991_u64))]
    pub expected_operation_seq: u64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransactionStatusInput {
    #[schemars(regex(pattern = r"^transaction:[0-9a-f]{32}$"))]
    pub transaction_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UndoTransactionInput {
    #[schemars(regex(pattern = r"^transaction:[0-9a-f]{32}$"))]
    pub transaction_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991_u64))]
    pub expected_transaction_seq: u64,
    #[schemars(range(max = 9_007_199_254_740_991_u64))]
    pub expected_scene_revision: u64,
    #[schemars(range(max = 9_007_199_254_740_991_u64))]
    pub expected_operation_seq: u64,
}

pub(crate) struct McpApprovalProvider {
    context: RequestContext<RoleServer>,
}

impl McpApprovalProvider {
    pub(crate) fn new(context: RequestContext<RoleServer>) -> Self {
        Self { context }
    }

    fn elicitation(prompt: &ApprovalPrompt) -> ElicitRequestParams {
        let schema = ElicitationSchema::builder()
            .required_property(
                "confirm",
                PrimitiveSchemaDefinition::Boolean(
                    BooleanSchema::new()
                        .description("Confirm this exact Godot editor transaction preview."),
                ),
            )
            .build()
            .expect("fixed transaction approval schema");
        let scope = serde_json::to_value(prompt.scope)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned());
        let risk = serde_json::to_value(prompt.risk)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned());
        let fixed = format!(
            "Approve this exact Godot editor transaction.\nTransaction: {}\nDigest: {}\nScope: {scope}\nRisk: {risk}\nPreview JSON: ",
            prompt.transaction_id, prompt.preview_digest
        );
        let remaining = 8_192_usize.saturating_sub(fixed.len());
        let preview = serde_json::to_string(&prompt.preview).unwrap_or_else(|_| "{}".to_owned());
        let preview = if preview.len() <= remaining {
            preview
        } else {
            const SUFFIX: &str = "\n[preview truncated to MCP approval limit]";
            let body = truncate_utf8(&preview, remaining.saturating_sub(SUFFIX.len()));
            format!("{body}{SUFFIX}")
        };
        ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: format!("{fixed}{preview}"),
            requested_schema: schema,
        }
    }
}

impl ApprovalProvider for McpApprovalProvider {
    fn request<'a>(&'a self, prompt: ApprovalPrompt) -> BridgeFuture<'a, ApprovalDecision> {
        Box::pin(async move {
            if !self
                .context
                .peer
                .supported_elicitation_modes()
                .contains(&ElicitationMode::Form)
            {
                return ApprovalDecision::Unsupported;
            }
            match self
                .context
                .peer
                .create_elicitation_with_timeout(
                    Self::elicitation(&prompt),
                    Some(Duration::from_secs(120)),
                )
                .await
            {
                Ok(result) => {
                    let exact_confirmation = result.content.as_ref().is_some_and(|value| {
                        value.as_object().is_some_and(|object| {
                            object.len() == 1
                                && object.get("confirm").and_then(Value::as_bool) == Some(true)
                        })
                    });
                    match result.action {
                        ElicitationAction::Accept if exact_confirmation => ApprovalDecision::Accept,
                        ElicitationAction::Accept => ApprovalDecision::Invalid,
                        ElicitationAction::Decline => ApprovalDecision::Decline,
                        ElicitationAction::Cancel => ApprovalDecision::Cancel,
                        _ => ApprovalDecision::Invalid,
                    }
                }
                Err(ServiceError::Timeout { .. }) => ApprovalDecision::Timeout,
                Err(_) => ApprovalDecision::Unsupported,
            }
        })
    }
}

fn truncate_utf8(value: &str, maximum_bytes: usize) -> &str {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut end = maximum_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(crate) fn transaction_error(error: TransactionError) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "error": {
            "code": error.code,
            "message": error.message,
            "retryable": error.retryable,
            "transaction_id": error.transaction_id,
        }
    }))
}

pub(crate) fn transaction_result(value: impl serde::Serialize) -> CallToolResult {
    let Ok(value) = serde_json::to_value(value) else {
        return CallToolResult::structured_error(json!({
            "error": {
                "code": "transaction_too_large",
                "message": "The transaction result could not be serialized safely.",
                "retryable": false
            }
        }));
    };
    if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > 65_536) {
        return CallToolResult::structured_error(json!({
            "error": {
                "code": "transaction_too_large",
                "message": "The transaction result exceeds the 64 KiB MCP limit.",
                "retryable": false
            }
        }));
    }
    CallToolResult::structured(value)
}

pub(crate) fn coordinator_unavailable() -> CallToolResult {
    CallToolResult::structured_error(json!({
        "error": {
            "code": "transaction_coordinator_unavailable",
            "message": "The project-scoped transaction coordinator is unavailable.",
            "retryable": true
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use godot_codex_bridge_client::{ApprovalScope, OperationKind, Risk, TransactionPreview};

    #[test]
    fn transaction_results_fail_closed_above_sixty_four_kibibytes() {
        let result = transaction_result("x".repeat(65_537));
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.unwrap()["error"]["code"],
            "transaction_too_large"
        );
    }

    #[test]
    fn approval_message_truncation_preserves_utf8_and_byte_limit() {
        let prompt = ApprovalPrompt {
            transaction_id: format!("transaction:{}", "1".repeat(32)),
            preview_digest: format!("sha256:{}", "2".repeat(64)),
            scope: ApprovalScope::ScenePropertySet,
            risk: Risk::Write,
            preview: TransactionPreview {
                operation_kind: OperationKind::SetProperty,
                summary: "🔥".repeat(8_192),
                dirty_effect: godot_codex_bridge_client::DirtyEffect::MarksSceneDirty,
                save_effect: godot_codex_bridge_client::SaveEffect::NotSaved,
                preconditions: Vec::new(),
                truncated: true,
            },
            expires_at_ms: 301_000,
        };
        let request = McpApprovalProvider::elicitation(&prompt);
        let serialized = serde_json::to_value(request).unwrap();
        assert!(serialized["message"].as_str().unwrap().len() <= 8_192);
    }
}
