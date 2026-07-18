mod cursor;

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use cursor::{CursorBinding, CursorCodec, CursorTool};
use godot_codex_index_store::{
    DependencyEdge, IndexRead, IndexReadSnapshot, ResourceEntity, ResourceQuery, ResourceSelector,
    SceneEntity, SceneNode, SceneProperty, SceneRelation, ScriptAdapterAvailability,
    ScriptCompleteness, ScriptDiagnostic, ScriptDocument, ScriptEndpoint, ScriptLanguage,
    ScriptPredicate, ScriptRelation, ScriptSourceRange, ScriptSymbol, ScriptSymbolInspectionQuery,
    ScriptSymbolInspectionResult, ScriptSymbolKind, ScriptSymbolMatch, ScriptSymbolQuery,
    ScriptSymbolQueryResult, ScriptSymbolSelector, StoreError,
};
use godot_codex_resource_indexer::{
    ResourceIndexReadError, ResourceIndexReader, SceneIndexReadError, SceneIndexReader,
    ScriptIndexReadError, ScriptIndexReader, normalize_resource_path,
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
const MAX_SCRIPT_DIAGNOSTICS: usize = 200;

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

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SceneGraphInput {
    scene: String,
    #[serde(default = "default_resource_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct InspectNodeInput {
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    scene: Option<String>,
    #[serde(default)]
    node_path: Option<String>,
    #[serde(default = "default_resource_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SymbolMatchInput {
    Exact,
    Prefix,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ScriptLanguageInput {
    Gdscript,
    Csharp,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ScriptSymbolKindInput {
    Script,
    Class,
    Method,
    Function,
    Property,
    Constant,
    Enum,
    EnumMember,
    Signal,
}

impl From<SymbolMatchInput> for ScriptSymbolMatch {
    fn from(value: SymbolMatchInput) -> Self {
        match value {
            SymbolMatchInput::Exact => Self::Exact,
            SymbolMatchInput::Prefix => Self::Prefix,
        }
    }
}

impl From<ScriptLanguageInput> for ScriptLanguage {
    fn from(value: ScriptLanguageInput) -> Self {
        match value {
            ScriptLanguageInput::Gdscript => Self::Gdscript,
            ScriptLanguageInput::Csharp => Self::Csharp,
        }
    }
}

impl From<ScriptSymbolKindInput> for ScriptSymbolKind {
    fn from(value: ScriptSymbolKindInput) -> Self {
        match value {
            ScriptSymbolKindInput::Script => Self::Script,
            ScriptSymbolKindInput::Class => Self::Class,
            ScriptSymbolKindInput::Method => Self::Method,
            ScriptSymbolKindInput::Function => Self::Function,
            ScriptSymbolKindInput::Property => Self::Property,
            ScriptSymbolKindInput::Constant => Self::Constant,
            ScriptSymbolKindInput::Enum => Self::Enum,
            ScriptSymbolKindInput::EnumMember => Self::EnumMember,
            ScriptSymbolKindInput::Signal => Self::Signal,
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchSymbolsInput {
    query: String,
    #[serde(rename = "match")]
    match_mode: SymbolMatchInput,
    #[serde(default)]
    language: Option<ScriptLanguageInput>,
    #[serde(default)]
    kind: Option<ScriptSymbolKindInput>,
    #[serde(default)]
    script: Option<String>,
    #[serde(default = "default_resource_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct InspectSymbolInput {
    #[serde(default)]
    symbol_id: Option<String>,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    qualified_name: Option<String>,
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
    scene_index: SceneIndexReader,
    script_index: ScriptIndexReader,
    cursor_codec: CursorCodec,
}

impl std::fmt::Debug for GodotMcpServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GodotMcpServer")
            .field("replica_status", &self.replicator.status())
            .field("resource_index_status", &self.resource_index.status())
            .field("scene_index_status", &self.scene_index.status())
            .field("script_index_status", &self.script_index.status())
            .finish_non_exhaustive()
    }
}

impl GodotMcpServer {
    pub fn new(replicator: SnapshotReplicator) -> Self {
        Self::with_semantic_indexes(
            replicator,
            ResourceIndexReader::new(),
            SceneIndexReader::new(),
        )
    }

    pub fn with_resource_index(
        replicator: SnapshotReplicator,
        resource_index: ResourceIndexReader,
    ) -> Self {
        Self::with_semantic_indexes(replicator, resource_index, SceneIndexReader::new())
    }

    pub fn with_semantic_indexes(
        replicator: SnapshotReplicator,
        resource_index: ResourceIndexReader,
        scene_index: SceneIndexReader,
    ) -> Self {
        Self::with_all_indexes(
            replicator,
            resource_index,
            scene_index,
            ScriptIndexReader::new(),
        )
    }

    pub fn with_all_indexes(
        replicator: SnapshotReplicator,
        resource_index: ResourceIndexReader,
        scene_index: SceneIndexReader,
        script_index: ScriptIndexReader,
    ) -> Self {
        Self {
            tool_router: Self::tool_router(),
            replicator,
            resource_index,
            scene_index,
            script_index,
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

    fn resource_query(&self, input: ResourceInput, tool: CursorTool) -> CallToolResult {
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
            resource_revision: generation.checkpoint.resource_revision,
            scene_graph_revision: None,
            script_graph_revision: None,
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
            CursorTool::Dependencies => snapshot.direct_dependencies(&query),
            CursorTool::Owners => snapshot.reverse_owners(&query),
            CursorTool::SceneGraph
            | CursorTool::InspectNode
            | CursorTool::SearchSymbols
            | CursorTool::InspectSymbol => unreachable!("resource tool"),
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

    fn scene_graph_query(&self, input: SceneGraphInput) -> CallToolResult {
        if !(1..=MAX_RESOURCE_LIMIT).contains(&input.limit) {
            return structured_error("invalid_limit", "limit must be between 1 and 200", false);
        }
        let snapshot = match self.scene_index.pin_current() {
            Ok(snapshot) => snapshot,
            Err(error) => return scene_index_error(error),
        };
        let generation = snapshot.generation();
        let (scene, canonical_selector) =
            match resolve_scene(&generation.scene.scenes, &input.scene) {
                Ok(scene) => scene,
                Err((code, message)) => return structured_error(code, message, false),
            };
        let binding = CursorBinding {
            project_id: &generation.project_id,
            tool: CursorTool::SceneGraph,
            selector: &canonical_selector,
            limit: input.limit,
            generation_id: &generation.generation_id,
            index_revision: generation.index_revision,
            resource_revision: generation.scene.resource_revision,
            scene_graph_revision: Some(generation.scene.scene_graph_revision),
            script_graph_revision: None,
        };
        let now = unix_seconds();
        let offset = match cursor_offset(input.cursor.as_deref(), &self.cursor_codec, &binding, now)
        {
            Ok(offset) => offset,
            Err(result) => return result,
        };
        let mut occurrences: Vec<_> = generation
            .scene
            .relations
            .iter()
            .filter(|relation| {
                relation.relation == "occurrence"
                    && relation.scene_entity_id.as_deref() == Some(&scene.scene_entity_id)
            })
            .collect();
        occurrences.sort_by(|left, right| {
            relation_path(left)
                .cmp(relation_path(right))
                .then_with(|| left.source.cmp(&right.source))
        });
        if offset > occurrences.len() || offset >= MAX_TOTAL_RESULTS {
            return structured_error(
                "stale_cursor",
                "cursor offset is outside the current scene generation",
                false,
            );
        }
        let has_more = offset
            .checked_add(input.limit)
            .is_some_and(|end| end < occurrences.len());
        let page: Vec<_> = occurrences
            .iter()
            .skip(offset)
            .take(input.limit)
            .map(|occurrence| scene_node_view(generation, scene, occurrence, &occurrences))
            .collect();
        let next_cursor = match next_cursor(
            has_more,
            offset,
            page.len(),
            &self.cursor_codec,
            &binding,
            now,
        ) {
            Ok(cursor) => cursor,
            Err(result) => return result,
        };
        scene_graph_success(
            generation,
            scene,
            input.limit,
            offset,
            page,
            has_more,
            next_cursor,
        )
    }

    fn inspect_node_query(&self, input: InspectNodeInput) -> CallToolResult {
        if !(1..=MAX_RESOURCE_LIMIT).contains(&input.limit) {
            return structured_error("invalid_limit", "limit must be between 1 and 200", false);
        }
        let valid_selector = match (&input.node_id, &input.scene, &input.node_path) {
            (Some(node_id), None, None) if valid_opaque_node_id(node_id) => true,
            (None, Some(_), Some(node_path)) if valid_node_path(node_path) => true,
            _ => false,
        };
        if !valid_selector {
            return structured_error(
                "invalid_query",
                "provide exactly node_id, or scene together with a relative node-only node_path",
                false,
            );
        }
        let snapshot = match self.scene_index.pin_current() {
            Ok(snapshot) => snapshot,
            Err(error) => return scene_index_error(error),
        };
        let generation = snapshot.generation();
        let selected = match resolve_node_selection(
            generation,
            input.node_id.as_deref(),
            input.scene.as_deref(),
            input.node_path.as_deref(),
        ) {
            Ok(selected) => selected,
            Err((code, message)) => return structured_error(code, message, false),
        };
        let binding = CursorBinding {
            project_id: &generation.project_id,
            tool: CursorTool::InspectNode,
            selector: &selected.canonical_selector,
            limit: input.limit,
            generation_id: &generation.generation_id,
            index_revision: generation.index_revision,
            resource_revision: generation.scene.resource_revision,
            scene_graph_revision: Some(generation.scene.scene_graph_revision),
            script_graph_revision: None,
        };
        let now = unix_seconds();
        let offset = match cursor_offset(input.cursor.as_deref(), &self.cursor_codec, &binding, now)
        {
            Ok(offset) => offset,
            Err(result) => return result,
        };
        let mut properties: Vec<_> = generation
            .scene
            .properties
            .iter()
            .filter(|property| {
                property.scene_entity_id == selected.scene.scene_entity_id
                    && property.subject_entity_id == selected.subject_id
            })
            .collect();
        properties.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.property_id.cmp(&right.property_id))
        });
        if offset > properties.len() || offset >= MAX_TOTAL_RESULTS {
            return structured_error(
                "stale_cursor",
                "cursor offset is outside the current scene generation",
                false,
            );
        }
        let has_more = offset
            .checked_add(input.limit)
            .is_some_and(|end| end < properties.len());
        let page: Vec<_> = properties
            .iter()
            .skip(offset)
            .take(input.limit)
            .map(|property| property_view(property))
            .collect();
        let next_cursor = match next_cursor(
            has_more,
            offset,
            page.len(),
            &self.cursor_codec,
            &binding,
            now,
        ) {
            Ok(cursor) => cursor,
            Err(result) => return result,
        };
        inspect_node_success(
            generation,
            &selected,
            input.limit,
            offset,
            page,
            has_more,
            next_cursor,
        )
    }

    fn search_symbols_query(&self, input: SearchSymbolsInput) -> CallToolResult {
        if !(1..=MAX_RESOURCE_LIMIT).contains(&input.limit) {
            return structured_error("invalid_limit", "limit must be between 1 and 200", false);
        }
        if !valid_symbol_search_query(&input.query) {
            return structured_error(
                "invalid_query",
                "query must be a non-empty bounded declaration name without control characters",
                false,
            );
        }
        let snapshot = match self.script_index.pin_current() {
            Ok(snapshot) => snapshot,
            Err(error) => return script_index_error(error),
        };
        let generation = snapshot.generation();
        let script_filter = match input.script.as_deref() {
            Some(selector) => match resolve_script(generation, selector) {
                Ok((document, canonical)) => Some((
                    document.script_resource_id.clone(),
                    canonical,
                    document.path.clone(),
                )),
                Err((code, message)) => return structured_error(code, message, false),
            },
            None => None,
        };
        let language = input.language.map(ScriptLanguage::from);
        let kind = input.kind.map(ScriptSymbolKind::from);
        let match_mode = ScriptSymbolMatch::from(input.match_mode);
        let selector_binding = json!({
            "query": input.query,
            "match": input.match_mode,
            "language": input.language,
            "kind": input.kind,
            "script": script_filter.as_ref().map(|(_, canonical, _)| canonical),
        })
        .to_string();
        let binding = CursorBinding {
            project_id: &generation.project_id,
            tool: CursorTool::SearchSymbols,
            selector: &selector_binding,
            limit: input.limit,
            generation_id: &generation.generation_id,
            index_revision: generation.index_revision,
            resource_revision: generation.script.resource_revision,
            scene_graph_revision: None,
            script_graph_revision: Some(generation.script.script_graph_revision),
        };
        let now = unix_seconds();
        let offset = match cursor_offset(input.cursor.as_deref(), &self.cursor_codec, &binding, now)
        {
            Ok(offset) => offset,
            Err(result) => return result,
        };
        if offset >= MAX_TOTAL_RESULTS {
            return structured_error(
                "result_limit_exceeded",
                "symbol query exceeded the bounded result window",
                false,
            );
        }
        let result = match generation.script.search_symbols(&ScriptSymbolQuery {
            query: input.query.clone(),
            match_mode,
            language,
            kind,
            script_resource_id: script_filter
                .as_ref()
                .map(|(script_id, _, _)| script_id.clone()),
            limit: input.limit,
            offset,
        }) {
            Ok(result) => result,
            Err(error) => return script_store_error(error),
        };
        let next_cursor = match next_cursor(
            result.has_more,
            offset,
            result.symbols.len(),
            &self.cursor_codec,
            &binding,
            now,
        ) {
            Ok(cursor) => cursor,
            Err(result) => return result,
        };
        search_symbols_success(
            generation,
            &input,
            script_filter.as_ref(),
            offset,
            result,
            next_cursor,
        )
    }

    fn inspect_symbol_query(&self, input: InspectSymbolInput) -> CallToolResult {
        if !(1..=MAX_RESOURCE_LIMIT).contains(&input.limit) {
            return structured_error("invalid_limit", "limit must be between 1 and 200", false);
        }
        let selector_shape_valid = match (
            input.symbol_id.as_deref(),
            input.script.as_deref(),
            input.qualified_name.as_deref(),
        ) {
            (Some(symbol_id), None, None) => valid_opaque_symbol_id(symbol_id),
            (None, Some(_), Some(qualified_name)) => valid_qualified_name(qualified_name),
            _ => false,
        };
        if !selector_shape_valid {
            return structured_error(
                "invalid_query",
                "provide exactly symbol_id, or script together with a canonical qualified_name",
                false,
            );
        }
        let snapshot = match self.script_index.pin_current() {
            Ok(snapshot) => snapshot,
            Err(error) => return script_index_error(error),
        };
        let generation = snapshot.generation();
        let (selector, selector_binding) = match (
            input.symbol_id.as_deref(),
            input.script.as_deref(),
            input.qualified_name.as_deref(),
        ) {
            (Some(symbol_id), None, None) => (
                ScriptSymbolSelector::SymbolId {
                    symbol_id: symbol_id.to_owned(),
                },
                format!("symbol_id:{symbol_id}"),
            ),
            (None, Some(script), Some(qualified_name)) => {
                let (document, canonical_script) = match resolve_script(generation, script) {
                    Ok(resolved) => resolved,
                    Err((code, message)) => return structured_error(code, message, false),
                };
                (
                    ScriptSymbolSelector::ScriptQualified {
                        script_resource_id: document.script_resource_id.clone(),
                        qualified_key: qualified_name.to_owned(),
                    },
                    format!("{canonical_script}\0qualified_name:{qualified_name}"),
                )
            }
            _ => unreachable!("selector shape was validated"),
        };
        let binding = CursorBinding {
            project_id: &generation.project_id,
            tool: CursorTool::InspectSymbol,
            selector: &selector_binding,
            limit: input.limit,
            generation_id: &generation.generation_id,
            index_revision: generation.index_revision,
            resource_revision: generation.script.resource_revision,
            scene_graph_revision: Some(generation.script.scene_graph_revision),
            script_graph_revision: Some(generation.script.script_graph_revision),
        };
        let now = unix_seconds();
        let offset = match cursor_offset(input.cursor.as_deref(), &self.cursor_codec, &binding, now)
        {
            Ok(offset) => offset,
            Err(result) => return result,
        };
        if offset >= MAX_TOTAL_RESULTS {
            return structured_error(
                "result_limit_exceeded",
                "symbol inspection exceeded the bounded result window",
                false,
            );
        }
        let result = match generation
            .script
            .inspect_symbol(&ScriptSymbolInspectionQuery {
                selector,
                limit: input.limit,
                offset,
            }) {
            Ok(result) => result,
            Err(error) => return script_store_error(error),
        };
        let next_cursor = match next_cursor(
            result.has_more,
            offset,
            result.relations.len(),
            &self.cursor_codec,
            &binding,
            now,
        ) {
            Ok(cursor) => cursor,
            Err(result) => return result,
        };
        inspect_symbol_success(
            generation,
            &selector_binding,
            input.limit,
            offset,
            result,
            next_cursor,
        )
    }
}

fn valid_symbol_search_query(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1_024
        && value == value.trim()
        && !value.chars().any(char::is_control)
}

fn valid_opaque_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == 43
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    })
}

fn valid_opaque_script_id(value: &str) -> bool {
    valid_opaque_id(value, "godot:resource:uid:v1:")
        || valid_opaque_id(value, "godot:resource:path-content:v1:")
}

fn valid_opaque_symbol_id(value: &str) -> bool {
    valid_opaque_id(value, "godot:script-symbol:named:v1:")
        || valid_opaque_id(value, "godot:script-symbol:content-revision:v1:")
}

fn valid_qualified_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 2_048
        && !value.chars().any(char::is_control)
        && (value == "script" || value.starts_with("script/") || value.starts_with("class:"))
        && value
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

fn resolve_script<'a>(
    generation: &'a godot_codex_index_store::IndexGeneration,
    selector: &str,
) -> Result<(&'a ScriptDocument, String), (&'static str, &'static str)> {
    if selector.starts_with("godot:resource:") {
        if !valid_opaque_script_id(selector) {
            return Err(("invalid_query", "opaque script identifier is invalid"));
        }
        return generation
            .script
            .documents
            .iter()
            .find(|document| document.script_resource_id == selector)
            .map(|document| (document, format!("script_id:{selector}")))
            .ok_or((
                "script_not_found",
                "script was not found in the current index",
            ));
    }
    if selector.starts_with("uid://") {
        let (resource_selector, canonical, _) = parse_selector(selector)?;
        let ResourceSelector::Uid(uid) = resource_selector else {
            unreachable!("uid selector parser returned another variant")
        };
        let script_id = generation
            .resources
            .iter()
            .find(|resource| resource.uid.as_deref() == Some(uid.as_str()))
            .map(|resource| resource.entity_id.as_str());
        return script_id
            .and_then(|script_id| {
                generation
                    .script
                    .documents
                    .iter()
                    .find(|document| document.script_resource_id == script_id)
            })
            .map(|document| (document, canonical))
            .ok_or((
                "script_not_found",
                "script was not found in the current index",
            ));
    }
    if selector.starts_with("res://") {
        let path = normalize_resource_path(selector)
            .map_err(|_| ("invalid_path", "script path is invalid or unsafe"))?;
        return generation
            .script
            .documents
            .iter()
            .find(|document| document.path == path.comparison)
            .map(|document| (document, format!("path:{}", path.comparison)))
            .ok_or((
                "script_not_found",
                "script was not found in the current index",
            ));
    }
    Err((
        "invalid_query",
        "script must be an opaque resource ID, uid://, or res:// selector",
    ))
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

struct NodeSelection<'a> {
    scene: &'a SceneEntity,
    definition: &'a SceneNode,
    occurrence: Option<&'a SceneRelation>,
    subject_id: String,
    canonical_selector: String,
}

fn resolve_scene<'a>(
    scenes: &'a [SceneEntity],
    selector: &str,
) -> Result<(&'a SceneEntity, String), (&'static str, &'static str)> {
    let (scene, canonical) = if selector.starts_with("godot:scene:") {
        if selector.len() > 256
            || !selector
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-'))
        {
            return Err(("invalid_query", "opaque scene identifier is invalid"));
        }
        (
            scenes
                .iter()
                .find(|scene| scene.scene_entity_id == selector),
            format!("scene_id:{selector}"),
        )
    } else if selector.starts_with("uid://") {
        if selector.len() > 128
            || selector.len() <= 6
            || !selector
                .bytes()
                .skip(6)
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(("invalid_query", "scene UID is invalid"));
        }
        (
            scenes
                .iter()
                .find(|scene| scene.uid.as_deref() == Some(selector)),
            format!("uid:{selector}"),
        )
    } else if selector.starts_with("res://") {
        let path = normalize_resource_path(selector)
            .map_err(|_| ("invalid_path", "scene path is invalid or unsafe"))?;
        (
            scenes
                .iter()
                .find(|scene| scene.comparison_path == path.comparison),
            format!("path:{}", path.comparison),
        )
    } else {
        return Err((
            "invalid_query",
            "scene must be an opaque scene ID, uid://, or res:// selector",
        ));
    };
    scene.map(|scene| (scene, canonical)).ok_or((
        "scene_not_found",
        "scene was not found in the current index",
    ))
}

fn valid_opaque_node_id(value: &str) -> bool {
    (value.starts_with("godot:node:scene-id:v1:")
        || value.starts_with("godot:node:path-content:v1:")
        || value.starts_with("godot:node-occurrence:v1:"))
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-'))
}

fn valid_node_path(value: &str) -> bool {
    value == "."
        || (!value.is_empty()
            && value.len() <= 2_048
            && !value.starts_with('/')
            && !value
                .bytes()
                .any(|byte| byte.is_ascii_control() || matches!(byte, b'\\' | b':'))
            && value
                .split('/')
                .all(|component| !component.is_empty() && !matches!(component, "." | "..")))
}

fn resolve_node_selection<'a>(
    generation: &'a godot_codex_index_store::IndexGeneration,
    node_id: Option<&str>,
    scene_selector: Option<&str>,
    node_path: Option<&str>,
) -> Result<NodeSelection<'a>, (&'static str, &'static str)> {
    if let Some(node_id) = node_id {
        if let Some(occurrence) = generation
            .scene
            .relations
            .iter()
            .find(|relation| relation.relation == "occurrence" && relation.source == node_id)
        {
            let scene_id = occurrence.scene_entity_id.as_deref().ok_or((
                "index_not_current",
                "node occurrence is not bound to a scene",
            ))?;
            let scene = generation
                .scene
                .scenes
                .iter()
                .find(|scene| scene.scene_entity_id == scene_id)
                .ok_or(("index_not_current", "node occurrence scene is missing"))?;
            let definition_id = occurrence
                .target
                .as_deref()
                .ok_or(("index_not_current", "node occurrence definition is missing"))?;
            let definition = generation
                .scene
                .nodes
                .iter()
                .find(|node| node.node_entity_id == definition_id)
                .ok_or(("index_not_current", "node definition is missing"))?;
            return Ok(NodeSelection {
                scene,
                definition,
                occurrence: Some(occurrence),
                subject_id: node_id.to_owned(),
                canonical_selector: format!("node_id:{node_id}"),
            });
        }
        let definition = generation
            .scene
            .nodes
            .iter()
            .find(|node| node.node_entity_id == node_id)
            .ok_or(("node_not_found", "node was not found in the current index"))?;
        let scene = generation
            .scene
            .scenes
            .iter()
            .find(|scene| scene.scene_entity_id == definition.scene_entity_id)
            .ok_or(("index_not_current", "node definition scene is missing"))?;
        return Ok(NodeSelection {
            scene,
            definition,
            occurrence: None,
            subject_id: node_id.to_owned(),
            canonical_selector: format!("node_id:{node_id}"),
        });
    }

    let (scene, scene_selector) = resolve_scene(
        &generation.scene.scenes,
        scene_selector.expect("validated scene selector"),
    )?;
    let node_path = node_path.expect("validated node path");
    let occurrence = generation.scene.relations.iter().find(|relation| {
        relation.relation == "occurrence"
            && relation.scene_entity_id.as_deref() == Some(&scene.scene_entity_id)
            && relation_path(relation) == node_path
    });
    let Some(occurrence) = occurrence else {
        return Err((
            "node_path_not_found",
            "node path was not found in the selected scene",
        ));
    };
    let definition_id = occurrence
        .target
        .as_deref()
        .ok_or(("index_not_current", "node occurrence definition is missing"))?;
    let definition = generation
        .scene
        .nodes
        .iter()
        .find(|node| node.node_entity_id == definition_id)
        .ok_or(("index_not_current", "node definition is missing"))?;
    Ok(NodeSelection {
        scene,
        definition,
        occurrence: Some(occurrence),
        subject_id: occurrence.source.clone(),
        canonical_selector: format!("{scene_selector}\0node_path:{node_path}"),
    })
}

fn cursor_offset(
    cursor: Option<&str>,
    codec: &CursorCodec,
    binding: &CursorBinding<'_>,
    now: u64,
) -> Result<usize, CallToolResult> {
    match cursor {
        Some(cursor) => codec.validate(cursor, binding, now).map_err(|()| {
            structured_error(
                "stale_cursor",
                "cursor is invalid, expired, or belongs to another index generation",
                false,
            )
        }),
        None => Ok(0),
    }
}

fn next_cursor(
    has_more: bool,
    offset: usize,
    page_len: usize,
    codec: &CursorCodec,
    binding: &CursorBinding<'_>,
    now: u64,
) -> Result<Option<String>, CallToolResult> {
    if !has_more {
        return Ok(None);
    }
    let next_offset = offset.checked_add(page_len).ok_or_else(|| {
        structured_error(
            "result_limit_exceeded",
            "semantic query exceeded the bounded result window",
            false,
        )
    })?;
    if next_offset >= MAX_TOTAL_RESULTS {
        return Err(structured_error(
            "result_limit_exceeded",
            "semantic query exceeded the bounded result window",
            false,
        ));
    }
    codec
        .issue(binding, next_offset, now)
        .map(Some)
        .map_err(|()| {
            structured_error(
                "index_not_current",
                "the current semantic index page could not be pinned",
                true,
            )
        })
}

fn relation_path(relation: &SceneRelation) -> &str {
    relation
        .attributes
        .get("effective_path")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn scene_node_view(
    generation: &godot_codex_index_store::IndexGeneration,
    scene: &SceneEntity,
    occurrence: &SceneRelation,
    occurrences: &[&SceneRelation],
) -> Value {
    let definition = occurrence.target.as_ref().and_then(|definition_id| {
        generation
            .scene
            .nodes
            .iter()
            .find(|node| node.node_entity_id == *definition_id)
    });
    let path = relation_path(occurrence);
    let parent_path = if path == "." {
        None
    } else {
        Some(path.rsplit_once('/').map_or(".", |(parent, _)| parent))
    };
    let parent_node_id = parent_path.and_then(|parent| {
        occurrences
            .iter()
            .find(|candidate| relation_path(candidate) == parent)
            .map(|candidate| candidate.source.clone())
    });
    let chain = occurrence.attributes.get("instance_chain");
    let owner_node_id = definition
        .and_then(|definition| definition.owner_node_entity_id.as_ref())
        .and_then(|owner| {
            occurrences
                .iter()
                .find(|candidate| {
                    candidate.target.as_ref() == Some(owner)
                        && candidate.attributes.get("instance_chain") == chain
                })
                .map(|candidate| candidate.source.clone())
        });
    let groups: Vec<_> = generation
        .scene
        .groups
        .iter()
        .filter(|group| {
            group.scene_entity_id == scene.scene_entity_id
                && group.member_node_entity_id == occurrence.source
        })
        .map(|group| group.group.clone())
        .collect();
    let connections: Vec<_> = generation
        .scene
        .connections
        .iter()
        .filter(|connection| {
            connection.scene_entity_id == scene.scene_entity_id
                && (connection.emitter_node_entity_id == occurrence.source
                    || connection.receiver_node_entity_id.as_ref() == Some(&occurrence.source))
        })
        .map(|connection| {
            json!({
                "connection_id": connection.connection_id,
                "emitter_node_id": connection.emitter_node_entity_id,
                "signal": connection.signal,
                "receiver_node_id": connection.receiver_node_entity_id,
                "method": connection.method,
                "flags": connection.flags,
            })
        })
        .collect();
    json!({
        "node_id": occurrence.source,
        "node_path": path,
        "definition": definition.map(definition_view),
        "origin_scene_id": definition.map(|node| node.scene_entity_id.clone()),
        "parent_node_id": parent_node_id,
        "owner_node_id": owner_node_id,
        "instance_chain": occurrence.attributes.get("instance_chain"),
        "editable": occurrence.attributes.get("editable"),
        "internal": occurrence.attributes.get("internal"),
        "groups": groups,
        "connections": connections,
    })
}

fn definition_view(node: &SceneNode) -> Value {
    json!({
        "node_id": node.node_entity_id,
        "defining_scene_id": node.scene_entity_id,
        "node_path": node.node_path,
        "name": node.name,
        "godot_type": node.godot_type,
        "unique_scene_id": node.unique_scene_id,
        "identity_scope": node.identity_scope,
        "owned": node.owned,
        "internal": node.internal,
        "attached_script_resource_id": node.attached_script_entity_id,
        "instance_scene_id": node.instance_scene_entity_id,
        "authority": node.authority,
    })
}

fn property_view(property: &SceneProperty) -> Value {
    json!({
        "property_id": property.property_id,
        "name": property.name,
        "value": {
            "type": property.value_type,
            "value": property.value,
            "truncated": property.truncated,
        },
        "origin": property.origin,
        "declaring_scene_id": property.declaring_scene_entity_id,
        "declaring_node_id": property.declaring_node_entity_id,
        "overridden_property_id": property.overridden_property_id,
        "authority": property.authority,
        "resource_revision": property.resource_revision,
        "scene_graph_revision": property.scene_graph_revision,
    })
}

fn search_symbols_success(
    generation: &godot_codex_index_store::IndexGeneration,
    input: &SearchSymbolsInput,
    script_filter: Option<&(String, String, String)>,
    offset: usize,
    result: ScriptSymbolQueryResult,
    next_cursor: Option<String>,
) -> CallToolResult {
    let script_ids: BTreeSet<_> = script_filter
        .map(|(script_id, _, _)| std::iter::once(script_id.as_str()).collect())
        .unwrap_or_else(|| {
            result
                .symbols
                .iter()
                .map(|symbol| symbol.script_resource_id.as_str())
                .collect()
        });
    let (diagnostics, diagnostics_truncated) = script_diagnostics(generation, &script_ids);
    let mut partial_reasons = script_partial_reasons(
        generation,
        input.language.map(ScriptLanguage::from),
        &script_ids,
        script_filter.is_some(),
    );
    partial_reasons.extend(diagnostic_codes(&diagnostics));
    if diagnostics_truncated {
        partial_reasons.insert("diagnostics_truncated".to_owned());
    }
    let symbols = result
        .symbols
        .iter()
        .map(|symbol| script_symbol_view(generation, symbol))
        .collect::<Vec<_>>();
    CallToolResult::structured(json!({
        "project_id": generation.project_id,
        "schema_version": generation.schema_version,
        "generation_id": generation.generation_id,
        "index_revision": generation.index_revision,
        "resource_revision": generation.script.resource_revision,
        "scene_graph_revision": generation.script.scene_graph_revision,
        "script_graph_revision": generation.script.script_graph_revision,
        "freshness": "current",
        "status": if partial_reasons.is_empty() { "exact" } else { "partial" },
        "query": {
            "query": input.query,
            "match": input.match_mode,
            "language": input.language,
            "kind": input.kind,
            "script": script_filter.map(|(_, _, path)| path),
            "limit": input.limit,
            "offset": offset,
        },
        "symbols": symbols,
        "total_matches": result.total_matches,
        "diagnostics": diagnostics,
        "partial_reasons": partial_reasons,
        "truncated": result.has_more,
        "next_cursor": next_cursor,
        "validated_checkpoint": script_checkpoint(generation),
        "evidence": {
            "source": "persistent_segment_index",
            "source_complete": generation.script.source_complete,
            "authorities": ["gdscript_parser_analyzer", "resource_graph", "scene_state"],
        },
    }))
}

fn inspect_symbol_success(
    generation: &godot_codex_index_store::IndexGeneration,
    selector: &str,
    limit: usize,
    offset: usize,
    result: ScriptSymbolInspectionResult,
    next_cursor: Option<String>,
) -> CallToolResult {
    let script_ids = BTreeSet::from([result.document.script_resource_id.as_str()]);
    let mut diagnostics = result
        .diagnostics
        .iter()
        .take(MAX_SCRIPT_DIAGNOSTICS)
        .map(script_diagnostic_view)
        .collect::<Vec<_>>();
    diagnostics.sort_by(|left, right| {
        left.get("diagnostic_id")
            .and_then(Value::as_str)
            .cmp(&right.get("diagnostic_id").and_then(Value::as_str))
    });
    let diagnostics_truncated = result.diagnostics.len() > MAX_SCRIPT_DIAGNOSTICS;
    let mut partial_reasons = script_partial_reasons(
        generation,
        Some(result.document.language),
        &script_ids,
        true,
    );
    partial_reasons.extend(diagnostic_codes(&diagnostics));
    if diagnostics_truncated {
        partial_reasons.insert("diagnostics_truncated".to_owned());
    }
    let (scene_attachments, outgoing_relations): (Vec<_>, Vec<_>) = result
        .relations
        .iter()
        .partition(|relation| relation.predicate == ScriptPredicate::AttachesScript);
    let scene_attachments = scene_attachments
        .into_iter()
        .map(|relation| script_relation_view(generation, relation))
        .collect::<Vec<_>>();
    let outgoing_relations = outgoing_relations
        .into_iter()
        .map(|relation| script_relation_view(generation, relation))
        .collect::<Vec<_>>();
    CallToolResult::structured(json!({
        "project_id": generation.project_id,
        "schema_version": generation.schema_version,
        "generation_id": generation.generation_id,
        "index_revision": generation.index_revision,
        "resource_revision": generation.script.resource_revision,
        "scene_graph_revision": generation.script.scene_graph_revision,
        "script_graph_revision": generation.script.script_graph_revision,
        "freshness": "current",
        "status": if partial_reasons.is_empty() { "exact" } else { "partial" },
        "query": {"selector": selector, "limit": limit, "offset": offset},
        "document": script_document_view(&result.document),
        "declaration": script_symbol_view(generation, &result.symbol),
        "owner": result.owner.as_ref().map(|owner| script_symbol_view(generation, owner)),
        "outgoing_relations": outgoing_relations,
        "scene_attachments": scene_attachments,
        "total_relations": result.total_relations,
        "diagnostics": diagnostics,
        "partial_reasons": partial_reasons,
        "truncated": result.has_more,
        "next_cursor": next_cursor,
        "validated_checkpoint": script_checkpoint(generation),
        "evidence": {
            "source": "persistent_segment_index",
            "source_complete": generation.script.source_complete,
            "authorities": ["gdscript_parser_analyzer", "resource_graph", "scene_state"],
        },
    }))
}

fn script_symbol_view(
    generation: &godot_codex_index_store::IndexGeneration,
    symbol: &ScriptSymbol,
) -> Value {
    let document = generation
        .script
        .documents
        .iter()
        .find(|document| document.script_resource_id == symbol.script_resource_id);
    json!({
        "symbol_id": symbol.symbol_id,
        "script_resource_id": symbol.script_resource_id,
        "script_path": document.map(|document| document.path.as_str()),
        "language": symbol.language,
        "kind": symbol.kind,
        "name": symbol.name,
        "qualified_name": symbol.qualified_key,
        "owner_symbol_id": symbol.owner_symbol_id,
        "identity_scope": symbol.identity_scope,
        "signature": symbol.signature,
        "type": {"name": symbol.type_name, "state": symbol.type_state},
        "visibility": symbol.visibility,
        "modifiers": symbol.modifiers,
        "documentation_present": symbol.documentation_present,
        "declaration": script_range_view(&symbol.declaration_range),
        "script_graph_revision": symbol.script_graph_revision,
    })
}

fn script_document_view(document: &ScriptDocument) -> Value {
    json!({
        "script_resource_id": document.script_resource_id,
        "path": document.path,
        "language": document.language,
        "content_sha256": document.content_sha256,
        "adapter_profile": document.adapter_profile,
        "completeness": document.completeness,
        "resource_revision": document.resource_revision,
        "script_graph_revision": document.script_graph_revision,
    })
}

fn script_range_view(range: &ScriptSourceRange) -> Value {
    json!({
        "path": range.path,
        "content_sha256": range.content_sha256,
        "start_byte": range.start_byte,
        "end_byte": range.end_byte,
        "start": {"line": range.start_line, "column": range.start_column},
        "end": {"line": range.end_line, "column": range.end_column},
    })
}

fn script_relation_view(
    generation: &godot_codex_index_store::IndexGeneration,
    relation: &ScriptRelation,
) -> Value {
    json!({
        "relation_id": relation.relation_id,
        "predicate": relation.predicate,
        "source": script_endpoint_view(generation, &relation.source),
        "target": relation.target.as_ref().map(|target| script_endpoint_view(generation, target)),
        "confidence": relation.confidence,
        "detail": relation.detail,
        "authority": relation.authority,
        "evidence": relation.evidence_range.as_ref().map(script_range_view),
        "script_graph_revision": relation.script_graph_revision,
    })
}

fn script_endpoint_view(
    generation: &godot_codex_index_store::IndexGeneration,
    endpoint: &ScriptEndpoint,
) -> Value {
    match endpoint {
        ScriptEndpoint::Symbol { symbol_id } => {
            let symbol = generation
                .script
                .symbols
                .iter()
                .find(|symbol| symbol.symbol_id == *symbol_id);
            json!({
                "kind": "symbol",
                "symbol_id": symbol_id,
                "name": symbol.and_then(|symbol| symbol.name.as_deref()),
                "qualified_name": symbol.map(|symbol| symbol.qualified_key.as_str()),
                "script_resource_id": symbol.map(|symbol| symbol.script_resource_id.as_str()),
            })
        }
        ScriptEndpoint::Resource { resource_entity_id } => {
            let resource = generation
                .resources
                .iter()
                .find(|resource| resource.entity_id == *resource_entity_id);
            json!({
                "kind": "resource",
                "resource_entity_id": resource_entity_id,
                "resource": resource.map(resource_view),
            })
        }
        ScriptEndpoint::SceneNode { node_entity_id } => {
            let node = generation
                .scene
                .nodes
                .iter()
                .find(|node| node.node_entity_id == *node_entity_id);
            let occurrence = generation.scene.relations.iter().find(|relation| {
                relation.relation == "occurrence" && relation.source == *node_entity_id
            });
            json!({
                "kind": "scene_node",
                "node_id": node_entity_id,
                "scene_id": node.map(|node| node.scene_entity_id.as_str()).or_else(|| occurrence.and_then(|relation| relation.scene_entity_id.as_deref())),
                "node_path": node.map(|node| node.node_path.as_str()).or_else(|| occurrence.map(relation_path)),
            })
        }
    }
}

fn script_diagnostic_view(diagnostic: &ScriptDiagnostic) -> Value {
    json!({
        "diagnostic_id": diagnostic.diagnostic_id,
        "script_resource_id": diagnostic.script_resource_id,
        "language": diagnostic.language,
        "content_sha256": diagnostic.content_sha256,
        "code": diagnostic.code,
        "severity": diagnostic.severity,
        "message": diagnostic.safe_message,
        "range": diagnostic.range.as_ref().map(script_range_view),
        "authority": diagnostic.authority,
        "script_graph_revision": diagnostic.script_graph_revision,
    })
}

fn script_diagnostics(
    generation: &godot_codex_index_store::IndexGeneration,
    script_ids: &BTreeSet<&str>,
) -> (Vec<Value>, bool) {
    let matching = generation
        .script
        .diagnostics
        .iter()
        .filter(|diagnostic| script_ids.contains(diagnostic.script_resource_id.as_str()))
        .collect::<Vec<_>>();
    let truncated = matching.len() > MAX_SCRIPT_DIAGNOSTICS;
    let diagnostics = matching
        .into_iter()
        .take(MAX_SCRIPT_DIAGNOSTICS)
        .map(script_diagnostic_view)
        .collect();
    (diagnostics, truncated)
}

fn script_partial_reasons(
    generation: &godot_codex_index_store::IndexGeneration,
    language_filter: Option<ScriptLanguage>,
    script_ids: &BTreeSet<&str>,
    narrow_to_scripts: bool,
) -> BTreeSet<String> {
    let languages: BTreeSet<_> = if let Some(language) = language_filter {
        BTreeSet::from([language])
    } else if narrow_to_scripts && !script_ids.is_empty() {
        generation
            .script
            .documents
            .iter()
            .filter(|document| script_ids.contains(document.script_resource_id.as_str()))
            .map(|document| document.language)
            .collect()
    } else {
        generation
            .script
            .adapter_statuses
            .iter()
            .map(|status| status.language)
            .collect()
    };
    let mut reasons = BTreeSet::new();
    for status in &generation.script.adapter_statuses {
        if !languages.contains(&status.language) {
            continue;
        }
        match status.availability {
            ScriptAdapterAvailability::Available => {}
            ScriptAdapterAvailability::DiscoveryOnly => {
                reasons.insert(format!(
                    "{}_semantics_discovery_only",
                    script_language_label(status.language)
                ));
            }
            ScriptAdapterAvailability::Unavailable => {
                reasons.insert(format!(
                    "{}_semantics_unavailable",
                    script_language_label(status.language)
                ));
            }
        }
    }
    for document in &generation.script.documents {
        if (narrow_to_scripts
            && !script_ids.is_empty()
            && !script_ids.contains(document.script_resource_id.as_str()))
            || !languages.contains(&document.language)
        {
            continue;
        }
        let reason = match document.completeness {
            ScriptCompleteness::Complete => None,
            ScriptCompleteness::Partial => Some("script_document_partial"),
            ScriptCompleteness::Invalid => Some("script_document_invalid"),
            ScriptCompleteness::Unavailable => Some("script_document_unavailable"),
        };
        if let Some(reason) = reason {
            reasons.insert(reason.to_owned());
        }
    }
    reasons
}

fn script_language_label(language: ScriptLanguage) -> &'static str {
    match language {
        ScriptLanguage::Gdscript => "gdscript",
        ScriptLanguage::Csharp => "csharp",
    }
}

fn script_checkpoint(generation: &godot_codex_index_store::IndexGeneration) -> Value {
    json!({
        "editor_session_id": generation.script.editor_session_id,
        "resource_revision": generation.script.resource_revision,
        "scene_graph_revision": generation.script.scene_graph_revision,
        "script_graph_revision": generation.script.script_graph_revision,
        "source_complete": generation.script.source_complete,
        "snapshot_checksum": generation.script.snapshot_checksum,
        "semantic_digest": generation.script.semantic_digest,
    })
}

fn scene_graph_success(
    generation: &godot_codex_index_store::IndexGeneration,
    scene: &SceneEntity,
    limit: usize,
    offset: usize,
    nodes: Vec<Value>,
    has_more: bool,
    next_cursor: Option<String>,
) -> CallToolResult {
    let subjects: BTreeSet<_> = nodes
        .iter()
        .filter_map(|node| node.get("node_id").and_then(Value::as_str))
        .collect();
    let diagnostics = scene_diagnostics(generation, scene, &subjects);
    let mut partial_reasons = diagnostic_codes(&diagnostics);
    let project_context_relations: Vec<_> = generation
        .scene
        .relations
        .iter()
        .filter(|relation| relation.relation == "project_context")
        .collect();
    let project_context_truncated = project_context_relations.len() > MAX_RESOURCE_LIMIT;
    if project_context_truncated {
        partial_reasons.insert("project_context_truncated".to_owned());
    }
    let project_context: Vec<_> = project_context_relations
        .into_iter()
        .take(MAX_RESOURCE_LIMIT)
        .map(|relation| {
            json!({
                "key": relation.source,
                "value": {
                    "type": relation.attributes.get("value_type"),
                    "value": relation.attributes.get("value"),
                    "truncated": relation.attributes.get("truncated"),
                },
                "authority": relation.authority,
            })
        })
        .collect();
    CallToolResult::structured(json!({
        "project_id": generation.project_id,
        "schema_version": generation.schema_version,
        "generation_id": generation.generation_id,
        "index_revision": generation.index_revision,
        "resource_revision": generation.scene.resource_revision,
        "scene_graph_revision": generation.scene.scene_graph_revision,
        "freshness": "current",
        "status": if partial_reasons.is_empty() { "exact" } else { "partial" },
        "scene": scene_view(scene),
        "query": {"scene": scene.comparison_path, "limit": limit, "offset": offset},
        "nodes": nodes,
        "project_context": project_context,
        "project_context_truncated": project_context_truncated,
        "diagnostics": diagnostics,
        "partial_reasons": partial_reasons,
        "truncated": has_more,
        "next_cursor": next_cursor,
        "validated_checkpoint": scene_checkpoint(generation),
        "evidence": {
            "source": "persistent_segment_index",
            "source_complete": generation.scene.source_complete,
            "authorities": ["packed_scene_state", "scene_composer"],
        },
    }))
}

fn inspect_node_success(
    generation: &godot_codex_index_store::IndexGeneration,
    selected: &NodeSelection<'_>,
    limit: usize,
    offset: usize,
    properties: Vec<Value>,
    has_more: bool,
    next_cursor: Option<String>,
) -> CallToolResult {
    let occurrences: Vec<_> = generation
        .scene
        .relations
        .iter()
        .filter(|relation| {
            relation.relation == "occurrence"
                && relation.scene_entity_id.as_deref() == Some(&selected.scene.scene_entity_id)
        })
        .collect();
    let node = selected.occurrence.map_or_else(
        || definition_view(selected.definition),
        |occurrence| scene_node_view(generation, selected.scene, occurrence, &occurrences),
    );
    let related_ids = [&selected.subject_id, &selected.definition.node_entity_id];
    let relations: Vec<_> = generation
        .scene
        .relations
        .iter()
        .filter(|relation| {
            let direct = matches!(
                relation.relation.as_str(),
                "attached_script" | "resource_reference"
            ) && (related_ids.contains(&&relation.source)
                || relation
                    .target
                    .as_ref()
                    .is_some_and(|target| related_ids.contains(&target)));
            let ownership_prefix = format!("{}:", selected.definition.node_path);
            let owned_subresource = relation.relation == "subresource"
                && relation.scene_entity_id.as_deref()
                    == Some(&selected.definition.scene_entity_id)
                && relation
                    .attributes
                    .get("ownership_paths")
                    .and_then(Value::as_array)
                    .is_some_and(|paths| {
                        paths.iter().any(|path| {
                            path.as_str()
                                .is_some_and(|path| path.starts_with(&ownership_prefix))
                        })
                    });
            direct || owned_subresource
        })
        .map(|relation| {
            json!({
                "relation_id": relation.relation_id,
                "kind": relation.relation,
                "source": relation.source,
                "target": relation.target,
                "attributes": relation.attributes,
                "authority": relation.authority,
            })
        })
        .collect();
    let groups: Vec<_> = generation
        .scene
        .groups
        .iter()
        .filter(|group| {
            group.scene_entity_id == selected.scene.scene_entity_id
                && related_ids.contains(&&group.member_node_entity_id)
        })
        .map(|group| json!({"group": group.group, "declaration_scope": group.declaration_scope}))
        .collect();
    let connections: Vec<_> = generation
        .scene
        .connections
        .iter()
        .filter(|connection| {
            connection.scene_entity_id == selected.scene.scene_entity_id
                && (related_ids.contains(&&connection.emitter_node_entity_id)
                    || connection
                        .receiver_node_entity_id
                        .as_ref()
                        .is_some_and(|receiver| related_ids.contains(&receiver)))
        })
        .map(|connection| json!(connection))
        .collect();
    let animations: Vec<_> = generation
        .scene
        .animations
        .iter()
        .filter(|animation| {
            animation.scene_entity_id == selected.scene.scene_entity_id
                && (related_ids.contains(&&animation.mixer_node_entity_id)
                    || animation
                        .target_node_entity_id
                        .as_ref()
                        .is_some_and(|target| related_ids.contains(&target)))
        })
        .map(|animation| json!(animation))
        .collect();
    let subjects: BTreeSet<_> = related_ids.into_iter().map(String::as_str).collect();
    let diagnostics = scene_diagnostics(generation, selected.scene, &subjects);
    let mut partial_reasons = diagnostic_codes(&diagnostics);
    if properties.iter().any(|property| {
        property
            .pointer("/value/truncated")
            .and_then(Value::as_bool)
            == Some(true)
    }) {
        partial_reasons.insert("projected_value_truncated".to_owned());
    }
    CallToolResult::structured(json!({
        "project_id": generation.project_id,
        "schema_version": generation.schema_version,
        "generation_id": generation.generation_id,
        "index_revision": generation.index_revision,
        "resource_revision": generation.scene.resource_revision,
        "scene_graph_revision": generation.scene.scene_graph_revision,
        "freshness": "current",
        "status": if partial_reasons.is_empty() { "exact" } else { "partial" },
        "scene": scene_view(selected.scene),
        "node": node,
        "query": {"selector": selected.canonical_selector, "limit": limit, "offset": offset},
        "properties": properties,
        "attached_script_resource_id": selected.definition.attached_script_entity_id,
        "resources": relations,
        "groups": groups,
        "connections": connections,
        "animation_references": animations,
        "diagnostics": diagnostics,
        "partial_reasons": partial_reasons,
        "truncated": has_more,
        "next_cursor": next_cursor,
        "validated_checkpoint": scene_checkpoint(generation),
        "evidence": {
            "source": "persistent_segment_index",
            "source_complete": generation.scene.source_complete,
            "authorities": ["packed_scene_state", "scene_composer"],
        },
    }))
}

fn scene_view(scene: &SceneEntity) -> Value {
    json!({
        "scene_id": scene.scene_entity_id,
        "resource_entity_id": scene.source_resource_entity_id,
        "uid": scene.uid,
        "path": scene.comparison_path,
        "content_generation": scene.content_generation,
        "identity_scope": scene.identity_scope,
        "base_scene_id": scene.base_scene_entity_id,
        "authority": scene.authority,
    })
}

fn scene_checkpoint(generation: &godot_codex_index_store::IndexGeneration) -> Value {
    json!({
        "editor_session_id": generation.scene.editor_session_id,
        "resource_revision": generation.scene.resource_revision,
        "scene_graph_revision": generation.scene.scene_graph_revision,
        "source_complete": generation.scene.source_complete,
        "snapshot_checksum": generation.scene.snapshot_checksum,
    })
}

fn scene_diagnostics(
    generation: &godot_codex_index_store::IndexGeneration,
    scene: &SceneEntity,
    subjects: &BTreeSet<&str>,
) -> Vec<Value> {
    generation
        .scene
        .relations
        .iter()
        .filter(|relation| {
            relation.relation == "diagnostic"
                && (relation.scene_entity_id.as_ref() == Some(&scene.scene_entity_id)
                    || relation.declaration_scope.as_ref() == Some(&scene.scene_entity_id)
                    || subjects.contains(relation.source.as_str()))
        })
        .map(|relation| {
            json!({
                "diagnostic_id": relation.relation_id,
                "code": relation.attributes.get("code"),
                "subject": relation.source,
                "detail": relation.attributes.get("detail"),
                "authority": relation.authority,
            })
        })
        .collect()
}

fn diagnostic_codes(diagnostics: &[Value]) -> BTreeSet<String> {
    diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.get("code").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn resource_success(
    snapshot: &IndexReadSnapshot,
    tool: CursorTool,
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
            CursorTool::Dependencies => dependency_view(snapshot, edge),
            CursorTool::Owners => owner_view(snapshot, edge),
            CursorTool::SceneGraph
            | CursorTool::InspectNode
            | CursorTool::SearchSymbols
            | CursorTool::InspectSymbol => unreachable!("resource tool"),
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
        CursorTool::Dependencies => "dependencies",
        CursorTool::Owners => "owners",
        CursorTool::SceneGraph
        | CursorTool::InspectNode
        | CursorTool::SearchSymbols
        | CursorTool::InspectSymbol => unreachable!("resource tool"),
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

fn scene_index_error(error: SceneIndexReadError) -> CallToolResult {
    match error {
        SceneIndexReadError::ProjectNotBound => {
            structured_error("project_not_bound", "Godot project is not bound", true)
        }
        SceneIndexReadError::NotReady => structured_error(
            "index_not_ready",
            "scene index has no committed generation",
            true,
        ),
        SceneIndexReadError::NotCurrent => structured_error(
            "index_not_current",
            "scene index freshness is not confirmed",
            true,
        ),
        SceneIndexReadError::CapabilityUnavailable => structured_error(
            "capability_unavailable",
            "Bridge scene graph capability is unavailable",
            true,
        ),
    }
}

fn script_index_error(error: ScriptIndexReadError) -> CallToolResult {
    match error {
        ScriptIndexReadError::ProjectNotBound => {
            structured_error("project_not_bound", "Godot project is not bound", true)
        }
        ScriptIndexReadError::NotReady => structured_error(
            "index_not_ready",
            "script index has no committed generation",
            true,
        ),
        ScriptIndexReadError::NotCurrent => structured_error(
            "index_not_current",
            "script index freshness is not confirmed",
            true,
        ),
        ScriptIndexReadError::CapabilityUnavailable => structured_error(
            "capability_unavailable",
            "Bridge script graph capability is unavailable",
            true,
        ),
    }
}

fn script_store_error(error: StoreError) -> CallToolResult {
    match error {
        StoreError::ScriptSymbolNotFound => structured_error(
            "symbol_not_found",
            "symbol selector was not found in the current script index",
            false,
        ),
        StoreError::QueryOffsetOutOfRange => structured_error(
            "stale_cursor",
            "cursor offset is outside the current script generation",
            false,
        ),
        StoreError::NotReady => structured_error(
            "index_not_ready",
            "script index has no committed generation",
            true,
        ),
        _ => structured_error(
            "index_not_current",
            "script index could not provide a current immutable page",
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
        self.resource_query(input, CursorTool::Dependencies)
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
        self.resource_query(input, CursorTool::Owners)
    }

    #[tool(
        description = "Return the canonical composed node graph for one indexed PackedScene",
        annotations(
            title = "Godot scene graph",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_get_scene_graph(
        &self,
        Parameters(input): Parameters<SceneGraphInput>,
    ) -> CallToolResult {
        self.scene_graph_query(input)
    }

    #[tool(
        description = "Inspect one canonical scene node, its effective properties, provenance, resources, groups, signals, and animations",
        annotations(
            title = "Godot indexed node",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_inspect_node(
        &self,
        Parameters(input): Parameters<InspectNodeInput>,
    ) -> CallToolResult {
        self.inspect_node_query(input)
    }

    #[tool(
        description = "Search deterministic saved-script declarations by exact name or prefix with optional language, kind, and script filters",
        annotations(
            title = "Godot script symbols",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_search_symbols(
        &self,
        Parameters(input): Parameters<SearchSymbolsInput>,
    ) -> CallToolResult {
        self.search_symbols_query(input)
    }

    #[tool(
        description = "Inspect one canonical saved-script declaration, its forward semantic relations, scene attachments, diagnostics, and source evidence",
        annotations(
            title = "Godot script symbol",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_inspect_symbol(
        &self,
        Parameters(input): Parameters<InspectSymbolInput>,
    ) -> CallToolResult {
        self.inspect_symbol_query(input)
    }
}

#[tool_handler]
impl ServerHandler for GodotMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2025_11_25)
            .with_instructions(
                "Read-only, project-scoped Godot editor, resource, composed scene, and saved-script symbol context. Results are returned only from checksum-verified current snapshots and immutable index generations.",
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
        SceneAnimationReference, SceneAnimationResolution, SceneConnection, SceneDomainGeneration,
        SceneGroupMembership, SceneIdentityScope, ScenePropertyOrigin, ScriptAdapterProfile,
        ScriptAdapterStatus, ScriptConfidence, ScriptDiagnosticAuthority, ScriptDiagnosticSeverity,
        ScriptDomainGeneration, ScriptIdentityScope, ScriptModifier, ScriptReference,
        ScriptRelationAuthority, ScriptTypeState, ScriptVisibility, SegmentStore, SourceDocument,
    };
    use godot_codex_resource_indexer::{ResourceIndexReader, SceneIndexReader, ScriptIndexReader};
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
        indexed_server_fixture(false, false, false)
    }

    fn indexed_resource_server_fixture(include_missing: bool) -> GodotMcpServer {
        indexed_server_fixture(include_missing, false, false)
    }

    fn indexed_semantic_server() -> GodotMcpServer {
        indexed_server_fixture(false, true, false)
    }

    fn indexed_script_server() -> GodotMcpServer {
        indexed_server_fixture(false, true, true)
    }

    fn scene_domain_fixture() -> SceneDomainGeneration {
        const SCENE: &str = "godot:scene:uid:v1:testscene";
        const ROOT: &str = "godot:node:scene-id:v1:root";
        const CHILD: &str = "godot:node:scene-id:v1:player";
        const ROOT_OCCURRENCE: &str = "godot:node-occurrence:v1:root";
        const CHILD_OCCURRENCE: &str = "godot:node-occurrence:v1:player";
        let attributes = |path: &str| {
            [
                ("editable".to_owned(), json!(true)),
                ("effective_path".to_owned(), json!(path)),
                ("instance_chain".to_owned(), json!([])),
                ("internal".to_owned(), json!(false)),
            ]
            .into_iter()
            .collect()
        };
        let mut domain = SceneDomainGeneration {
            editor_session_id: "editor:0123456789abcdef0123456789abcdef".to_owned(),
            resource_revision: 1,
            scene_graph_revision: 3,
            source_complete: true,
            snapshot_checksum: format!("sha256:{}", "e".repeat(64)),
            scenes: vec![SceneEntity {
                scene_entity_id: SCENE.to_owned(),
                source_resource_entity_id: None,
                uid: Some("uid://scene".to_owned()),
                comparison_path: "res://main.tscn".to_owned(),
                content_generation: format!("sha256:{}", "f".repeat(64)),
                identity_scope: SceneIdentityScope::Persistent,
                base_scene_entity_id: None,
                authority: "packed_scene_state".to_owned(),
                resource_revision: 1,
                scene_graph_revision: 3,
            }],
            nodes: vec![
                SceneNode {
                    node_entity_id: ROOT.to_owned(),
                    scene_entity_id: SCENE.to_owned(),
                    node_path: ".".to_owned(),
                    identity_scope: SceneIdentityScope::Persistent,
                    unique_scene_id: Some(1),
                    parent_node_entity_id: None,
                    owner_node_entity_id: None,
                    name: "Main".to_owned(),
                    godot_type: "Node".to_owned(),
                    node_index: 0,
                    owned: true,
                    internal: false,
                    attached_script_entity_id: None,
                    instance_scene_entity_id: None,
                    authority: "packed_scene_state".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 3,
                },
                SceneNode {
                    node_entity_id: CHILD.to_owned(),
                    scene_entity_id: SCENE.to_owned(),
                    node_path: "Player".to_owned(),
                    identity_scope: SceneIdentityScope::Persistent,
                    unique_scene_id: Some(2),
                    parent_node_entity_id: Some(ROOT.to_owned()),
                    owner_node_entity_id: Some(ROOT.to_owned()),
                    name: "Player".to_owned(),
                    godot_type: "CharacterBody2D".to_owned(),
                    node_index: 0,
                    owned: true,
                    internal: false,
                    attached_script_entity_id: Some("entity-b".to_owned()),
                    instance_scene_entity_id: None,
                    authority: "packed_scene_state".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 3,
                },
            ],
            properties: vec![
                SceneProperty {
                    property_id: "property-health".to_owned(),
                    scene_entity_id: SCENE.to_owned(),
                    subject_entity_id: CHILD_OCCURRENCE.to_owned(),
                    name: "health".to_owned(),
                    value_type: "int".to_owned(),
                    value: json!(100),
                    truncated: false,
                    declaring_scene_entity_id: SCENE.to_owned(),
                    declaring_node_entity_id: CHILD.to_owned(),
                    origin: ScenePropertyOrigin::Inherited,
                    overridden_property_id: None,
                    authority: "scene_composer".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 3,
                },
                SceneProperty {
                    property_id: "property-speed".to_owned(),
                    scene_entity_id: SCENE.to_owned(),
                    subject_entity_id: CHILD_OCCURRENCE.to_owned(),
                    name: "speed".to_owned(),
                    value_type: "float".to_owned(),
                    value: json!(275.0),
                    truncated: false,
                    declaring_scene_entity_id: SCENE.to_owned(),
                    declaring_node_entity_id: CHILD.to_owned(),
                    origin: ScenePropertyOrigin::InstanceOverride,
                    overridden_property_id: Some("property-base-speed".to_owned()),
                    authority: "scene_composer".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 3,
                },
            ],
            relations: vec![
                SceneRelation {
                    relation_id: "occurrence-root".to_owned(),
                    scene_entity_id: Some(SCENE.to_owned()),
                    relation: "occurrence".to_owned(),
                    source: ROOT_OCCURRENCE.to_owned(),
                    target: Some(ROOT.to_owned()),
                    declaration_scope: Some(SCENE.to_owned()),
                    attributes: attributes("."),
                    authority: "scene_composer".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 3,
                },
                SceneRelation {
                    relation_id: "occurrence-player".to_owned(),
                    scene_entity_id: Some(SCENE.to_owned()),
                    relation: "occurrence".to_owned(),
                    source: CHILD_OCCURRENCE.to_owned(),
                    target: Some(CHILD.to_owned()),
                    declaration_scope: Some(SCENE.to_owned()),
                    attributes: attributes("Player"),
                    authority: "scene_composer".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 3,
                },
                SceneRelation {
                    relation_id: "resource-player-script".to_owned(),
                    scene_entity_id: Some(SCENE.to_owned()),
                    relation: "attached_script".to_owned(),
                    source: CHILD_OCCURRENCE.to_owned(),
                    target: Some("entity-b".to_owned()),
                    declaration_scope: Some(SCENE.to_owned()),
                    attributes: Default::default(),
                    authority: "scene_composer".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 3,
                },
                SceneRelation {
                    relation_id: "subresource-player-material".to_owned(),
                    scene_entity_id: Some(SCENE.to_owned()),
                    relation: "subresource".to_owned(),
                    source: "godot:subresource:scene-id:v1:material".to_owned(),
                    target: Some(SCENE.to_owned()),
                    declaration_scope: Some(SCENE.to_owned()),
                    attributes: [
                        ("resource_type".to_owned(), json!("Gradient")),
                        ("identity_scope".to_owned(), json!("persistent")),
                        ("scene_unique_id".to_owned(), json!("Gradient_material")),
                        ("ownership_paths".to_owned(), json!(["Player:material"])),
                    ]
                    .into_iter()
                    .collect(),
                    authority: "packed_scene_state".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 3,
                },
                SceneRelation {
                    relation_id: "project-context-main-scene".to_owned(),
                    scene_entity_id: None,
                    relation: "project_context".to_owned(),
                    source: "application/run/main_scene".to_owned(),
                    target: None,
                    declaration_scope: None,
                    attributes: [
                        ("value_type".to_owned(), json!("string")),
                        ("value".to_owned(), json!("uid://scene")),
                        ("truncated".to_owned(), json!(false)),
                    ]
                    .into_iter()
                    .collect(),
                    authority: "project_settings".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 3,
                },
                SceneRelation {
                    relation_id: "diagnostic-player".to_owned(),
                    scene_entity_id: Some(SCENE.to_owned()),
                    relation: "diagnostic".to_owned(),
                    source: CHILD_OCCURRENCE.to_owned(),
                    target: None,
                    declaration_scope: Some(SCENE.to_owned()),
                    attributes: [
                        ("code".to_owned(), json!("unresolved_export_metadata")),
                        ("detail".to_owned(), json!("Player:custom_value")),
                    ]
                    .into_iter()
                    .collect(),
                    authority: "scene_composer".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 3,
                },
            ],
            connections: vec![SceneConnection {
                connection_id: "connection-ready".to_owned(),
                scene_entity_id: SCENE.to_owned(),
                emitter_node_entity_id: ROOT_OCCURRENCE.to_owned(),
                signal: "ready".to_owned(),
                receiver_node_entity_id: Some(CHILD_OCCURRENCE.to_owned()),
                method: "_on_ready".to_owned(),
                flags: 1,
                unbinds: 0,
                binds: Vec::new(),
                declaration_scope: SCENE.to_owned(),
                authority: "packed_scene_state".to_owned(),
                resource_revision: 1,
                scene_graph_revision: 3,
            }],
            groups: vec![SceneGroupMembership {
                membership_id: "group-player".to_owned(),
                scene_entity_id: SCENE.to_owned(),
                member_node_entity_id: CHILD_OCCURRENCE.to_owned(),
                group: "players".to_owned(),
                declaration_scope: SCENE.to_owned(),
                authority: "packed_scene_state".to_owned(),
                resource_revision: 1,
                scene_graph_revision: 3,
            }],
            animations: vec![SceneAnimationReference {
                animation_reference_id: "animation-player-position".to_owned(),
                scene_entity_id: SCENE.to_owned(),
                mixer_node_entity_id: ROOT_OCCURRENCE.to_owned(),
                library: "".to_owned(),
                animation: "walk".to_owned(),
                track_index: 0,
                node_path: "Player:position".to_owned(),
                target_node_entity_id: Some(CHILD_OCCURRENCE.to_owned()),
                resolution: SceneAnimationResolution::Resolved,
                authority: "packed_scene_state".to_owned(),
                resource_revision: 1,
                scene_graph_revision: 3,
            }],
            validation_digest: String::new(),
        };
        domain.canonicalize();
        domain.validation_digest = domain.compute_validation_digest();
        domain
    }

    fn opaque_test_id(prefix: &str, marker: char) -> String {
        format!("{prefix}{}", marker.to_string().repeat(43))
    }

    fn script_resource_id() -> String {
        opaque_test_id("godot:resource:uid:v1:", 'S')
    }

    fn named_symbol_id(marker: char) -> String {
        opaque_test_id("godot:script-symbol:named:v1:", marker)
    }

    fn script_domain_fixture() -> ScriptDomainGeneration {
        const SCRIPT_PATH: &str = "res://scripts/player.gd";
        const PLAYER_OCCURRENCE: &str = "godot:node-occurrence:v1:player";
        let script_id = script_resource_id();
        let script_symbol_id = named_symbol_id('R');
        let base_class_id = named_symbol_id('B');
        let base_method_id = named_symbol_id('M');
        let player_class_id = named_symbol_id('P');
        let attack_id = named_symbol_id('A');
        let attack_special_id = named_symbol_id('T');
        let local_id = opaque_test_id("godot:script-symbol:content-revision:v1:", 'L');
        let content_sha256 = format!("sha256:{}", "a".repeat(64));
        let source_range = |start_byte: u64, end_byte: u64| ScriptSourceRange {
            path: SCRIPT_PATH.to_owned(),
            content_sha256: content_sha256.clone(),
            start_byte,
            end_byte,
            start_line: 1,
            start_column: u32::try_from(start_byte + 1).unwrap(),
            end_line: 1,
            end_column: u32::try_from(end_byte + 1).unwrap(),
        };
        let symbol = |symbol_id: String,
                      kind: ScriptSymbolKind,
                      name: &str,
                      qualified_key: &str,
                      owner_symbol_id: Option<String>,
                      range: ScriptSourceRange| ScriptSymbol {
            symbol_id,
            script_resource_id: script_id.clone(),
            language: ScriptLanguage::Gdscript,
            kind,
            name: Some(name.to_owned()),
            qualified_key: qualified_key.to_owned(),
            owner_symbol_id,
            identity_scope: ScriptIdentityScope::Persistent,
            signature: None,
            type_name: None,
            type_state: ScriptTypeState::Dynamic,
            visibility: ScriptVisibility::Public,
            modifiers: Vec::new(),
            declaration_range: range,
            documentation_present: false,
            script_graph_revision: 5,
        };
        let mut symbols = vec![
            symbol(
                script_symbol_id,
                ScriptSymbolKind::Script,
                "player",
                "script",
                None,
                source_range(0, 1),
            ),
            symbol(
                base_class_id.clone(),
                ScriptSymbolKind::Class,
                "BasePlayer",
                "class:BasePlayer",
                None,
                source_range(2, 12),
            ),
            symbol(
                base_method_id.clone(),
                ScriptSymbolKind::Method,
                "base_attack",
                "class:BasePlayer/method:base_attack",
                Some(base_class_id.clone()),
                source_range(13, 19),
            ),
            symbol(
                player_class_id.clone(),
                ScriptSymbolKind::Class,
                "Player",
                "class:Player",
                None,
                source_range(20, 26),
            ),
            symbol(
                attack_id.clone(),
                ScriptSymbolKind::Method,
                "attack",
                "class:Player/method:attack",
                Some(player_class_id.clone()),
                source_range(27, 33),
            ),
            symbol(
                attack_special_id.clone(),
                ScriptSymbolKind::Method,
                "attack_special",
                "class:Player/method:attack_special",
                Some(player_class_id.clone()),
                source_range(34, 48),
            ),
        ];
        let mut local = symbol(
            local_id,
            ScriptSymbolKind::Local,
            "attack_local",
            "class:Player/method:attack/local:attack_local@49",
            Some(attack_id.clone()),
            source_range(49, 61),
        );
        local.identity_scope = ScriptIdentityScope::ContentRevision;
        symbols.push(local);
        symbols
            .iter_mut()
            .find(|symbol| symbol.symbol_id == attack_id)
            .expect("attack symbol")
            .signature = Some("attack(target: Node) -> void".to_owned());
        symbols
            .iter_mut()
            .find(|symbol| symbol.symbol_id == attack_id)
            .expect("attack symbol")
            .modifiers = vec![ScriptModifier::Override];

        let relation = |relation_id: &str,
                        source: ScriptEndpoint,
                        predicate: ScriptPredicate,
                        target: Option<ScriptEndpoint>,
                        confidence: ScriptConfidence,
                        evidence_range: Option<ScriptSourceRange>,
                        authority: ScriptRelationAuthority| ScriptRelation {
            relation_id: relation_id.to_owned(),
            script_resource_id: script_id.clone(),
            source,
            predicate,
            target,
            confidence,
            evidence_range,
            detail: None,
            authority,
            script_graph_revision: 5,
        };
        let symbol_endpoint = |symbol_id: &str| ScriptEndpoint::Symbol {
            symbol_id: symbol_id.to_owned(),
        };
        let relations = vec![
            relation(
                "relation-attach-player",
                ScriptEndpoint::SceneNode {
                    node_entity_id: PLAYER_OCCURRENCE.to_owned(),
                },
                ScriptPredicate::AttachesScript,
                Some(symbol_endpoint(&player_class_id)),
                ScriptConfidence::Exact,
                None,
                ScriptRelationAuthority::SceneState,
            ),
            relation(
                "relation-call-exact",
                symbol_endpoint(&attack_id),
                ScriptPredicate::Calls,
                Some(symbol_endpoint(&attack_special_id)),
                ScriptConfidence::Exact,
                Some(source_range(62, 68)),
                ScriptRelationAuthority::GdscriptParserAnalyzer,
            ),
            relation(
                "relation-call-dynamic",
                symbol_endpoint(&attack_id),
                ScriptPredicate::Calls,
                None,
                ScriptConfidence::Dynamic,
                Some(source_range(69, 75)),
                ScriptRelationAuthority::GdscriptParserAnalyzer,
            ),
            relation(
                "relation-contains-attack",
                symbol_endpoint(&player_class_id),
                ScriptPredicate::Contains,
                Some(symbol_endpoint(&attack_id)),
                ScriptConfidence::Exact,
                Some(source_range(27, 33)),
                ScriptRelationAuthority::GdscriptParserAnalyzer,
            ),
            relation(
                "relation-inherits-base",
                symbol_endpoint(&player_class_id),
                ScriptPredicate::Inherits,
                Some(symbol_endpoint(&base_class_id)),
                ScriptConfidence::Exact,
                Some(source_range(20, 26)),
                ScriptRelationAuthority::GdscriptParserAnalyzer,
            ),
            relation(
                "relation-overrides-attack",
                symbol_endpoint(&attack_id),
                ScriptPredicate::Overrides,
                Some(symbol_endpoint(&base_method_id)),
                ScriptConfidence::Exact,
                Some(source_range(27, 33)),
                ScriptRelationAuthority::GdscriptParserAnalyzer,
            ),
        ];
        let references = relations
            .iter()
            .filter_map(|relation| {
                relation.target.as_ref().map(|target| ScriptReference {
                    reference_id: format!("reference-{}", relation.relation_id),
                    relation_id: relation.relation_id.clone(),
                    script_resource_id: relation.script_resource_id.clone(),
                    source: relation.source.clone(),
                    predicate: relation.predicate,
                    target: target.clone(),
                    authority: relation.authority,
                    script_graph_revision: 5,
                })
            })
            .collect();
        let mut domain = ScriptDomainGeneration {
            editor_session_id: "editor:0123456789abcdef0123456789abcdef".to_owned(),
            resource_revision: 1,
            scene_graph_revision: 3,
            script_graph_revision: 5,
            source_complete: true,
            snapshot_checksum: "b".repeat(64),
            semantic_digest: format!("sha256:{}", "c".repeat(64)),
            documents: vec![ScriptDocument {
                script_resource_id: script_id.clone(),
                path: SCRIPT_PATH.to_owned(),
                language: ScriptLanguage::Gdscript,
                content_sha256: content_sha256.clone(),
                adapter_profile: ScriptAdapterProfile::GdscriptParserAnalyzerV1,
                completeness: ScriptCompleteness::Complete,
                resource_revision: 1,
                script_graph_revision: 5,
            }],
            symbols,
            relations,
            references,
            diagnostics: vec![ScriptDiagnostic {
                diagnostic_id: "godot:script-diagnostic:v1:dynamic-call".to_owned(),
                script_resource_id: script_id,
                language: ScriptLanguage::Gdscript,
                content_sha256: content_sha256.clone(),
                code: "DYNAMIC_CALL_TARGET".to_owned(),
                severity: ScriptDiagnosticSeverity::Warning,
                safe_message: "Dynamic call target is not statically resolvable.".to_owned(),
                range: Some(source_range(69, 75)),
                authority: ScriptDiagnosticAuthority::GdscriptAnalyzer,
                script_graph_revision: 5,
            }],
            adapter_statuses: vec![
                ScriptAdapterStatus {
                    language: ScriptLanguage::Gdscript,
                    availability: ScriptAdapterAvailability::Available,
                    profile: Some(ScriptAdapterProfile::GdscriptParserAnalyzerV1),
                    version: Some("4.8".to_owned()),
                    diagnostic: None,
                },
                ScriptAdapterStatus {
                    language: ScriptLanguage::Csharp,
                    availability: ScriptAdapterAvailability::DiscoveryOnly,
                    profile: Some(ScriptAdapterProfile::CsharpDiscoveryOnlyV1),
                    version: Some("1.0".to_owned()),
                    diagnostic: None,
                },
            ],
            validation_digest: String::new(),
        };
        domain.canonicalize();
        domain.validation_digest = domain.compute_validation_digest();
        domain
    }

    fn indexed_server_fixture(
        include_missing: bool,
        include_scene: bool,
        include_script: bool,
    ) -> GodotMcpServer {
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
        let mut resources = vec![resource("a"), resource("b"), resource("c")];
        if include_script {
            resources.push(ResourceEntity {
                entity_id: script_resource_id(),
                identity_input: "uid://player-script".to_owned(),
                uid: Some("uid://player-script".to_owned()),
                display_path: "res://scripts/player.gd".to_owned(),
                comparison_path: "res://scripts/player.gd".to_owned(),
                identity_strength: IdentityStrength::ResourceUid,
                resource_type: "GDScript".to_owned(),
                source_kind: "source".to_owned(),
                import_state: "not_imported".to_owned(),
                authority: "editor_file_system".to_owned(),
                content_generation: Some(format!("sha256:{}", "a".repeat(64))),
                mtime_ns: 1,
                byte_size: 76,
                validity: RecordValidity::Valid,
                resource_revision: 1,
            });
        }
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
            scene: if include_scene {
                scene_domain_fixture()
            } else {
                SceneDomainGeneration::default()
            },
            script: if include_script {
                script_domain_fixture()
            } else {
                ScriptDomainGeneration::default()
            },
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
        let scene_reader = if include_scene {
            SceneIndexReader::from_validated_store(
                &store,
                "editor:0123456789abcdef0123456789abcdef",
                3,
            )
            .unwrap()
        } else {
            SceneIndexReader::new()
        };
        let script_reader = if include_script {
            ScriptIndexReader::from_validated_store(
                &store,
                "editor:0123456789abcdef0123456789abcdef",
                1,
                3,
                5,
            )
            .unwrap()
        } else {
            ScriptIndexReader::new()
        };
        GodotMcpServer::with_all_indexes(
            SnapshotReplicator::new(),
            reader,
            scene_reader,
            script_reader,
        )
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
    fn exactly_nine_tools_are_declared_read_only_with_closed_schemas() {
        let server = GodotMcpServer::new(SnapshotReplicator::new());
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 9);
        let names: BTreeSet<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert!(names.contains("godot_get_scene_graph"));
        assert!(names.contains("godot_inspect_node"));
        assert!(names.contains("godot_search_symbols"));
        assert!(names.contains("godot_inspect_symbol"));
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
        let tools = server.tool_router.list_all();
        let search = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "godot_search_symbols")
            .expect("symbol search tool");
        let required = search.input_schema["required"]
            .as_array()
            .expect("required fields");
        assert!(required.contains(&json!("query")));
        assert!(required.contains(&json!("match")));
        assert_eq!(
            search.input_schema["$defs"]["SymbolMatchInput"]["enum"],
            json!(["exact", "prefix"])
        );
        assert_eq!(
            search.input_schema["$defs"]["ScriptSymbolKindInput"]["enum"],
            json!([
                "script",
                "class",
                "method",
                "function",
                "property",
                "constant",
                "enum",
                "enum_member",
                "signal"
            ])
        );
    }

    #[test]
    fn symbol_search_filters_and_paginates_one_script_generation() {
        let server = indexed_script_server();
        let first = server.godot_search_symbols(Parameters(SearchSymbolsInput {
            query: "attack".to_owned(),
            match_mode: SymbolMatchInput::Prefix,
            language: Some(ScriptLanguageInput::Gdscript),
            kind: Some(ScriptSymbolKindInput::Method),
            script: Some("res://scripts/player.gd".to_owned()),
            limit: 1,
            cursor: None,
        }));
        assert_ne!(first.is_error, Some(true));
        let first = structured_content(&first).expect("first symbol page");
        assert_eq!(first["generation_id"], "generation:test");
        assert_eq!(first["resource_revision"], 1);
        assert_eq!(first["scene_graph_revision"], 3);
        assert_eq!(first["script_graph_revision"], 5);
        assert_eq!(first["symbols"][0]["name"], "attack");
        assert_eq!(first["total_matches"], 2);
        assert_eq!(first["truncated"], true);
        assert_eq!(first["status"], "partial");
        assert!(
            first["partial_reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("DYNAMIC_CALL_TARGET"))
        );
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();

        let second = server.godot_search_symbols(Parameters(SearchSymbolsInput {
            query: "attack".to_owned(),
            match_mode: SymbolMatchInput::Prefix,
            language: Some(ScriptLanguageInput::Gdscript),
            kind: Some(ScriptSymbolKindInput::Method),
            script: Some("res://scripts/player.gd".to_owned()),
            limit: 1,
            cursor: Some(cursor.clone()),
        }));
        let second = structured_content(&second).expect("second symbol page");
        assert_eq!(second["symbols"][0]["name"], "attack_special");
        assert_eq!(second["truncated"], false);
        assert_eq!(second["next_cursor"], Value::Null);
        assert!(
            second["symbols"]
                .as_array()
                .unwrap()
                .iter()
                .all(|symbol| symbol["name"] != "attack_local")
        );

        let exact = server.godot_search_symbols(Parameters(SearchSymbolsInput {
            query: "Player".to_owned(),
            match_mode: SymbolMatchInput::Exact,
            language: Some(ScriptLanguageInput::Gdscript),
            kind: Some(ScriptSymbolKindInput::Class),
            script: Some("uid://player-script".to_owned()),
            limit: 50,
            cursor: None,
        }));
        let exact = structured_content(&exact).expect("exact symbol search");
        assert_eq!(exact["total_matches"], 1);
        assert_eq!(exact["symbols"][0]["qualified_name"], "class:Player");

        let changed_filter = server.godot_search_symbols(Parameters(SearchSymbolsInput {
            query: "attack".to_owned(),
            match_mode: SymbolMatchInput::Prefix,
            language: Some(ScriptLanguageInput::Gdscript),
            kind: None,
            script: Some("res://scripts/player.gd".to_owned()),
            limit: 1,
            cursor: Some(cursor.clone()),
        }));
        assert_eq!(
            structured_content(&changed_filter)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("stale_cursor")
        );

        let cross_tool = server.godot_inspect_symbol(Parameters(InspectSymbolInput {
            symbol_id: Some(attack_symbol_id()),
            script: None,
            qualified_name: None,
            limit: 1,
            cursor: Some(cursor),
        }));
        assert_eq!(
            structured_content(&cross_tool)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("stale_cursor")
        );
    }

    fn attack_symbol_id() -> String {
        named_symbol_id('A')
    }

    #[test]
    fn symbol_search_reports_discovery_only_language_as_partial() {
        let result = indexed_script_server().godot_search_symbols(Parameters(SearchSymbolsInput {
            query: "Enemy".to_owned(),
            match_mode: SymbolMatchInput::Prefix,
            language: Some(ScriptLanguageInput::Csharp),
            kind: None,
            script: None,
            limit: 50,
            cursor: None,
        }));
        assert_ne!(result.is_error, Some(true));
        let content = structured_content(&result).unwrap();
        assert_eq!(content["symbols"], json!([]));
        assert_eq!(content["status"], "partial");
        assert!(
            content["partial_reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("csharp_semantics_discovery_only"))
        );
    }

    #[test]
    fn symbol_inspection_returns_forward_relations_attachments_and_safe_evidence() {
        let server = indexed_script_server();
        let result = server.godot_inspect_symbol(Parameters(InspectSymbolInput {
            symbol_id: Some(attack_symbol_id()),
            script: None,
            qualified_name: None,
            limit: 50,
            cursor: None,
        }));
        assert_ne!(result.is_error, Some(true));
        let content = structured_content(&result).unwrap();
        assert_eq!(content["declaration"]["name"], "attack");
        assert_eq!(
            content["declaration"]["signature"],
            "attack(target: Node) -> void"
        );
        assert_eq!(content["owner"]["name"], "Player");
        assert_eq!(content["scene_attachments"].as_array().unwrap().len(), 1);
        assert_eq!(
            content["scene_attachments"][0]["source"]["node_path"],
            "Player"
        );
        let outgoing = content["outgoing_relations"].as_array().unwrap();
        assert!(outgoing.iter().any(|relation| {
            relation["predicate"] == "calls" && relation["confidence"] == "exact"
        }));
        assert!(outgoing.iter().any(|relation| {
            relation["predicate"] == "calls" && relation["confidence"] == "dynamic"
        }));
        assert!(
            outgoing
                .iter()
                .any(|relation| relation["predicate"] == "overrides")
        );
        assert!(
            outgoing
                .iter()
                .all(|relation| { relation["relation_id"] != "relation-contains-attack" })
        );
        assert_eq!(content["diagnostics"][0]["code"], "DYNAMIC_CALL_TARGET");
        assert_eq!(
            content["declaration"]["declaration"]["path"],
            "res://scripts/player.gd"
        );
        assert_eq!(content["status"], "partial");
        let serialized = content.to_string();
        assert!(!serialized.contains("/Users/"));
        assert!(!serialized.contains("source_excerpt"));
        assert!(!serialized.contains("authorization"));
    }

    #[test]
    fn symbol_inspection_supports_qualified_selector_and_isolation() {
        let server = indexed_script_server();
        let first = server.godot_inspect_symbol(Parameters(InspectSymbolInput {
            symbol_id: None,
            script: Some("uid://player-script".to_owned()),
            qualified_name: Some("class:Player/method:attack".to_owned()),
            limit: 1,
            cursor: None,
        }));
        let first = structured_content(&first).expect("first inspection page");
        assert_eq!(first["declaration"]["name"], "attack");
        assert_eq!(first["truncated"], true);
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();

        let second = server.godot_inspect_symbol(Parameters(InspectSymbolInput {
            symbol_id: None,
            script: Some("uid://player-script".to_owned()),
            qualified_name: Some("class:Player/method:attack".to_owned()),
            limit: 1,
            cursor: Some(cursor.clone()),
        }));
        let second = structured_content(&second).expect("second inspection page");
        assert_eq!(second["query"]["offset"], 1);

        let selector_changed = server.godot_inspect_symbol(Parameters(InspectSymbolInput {
            symbol_id: Some(attack_symbol_id()),
            script: None,
            qualified_name: None,
            limit: 1,
            cursor: Some(cursor),
        }));
        assert_eq!(
            structured_content(&selector_changed)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("stale_cursor")
        );
    }

    #[test]
    fn symbol_tools_return_stable_validation_and_availability_errors() {
        let unavailable = GodotMcpServer::new(SnapshotReplicator::new()).godot_search_symbols(
            Parameters(SearchSymbolsInput {
                query: "Player".to_owned(),
                match_mode: SymbolMatchInput::Exact,
                language: None,
                kind: None,
                script: None,
                limit: 50,
                cursor: None,
            }),
        );
        assert_eq!(
            structured_content(&unavailable)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("project_not_bound")
        );
        let invalid_query =
            indexed_script_server().godot_search_symbols(Parameters(SearchSymbolsInput {
                query: String::new(),
                match_mode: SymbolMatchInput::Exact,
                language: None,
                kind: None,
                script: None,
                limit: 50,
                cursor: None,
            }));
        assert_eq!(
            structured_content(&invalid_query)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("invalid_query")
        );
        let invalid_limit =
            indexed_script_server().godot_search_symbols(Parameters(SearchSymbolsInput {
                query: "Player".to_owned(),
                match_mode: SymbolMatchInput::Exact,
                language: None,
                kind: None,
                script: None,
                limit: 201,
                cursor: None,
            }));
        assert_eq!(
            structured_content(&invalid_limit)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("invalid_limit")
        );
        let invalid_selector =
            indexed_script_server().godot_inspect_symbol(Parameters(InspectSymbolInput {
                symbol_id: Some(attack_symbol_id()),
                script: Some("res://scripts/player.gd".to_owned()),
                qualified_name: Some("class:Player/method:attack".to_owned()),
                limit: 50,
                cursor: None,
            }));
        assert_eq!(
            structured_content(&invalid_selector)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("invalid_query")
        );
        let missing_symbol =
            indexed_script_server().godot_inspect_symbol(Parameters(InspectSymbolInput {
                symbol_id: Some(named_symbol_id('Z')),
                script: None,
                qualified_name: None,
                limit: 50,
                cursor: None,
            }));
        assert_eq!(
            structured_content(&missing_symbol)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("symbol_not_found")
        );
        let unsafe_script =
            indexed_script_server().godot_inspect_symbol(Parameters(InspectSymbolInput {
                symbol_id: None,
                script: Some("res://../player.gd".to_owned()),
                qualified_name: Some("class:Player".to_owned()),
                limit: 50,
                cursor: None,
            }));
        assert_eq!(
            structured_content(&unsafe_script)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("invalid_path")
        );
        let missing_script =
            indexed_script_server().godot_inspect_symbol(Parameters(InspectSymbolInput {
                symbol_id: None,
                script: Some("uid://missing-script".to_owned()),
                qualified_name: Some("class:Player".to_owned()),
                limit: 50,
                cursor: None,
            }));
        assert_eq!(
            structured_content(&missing_script)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("script_not_found")
        );
    }

    #[test]
    fn scene_graph_paginates_one_composed_generation_without_duplicates() {
        let server = indexed_semantic_server();
        let first = server.godot_get_scene_graph(Parameters(SceneGraphInput {
            scene: "res://main.tscn".to_owned(),
            limit: 1,
            cursor: None,
        }));
        assert_ne!(first.is_error, Some(true));
        let first = structured_content(&first).unwrap();
        assert_eq!(first["generation_id"], "generation:test");
        assert_eq!(first["scene_graph_revision"], 3);
        assert_eq!(first["scene"]["uid"], "uid://scene");
        assert_eq!(first["nodes"][0]["node_path"], ".");
        assert_eq!(
            first["project_context"][0]["key"],
            "application/run/main_scene"
        );
        assert_eq!(first["project_context"][0]["value"]["value"], "uid://scene");
        assert_eq!(first["project_context_truncated"], false);
        assert_eq!(first["status"], "partial");
        assert_eq!(first["partial_reasons"][0], "unresolved_export_metadata");
        assert_eq!(first["truncated"], true);
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();

        let second = server.godot_get_scene_graph(Parameters(SceneGraphInput {
            scene: "uid://scene".to_owned(),
            limit: 1,
            cursor: Some(cursor.clone()),
        }));
        assert_eq!(
            structured_content(&second)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("stale_cursor"),
            "a cursor is bound to the canonical selector used for its first page"
        );

        let second = server.godot_get_scene_graph(Parameters(SceneGraphInput {
            scene: "res://main.tscn".to_owned(),
            limit: 1,
            cursor: Some(cursor.clone()),
        }));
        let second = structured_content(&second).unwrap();
        assert_eq!(second["nodes"][0]["node_path"], "Player");
        assert_eq!(second["nodes"][0]["groups"][0], "players");
        assert_eq!(
            second["nodes"][0]["parent_node_id"],
            "godot:node-occurrence:v1:root"
        );
        assert_eq!(second["next_cursor"], Value::Null);

        let cross_tool = server.godot_inspect_node(Parameters(InspectNodeInput {
            node_id: Some("godot:node-occurrence:v1:player".to_owned()),
            scene: None,
            node_path: None,
            limit: 1,
            cursor: Some(cursor),
        }));
        assert_eq!(
            structured_content(&cross_tool)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("stale_cursor")
        );
    }

    #[test]
    fn inspect_node_returns_effective_values_and_provenance() {
        let server = indexed_semantic_server();
        let first = server.godot_inspect_node(Parameters(InspectNodeInput {
            node_id: None,
            scene: Some("res://main.tscn".to_owned()),
            node_path: Some("Player".to_owned()),
            limit: 1,
            cursor: None,
        }));
        assert_ne!(first.is_error, Some(true));
        let first = structured_content(&first).unwrap();
        assert_eq!(first["node"]["node_path"], "Player");
        assert_eq!(first["properties"][0]["name"], "health");
        assert_eq!(first["properties"][0]["origin"], "inherited");
        assert_eq!(first["attached_script_resource_id"], "entity-b");
        assert_eq!(first["resources"][0]["kind"], "attached_script");
        assert!(
            first["resources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|relation| relation["kind"] == "subresource")
        );
        assert_eq!(first["groups"][0]["group"], "players");
        assert_eq!(first["connections"][0]["method"], "_on_ready");
        assert_eq!(first["animation_references"][0]["animation"], "walk");
        assert_eq!(first["status"], "partial");
        assert!(
            !first
                .to_string()
                .contains(std::env::temp_dir().to_string_lossy().as_ref())
        );
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();

        let second = server.godot_inspect_node(Parameters(InspectNodeInput {
            node_id: None,
            scene: Some("res://main.tscn".to_owned()),
            node_path: Some("Player".to_owned()),
            limit: 1,
            cursor: Some(cursor),
        }));
        let second = structured_content(&second).unwrap();
        assert_eq!(second["properties"][0]["name"], "speed");
        assert_eq!(second["properties"][0]["origin"], "instance_override");
        assert_eq!(
            second["properties"][0]["overridden_property_id"],
            "property-base-speed"
        );
        assert_eq!(second["truncated"], false);
    }

    #[test]
    fn scene_tools_enforce_strict_selectors_and_current_scene_index() {
        let unavailable = GodotMcpServer::new(SnapshotReplicator::new()).godot_get_scene_graph(
            Parameters(SceneGraphInput {
                scene: "res://main.tscn".to_owned(),
                limit: 50,
                cursor: None,
            }),
        );
        assert_eq!(
            structured_content(&unavailable)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("project_not_bound")
        );

        let invalid = indexed_semantic_server().godot_inspect_node(Parameters(InspectNodeInput {
            node_id: Some("godot:node-occurrence:v1:player".to_owned()),
            scene: Some("res://main.tscn".to_owned()),
            node_path: Some("Player".to_owned()),
            limit: 50,
            cursor: None,
        }));
        assert_eq!(
            structured_content(&invalid)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("invalid_query")
        );

        let unsafe_path =
            indexed_semantic_server().godot_inspect_node(Parameters(InspectNodeInput {
                node_id: None,
                scene: Some("res://main.tscn".to_owned()),
                node_path: Some("../Player".to_owned()),
                limit: 50,
                cursor: None,
            }));
        assert_eq!(
            structured_content(&unsafe_path)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("invalid_query")
        );
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
