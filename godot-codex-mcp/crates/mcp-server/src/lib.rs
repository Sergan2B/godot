use godot_codex_semantic_model::SnapshotReplicator;
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ProtocolVersion, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use serde_json::{Value, json};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Clone, Copy)]
enum Query {
    EditorState,
    CurrentScene,
    SelectedNodes,
}

#[derive(Clone)]
pub struct GodotMcpServer {
    #[allow(dead_code, reason = "tool_handler macro accesses this router field")]
    tool_router: ToolRouter<Self>,
    replicator: SnapshotReplicator,
}

impl std::fmt::Debug for GodotMcpServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GodotMcpServer")
            .field("replica_status", &self.replicator.status())
            .finish_non_exhaustive()
    }
}

impl GodotMcpServer {
    pub fn new(replicator: SnapshotReplicator) -> Self {
        Self {
            tool_router: Self::tool_router(),
            replicator,
        }
    }

    fn query(&self, query: Query) -> CallToolResult {
        match self.replicator.read() {
            Ok(snapshot) => {
                let value = match query {
                    Query::EditorState => snapshot.editor_state_result(),
                    Query::CurrentScene => snapshot.current_scene_result(),
                    Query::SelectedNodes => snapshot.selected_nodes_result(),
                };
                CallToolResult::structured(value)
            }
            Err(error) => CallToolResult::structured_error(json!({
                "error": {
                    "code": "editor_state_unavailable",
                    "message": error.message,
                    "retryable": true,
                    "replica_status": error.status,
                }
            })),
        }
    }
}

#[tool_router]
impl GodotMcpServer {
    #[tool(
        description = "Return the live Godot editor state for this exact project, including revision and freshness metadata",
        annotations(
            title = "Godot editor state",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_get_editor_state(
        &self,
        Parameters(EmptyInput {}): Parameters<EmptyInput>,
    ) -> CallToolResult {
        self.query(Query::EditorState)
    }

    #[tool(
        description = "Return the current Godot scene and its bounded node projection for this exact project",
        annotations(
            title = "Godot current scene",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_get_current_scene(
        &self,
        Parameters(EmptyInput {}): Parameters<EmptyInput>,
    ) -> CallToolResult {
        self.query(Query::CurrentScene)
    }

    #[tool(
        description = "Return the selected Godot nodes and their bounded live inspector properties for this exact project",
        annotations(
            title = "Godot selected nodes",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_get_selected_nodes(
        &self,
        Parameters(EmptyInput {}): Parameters<EmptyInput>,
    ) -> CallToolResult {
        self.query(Query::SelectedNodes)
    }
}

#[tool_handler]
impl ServerHandler for GodotMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2025_11_25)
            .with_instructions(
                "Read-only, project-scoped Godot editor context. Results are returned only from a checksum-verified current snapshot.",
            )
    }
}

pub fn structured_content(result: &CallToolResult) -> Option<&Value> {
    result.structured_content.as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use godot_codex_semantic_model::{SnapshotChunk, SnapshotEnd, SnapshotMetadata};

    fn canonical_ready_replica() -> SnapshotReplicator {
        let response: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/rpc-snapshot-response.json"
        ))
        .unwrap();
        let begin: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/snapshot-begin.json"
        ))
        .unwrap();
        let chunk: SnapshotChunk = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/snapshot-chunk.json"
        ))
        .unwrap();
        let end_message: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/snapshot-end.json"
        ))
        .unwrap();
        let result = &response["result"];
        let metadata = SnapshotMetadata {
            snapshot_id: result["snapshot_id"].as_str().unwrap().to_owned(),
            project_id: response["context"]["project_id"]
                .as_str()
                .unwrap()
                .to_owned(),
            editor_session_id: response["context"]["editor_session_id"]
                .as_str()
                .unwrap()
                .to_owned(),
            base_event_seq: result["base_event_seq"].as_u64().unwrap(),
            revisions: serde_json::from_value(result["revisions"].clone()).unwrap(),
            chunk_count: usize::try_from(begin["params"]["chunk_count"].as_u64().unwrap()).unwrap(),
            limits_applied: result["limits_applied"].clone(),
        };
        let end = SnapshotEnd {
            snapshot_id: end_message["params"]["snapshot_id"]
                .as_str()
                .unwrap()
                .to_owned(),
            chunk_count: usize::try_from(end_message["params"]["chunk_count"].as_u64().unwrap())
                .unwrap(),
            entity_count: usize::try_from(end_message["params"]["entity_count"].as_u64().unwrap())
                .unwrap(),
            checksum: end_message["params"]["checksum"]
                .as_str()
                .unwrap()
                .to_owned(),
            revisions: serde_json::from_value(end_message["params"]["revisions"].clone()).unwrap(),
        };
        let replicator = SnapshotReplicator::new();
        replicator.begin(metadata).unwrap();
        replicator.push_chunk(chunk).unwrap();
        replicator.end(end).unwrap();
        replicator
    }

    #[test]
    fn unavailable_replica_is_a_retryable_tool_error() {
        let server = GodotMcpServer::new(SnapshotReplicator::new());
        let result = server.godot_get_editor_state(Parameters(EmptyInput {}));
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            structured_content(&result)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("editor_state_unavailable")
        );
    }

    #[test]
    fn server_pins_the_2025_11_25_protocol() {
        let server = GodotMcpServer::new(SnapshotReplicator::new());
        assert_eq!(
            server.get_info().protocol_version,
            ProtocolVersion::V_2025_11_25
        );
    }

    #[test]
    fn exactly_three_tools_are_declared_read_only() {
        let server = GodotMcpServer::new(SnapshotReplicator::new());
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 3);
        for tool in tools {
            let annotations = tool.annotations.as_ref().expect("annotations");
            assert_eq!(annotations.read_only_hint, Some(true));
            assert_eq!(annotations.destructive_hint, Some(false));
            assert_eq!(annotations.open_world_hint, Some(false));
            assert_eq!(
                tool.input_schema.get("type").and_then(Value::as_str),
                Some("object")
            );
            assert_eq!(
                tool.input_schema
                    .get("additionalProperties")
                    .and_then(Value::as_bool),
                Some(false)
            );
        }
    }

    #[test]
    fn canonical_snapshot_reaches_the_selected_nodes_tool_without_a_model() {
        let server = GodotMcpServer::new(canonical_ready_replica());
        let result = server.godot_get_selected_nodes(Parameters(EmptyInput {}));
        assert_ne!(result.is_error, Some(true));
        let content = structured_content(&result).expect("structured tool result");
        assert_eq!(content.pointer("/scene_dirty"), Some(&Value::Bool(true)));
        assert_eq!(
            content
                .pointer("/limits_applied/total_inspector_bytes")
                .and_then(Value::as_u64),
            Some(4_194_304)
        );
        assert_eq!(
            content
                .pointer("/selected_nodes/0/node_path")
                .and_then(Value::as_str),
            Some("Player")
        );
        assert_eq!(
            content
                .pointer("/selected_nodes/0/script_path")
                .and_then(Value::as_str),
            Some("res://player.gd")
        );
        assert_eq!(
            content
                .pointer("/selected_nodes/0/properties/0/value")
                .and_then(Value::as_f64),
            Some(275.0)
        );
        assert_eq!(
            content
                .pointer("/selected_nodes/0/properties/0/source")
                .and_then(Value::as_str),
            Some("live_editor_property")
        );
    }
}
