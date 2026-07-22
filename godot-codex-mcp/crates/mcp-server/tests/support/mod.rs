use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;

use rmcp::model::{
    BooleanSchema, CallToolRequestParams, CallToolResult, ElicitRequestParams, ElicitationAction,
    ElicitationSchema, ListToolsResult, PaginatedRequestParams, PrimitiveSchemaDefinition,
    ProtocolVersion, ServerCapabilities, ServerInfo, Tool, ToolAnnotations,
};
use rmcp::service::{ElicitationMode, RequestContext, ServiceError};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::{Value, json};

pub const APPROVAL_PROBE_TOOL: &str = "godot_s9_approval_probe";

#[derive(Clone, Debug)]
pub struct ApprovalProbeServer {
    output: Option<PathBuf>,
    timeout: Duration,
}

impl ApprovalProbeServer {
    pub fn new(timeout: Duration, output: Option<PathBuf>) -> Self {
        Self { output, timeout }
    }

    fn tool() -> Tool {
        let input_schema = json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
        .as_object()
        .expect("object schema")
        .clone();
        Tool::new(
            APPROVAL_PROBE_TOOL,
            "Test-only Sprint 9 probe that requests explicit form elicitation and never mutates a Godot project",
            input_schema,
        )
        .with_title("Godot Sprint 9 approval probe")
        .with_annotations(
            ToolAnnotations::with_title("Godot Sprint 9 approval probe")
                .read_only(false)
                .destructive(true)
                .idempotent(false)
                .open_world(false),
        )
    }

    fn elicitation() -> ElicitRequestParams {
        let schema = ElicitationSchema::builder()
            .required_property(
                "confirm",
                PrimitiveSchemaDefinition::Boolean(BooleanSchema::new().description(
                    "Confirm this test-only approval probe. No Godot project data will be changed.",
                )),
            )
            .build()
            .expect("fixed approval schema");
        ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: "Approve the test-only Godot Sprint 9 transaction boundary probe. Scope: scene.node.create. Risk: destructive-tool simulation. This probe does not apply a transaction or modify project content.".to_owned(),
            requested_schema: schema,
        }
    }

    fn unsupported_host(&self, context: &RequestContext<RoleServer>) -> Value {
        let peer = context.peer.peer_info();
        json!({
            "schema_version": "approval-probe/1.0",
            "status": "error",
            "code": "approval_host_unsupported",
            "host": peer.as_ref().map(|info| json!({
                "name": info.client_info.name,
                "version": info.client_info.version,
                "protocol_version": info.protocol_version.to_string(),
                "form_elicitation": false
            })),
            "receipt_eligible": false,
            "project_mutated": false
        })
    }

    fn outcome(
        &self,
        context: &RequestContext<RoleServer>,
        action: &str,
        confirmed: Option<bool>,
        code: &str,
    ) -> Value {
        let peer = context.peer.peer_info();
        json!({
            "schema_version": "approval-probe/1.0",
            "status": if code == "approval_accepted" { "ok" } else { "error" },
            "code": code,
            "host": peer.as_ref().map(|info| json!({
                "name": info.client_info.name,
                "version": info.client_info.version,
                "protocol_version": info.protocol_version.to_string(),
                "form_elicitation": true
            })),
            "action": action,
            "confirmed": confirmed,
            "receipt_eligible": code == "approval_accepted",
            "project_mutated": false
        })
    }

    fn record(&self, outcome: &Value) -> Result<(), std::io::Error> {
        let Some(path) = &self.output else {
            return Ok(());
        };
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(path)?;
        serde_json::to_writer(&mut file, outcome)?;
        file.write_all(b"\n")?;
        file.sync_all()
    }

    fn result(&self, outcome: Value) -> CallToolResult {
        if self.record(&outcome).is_err() {
            return CallToolResult::structured_error(json!({
                "schema_version": "approval-probe/1.0",
                "status": "error",
                "code": "probe_output_failed",
                "receipt_eligible": false,
                "project_mutated": false
            }));
        }
        if outcome["status"] == "ok" {
            CallToolResult::structured(outcome)
        } else {
            CallToolResult::structured_error(outcome)
        }
    }
}

impl ServerHandler for ApprovalProbeServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2025_11_25)
            .with_instructions(
                "Test-only S9 approval boundary probe. It never connects to Godot or mutates project content.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(vec![Self::tool()]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if request.name.as_ref() != APPROVAL_PROBE_TOOL {
            return Err(McpError::invalid_params(
                "unknown approval probe tool",
                None,
            ));
        }
        if request
            .arguments
            .as_ref()
            .is_some_and(|arguments| !arguments.is_empty())
        {
            return Err(McpError::invalid_params(
                "approval probe accepts no arguments",
                None,
            ));
        }
        if !context
            .peer
            .supported_elicitation_modes()
            .contains(&ElicitationMode::Form)
        {
            let outcome = self.unsupported_host(&context);
            return Ok(self.result(outcome));
        }

        let response = context
            .peer
            .create_elicitation_with_timeout(Self::elicitation(), Some(self.timeout))
            .await;
        let outcome = match response {
            Ok(result) => {
                let content = result.content.as_ref();
                let confirmed = content
                    .and_then(|value| value.get("confirm"))
                    .and_then(Value::as_bool);
                let exact_confirmation = content.is_some_and(|value| {
                    value.as_object().is_some_and(|object| {
                        object.len() == 1
                            && object.get("confirm").and_then(Value::as_bool) == Some(true)
                    })
                });
                match result.action {
                    ElicitationAction::Accept if exact_confirmation => {
                        self.outcome(&context, "accept", confirmed, "approval_accepted")
                    }
                    ElicitationAction::Accept => {
                        self.outcome(&context, "accept", confirmed, "approval_invalid")
                    }
                    ElicitationAction::Decline => {
                        self.outcome(&context, "decline", None, "approval_declined")
                    }
                    ElicitationAction::Cancel => {
                        self.outcome(&context, "cancel", None, "approval_cancelled")
                    }
                    _ => self.outcome(&context, "unknown", None, "approval_invalid"),
                }
            }
            Err(ServiceError::Timeout { .. }) => {
                self.outcome(&context, "timeout", None, "approval_timeout")
            }
            Err(_) => self.outcome(&context, "unavailable", None, "approval_host_unsupported"),
        };
        Ok(self.result(outcome))
    }
}

pub fn approval_schema_json() -> Value {
    serde_json::to_value(ApprovalProbeServer::elicitation()).expect("serialize fixed schema")
}

pub fn approval_tool() -> Tool {
    ApprovalProbeServer::tool()
}
