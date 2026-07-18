mod cursor;

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use cursor::{CursorBinding, CursorCodec, ResourceTool};
use godot_codex_index_store::{
    DependencyEdge, IndexRead, IndexReadSnapshot, ResourceEntity, ResourceQuery, ResourceSelector,
    StoreError,
};
use godot_codex_resource_indexer::{
    ResourceIndexReadError, ResourceIndexReader, normalize_resource_path,
};
use godot_codex_semantic_model::SnapshotReplicator;
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ProtocolVersion, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use serde_json::{Value, json};

const DEFAULT_RESOURCE_LIMIT: usize = 50;
const MAX_RESOURCE_LIMIT: usize = 200;
const MAX_TOTAL_RESULTS: usize = 250_000;

fn default_resource_limit() -> usize {
    DEFAULT_RESOURCE_LIMIT
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResourceInput {
    resource: String,
    #[serde(default = "default_resource_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

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
    resource_index: ResourceIndexReader,
    cursor_codec: CursorCodec,
}

impl std::fmt::Debug for GodotMcpServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GodotMcpServer")
            .field("replica_status", &self.replicator.status())
            .field("resource_index_status", &self.resource_index.status())
            .finish_non_exhaustive()
    }
}

impl GodotMcpServer {
    pub fn new(replicator: SnapshotReplicator) -> Self {
        Self::with_resource_index(replicator, ResourceIndexReader::new())
    }

    pub fn with_resource_index(
        replicator: SnapshotReplicator,
        resource_index: ResourceIndexReader,
    ) -> Self {
        Self {
            tool_router: Self::tool_router(),
            replicator,
            resource_index,
            cursor_codec: CursorCodec::new(),
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

    fn resource_query(&self, input: ResourceInput, tool: ResourceTool) -> CallToolResult {
        if !(1..=MAX_RESOURCE_LIMIT).contains(&input.limit) {
            return structured_error("invalid_limit", "limit must be between 1 and 200", false);
        }
        let (selector, canonical_selector, display_selector) = match parse_selector(&input.resource)
        {
            Ok(selector) => selector,
            Err((code, message)) => return structured_error(code, message, false),
        };
        let snapshot = match self.resource_index.pin_current() {
            Ok(snapshot) => snapshot,
            Err(error) => return resource_index_error(error),
        };
        let generation = snapshot.generation();
        let binding = CursorBinding {
            project_id: &generation.project_id,
            tool,
            selector: &canonical_selector,
            limit: input.limit,
            generation_id: &generation.generation_id,
            index_revision: generation.index_revision,
        };
        let now = unix_seconds();
        let offset = match input.cursor.as_deref() {
            Some(cursor) => match self.cursor_codec.validate(cursor, &binding, now) {
                Ok(offset) => offset,
                Err(()) => {
                    return structured_error(
                        "stale_cursor",
                        "cursor is invalid, expired, or belongs to another index generation",
                        false,
                    );
                }
            },
            None => 0,
        };
        if offset >= MAX_TOTAL_RESULTS {
            return structured_error(
                "result_limit_exceeded",
                "resource query exceeded the bounded result window",
                false,
            );
        }
        let query = ResourceQuery {
            selector,
            limit: input.limit,
            offset,
        };
        let result = match tool {
            ResourceTool::Dependencies => snapshot.direct_dependencies(&query),
            ResourceTool::Owners => snapshot.reverse_owners(&query),
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => return store_error(error),
        };
        let next_offset = match offset.checked_add(result.edges.len()) {
            Some(offset) => offset,
            None => {
                return structured_error(
                    "result_limit_exceeded",
                    "resource query exceeded the bounded result window",
                    false,
                );
            }
        };
        let next_cursor = if result.has_more {
            if next_offset >= MAX_TOTAL_RESULTS {
                return structured_error(
                    "result_limit_exceeded",
                    "resource query exceeded the bounded result window",
                    false,
                );
            }
            match self.cursor_codec.issue(&binding, next_offset, now) {
                Ok(cursor) => Some(cursor),
                Err(()) => {
                    return structured_error(
                        "index_not_current",
                        "the current index page could not be pinned",
                        true,
                    );
                }
            }
        } else {
            None
        };
        resource_success(
            &snapshot,
            tool,
            &display_selector,
            input.limit,
            offset,
            result,
            next_cursor,
        )
    }
}

fn parse_selector(
    value: &str,
) -> Result<(ResourceSelector, String, String), (&'static str, &'static str)> {
    if value.starts_with("uid://") {
        if value.len() <= 128
            && value.len() > 6
            && value
                .bytes()
                .skip(6)
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Ok((
                ResourceSelector::Uid(value.to_owned()),
                format!("uid:{value}"),
                value.to_owned(),
            ));
        }
        return Err(("invalid_query", "resource UID is invalid"));
    }
    if value.starts_with("res://") {
        let path = normalize_resource_path(value)
            .map_err(|_| ("invalid_path", "resource path is invalid or unsafe"))?;
        return Ok((
            ResourceSelector::ComparisonPath(path.comparison.clone()),
            format!("path:{}", path.comparison),
            path.display,
        ));
    }
    Err((
        "invalid_query",
        "resource must be a canonical uid:// or res:// selector",
    ))
}

fn resource_success(
    snapshot: &IndexReadSnapshot,
    tool: ResourceTool,
    selector: &str,
    limit: usize,
    offset: usize,
    result: godot_codex_index_store::ResourceQueryResult,
    next_cursor: Option<String>,
) -> CallToolResult {
    let generation = snapshot.generation();
    let subjects: BTreeSet<_> = std::iter::once(result.resource.entity_id.as_str())
        .chain(
            result
                .edges
                .iter()
                .map(|edge| edge.source_entity_id.as_str()),
        )
        .collect();
    let diagnostics: Vec<_> = generation
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.active && subjects.contains(diagnostic.subject.as_str()))
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code,
                "subject": diagnostic.subject,
                "detail": diagnostic.detail,
                "first_index_revision": diagnostic.first_index_revision,
                "last_index_revision": diagnostic.last_index_revision,
            })
        })
        .collect();
    let related: Vec<_> = result
        .edges
        .iter()
        .map(|edge| match tool {
            ResourceTool::Dependencies => dependency_view(snapshot, edge),
            ResourceTool::Owners => owner_view(snapshot, edge),
        })
        .collect();
    let mut response = json!({
        "project_id": generation.project_id,
        "schema_version": {
            "major": generation.schema_version.major,
            "minor": generation.schema_version.minor,
        },
        "generation_id": result.generation_id,
        "index_revision": result.index_revision,
        "validated_checkpoint": generation.checkpoint,
        "query": {
            "resource": selector,
            "limit": limit,
            "offset": offset,
        },
        "resource": resource_view(&result.resource),
        "status": if result.exact { "exact" } else { "partial" },
        "freshness": "current",
        "diagnostics": diagnostics,
        "truncated": result.has_more,
        "next_cursor": next_cursor,
        "evidence": {
            "source": "persistent_segment_index",
            "source_complete": generation.checkpoint.source_complete,
            "authorities": ["editor_file_system", "godot_resource_loader"],
        },
    });
    response[match tool {
        ResourceTool::Dependencies => "dependencies",
        ResourceTool::Owners => "owners",
    }] = Value::Array(related);
    CallToolResult::structured(response)
}

fn resource_view(resource: &ResourceEntity) -> Value {
    json!({
        "entity_id": resource.entity_id,
        "uid": resource.uid,
        "path": resource.display_path,
        "type": resource.resource_type,
        "source_kind": resource.source_kind,
        "import_state": resource.import_state,
        "authority": resource.authority,
        "content_generation": resource.content_generation,
        "validity": resource.validity,
        "resource_revision": resource.resource_revision,
    })
}

fn dependency_view(snapshot: &IndexReadSnapshot, edge: &DependencyEdge) -> Value {
    let target = edge
        .target_entity_id
        .as_ref()
        .and_then(|entity_id| {
            snapshot
                .resource(&ResourceSelector::EntityId(entity_id.clone()))
                .ok()
        })
        .map(|resource| resource_view(&resource));
    json!({
        "edge_id": edge.edge_id,
        "relation": edge.relation,
        "target": target,
        "target_uid": edge.target_uid,
        "target_path": edge.target_display_path,
        "resolved_target_path": edge.resolved_target_path,
        "declared_type": edge.declared_type,
        "resolution": edge.resolution,
        "authority": edge.authority,
        "resource_revision": edge.resource_revision,
    })
}

fn owner_view(snapshot: &IndexReadSnapshot, edge: &DependencyEdge) -> Value {
    let owner = snapshot
        .resource(&ResourceSelector::EntityId(edge.source_entity_id.clone()))
        .ok()
        .map(|resource| resource_view(&resource));
    json!({
        "edge_id": edge.edge_id,
        "relation": edge.relation,
        "owner": owner,
        "declared_type": edge.declared_type,
        "resolution": edge.resolution,
        "authority": edge.authority,
        "resource_revision": edge.resource_revision,
    })
}

fn resource_index_error(error: ResourceIndexReadError) -> CallToolResult {
    match error {
        ResourceIndexReadError::ProjectNotBound => {
            structured_error("project_not_bound", "Godot project is not bound", true)
        }
        ResourceIndexReadError::NotReady => structured_error(
            "index_not_ready",
            "resource index has no committed generation",
            true,
        ),
        ResourceIndexReadError::NotCurrent => structured_error(
            "index_not_current",
            "resource index freshness is not confirmed",
            true,
        ),
        ResourceIndexReadError::CapabilityUnavailable => structured_error(
            "capability_unavailable",
            "Bridge resource graph capability is unavailable",
            true,
        ),
    }
}

fn store_error(error: StoreError) -> CallToolResult {
    match error {
        StoreError::ResourceNotFound => structured_error(
            "resource_not_found",
            "resource selector was not found in the current index",
            false,
        ),
        StoreError::NotReady => structured_error(
            "index_not_ready",
            "resource index has no committed generation",
            true,
        ),
        _ => structured_error(
            "index_not_current",
            "resource index could not provide a current immutable page",
            true,
        ),
    }
}

fn structured_error(code: &str, message: &str, retryable: bool) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "error": {
            "code": code,
            "message": message,
            "retryable": retryable,
        }
    }))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
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

    #[tool(
        description = "Return direct indexed dependencies for one uid:// or res:// Godot resource",
        annotations(
            title = "Godot resource dependencies",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_get_resource_dependencies(
        &self,
        Parameters(input): Parameters<ResourceInput>,
    ) -> CallToolResult {
        self.resource_query(input, ResourceTool::Dependencies)
    }

    #[tool(
        description = "Return direct indexed owners that reference one uid:// or res:// Godot resource",
        annotations(
            title = "Godot resource owners",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_find_resource_owners(
        &self,
        Parameters(input): Parameters<ResourceInput>,
    ) -> CallToolResult {
        self.resource_query(input, ResourceTool::Owners)
    }
}

#[tool_handler]
impl ServerHandler for GodotMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2025_11_25)
            .with_instructions(
                "Read-only, project-scoped Godot editor and resource context. Results are returned only from checksum-verified current snapshots and immutable index generations.",
            )
    }
}

pub fn structured_content(result: &CallToolResult) -> Option<&Value> {
    result.structured_content.as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use godot_codex_index_store::{
        DependencyEdge, DependencyResolution, Diagnostic, GenerationState, IdentityStrength,
        IndexGeneration, IngestionCheckpoint, LOGICAL_SCHEMA_V1, RecordValidity, ResourceEntity,
        SegmentStore, SourceDocument,
    };
    use godot_codex_resource_indexer::ResourceIndexReader;
    use godot_codex_semantic_model::{SnapshotChunk, SnapshotEnd, SnapshotMetadata};
    use tempfile::TempDir;

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

    fn indexed_resource_server() -> GodotMcpServer {
        indexed_resource_server_fixture(false)
    }

    fn indexed_resource_server_fixture(include_missing: bool) -> GodotMcpServer {
        let resource = |suffix: &str| ResourceEntity {
            entity_id: format!("entity-{suffix}"),
            identity_input: format!("uid://{suffix}"),
            uid: Some(format!("uid://{suffix}")),
            display_path: format!("res://{suffix}.tres"),
            comparison_path: format!("res://{suffix}.tres"),
            identity_strength: IdentityStrength::ResourceUid,
            resource_type: "Resource".to_owned(),
            source_kind: "source".to_owned(),
            import_state: "not_imported".to_owned(),
            authority: "editor_file_system".to_owned(),
            content_generation: Some(format!("sha256:{}", suffix.repeat(64))),
            mtime_ns: 1,
            byte_size: 1,
            validity: RecordValidity::Valid,
            resource_revision: 1,
        };
        let resources = vec![resource("a"), resource("b"), resource("c")];
        let source_documents = resources
            .iter()
            .map(|resource| SourceDocument {
                entity_id: resource.entity_id.clone(),
                comparison_path: resource.comparison_path.clone(),
                size_before: 1,
                size_after: 1,
                mtime_before_ns: 1,
                mtime_after_ns: 1,
                content_generation: resource.content_generation.clone(),
                ingest_state: "ready".to_owned(),
            })
            .collect();
        let edge = |target: &str| DependencyEdge {
            edge_id: format!("edge-a-{target}"),
            source_entity_id: "entity-a".to_owned(),
            target_uid: Some(format!("uid://{target}")),
            target_comparison_path: Some(format!("res://{target}.tres")),
            target_display_path: Some(format!("res://{target}.tres")),
            target_entity_id: Some(format!("entity-{target}")),
            resolved_target_path: Some(format!("res://{target}.tres")),
            relation: "references".to_owned(),
            declared_type: Some("Resource".to_owned()),
            authority: "godot_resource_loader".to_owned(),
            resolution: DependencyResolution::Resolved,
            resource_revision: 1,
        };
        let mut dependencies = vec![edge("b"), edge("c")];
        let mut diagnostics = Vec::new();
        if include_missing {
            dependencies.push(DependencyEdge {
                edge_id: "edge-a-missing".to_owned(),
                source_entity_id: "entity-a".to_owned(),
                target_uid: None,
                target_comparison_path: Some("res://missing.tres".to_owned()),
                target_display_path: Some("res://missing.tres".to_owned()),
                target_entity_id: None,
                resolved_target_path: None,
                relation: "references".to_owned(),
                declared_type: Some("Resource".to_owned()),
                authority: "godot_resource_loader".to_owned(),
                resolution: DependencyResolution::Missing,
                resource_revision: 1,
            });
            diagnostics.push(Diagnostic {
                diagnostic_id: "diagnostic-missing".to_owned(),
                code: "missing_dependency".to_owned(),
                subject: "entity-a".to_owned(),
                detail: Some("res://missing.tres".to_owned()),
                first_index_revision: 1,
                last_index_revision: 1,
                active: true,
            });
        }
        let mut generation = IndexGeneration {
            generation_id: "generation:test".to_owned(),
            parent_generation_id: None,
            schema_version: LOGICAL_SCHEMA_V1,
            project_id: "project:test".to_owned(),
            index_revision: 1,
            state: GenerationState::Active,
            creation_reason: "full_snapshot".to_owned(),
            checkpoint: IngestionCheckpoint {
                editor_session_id: "editor:0123456789abcdef0123456789abcdef".to_owned(),
                resource_revision: 1,
                project_revision: 1,
                index_revision: 1,
                source_complete: true,
                snapshot_checksum: format!("sha256:{}", "d".repeat(64)),
                last_batch_id: None,
                last_batch_checksum: None,
            },
            resources,
            source_documents,
            dependencies,
            diagnostics,
            tombstones: Vec::new(),
            scene: godot_codex_index_store::SceneDomainGeneration::default(),
            validation_digest: String::new(),
        };
        generation.canonicalize();
        generation.validation_digest = generation.compute_validation_digest();
        let temp = TempDir::new().unwrap();
        let mut store = SegmentStore::open(temp.path(), "project:test").unwrap();
        store.activate(&generation, None).unwrap();
        let reader = ResourceIndexReader::from_validated_store(
            &store,
            "editor:0123456789abcdef0123456789abcdef",
            1,
        )
        .unwrap();
        GodotMcpServer::with_resource_index(SnapshotReplicator::new(), reader)
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
    fn exactly_five_tools_are_declared_read_only() {
        let server = GodotMcpServer::new(SnapshotReplicator::new());
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 5);
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
    fn resource_tools_paginate_one_pinned_generation_without_duplicates() {
        let server = indexed_resource_server();
        let first = server.godot_get_resource_dependencies(Parameters(ResourceInput {
            resource: "uid://a".to_owned(),
            limit: 1,
            cursor: None,
        }));
        let first = structured_content(&first).unwrap();
        assert_eq!(first["generation_id"], "generation:test");
        assert_eq!(first["freshness"], "current");
        assert_eq!(first["truncated"], true);
        assert_eq!(first["dependencies"][0]["target"]["uid"], "uid://b");
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();

        let second = server.godot_get_resource_dependencies(Parameters(ResourceInput {
            resource: "uid://a".to_owned(),
            limit: 1,
            cursor: Some(cursor.clone()),
        }));
        let second = structured_content(&second).unwrap();
        assert_eq!(second["truncated"], false);
        assert_eq!(second["next_cursor"], Value::Null);
        assert_eq!(second["dependencies"][0]["target"]["uid"], "uid://c");

        let cross_tool = server.godot_find_resource_owners(Parameters(ResourceInput {
            resource: "uid://a".to_owned(),
            limit: 1,
            cursor: Some(cursor),
        }));
        assert_eq!(
            structured_content(&cross_tool)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("stale_cursor")
        );

        let owners = server.godot_find_resource_owners(Parameters(ResourceInput {
            resource: "res://b.tres".to_owned(),
            limit: 50,
            cursor: None,
        }));
        assert_eq!(
            structured_content(&owners).unwrap()["owners"][0]["owner"]["uid"],
            "uid://a"
        );
    }

    #[test]
    fn resource_tools_return_stable_validation_and_availability_errors() {
        let server = GodotMcpServer::new(SnapshotReplicator::new());
        let unavailable = server.godot_get_resource_dependencies(Parameters(ResourceInput {
            resource: "uid://a".to_owned(),
            limit: 50,
            cursor: None,
        }));
        assert_eq!(
            structured_content(&unavailable)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("project_not_bound")
        );
        let invalid =
            indexed_resource_server().godot_get_resource_dependencies(Parameters(ResourceInput {
                resource: "res://../project.godot".to_owned(),
                limit: 201,
                cursor: None,
            }));
        assert_eq!(
            structured_content(&invalid)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("invalid_limit")
        );
        let invalid_path =
            indexed_resource_server().godot_get_resource_dependencies(Parameters(ResourceInput {
                resource: "res://../project.godot".to_owned(),
                limit: 50,
                cursor: None,
            }));
        assert_eq!(
            structured_content(&invalid_path)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("invalid_path")
        );
        let missing =
            indexed_resource_server().godot_get_resource_dependencies(Parameters(ResourceInput {
                resource: "uid://missing".to_owned(),
                limit: 50,
                cursor: None,
            }));
        assert_eq!(
            structured_content(&missing)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("resource_not_found")
        );
    }

    #[test]
    fn unresolved_dependency_is_a_partial_success_with_safe_diagnostics() {
        let result = indexed_resource_server_fixture(true).godot_get_resource_dependencies(
            Parameters(ResourceInput {
                resource: "uid://a".to_owned(),
                limit: 50,
                cursor: None,
            }),
        );
        assert_ne!(result.is_error, Some(true));
        let content = structured_content(&result).unwrap();
        assert_eq!(content["status"], "partial");
        assert_eq!(content["dependencies"][2]["resolution"], "missing");
        assert_eq!(content["diagnostics"][0]["code"], "missing_dependency");
        assert!(
            !content
                .to_string()
                .contains(std::env::temp_dir().to_string_lossy().as_ref())
        );
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
