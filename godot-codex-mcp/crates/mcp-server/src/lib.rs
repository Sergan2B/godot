mod change_set_tools;
mod connection_status;
mod cursor;
mod live_overlay;
mod output_schema;
mod runtime_overlay;
mod transaction_tools;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use change_set_tools::{
    ConfirmationPolicyInput, ConfirmationPolicyStore, LOW_RISK_MEMORY_ONLY_SCOPE,
    PrepareChangeSetInput, ValidationReportInput, report_page_value, session_grant_eligible,
    validation_report_error,
};
use connection_status::{
    CONNECTION_STATUS_URI, ConnectionStatusInput, IndexCoordinates, IndexObservation,
    ReplicaObservation, ServerConnectionObservation, connection_health,
};
use cursor::{CursorBinding, CursorCodec, CursorTool};
use godot_codex_bridge_client::{
    ApprovalBinding, ApprovalScope, BridgeError, BridgeFailureClass, Risk, RuntimeDomain,
    RuntimeEntity, RuntimeNode, RuntimeTarget, RuntimeViewportCapture,
};
use godot_codex_index_store::{
    ContextSummaryError, DependencyEdge, FIND_USAGES_DEFAULT_LIMIT, FindUsagesQuery,
    FindUsagesQueryError, FindUsagesScope, IndexRead, IndexReadSnapshot, RecordValidity,
    ResourceEntity, ResourceQuery, ResourceSelector, SceneEntity, SceneNode, SceneProperty,
    SceneRelation, ScriptAdapterAvailability, ScriptCompleteness, ScriptDiagnostic, ScriptDocument,
    ScriptEndpoint, ScriptLanguage, ScriptPredicate, ScriptRelation, ScriptSourceRange,
    ScriptSymbol, ScriptSymbolInspectionQuery, ScriptSymbolInspectionResult, ScriptSymbolKind,
    ScriptSymbolMatch, ScriptSymbolQuery, ScriptSymbolQueryResult, ScriptSymbolSelector,
    SemanticConfidence, SemanticEntityKind, SemanticQueryIndex, StoreError, build_project_summary,
    build_scene_summary, signal_entity_id,
};
use godot_codex_product::{
    CONNECTION_STATUS_MAX_BYTES, ComponentCondition, ConnectionHealth, ConnectionStatus,
    ProductStartupObservation,
};
use godot_codex_resource_indexer::{
    ResourceIndexReadError, ResourceIndexReader, ResourceIndexStaleReason, ResourceIndexStatus,
    SceneIndexReadError, SceneIndexReader, SceneIndexStaleReason, SceneIndexStatus,
    ScriptIndexReadError, ScriptIndexReader, ScriptIndexStaleReason, ScriptIndexStatus,
    SemanticIndexFreshness, SemanticIndexReadError, SemanticIndexReader, SemanticPartialCode,
    SemanticPartialDomain, SemanticPartialReason, normalize_resource_path,
};
use godot_codex_semantic_model::{
    ReplicaFailure, ReplicaStatus, SemanticSnapshot, SnapshotReplicator,
};
use godot_codex_transactions::{
    ApplyCommand, CheckAuthority, CheckOutcome, DiagnosticFingerprint, DiagnosticSeverity,
    DiagnosticSummary, ExpectedSemanticDelta, PrepareCommand, SemanticComparison,
    SemanticSnapshot as ValidationSemanticSnapshot, TransactionCoordinator, UndoCommand,
    ValidationCheck, ValidationCoordinator, ValidationPolicy, ValidationReportOutcome,
};
use live_overlay::{LiveOverlay, overlay_metadata};
use output_schema::AvailabilityDomain;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ListResourceTemplatesResult, ListResourcesResult,
        PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResult,
        Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use runtime_overlay::{CachedRuntimeSnapshot, RuntimeOverlay};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use transaction_tools::{
    ApplyTransactionInput, ChangeSetApprovalDecision, McpApprovalProvider,
    PrepareAttachScriptInput, PrepareConnectSignalInput, PrepareCreateNodeInput,
    PrepareDeleteNodeInput, PrepareDetachScriptInput, PrepareDisconnectSignalInput,
    PrepareReparentNodeInput, PrepareSetPropertyInput, TransactionStatusInput,
    UndoTransactionInput, coordinator_unavailable, transaction_error, transaction_result,
};

const DEFAULT_RESOURCE_LIMIT: usize = 50;
const MAX_RESOURCE_LIMIT: usize = 200;
const MAX_TOTAL_RESULTS: usize = 250_000;
const MAX_SCRIPT_DIAGNOSTICS: usize = 200;
const MAX_SAFE_RUNTIME_SEQUENCE: u64 = 9_007_199_254_740_991;
const EDITOR_SUMMARY_MAX_BYTES: usize = 4096;

fn has_tool_domain_authority(health: &ConnectionHealth, domain: AvailabilityDomain) -> bool {
    let lifecycle_allows_live_probe = matches!(
        health.status,
        ConnectionStatus::Ready | ConnectionStatus::Syncing
    );
    if !lifecycle_allows_live_probe {
        return false;
    }
    match domain {
        AvailabilityDomain::Editor => {
            health.bridge.condition == godot_codex_product::BridgeCondition::Ready
                && health.components.editor == ComponentCondition::Ready
        }
        AvailabilityDomain::Runtime => matches!(
            health.bridge.condition,
            godot_codex_product::BridgeCondition::Ready
                | godot_codex_product::BridgeCondition::DiscoveryStale
                | godot_codex_product::BridgeCondition::Syncing
        ),
    }
}
const PROJECT_SUMMARY_URI: &str = "godot://project/summary";
const EDITOR_SUMMARY_URI: &str = "godot://editor/summary";
const RUNTIME_SUMMARY_URI: &str = "godot://runtime/summary";
const SCENE_SUMMARY_PREFIX: &str = "godot://scene/";
const SCENE_SUMMARY_SUFFIX: &str = "/summary";

fn default_resource_limit() -> usize {
    DEFAULT_RESOURCE_LIMIT
}

fn default_find_usages_limit() -> usize {
    FIND_USAGES_DEFAULT_LIMIT
}

#[derive(Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LiveGuardInput {
    #[serde(default)]
    expected_editor_session_id: Option<String>,
    #[serde(default)]
    expected_event_seq: Option<u64>,
    #[serde(default)]
    expected_scene_revision: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LiveListInput {
    #[serde(default = "default_resource_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    expected_editor_session_id: Option<String>,
    #[serde(default)]
    expected_event_seq: Option<u64>,
    #[serde(default)]
    expected_scene_revision: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum DiagnosticScopeInput {
    #[default]
    Editor,
    Runtime,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DiagnosticInput {
    #[serde(default)]
    scope: DiagnosticScopeInput,
    #[serde(default = "default_resource_limit")]
    #[schemars(range(min = 1, max = 200))]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    expected_editor_session_id: Option<String>,
    #[serde(default)]
    expected_event_seq: Option<u64>,
    #[serde(default)]
    expected_scene_revision: Option<u64>,
    #[serde(default)]
    #[schemars(regex(pattern = r"^runtime:[0-9a-f]{32}$"))]
    runtime_session_id: Option<String>,
    #[serde(default)]
    #[schemars(range(min = 1, max = 9_007_199_254_740_991_u64))]
    expected_runtime_event_seq: Option<u64>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RuntimeRunInput {}

#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RuntimeGuardInput {
    #[schemars(regex(pattern = r"^runtime:[0-9a-f]{32}$"))]
    runtime_session_id: String,
    #[serde(default)]
    #[schemars(range(min = 1, max = 9_007_199_254_740_991_u64))]
    expected_runtime_event_seq: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RuntimeTreeInput {
    #[schemars(regex(pattern = r"^runtime:[0-9a-f]{32}$"))]
    runtime_session_id: String,
    #[serde(default)]
    #[schemars(range(min = 1, max = 9_007_199_254_740_991_u64))]
    expected_runtime_event_seq: Option<u64>,
    #[serde(default = "default_resource_limit")]
    #[schemars(range(min = 1, max = 200))]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RuntimeObjectInput {
    #[schemars(regex(pattern = r"^runtime:[0-9a-f]{32}$"))]
    runtime_session_id: String,
    #[schemars(regex(pattern = r"^runtime-object:[A-Za-z0-9_-]{43}$"))]
    runtime_object_id: String,
    #[serde(default)]
    #[schemars(range(min = 1, max = 9_007_199_254_740_991_u64))]
    expected_runtime_event_seq: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RuntimeStackInput {
    #[schemars(regex(pattern = r"^runtime:[0-9a-f]{32}$"))]
    runtime_session_id: String,
    #[schemars(regex(pattern = r"^runtime-stack:[A-Za-z0-9_-]{43}$"))]
    runtime_stack_id: String,
    #[serde(default)]
    #[schemars(range(min = 1, max = 9_007_199_254_740_991_u64))]
    expected_runtime_event_seq: Option<u64>,
}

fn default_capture_width() -> u32 {
    1_280
}

fn default_capture_height() -> u32 {
    720
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RuntimeCaptureInput {
    #[schemars(regex(pattern = r"^runtime:[0-9a-f]{32}$"))]
    runtime_session_id: String,
    #[serde(default)]
    #[schemars(range(min = 1, max = 9_007_199_254_740_991_u64))]
    expected_runtime_event_seq: Option<u64>,
    #[serde(default = "default_capture_width")]
    #[schemars(range(min = 1, max = 1_280))]
    max_width: u32,
    #[serde(default = "default_capture_height")]
    #[schemars(range(min = 1, max = 720))]
    max_height: u32,
}

impl From<&LiveListInput> for LiveGuardInput {
    fn from(input: &LiveListInput) -> Self {
        Self {
            expected_editor_session_id: input.expected_editor_session_id.clone(),
            expected_event_seq: input.expected_event_seq,
            expected_scene_revision: input.expected_scene_revision,
        }
    }
}

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

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
enum FindUsagesTargetInput {
    EntityId {
        entity_id: String,
    },
    Resource {
        selector: String,
    },
    Scene {
        selector: String,
    },
    NodePath {
        scene: String,
        node_path: String,
    },
    ScriptSymbol {
        script: String,
        qualified_name: String,
    },
    Signal {
        scene: String,
        emitter_node_path: String,
        signal: String,
    },
}

#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Ord,
    PartialEq,
    PartialOrd,
    serde::Deserialize,
    serde::Serialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
enum SemanticEntityKindInput {
    Project,
    Resource,
    Scene,
    Node,
    Script,
    Symbol,
    Signal,
}

impl From<SemanticEntityKindInput> for SemanticEntityKind {
    fn from(value: SemanticEntityKindInput) -> Self {
        match value {
            SemanticEntityKindInput::Project => Self::Project,
            SemanticEntityKindInput::Resource => Self::Resource,
            SemanticEntityKindInput::Scene => Self::Scene,
            SemanticEntityKindInput::Node => Self::Node,
            SemanticEntityKindInput::Script => Self::Script,
            SemanticEntityKindInput::Symbol => Self::Symbol,
            SemanticEntityKindInput::Signal => Self::Signal,
        }
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Ord,
    PartialEq,
    PartialOrd,
    serde::Deserialize,
    serde::Serialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
enum SemanticConfidenceInput {
    Dynamic,
    Probable,
    RuntimeConfirmed,
    Exact,
}

impl From<SemanticConfidenceInput> for SemanticConfidence {
    fn from(value: SemanticConfidenceInput) -> Self {
        match value {
            SemanticConfidenceInput::Dynamic => Self::Dynamic,
            SemanticConfidenceInput::Probable => Self::Probable,
            SemanticConfidenceInput::RuntimeConfirmed => Self::RuntimeConfirmed,
            SemanticConfidenceInput::Exact => Self::Exact,
        }
    }
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
enum FindUsagesScopeInput {
    #[default]
    Project,
    Scene {
        scene: String,
    },
    Script {
        script: String,
    },
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FindUsagesInput {
    target: FindUsagesTargetInput,
    #[serde(default)]
    source_kinds: Vec<SemanticEntityKindInput>,
    #[serde(default)]
    confidence: Vec<SemanticConfidenceInput>,
    #[serde(default)]
    scope: FindUsagesScopeInput,
    #[serde(default = "default_find_usages_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Clone, Copy)]
enum Query {
    CurrentScene,
    SelectedNodes,
}

#[derive(Clone, Copy)]
enum LiveListQuery {
    OpenScenes,
    OpenScripts,
    EditorHistory,
    Diagnostics,
}

#[derive(Clone, Copy)]
enum RuntimeControl {
    Run(RuntimeTarget),
    Stop,
    Pause,
    Continue,
}

#[derive(Clone, Debug)]
struct CompoundIndexBaseline {
    generation_id: String,
    index_revision: u64,
    resource_revision: u64,
    scene_graph_revision: u64,
    script_graph_revision: u64,
    validation_digest: String,
}

#[derive(Clone, Debug)]
struct PreparedChangeSetBaseline {
    project_id: String,
    editor_session_id: String,
    scene_id: String,
    preview_digest: String,
    preview: Value,
    live: Option<Arc<SemanticSnapshot>>,
    semantic: Option<ValidationSemanticSnapshot>,
    index: Option<CompoundIndexBaseline>,
    completed_report: Option<(String, String, String)>,
}

#[derive(Clone)]
pub struct GodotMcpServer {
    #[allow(dead_code, reason = "tool_handler macro accesses this router field")]
    tool_router: ToolRouter<Self>,
    replicator: SnapshotReplicator,
    resource_index: ResourceIndexReader,
    scene_index: SceneIndexReader,
    script_index: ScriptIndexReader,
    semantic_index: SemanticIndexReader,
    cursor_codec: CursorCodec,
    runtime_overlay: RuntimeOverlay,
    project_root: Option<PathBuf>,
    product_startup: ProductStartupObservation,
    transaction_coordinator: Option<Arc<TransactionCoordinator>>,
    validation_coordinator: Arc<tokio::sync::Mutex<ValidationCoordinator>>,
    confirmation_policy: ConfirmationPolicyStore,
    change_set_baselines: Arc<tokio::sync::Mutex<BTreeMap<String, PreparedChangeSetBaseline>>>,
    change_set_validation_tasks: Arc<tokio::sync::Mutex<BTreeSet<String>>>,
}

impl std::fmt::Debug for GodotMcpServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GodotMcpServer")
            .field("replica_status", &self.replicator.status())
            .field("resource_index_status", &self.resource_index.status())
            .field("scene_index_status", &self.scene_index.status())
            .field("script_index_status", &self.script_index.status())
            .field("runtime_summary", &self.runtime_overlay.summary())
            .field(
                "transaction_coordinator_available",
                &self.transaction_coordinator.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl output_schema::ToolAvailabilityGuard for GodotMcpServer {
    fn unavailable_tool_result(&self, tool_name: &str) -> Option<CallToolResult> {
        // Diagnostics is the one mixed editor/runtime tool. During a stale
        // semantic replica window the router must admit the request so the
        // deserialized scope can apply its exact domain guard: editor reads
        // still fail closed, while runtime reads can reauthenticate directly
        // against the live Bridge.
        if tool_name == "godot_get_diagnostics" {
            let health = self.connection_status();
            if has_tool_domain_authority(&health, AvailabilityDomain::Editor)
                || has_tool_domain_authority(&health, AvailabilityDomain::Runtime)
            {
                return None;
            }
        }
        output_schema::availability_domain(tool_name)
            .and_then(|domain| self.unavailable_error(domain))
    }
}

impl GodotMcpServer {
    fn offline_cache_active(&self) -> bool {
        matches!(
            (
                self.resource_index.status(),
                self.scene_index.status(),
                self.script_index.status(),
            ),
            (
                ResourceIndexStatus::OfflineCurrent {
                    project_id: resource_project,
                    generation_id: resource_generation,
                    ..
                },
                SceneIndexStatus::OfflineCurrent {
                    project_id: scene_project,
                    generation_id: scene_generation,
                    ..
                },
                ScriptIndexStatus::OfflineCurrent {
                    project_id: script_project,
                    generation_id: script_generation,
                    ..
                },
            ) if resource_project == scene_project
                && resource_project == script_project
                && resource_generation == scene_generation
                && resource_generation == script_generation
        )
    }

    /// Produces a structured fail-closed result unless the canonical health
    /// reducer proves a current editor binding. Cache presence never grants
    /// live, runtime, policy, validation, or mutation authority.
    fn unavailable_error(
        &self,
        domain: output_schema::AvailabilityDomain,
    ) -> Option<CallToolResult> {
        let health = self.connection_status();
        if has_tool_domain_authority(&health, domain) {
            return None;
        }
        let offline = matches!(
            health.status,
            ConnectionStatus::OfflineCached | ConnectionStatus::OfflineEmpty
        );
        let code = if offline {
            match domain {
                output_schema::AvailabilityDomain::Editor => "editor_offline",
                output_schema::AvailabilityDomain::Runtime => "runtime_unavailable",
            }
        } else {
            health.diagnostic.code.as_str()
        };
        let message = match domain {
            output_schema::AvailabilityDomain::Editor => health.diagnostic.summary,
            output_schema::AvailabilityDomain::Runtime => {
                "The Godot runtime is unavailable until the exact editor binding is ready."
                    .to_owned()
            }
        };
        Some(CallToolResult::structured_error(json!({
            "error": {
                "code": code,
                "message": message,
                "retryable": health.diagnostic.retryable,
                "status": health.status,
                "diagnostic_code": health.diagnostic.code,
                "compatibility": health.compatibility.status,
                "remediation_id": health.remediation_id,
            }
        })))
    }

    fn compound_index_baseline(&self) -> Option<CompoundIndexBaseline> {
        let snapshot = self.semantic_index.pin_current().ok()?;
        let generation = snapshot.generation();
        Some(CompoundIndexBaseline {
            generation_id: generation.generation_id.clone(),
            index_revision: generation.index_revision,
            resource_revision: generation.checkpoint.resource_revision,
            scene_graph_revision: generation.scene.scene_graph_revision,
            script_graph_revision: generation.script.script_graph_revision,
            validation_digest: generation.validation_digest.clone(),
        })
    }

    fn compound_semantic_snapshot(
        &self,
        preview: &Value,
        live: &SemanticSnapshot,
        scene_id: &str,
    ) -> Option<ValidationSemanticSnapshot> {
        if !live_scene_projection_complete(live, scene_id) {
            return None;
        }
        let mut snapshot = ValidationSemanticSnapshot::default();
        for (id, entity) in live.scenes.iter().chain(&live.nodes) {
            if snapshot.entities.len() >= 2_000 {
                return None;
            }
            snapshot.entities.insert(id.clone(), entity.clone());
        }
        let paths = change_set_paths(preview);
        if paths.is_empty() {
            return Some(snapshot);
        }
        let index = self.semantic_index.pin_current().ok()?;
        let generation = index.generation();
        for resource in &generation.resources {
            if paths.contains(&resource.display_path) {
                snapshot.entities.insert(
                    resource.entity_id.clone(),
                    serde_json::to_value(resource).ok()?,
                );
            }
        }
        for document in &generation.script.documents {
            if paths.contains(&document.path) {
                snapshot.entities.insert(
                    document.script_resource_id.clone(),
                    serde_json::to_value(document).ok()?,
                );
            }
        }
        (snapshot.entities.len() <= 2_000).then_some(snapshot)
    }

    async fn retain_change_set_baseline(&self, input: &PrepareChangeSetInput, result: &Value) {
        let Some(change_set_id) = result.get("change_set_id").and_then(Value::as_str) else {
            return;
        };
        let Some(preview_digest) = result.get("preview_digest").and_then(Value::as_str) else {
            return;
        };
        let Some(preview) = result.get("preview").cloned() else {
            return;
        };
        let live = self.replicator.read().ok();
        let semantic = live.as_deref().and_then(|snapshot| {
            self.compound_semantic_snapshot(&preview, snapshot, &input.coordinates.scene_id)
        });
        let baseline = PreparedChangeSetBaseline {
            project_id: input.project_id.clone(),
            editor_session_id: input.coordinates.editor_session_id.clone(),
            scene_id: input.coordinates.scene_id.clone(),
            preview_digest: preview_digest.to_owned(),
            preview,
            live,
            semantic,
            index: self.compound_index_baseline(),
            completed_report: None,
        };
        let mut baselines = self.change_set_baselines.lock().await;
        baselines.insert(change_set_id.to_owned(), baseline);
        while baselines.len() > 64 {
            let Some(oldest) = baselines.keys().next().cloned() else {
                break;
            };
            baselines.remove(&oldest);
        }
    }

    fn bind_change_set_resource_paths(&self, params: &mut Value) -> Result<(), (String, String)> {
        let save_scope: BTreeSet<String> = params
            .pointer("/save_scope/paths")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let Some(operations) = params.get_mut("operations").and_then(Value::as_array_mut) else {
            return Err((
                "change_set_invalid".to_owned(),
                "The normalized change set is missing its operations.".to_owned(),
            ));
        };
        if !operations.iter().any(|operation| {
            operation.get("kind").and_then(Value::as_str) == Some("update_resource")
                && operation
                    .get("resource")
                    .and_then(Value::as_str)
                    .is_some_and(|resource| !resource.starts_with("alias:"))
        }) {
            return Ok(());
        }
        let snapshot = self.semantic_index.pin_current().map_err(|_| {
            (
                "resource_index_unavailable".to_owned(),
                "The current resource index is unavailable for opaque-ID resolution.".to_owned(),
            )
        })?;
        let generation = snapshot.generation();
        for operation in operations {
            if operation.get("kind").and_then(Value::as_str) != Some("update_resource") {
                continue;
            }
            let Some(resource_id) = operation.get("resource").and_then(Value::as_str) else {
                return Err((
                    "change_set_invalid".to_owned(),
                    "A resource update is missing its opaque resource ID.".to_owned(),
                ));
            };
            if resource_id.starts_with("alias:") {
                continue;
            }
            let expected_hash = operation
                .get("expected_hash")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some(resource) = generation
                .resources
                .iter()
                .find(|candidate| candidate.entity_id == resource_id)
            else {
                return Err((
                    "resource_not_found".to_owned(),
                    "The opaque resource ID is not present in the current index generation."
                        .to_owned(),
                ));
            };
            if resource.validity != RecordValidity::Valid
                || !resource.display_path.ends_with(".tres")
                || resource.content_generation.as_deref() != Some(expected_hash)
            {
                return Err((
                    "stale_resource_state".to_owned(),
                    "The resource is not a current, exact .tres generation matching expected_hash."
                        .to_owned(),
                ));
            }
            if !save_scope.contains(&resource.display_path) {
                return Err((
                    "save_scope_mismatch".to_owned(),
                    "The resolved resource path is not explicitly present in save_scope."
                        .to_owned(),
                ));
            }
            let Some(object) = operation.as_object_mut() else {
                return Err((
                    "change_set_invalid".to_owned(),
                    "A resource update is not an object.".to_owned(),
                ));
            };
            object.insert(
                "resolved_path".to_owned(),
                Value::String(resource.display_path.clone()),
            );
        }
        Ok(())
    }

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
        Self::with_all_indexes_and_runtime(
            replicator,
            resource_index,
            scene_index,
            script_index,
            RuntimeOverlay::unavailable(),
            None,
            None,
        )
    }

    pub fn with_all_indexes_and_project_root(
        replicator: SnapshotReplicator,
        resource_index: ResourceIndexReader,
        scene_index: SceneIndexReader,
        script_index: ScriptIndexReader,
        project_root: PathBuf,
    ) -> Self {
        Self::with_all_indexes_and_runtime(
            replicator,
            resource_index,
            scene_index,
            script_index,
            RuntimeOverlay::for_project(project_root.clone()),
            Some(project_root),
            None,
        )
    }

    pub fn with_all_indexes_and_project_services(
        replicator: SnapshotReplicator,
        resource_index: ResourceIndexReader,
        scene_index: SceneIndexReader,
        script_index: ScriptIndexReader,
        project_root: PathBuf,
        transaction_coordinator: Arc<TransactionCoordinator>,
    ) -> Self {
        Self::with_all_indexes_and_runtime(
            replicator,
            resource_index,
            scene_index,
            script_index,
            RuntimeOverlay::for_project(project_root.clone()),
            Some(project_root),
            Some(transaction_coordinator),
        )
    }

    fn with_all_indexes_and_runtime(
        replicator: SnapshotReplicator,
        resource_index: ResourceIndexReader,
        scene_index: SceneIndexReader,
        script_index: ScriptIndexReader,
        runtime_overlay: RuntimeOverlay,
        project_root: Option<PathBuf>,
        transaction_coordinator: Option<Arc<TransactionCoordinator>>,
    ) -> Self {
        let semantic_index = SemanticIndexReader::new(
            resource_index.clone(),
            scene_index.clone(),
            script_index.clone(),
        );
        Self {
            tool_router: output_schema::install_output_contracts(Self::tool_router()),
            replicator,
            resource_index,
            scene_index,
            script_index,
            semantic_index,
            cursor_codec: CursorCodec::new(),
            runtime_overlay,
            project_root,
            product_startup: ProductStartupObservation::unverified(env!("CARGO_PKG_VERSION")),
            transaction_coordinator,
            validation_coordinator: Arc::new(tokio::sync::Mutex::new(
                ValidationCoordinator::default(),
            )),
            confirmation_policy: ConfirmationPolicyStore::default(),
            change_set_baselines: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            change_set_validation_tasks: Arc::new(tokio::sync::Mutex::new(BTreeSet::new())),
        }
    }

    /// Installs the bounded package/config observation produced by the shared
    /// operations startup adapter.
    #[must_use]
    pub fn with_product_startup_observation(
        mut self,
        observation: ProductStartupObservation,
    ) -> Self {
        self.product_startup = observation;
        self
    }

    fn connection_status(&self) -> ConnectionHealth {
        let replica_state = self.replicator.observation();
        let replica_status = replica_state.status;
        let replica = match replica_status {
            ReplicaStatus::Ready => {
                self.replicator
                    .read()
                    .map_or(ReplicaObservation::Stale, |snapshot| {
                        ReplicaObservation::Ready {
                            project_id: snapshot.project_id.clone(),
                        }
                    })
            }
            ReplicaStatus::Syncing => ReplicaObservation::Syncing,
            ReplicaStatus::Stale => ReplicaObservation::Stale,
            ReplicaStatus::Disconnected => match replica_state.failure {
                None => ReplicaObservation::Connecting,
                Some(ReplicaFailure::TransportDisconnected) => {
                    ReplicaObservation::TransportDisconnected
                }
                Some(ReplicaFailure::AuthenticationFailed) => {
                    ReplicaObservation::AuthenticationFailed
                }
                Some(ReplicaFailure::ProjectBindingMismatch) => {
                    ReplicaObservation::ProjectBindingMismatch
                }
                Some(ReplicaFailure::ProtocolVersionIncompatible) => {
                    ReplicaObservation::ProtocolIncompatible
                }
            },
        };
        let runtime = runtime_observation(&self.runtime_overlay.summary(), replica_status);
        let resource_status = self.resource_index.status();
        let scene_status = self.scene_index.status();
        let script_status = self.script_index.status();
        let static_cache_verified = self.offline_cache_active();
        let cache_age_seconds = cache_age_seconds(
            self.project_root.as_deref(),
            static_cache_verified,
            &resource_status,
            &scene_status,
            &script_status,
        );
        connection_health(ServerConnectionObservation {
            product: self.product_startup.clone(),
            replica,
            negotiated_bridge: replica_state.negotiated_bridge,
            resource_index: resource_index_observation(resource_status),
            scene_index: scene_index_observation(scene_status),
            script_index: script_index_observation(script_status),
            static_cache_verified,
            runtime,
            transactions_available: self.transaction_coordinator.is_some(),
            transaction_recovery_degraded: self
                .transaction_coordinator
                .as_ref()
                .is_some_and(|coordinator| coordinator.journal_recovered_corruption()),
            cache_age_seconds,
        })
    }

    async fn prepare_transaction(&self, command: PrepareCommand) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Editor) {
            return error;
        }
        let Some(coordinator) = &self.transaction_coordinator else {
            return coordinator_unavailable();
        };
        match coordinator.prepare(command).await {
            Ok(result) => transaction_result(result),
            Err(error) => transaction_error(error),
        }
    }

    async fn transaction_status(&self, transaction_id: &str) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Editor) {
            return error;
        }
        let Some(coordinator) = &self.transaction_coordinator else {
            return coordinator_unavailable();
        };
        match coordinator.status(transaction_id).await {
            Ok(result) => transaction_result(result),
            Err(error) => transaction_error(error),
        }
    }

    async fn ensure_change_set_baseline(
        &self,
        client: &godot_codex_bridge_client::BridgeClient,
        status: &Value,
    ) -> Option<PreparedChangeSetBaseline> {
        let change_set_id = status.get("change_set_id")?.as_str()?;
        if let Some(baseline) = self
            .change_set_baselines
            .lock()
            .await
            .get(change_set_id)
            .cloned()
        {
            return Some(baseline);
        }
        let coordinates = status.get("coordinates")?;
        let preview = status.get("preview")?.clone();
        let live = self.replicator.read().ok();
        let scene_id = coordinates.get("scene_id")?.as_str()?.to_owned();
        let semantic = live
            .as_deref()
            .and_then(|snapshot| self.compound_semantic_snapshot(&preview, snapshot, &scene_id));
        let baseline = PreparedChangeSetBaseline {
            project_id: client.project_id().to_owned(),
            editor_session_id: client.editor_session_id().to_owned(),
            scene_id,
            preview_digest: status.get("preview_digest")?.as_str()?.to_owned(),
            preview,
            live,
            semantic,
            index: self.compound_index_baseline(),
            completed_report: None,
        };
        self.change_set_baselines
            .lock()
            .await
            .insert(change_set_id.to_owned(), baseline.clone());
        Some(baseline)
    }

    async fn change_set_status(&self, change_set_id: &str) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Editor) {
            return error;
        }
        let Some(project_root) = &self.project_root else {
            return structured_error(
                "change_set_coordinator_unavailable",
                "The project-scoped compound coordinator is unavailable.",
                true,
            );
        };
        let mut client = match godot_codex_bridge_client::BridgeClient::connect(project_root).await
        {
            Ok(client) => client,
            Err(error) => return runtime_bridge_error(error),
        };
        match client.get_change_set_status(change_set_id).await {
            Ok(status) => {
                if status.get("state").and_then(Value::as_str) == Some("validating")
                    && self
                        .ensure_change_set_baseline(&client, &status)
                        .await
                        .is_some()
                {
                    self.spawn_change_set_validation(change_set_id.to_owned());
                }
                transaction_result(status)
            }
            Err(error) => runtime_bridge_error(error),
        }
    }

    fn spawn_change_set_validation(&self, change_set_id: String) {
        let server = self.clone();
        tokio::spawn(async move {
            {
                let mut tasks = server.change_set_validation_tasks.lock().await;
                if !tasks.insert(change_set_id.clone()) {
                    return;
                }
            }
            server.run_change_set_validation(&change_set_id).await;
            server
                .change_set_validation_tasks
                .lock()
                .await
                .remove(&change_set_id);
        });
    }

    async fn apply_compound_change_set(
        &self,
        input: ApplyTransactionInput,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let Some(project_root) = &self.project_root else {
            return structured_error(
                "change_set_coordinator_unavailable",
                "The project-scoped compound coordinator is unavailable.",
                true,
            );
        };
        let mut client = match godot_codex_bridge_client::BridgeClient::connect(project_root).await
        {
            Ok(client) => client,
            Err(error) => return runtime_bridge_error(error),
        };
        let status = match client.get_change_set_status(&input.transaction_id).await {
            Ok(status) => status,
            Err(error) => return runtime_bridge_error(error),
        };
        let Some(baseline) = self.ensure_change_set_baseline(&client, &status).await else {
            return structured_error(
                "change_set_baseline_unavailable",
                "The immutable change-set baseline could not be recovered safely.",
                false,
            );
        };
        if baseline.project_id != client.project_id()
            || baseline.editor_session_id != client.editor_session_id()
            || baseline.preview_digest != input.preview_digest
        {
            return structured_error(
                "approval_binding_mismatch",
                "The apply request does not match the retained project, editor session, or preview.",
                false,
            );
        }
        let Some(coordinates) = status.get("coordinates") else {
            return structured_error(
                "change_set_baseline_unavailable",
                "The Bridge did not return bound change-set coordinates.",
                false,
            );
        };
        if coordinates.get("scene_revision").and_then(Value::as_u64)
            != Some(input.expected_scene_revision)
            || coordinates.get("operation_seq").and_then(Value::as_u64)
                != Some(input.expected_operation_seq)
        {
            return structured_error(
                "stale_editor_state",
                "The apply coordinates do not match the immutable change-set preview.",
                true,
            );
        }
        let risk_name = status
            .get("risk")
            .and_then(Value::as_str)
            .unwrap_or("destructive");
        let risk = match risk_name {
            "low" => Risk::Low,
            "destructive" => Risk::Destructive,
            _ => {
                return structured_error(
                    "change_set_invalid",
                    "The Bridge returned an unsupported risk classification.",
                    false,
                );
            }
        };
        if baseline.preview.get("risk").and_then(Value::as_str) != Some(risk_name) {
            return structured_error(
                "approval_binding_mismatch",
                "The immutable preview and current status disagree on transaction risk.",
                false,
            );
        }
        let approval = McpApprovalProvider::new(context);
        if !approval.supports_form() {
            return structured_error(
                "approval_host_unsupported",
                "The MCP host does not support standard form elicitation.",
                false,
            );
        }
        let grant_eligible = session_grant_eligible(&baseline.preview, risk);
        let now_ms = unix_millis();
        let grant_applies = grant_eligible
            && self.confirmation_policy.allows_low_risk_memory_only(
                client.project_id(),
                client.editor_session_id(),
                LOW_RISK_MEMORY_ONLY_SCOPE,
                now_ms,
            );
        if !grant_applies {
            match approval
                .request_change_set(
                    &input.transaction_id,
                    &input.preview_digest,
                    risk_name,
                    &baseline.preview,
                    grant_eligible,
                )
                .await
            {
                ChangeSetApprovalDecision::Accept { grant_low_risk } => {
                    if grant_low_risk
                        && !self.confirmation_policy.grant_low_risk_for_session(
                            client.project_id(),
                            client.editor_session_id(),
                            unix_millis(),
                            change_set_tools::CONFIRMATION_GRANT_MAX_MS,
                        )
                    {
                        return structured_error(
                            "confirmation_policy_scope_mismatch",
                            "The requested session policy could not be installed safely.",
                            false,
                        );
                    }
                }
                ChangeSetApprovalDecision::Decline => {
                    return structured_error(
                        "approval_declined",
                        "The change-set approval was declined.",
                        false,
                    );
                }
                ChangeSetApprovalDecision::Cancel => {
                    return structured_error(
                        "approval_cancelled",
                        "The change-set approval was cancelled.",
                        true,
                    );
                }
                ChangeSetApprovalDecision::Timeout => {
                    return structured_error(
                        "approval_timeout",
                        "The change-set approval timed out.",
                        true,
                    );
                }
                ChangeSetApprovalDecision::Invalid => {
                    return structured_error(
                        "approval_invalid",
                        "The approval response did not contain one exact host accept action.",
                        true,
                    );
                }
                ChangeSetApprovalDecision::Unsupported => {
                    return structured_error(
                        "approval_host_unsupported",
                        "The MCP host does not support standard form elicitation.",
                        false,
                    );
                }
            }
        }

        let last_status = match client.get_change_set_status(&input.transaction_id).await {
            Ok(status) => status,
            Err(error) => return runtime_bridge_error(error),
        };
        if last_status.get("preview_digest").and_then(Value::as_str)
            != Some(input.preview_digest.as_str())
        {
            return structured_error(
                "preview_mismatch",
                "The immutable preview changed before apply.",
                false,
            );
        }
        let binding = ApprovalBinding {
            project_id: client.project_id().to_owned(),
            editor_session_id: client.editor_session_id().to_owned(),
            scene_id: baseline.scene_id,
            transaction_id: input.transaction_id.clone(),
            preview_digest: input.preview_digest.clone(),
            scope: ApprovalScope::ChangeSetAtomic,
            risk,
            scene_revision: input.expected_scene_revision,
            operation_seq: input.expected_operation_seq,
        };
        let approval = match client.issue_transaction_approval(&binding, unix_millis()) {
            Ok(approval) => approval,
            Err(error) => return runtime_bridge_error(error),
        };
        let result = match client
            .apply_change_set(
                &input.transaction_id,
                &input.preview_digest,
                input.expected_scene_revision,
                input.expected_operation_seq,
                approval.receipt,
            )
            .await
        {
            Ok(result) => result,
            Err(error) => match client.get_change_set_status(&input.transaction_id).await {
                Ok(observed)
                    if matches!(
                        observed.get("state").and_then(Value::as_str),
                        Some("validating" | "committed" | "rolled_back" | "failed" | "in_doubt")
                    ) =>
                {
                    observed
                }
                _ => return runtime_bridge_error(error),
            },
        };
        if result.get("state").and_then(Value::as_str) == Some("validating") {
            self.spawn_change_set_validation(input.transaction_id);
        }
        transaction_result(result)
    }

    async fn run_change_set_validation(&self, change_set_id: &str) {
        let Some(project_root) = &self.project_root else {
            return;
        };
        let Some(baseline) = self
            .change_set_baselines
            .lock()
            .await
            .get(change_set_id)
            .cloned()
        else {
            return;
        };
        let mut client = match godot_codex_bridge_client::BridgeClient::connect(project_root).await
        {
            Ok(client) => client,
            Err(_) => return,
        };
        let status = match client.get_change_set_status(change_set_id).await {
            Ok(status) => status,
            Err(_) => return,
        };
        if status.get("state").and_then(Value::as_str) != Some("validating") {
            return;
        }
        let Some(transaction_seq) = status.get("transaction_seq").and_then(Value::as_u64) else {
            return;
        };
        let Some(postimage_digest) = status
            .get("postimage_digest")
            .and_then(Value::as_str)
            .filter(|digest| digest.starts_with("sha256:") && digest.len() == 71)
        else {
            return;
        };
        if let Some((report_id, report_digest, outcome)) = &baseline.completed_report {
            if client
                .complete_change_set_validation(
                    change_set_id,
                    report_id,
                    report_digest,
                    outcome,
                    transaction_seq,
                    postimage_digest,
                )
                .await
                .is_ok()
            {
                self.change_set_baselines.lock().await.remove(change_set_id);
            }
            return;
        }
        drop(client);
        let policy_value = baseline
            .preview
            .get("validation_policy")
            .cloned()
            .unwrap_or_default();
        let policy = ValidationPolicy {
            rollback: policy_value
                .get("rollback")
                .and_then(Value::as_str)
                .unwrap_or("on_required_failure")
                .to_owned(),
            warnings: policy_value
                .get("warnings")
                .and_then(Value::as_str)
                .unwrap_or("allow")
                .to_owned(),
            runtime: policy_value
                .get("runtime")
                .and_then(Value::as_str)
                .unwrap_or("skip")
                .to_owned(),
        };
        let started_at_ms = unix_millis();
        let deadline_ms = started_at_ms.saturating_add(if policy.runtime == "skip" {
            30_000
        } else {
            60_000
        });
        let checks = [
            (ValidationCheck::Intrinsic, CheckAuthority::Required),
            (ValidationCheck::Persistence, CheckAuthority::Required),
            (ValidationCheck::ReloadReparse, CheckAuthority::Required),
            (ValidationCheck::IndexConvergence, CheckAuthority::Required),
            (ValidationCheck::SemanticGraph, CheckAuthority::Required),
            (ValidationCheck::Diagnostics, CheckAuthority::Required),
            (ValidationCheck::Runtime, CheckAuthority::Optional),
        ];
        {
            let mut validation = self.validation_coordinator.lock().await;
            if validation
                .begin(
                    change_set_id,
                    &baseline.preview_digest,
                    policy.clone(),
                    started_at_ms,
                    deadline_ms,
                    &checks,
                )
                .is_err()
            {
                return;
            }
        }

        let (has_scene, has_resource, has_script) = change_set_operation_flags(&baseline.preview);
        let scene_persisted = change_set_saves_scene(&baseline.preview);
        let index_required = scene_persisted || has_resource || has_script;
        let persistence_requested = baseline
            .preview
            .get("save_scope")
            .and_then(Value::as_array)
            .is_some_and(|scope| !scope.is_empty());
        let mut post_live = self.replicator.read().ok();
        let mut post_index = self.compound_index_baseline();
        let convergence_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut live_converged = !has_scene;
        let mut index_converged;
        loop {
            if let (Some(before), Some(after)) = (baseline.live.as_ref(), post_live.as_ref()) {
                live_converged = !has_scene
                    || (before.editor_session_id == after.editor_session_id
                        && after.revisions.event_seq > before.revisions.event_seq);
            }
            index_converged = if !index_required {
                true
            } else {
                match (&baseline.index, &post_index) {
                    (Some(before), Some(after)) => {
                        after.index_revision >= before.index_revision
                            && (!scene_persisted
                                || after.scene_graph_revision > before.scene_graph_revision)
                            && (!has_resource || after.resource_revision > before.resource_revision)
                            && (!has_script
                                || after.script_graph_revision > before.script_graph_revision)
                            && after.validation_digest != before.validation_digest
                    }
                    _ => false,
                }
            };
            if live_converged && index_converged {
                break;
            }
            if tokio::time::Instant::now() >= convergence_deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            post_live = self.replicator.read().ok();
            post_index = self.compound_index_baseline();
        }

        let intrinsic_passed = status
            .get("postimage_digest")
            .and_then(Value::as_str)
            .is_some_and(|digest| digest.starts_with("sha256:"));
        let post_semantic = post_live.as_deref().and_then(|live| {
            self.compound_semantic_snapshot(&baseline.preview, live, &baseline.scene_id)
        });
        let semantic_comparison =
            baseline
                .semantic
                .as_ref()
                .zip(post_semantic.as_ref())
                .map(|(before, after)| {
                    let expected = semantic_delta_for_change_set(
                        before,
                        after,
                        &baseline.preview,
                        &baseline.scene_id,
                    );
                    SemanticComparison::compare(before, after, &expected)
                });
        let diagnostics = baseline
            .live
            .as_ref()
            .zip(post_live.as_ref())
            .filter(|(before, after)| live_diagnostics_delta_complete(before, after))
            .map(|(before, after)| {
                DiagnosticSummary::classify(
                    &live_diagnostic_fingerprints(before),
                    &live_diagnostic_fingerprints(after),
                )
            });

        let mut runtime_outcome = CheckOutcome::Skipped;
        let mut runtime_summary = "runtime validation was not requested".to_owned();
        let mut runtime_session_id = None;
        if policy.runtime != "skip" {
            let target = if policy.runtime == "run_current_scene" {
                RuntimeTarget::CurrentScene
            } else {
                RuntimeTarget::Project
            };
            let mut runtime_client =
                match godot_codex_bridge_client::BridgeClient::connect(project_root).await {
                    Ok(client) => client,
                    Err(_) => return,
                };
            match runtime_client.run_runtime(target).await {
                Ok(runtime) => {
                    runtime_session_id = Some(runtime.runtime_session_id.clone());
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    match runtime_client
                        .get_runtime_snapshot(
                            &runtime.runtime_session_id,
                            None,
                            Some(vec![RuntimeDomain::RuntimeDiagnostics]),
                        )
                        .await
                    {
                        Ok(snapshot) => {
                            let introduced_error = snapshot.entities.iter().any(|entity| {
                                matches!(
                                    entity,
                                    RuntimeEntity::RuntimeDiagnostic(diagnostic)
                                        if diagnostic.severity
                                            == godot_codex_bridge_client::RuntimeDiagnosticSeverity::Error
                                )
                            });
                            runtime_outcome = if introduced_error {
                                CheckOutcome::Failed
                            } else {
                                CheckOutcome::Passed
                            };
                            runtime_summary = if introduced_error {
                                "fresh runtime session reported an error".to_owned()
                            } else {
                                "fresh runtime session completed bounded diagnostic capture"
                                    .to_owned()
                            };
                        }
                        Err(_) => {
                            runtime_outcome = CheckOutcome::Inconclusive;
                            runtime_summary =
                                "fresh runtime session could not provide diagnostics".to_owned();
                        }
                    }
                    let _ = runtime_client
                        .stop_runtime(&runtime.runtime_session_id, None)
                        .await;
                }
                Err(_) => {
                    runtime_outcome = CheckOutcome::Failed;
                    runtime_summary = "fresh runtime session failed to start".to_owned();
                }
            }
        }

        let completed_at_ms = unix_millis();
        let deadline_elapsed = completed_at_ms >= deadline_ms;
        let bounded_outcome = |outcome| {
            if deadline_elapsed && outcome != CheckOutcome::Skipped {
                CheckOutcome::TimedOut
            } else {
                outcome
            }
        };
        let reload_evidence = post_index.as_ref().map(|index| {
            json!({
                "generation_id": index.generation_id,
                "index_revision": index.index_revision,
            })
        });
        let report = {
            let mut validation = self.validation_coordinator.lock().await;
            let _ = validation.complete_check(
                change_set_id,
                ValidationCheck::Intrinsic,
                bounded_outcome(if intrinsic_passed {
                    CheckOutcome::Passed
                } else {
                    CheckOutcome::Failed
                }),
                if intrinsic_passed {
                    "Bridge proved the compound postimage and native action"
                } else {
                    "Bridge did not prove the compound postimage"
                },
                status.get("postimage_digest"),
                completed_at_ms,
            );
            let _ = validation.complete_check(
                change_set_id,
                ValidationCheck::Persistence,
                bounded_outcome(if intrinsic_passed {
                    CheckOutcome::Passed
                } else {
                    CheckOutcome::Failed
                }),
                if persistence_requested {
                    "Bridge verified every explicit persisted postimage"
                } else {
                    "save_scope was empty and project-content bytes were not requested"
                },
                status.get("postimage_digest"),
                completed_at_ms,
            );
            let _ = validation.complete_check(
                change_set_id,
                ValidationCheck::ReloadReparse,
                bounded_outcome(if !has_resource && !has_script || index_converged {
                    CheckOutcome::Passed
                } else {
                    CheckOutcome::Inconclusive
                }),
                if !has_resource && !has_script {
                    "resource/script reload was not applicable"
                } else if index_converged {
                    "resource/script generation converged after reload"
                } else {
                    "resource/script generation did not converge within the proof budget"
                },
                reload_evidence.as_ref(),
                completed_at_ms,
            );
            let convergence_evidence = json!({
                "live_converged": live_converged,
                "index_converged": index_converged,
                "index_revision": post_index.as_ref().map(|index| index.index_revision),
            });
            let _ = validation.complete_check(
                change_set_id,
                ValidationCheck::IndexConvergence,
                bounded_outcome(if live_converged && index_converged {
                    CheckOutcome::Passed
                } else {
                    CheckOutcome::Inconclusive
                }),
                if live_converged && index_converged {
                    "live overlay and semantic index reached newer bound generations"
                } else {
                    "live overlay or semantic index did not reach a provable generation"
                },
                Some(&convergence_evidence),
                completed_at_ms,
            );
            if let Some(comparison) = semantic_comparison.clone() {
                let matched = comparison.matched;
                let _ = validation.set_semantic(change_set_id, comparison.clone());
                let _ = validation.complete_check(
                    change_set_id,
                    ValidationCheck::SemanticGraph,
                    bounded_outcome(if matched {
                        CheckOutcome::Passed
                    } else {
                        CheckOutcome::Failed
                    }),
                    if matched {
                        "affected semantic closure matched the bounded expected delta"
                    } else {
                        "semantic changes escaped the bounded affected closure"
                    },
                    serde_json::to_value(comparison).ok().as_ref(),
                    completed_at_ms,
                );
            } else {
                let _ = validation.complete_check(
                    change_set_id,
                    ValidationCheck::SemanticGraph,
                    bounded_outcome(CheckOutcome::Inconclusive),
                    "the bounded affected semantic closure could not be compared",
                    None,
                    completed_at_ms,
                );
            }
            if let Some(summary) = diagnostics.clone() {
                let failed = summary.introduced_errors > 0
                    || (policy.warnings == "fail_on_introduced" && summary.introduced_warnings > 0);
                let _ = validation.set_diagnostics(change_set_id, summary.clone());
                let _ = validation.complete_check(
                    change_set_id,
                    ValidationCheck::Diagnostics,
                    bounded_outcome(if failed {
                        CheckOutcome::Failed
                    } else {
                        CheckOutcome::Passed
                    }),
                    if failed {
                        "the change set introduced diagnostics rejected by policy"
                    } else {
                        "no policy-rejected editor diagnostics were introduced"
                    },
                    serde_json::to_value(summary).ok().as_ref(),
                    completed_at_ms,
                );
            } else {
                let _ = validation.complete_check(
                    change_set_id,
                    ValidationCheck::Diagnostics,
                    bounded_outcome(CheckOutcome::Inconclusive),
                    "editor diagnostics were stale or truncated beyond the proof budget",
                    None,
                    completed_at_ms,
                );
            }
            if let Some(runtime_session_id) = &runtime_session_id {
                let _ = validation.set_runtime_session(change_set_id, runtime_session_id);
            }
            let _ = validation.complete_check(
                change_set_id,
                ValidationCheck::Runtime,
                bounded_outcome(runtime_outcome),
                &runtime_summary,
                runtime_session_id
                    .as_ref()
                    .map(|session| json!({"runtime_session_id": session}))
                    .as_ref(),
                completed_at_ms,
            );
            validation.finalize(change_set_id, completed_at_ms).ok()
        };
        let Some(report) = report else {
            return;
        };
        let outcome = match report.outcome {
            ValidationReportOutcome::Passed => "passed",
            ValidationReportOutcome::Failed => "failed",
            ValidationReportOutcome::Inconclusive => "inconclusive",
            ValidationReportOutcome::TimedOut => "timed_out",
        };
        if let Some(retained) = self
            .change_set_baselines
            .lock()
            .await
            .get_mut(change_set_id)
        {
            retained.completed_report = Some((
                report.report_id.clone(),
                report.report_digest.clone(),
                outcome.to_owned(),
            ));
        }
        let mut finalizer =
            match godot_codex_bridge_client::BridgeClient::connect(project_root).await {
                Ok(client) => client,
                Err(_) => return,
            };
        if finalizer
            .complete_change_set_validation(
                change_set_id,
                &report.report_id,
                &report.report_digest,
                outcome,
                transaction_seq,
                postimage_digest,
            )
            .await
            .is_ok()
        {
            self.change_set_baselines.lock().await.remove(change_set_id);
        }
    }

    fn live_snapshot(
        &self,
        guard: &LiveGuardInput,
    ) -> Result<Arc<SemanticSnapshot>, CallToolResult> {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Editor) {
            return Err(error);
        }
        let snapshot = self.replicator.read().map_err(|error| {
            CallToolResult::structured_error(json!({
                "error": {
                    "code": "editor_state_unavailable",
                    "message": error.message,
                    "retryable": true,
                    "replica_status": error.status,
                }
            }))
        })?;
        if guard
            .expected_editor_session_id
            .as_ref()
            .is_some_and(|expected| expected.len() > 128 || expected != &snapshot.editor_session_id)
            || guard
                .expected_event_seq
                .is_some_and(|expected| expected != snapshot.revisions.event_seq)
        {
            return Err(stale_live_error("stale_editor_state", &snapshot));
        }
        if let Some(expected) = guard.expected_scene_revision {
            let current = snapshot
                .current_scene_id
                .as_ref()
                .and_then(|scene_id| snapshot.revisions.scene_revisions.get(scene_id))
                .copied()
                .unwrap_or(0);
            if expected != current {
                return Err(stale_live_error("stale_scene_revision", &snapshot));
            }
        }
        Ok(snapshot)
    }

    fn query(&self, query: Query, guard: &LiveGuardInput) -> CallToolResult {
        let snapshot = match self.live_snapshot(guard) {
            Ok(snapshot) => snapshot,
            Err(error) => return error,
        };
        let value = match query {
            Query::CurrentScene => snapshot.current_scene_result(),
            Query::SelectedNodes => snapshot.selected_nodes_result(),
        };
        CallToolResult::structured(value)
    }

    fn editor_state_query(&self, guard: &LiveGuardInput) -> CallToolResult {
        let snapshot = match self.live_snapshot(guard) {
            Ok(snapshot) => snapshot,
            Err(error) => return error,
        };
        let mut value = snapshot.editor_state_result();
        if let Some(object) = value.as_object_mut() {
            object.insert("runtime".to_owned(), self.runtime_overlay.summary());
        }
        CallToolResult::structured(value)
    }

    async fn runtime_snapshot(
        &self,
        runtime_session_id: &str,
        expected_runtime_event_seq: Option<u64>,
        domains: Vec<RuntimeDomain>,
    ) -> Result<Arc<CachedRuntimeSnapshot>, CallToolResult> {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Runtime) {
            return Err(error);
        }
        let mut client = self
            .runtime_overlay
            .connect()
            .await
            .map_err(runtime_bridge_error)?;
        let project_id = client.project_id().to_owned();
        let editor_session_id = client.editor_session_id().to_owned();
        let snapshot = client
            .get_runtime_snapshot(
                runtime_session_id,
                expected_runtime_event_seq,
                Some(domains),
            )
            .await
            .map_err(runtime_bridge_error)?;
        Ok(self
            .runtime_overlay
            .record_snapshot(project_id, editor_session_id, snapshot))
    }

    async fn runtime_tree_query(&self, input: RuntimeTreeInput) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Runtime) {
            return error;
        }
        if !(1..=MAX_RESOURCE_LIMIT).contains(&input.limit) {
            return structured_error("invalid_limit", "limit must be between 1 and 200", false);
        }
        let cached = if input.cursor.is_some() {
            match self.runtime_overlay.snapshot() {
                Some(cached)
                    if cached.snapshot.accepted.runtime_session_id == input.runtime_session_id
                        && input.expected_runtime_event_seq.is_none_or(|expected| {
                            expected == cached.snapshot.accepted.runtime_event_seq
                        }) =>
                {
                    cached
                }
                _ => {
                    return structured_error(
                        "stale_cursor",
                        "runtime tree cursor no longer has its memory-only snapshot",
                        false,
                    );
                }
            }
        } else {
            match self
                .runtime_snapshot(
                    &input.runtime_session_id,
                    input.expected_runtime_event_seq,
                    vec![RuntimeDomain::RuntimeState, RuntimeDomain::RuntimeTree],
                )
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error) => return error,
            }
        };
        if input.cursor.is_some()
            && !self
                .runtime_cursor_is_current(
                    &cached.snapshot.accepted.runtime_session_id,
                    cached.snapshot.accepted.runtime_event_seq,
                )
                .await
        {
            return structured_error(
                "stale_cursor",
                "runtime changed after the cursor snapshot was created",
                false,
            );
        }
        let accepted = &cached.snapshot.accepted;
        let binding = CursorBinding {
            project_id: &cached.project_id,
            tool: CursorTool::RuntimeTree,
            selector: &accepted.runtime_session_id,
            limit: input.limit,
            generation_id: &accepted.snapshot_id,
            index_revision: 0,
            resource_revision: 0,
            scene_graph_revision: None,
            script_graph_revision: None,
            editor_session_id: Some(&cached.editor_session_id),
            snapshot_id: Some(&accepted.snapshot_id),
            event_seq: Some(accepted.runtime_event_seq),
            scene_revision: None,
        };
        let now = unix_seconds();
        let offset = match cursor_offset(input.cursor.as_deref(), &self.cursor_codec, &binding, now)
        {
            Ok(offset) => offset,
            Err(error) => return error,
        };
        let nodes = cached
            .snapshot
            .entities
            .iter()
            .filter_map(|entity| match entity {
                RuntimeEntity::RuntimeNode(node) => Some(node),
                _ => None,
            })
            .collect::<Vec<_>>();
        if offset > nodes.len() {
            return structured_error(
                "stale_cursor",
                "runtime tree cursor offset is outside the retained snapshot",
                false,
            );
        }
        let live_snapshot = self
            .replicator
            .read()
            .ok()
            .filter(|snapshot| snapshot.project_id == cached.project_id);
        let launched_scene_path = cached
            .snapshot
            .entities
            .iter()
            .find_map(|entity| match entity {
                RuntimeEntity::RuntimeState(state) => state.scene_path.as_deref(),
                _ => None,
            });
        let runtime_root_path = launched_scene_path.and_then(|scene_path| {
            nodes
                .iter()
                .filter(|node| {
                    node.source_hint.as_ref().is_some_and(|hint| {
                        hint.scene_path.as_deref() == Some(scene_path)
                            && hint.relative_node_path.as_deref() == Some(".")
                    })
                })
                .min_by_key(|node| node.depth)
                .map(|node| node.runtime_node_path.as_str())
        });
        let persistent_snapshot = self.scene_index.pin_current().ok().filter(|snapshot| {
            snapshot.generation().project_id == cached.project_id
                && snapshot.generation().scene.source_complete
        });
        let source_context = match (
            persistent_snapshot.as_ref(),
            live_snapshot.as_deref(),
            launched_scene_path,
            runtime_root_path,
        ) {
            (Some(persistent), Some(live), Some(scene_path), Some(root_path))
                if !accepted.limits_applied.truncated =>
            {
                Some(RuntimeSourceContext {
                    generation: persistent.generation(),
                    live,
                    launched_scene_path: scene_path,
                    runtime_root_path: root_path,
                })
            }
            _ => None,
        };
        let source_unavailable_reason = if accepted.limits_applied.truncated {
            "runtime_tree_truncated"
        } else if persistent_snapshot.is_none() {
            "persistent_scene_state_unavailable"
        } else if live_snapshot.is_none() {
            "live_editor_snapshot_unavailable"
        } else if launched_scene_path.is_none() || runtime_root_path.is_none() {
            "runtime_root_unresolved"
        } else {
            "source_mapping_unavailable"
        };
        let page = nodes
            .iter()
            .skip(offset)
            .take(input.limit)
            .map(|node| runtime_node_view(node, source_context.as_ref(), source_unavailable_reason))
            .collect::<Vec<_>>();
        let has_more = offset
            .checked_add(page.len())
            .is_some_and(|end| end < nodes.len());
        let next_cursor = match next_cursor(
            has_more,
            offset,
            page.len(),
            &self.cursor_codec,
            &binding,
            now,
        ) {
            Ok(cursor) => cursor,
            Err(error) => return error,
        };
        CallToolResult::structured(json!({
            "schema_version": "runtime/1.0",
            "project_id": cached.project_id,
            "editor_session_id": cached.editor_session_id,
            "runtime_session_id": accepted.runtime_session_id,
            "runtime_event_seq": accepted.runtime_event_seq,
            "state": accepted.state,
            "snapshot_id": accepted.snapshot_id,
            "limit": input.limit,
            "offset": offset,
            "total": nodes.len(),
            "nodes": page,
            "truncated": accepted.limits_applied.truncated || has_more,
            "limits_applied": accepted.limits_applied,
            "next_cursor": next_cursor,
            "evidence": [{"source": "checksum_verified_runtime_snapshot", "snapshot_id": accepted.snapshot_id}],
        }))
    }

    async fn runtime_diagnostics_query(&self, input: DiagnosticInput) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Runtime) {
            return error;
        }
        if !(1..=MAX_RESOURCE_LIMIT).contains(&input.limit) {
            return structured_error("invalid_limit", "limit must be between 1 and 200", false);
        }
        let Some(runtime_session_id) = input.runtime_session_id.as_deref() else {
            return structured_error(
                "invalid_query",
                "runtime_session_id is required when diagnostics scope is runtime",
                false,
            );
        };
        let cached = if input.cursor.is_some() {
            match self.runtime_overlay.snapshot() {
                Some(cached)
                    if cached.snapshot.accepted.runtime_session_id == runtime_session_id
                        && input.expected_runtime_event_seq.is_none_or(|expected| {
                            expected == cached.snapshot.accepted.runtime_event_seq
                        }) =>
                {
                    cached
                }
                _ => {
                    return structured_error(
                        "stale_cursor",
                        "runtime diagnostics cursor no longer has its memory-only snapshot",
                        false,
                    );
                }
            }
        } else {
            match self
                .runtime_snapshot(
                    runtime_session_id,
                    input.expected_runtime_event_seq,
                    vec![
                        RuntimeDomain::RuntimeState,
                        RuntimeDomain::RuntimeDiagnostics,
                        RuntimeDomain::RuntimeStacks,
                    ],
                )
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error) => return error,
            }
        };
        if input.cursor.is_some()
            && !self
                .runtime_cursor_is_current(
                    &cached.snapshot.accepted.runtime_session_id,
                    cached.snapshot.accepted.runtime_event_seq,
                )
                .await
        {
            return structured_error(
                "stale_cursor",
                "runtime changed after the cursor snapshot was created",
                false,
            );
        }
        let accepted = &cached.snapshot.accepted;
        let binding = CursorBinding {
            project_id: &cached.project_id,
            tool: CursorTool::Diagnostics,
            selector: &accepted.runtime_session_id,
            limit: input.limit,
            generation_id: &accepted.snapshot_id,
            index_revision: 0,
            resource_revision: 0,
            scene_graph_revision: None,
            script_graph_revision: None,
            editor_session_id: Some(&cached.editor_session_id),
            snapshot_id: Some(&accepted.snapshot_id),
            event_seq: Some(accepted.runtime_event_seq),
            scene_revision: None,
        };
        let now = unix_seconds();
        let offset = match cursor_offset(input.cursor.as_deref(), &self.cursor_codec, &binding, now)
        {
            Ok(offset) => offset,
            Err(error) => return error,
        };
        let diagnostics = cached
            .snapshot
            .entities
            .iter()
            .filter_map(|entity| match entity {
                RuntimeEntity::RuntimeDiagnostic(diagnostic) => Some(diagnostic),
                _ => None,
            })
            .collect::<Vec<_>>();
        let active_stack_id = cached
            .snapshot
            .entities
            .iter()
            .find_map(|entity| match entity {
                RuntimeEntity::RuntimeState(state) => state.active_stack_id.as_deref(),
                _ => None,
            });
        if offset > diagnostics.len() {
            return structured_error(
                "stale_cursor",
                "runtime diagnostics cursor offset is outside the retained snapshot",
                false,
            );
        }
        let page = diagnostics
            .iter()
            .skip(offset)
            .take(input.limit)
            .copied()
            .collect::<Vec<_>>();
        let has_more = offset
            .checked_add(page.len())
            .is_some_and(|end| end < diagnostics.len());
        let next_cursor = match next_cursor(
            has_more,
            offset,
            page.len(),
            &self.cursor_codec,
            &binding,
            now,
        ) {
            Ok(cursor) => cursor,
            Err(error) => return error,
        };
        CallToolResult::structured(json!({
            "schema_version": "runtime/1.0",
            "project_id": cached.project_id,
            "editor_session_id": cached.editor_session_id,
            "runtime_session_id": accepted.runtime_session_id,
            "runtime_event_seq": accepted.runtime_event_seq,
            "state": accepted.state,
            "active_stack_id": active_stack_id,
            "snapshot_id": accepted.snapshot_id,
            "diagnostics": page,
            "limit": input.limit,
            "offset": offset,
            "total": diagnostics.len(),
            "truncated": accepted.limits_applied.truncated || has_more,
            "limits_applied": accepted.limits_applied,
            "next_cursor": next_cursor,
            "evidence": [{"source": "checksum_verified_runtime_snapshot", "snapshot_id": accepted.snapshot_id}],
        }))
    }

    async fn runtime_control(
        &self,
        operation: RuntimeControl,
        guard: Option<&RuntimeGuardInput>,
    ) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Runtime) {
            return error;
        }
        let mut client = match self.runtime_overlay.connect().await {
            Ok(client) => client,
            Err(error) => return runtime_bridge_error(error),
        };
        let result = match operation {
            RuntimeControl::Run(target) => client.run_runtime(target).await,
            RuntimeControl::Stop => {
                let guard = guard.expect("guarded runtime control");
                client
                    .stop_runtime(&guard.runtime_session_id, guard.expected_runtime_event_seq)
                    .await
            }
            RuntimeControl::Pause => {
                let guard = guard.expect("guarded runtime control");
                client
                    .pause_runtime(&guard.runtime_session_id, guard.expected_runtime_event_seq)
                    .await
            }
            RuntimeControl::Continue => {
                let guard = guard.expect("guarded runtime control");
                client
                    .continue_runtime(&guard.runtime_session_id, guard.expected_runtime_event_seq)
                    .await
            }
        };
        match result {
            Ok(state) => {
                self.runtime_overlay.record_state(&state);
                CallToolResult::structured(json!(state))
            }
            Err(error) => runtime_bridge_error(error),
        }
    }

    async fn runtime_cursor_is_current(&self, runtime_session_id: &str, event_seq: u64) -> bool {
        let result = match self.runtime_overlay.connect().await {
            Ok(mut client) => {
                client
                    .get_runtime_snapshot(
                        runtime_session_id,
                        Some(event_seq),
                        Some(vec![RuntimeDomain::RuntimeState]),
                    )
                    .await
            }
            Err(_) => {
                self.runtime_overlay.invalidate("bridge_unavailable");
                return false;
            }
        };
        if result.is_err() {
            self.runtime_overlay.invalidate("runtime_changed");
            return false;
        }
        true
    }

    fn live_list_query(&self, input: LiveListInput, query: LiveListQuery) -> CallToolResult {
        if !(1..=MAX_RESOURCE_LIMIT).contains(&input.limit) {
            return structured_error("invalid_limit", "limit must be between 1 and 200", false);
        }
        let snapshot = match self.live_snapshot(&LiveGuardInput::from(&input)) {
            Ok(snapshot) => snapshot,
            Err(error) => return error,
        };
        let (tool, selector, state, item_key, mut items) = match query {
            LiveListQuery::OpenScenes => (
                CursorTool::OpenScenes,
                "open_scenes",
                Some(snapshot.editor_state.clone()),
                "scenes",
                snapshot.scenes.values().cloned().collect::<Vec<_>>(),
            ),
            LiveListQuery::OpenScripts => (
                CursorTool::OpenScripts,
                "open_scripts",
                snapshot.script_state.clone(),
                "scripts",
                snapshot.script_tabs.values().cloned().collect::<Vec<_>>(),
            ),
            LiveListQuery::EditorHistory => (
                CursorTool::EditorHistory,
                "editor_history",
                snapshot.history_state.clone(),
                "histories",
                snapshot.histories.values().cloned().collect::<Vec<_>>(),
            ),
            LiveListQuery::Diagnostics => (
                CursorTool::Diagnostics,
                "diagnostics",
                snapshot.diagnostic_state.clone(),
                "diagnostics",
                snapshot.diagnostics.values().cloned().collect::<Vec<_>>(),
            ),
        };
        items.sort_by(|left, right| match query {
            LiveListQuery::OpenScenes | LiveListQuery::OpenScripts => left
                .get("tab_index")
                .and_then(Value::as_u64)
                .cmp(&right.get("tab_index").and_then(Value::as_u64)),
            LiveListQuery::Diagnostics => left
                .get("output_seq")
                .and_then(Value::as_u64)
                .cmp(&right.get("output_seq").and_then(Value::as_u64)),
            LiveListQuery::EditorHistory => left
                .get("entity_id")
                .and_then(Value::as_str)
                .cmp(&right.get("entity_id").and_then(Value::as_str)),
        });
        let scene_revision = snapshot
            .current_scene_id
            .as_ref()
            .and_then(|scene_id| snapshot.revisions.scene_revisions.get(scene_id))
            .copied();
        let binding = CursorBinding {
            project_id: &snapshot.project_id,
            tool,
            selector,
            limit: input.limit,
            generation_id: &snapshot.snapshot_id,
            index_revision: 0,
            resource_revision: 0,
            scene_graph_revision: None,
            script_graph_revision: None,
            editor_session_id: Some(&snapshot.editor_session_id),
            snapshot_id: Some(&snapshot.snapshot_id),
            event_seq: Some(snapshot.revisions.event_seq),
            scene_revision,
        };
        let now = unix_seconds();
        let offset = match cursor_offset(input.cursor.as_deref(), &self.cursor_codec, &binding, now)
        {
            Ok(offset) => offset,
            Err(error) => return error,
        };
        if offset > items.len() || offset >= MAX_TOTAL_RESULTS {
            return structured_error(
                "stale_cursor",
                "cursor offset is outside the current editor snapshot",
                false,
            );
        }
        let page = items
            .iter()
            .skip(offset)
            .take(input.limit)
            .cloned()
            .collect::<Vec<_>>();
        let has_more = offset
            .checked_add(input.limit)
            .is_some_and(|end| end < items.len());
        let next_cursor = match next_cursor(
            has_more,
            offset,
            page.len(),
            &self.cursor_codec,
            &binding,
            now,
        ) {
            Ok(cursor) => cursor,
            Err(error) => return error,
        };
        let mut result = json!({
            "schema_version": "editor/1.0",
            "project_id": snapshot.project_id,
            "editor_session_id": snapshot.editor_session_id,
            "snapshot_id": snapshot.snapshot_id,
            "revision_vector": snapshot.revisions,
            "status": "ready",
            "freshness": "current",
            "state": state,
            "limit": input.limit,
            "offset": offset,
            "total": items.len(),
            "truncated": snapshot.truncated || has_more,
            "next_cursor": next_cursor,
            "evidence": [{"source": "live_editor_snapshot", "freshness": "current"}],
        });
        result[item_key] = json!(page);
        CallToolResult::structured(result)
    }

    fn live_snapshot_for_project(
        &self,
        project_id: &str,
    ) -> Result<Option<Arc<SemanticSnapshot>>, CallToolResult> {
        let Ok(snapshot) = self.replicator.read() else {
            return Ok(None);
        };
        LiveOverlay::bind(&snapshot, project_id).map_err(|_| {
            structured_error(
                "editor_project_mismatch",
                "live editor snapshot belongs to another project",
                true,
            )
        })?;
        Ok(Some(snapshot))
    }

    fn dirty_scripts_from(snapshot: Option<&Arc<SemanticSnapshot>>) -> Vec<Value> {
        snapshot
            .and_then(|snapshot| LiveOverlay::bind(snapshot, &snapshot.project_id).ok())
            .map_or_else(Vec::new, |composer| composer.dirty_scripts())
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
        let offline_cached = resource_generation_is_offline(
            &self.resource_index.status(),
            &generation.generation_id,
        );
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
            editor_session_id: None,
            snapshot_id: None,
            event_seq: None,
            scene_revision: None,
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
            | CursorTool::InspectSymbol
            | CursorTool::FindUsages
            | CursorTool::OpenScenes
            | CursorTool::OpenScripts
            | CursorTool::EditorHistory
            | CursorTool::Diagnostics
            | CursorTool::RuntimeTree => unreachable!("resource tool"),
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
            result,
            StaticPage {
                limit: input.limit,
                offset,
                next_cursor,
                offline_cached,
            },
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
        let offline_cached =
            scene_generation_is_offline(&self.scene_index.status(), &generation.generation_id);
        let (scene, canonical_selector) =
            match resolve_scene(&generation.scene.scenes, &input.scene) {
                Ok(scene) => scene,
                Err((code, message)) => return structured_error(code, message, false),
            };
        let live_snapshot = match offline_cached {
            true => None,
            false => match self.live_snapshot_for_project(&generation.project_id) {
                Ok(snapshot) => snapshot,
                Err(error) => return error,
            },
        };
        let live_scene_revision = live_snapshot.as_ref().and_then(|snapshot| {
            let composer = LiveOverlay::bind(snapshot, &generation.project_id).ok()?;
            let live_scene = composer.scene_for_path(&scene.comparison_path)?;
            live_scene.get("scene_revision").and_then(Value::as_u64)
        });
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
            editor_session_id: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.editor_session_id.as_str()),
            snapshot_id: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.snapshot_id.as_str()),
            event_seq: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.revisions.event_seq),
            scene_revision: live_scene_revision,
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
        let mut composed_nodes: Vec<_> = occurrences
            .iter()
            .map(|occurrence| scene_node_view(generation, scene, occurrence, &occurrences))
            .collect();
        let mut live_overlay = json!({"status": "unavailable"});
        let mut live_conflicts = Vec::new();
        if let Some(live_snapshot) = live_snapshot.as_ref() {
            let composer = LiveOverlay::bind(live_snapshot, &generation.project_id)
                .expect("project binding was validated before cursor construction");
            if let Some(composed) =
                composer.compose_scene_nodes(&scene.comparison_path, composed_nodes.clone())
            {
                composed_nodes = composed.nodes;
                live_conflicts = composed.conflicts;
                live_overlay = overlay_metadata(composer.snapshot(), Some(&composed.live_scene));
                if let Some(object) = live_overlay.as_object_mut() {
                    object.insert("status".to_owned(), json!("composed"));
                    object.insert(
                        "coverage".to_owned(),
                        json!(if composed.coverage_complete {
                            "complete"
                        } else {
                            "partial"
                        }),
                    );
                    object.insert(
                        "suppressed_disk_nodes".to_owned(),
                        json!(composed.suppressed_disk_nodes),
                    );
                    object.insert(
                        "live_only_nodes".to_owned(),
                        json!(composed.live_only_nodes),
                    );
                }
            } else {
                live_overlay = overlay_metadata(composer.snapshot(), None);
                live_overlay["status"] = json!("scene_not_open");
            }
        }
        if offset > composed_nodes.len() || offset >= MAX_TOTAL_RESULTS {
            return structured_error(
                "stale_cursor",
                "cursor offset is outside the current scene generation",
                false,
            );
        }
        let has_more = offset
            .checked_add(input.limit)
            .is_some_and(|end| end < composed_nodes.len());
        let page: Vec<_> = composed_nodes
            .iter()
            .skip(offset)
            .take(input.limit)
            .cloned()
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
            ValuePage {
                limit: input.limit,
                offset,
                items: page,
                has_more,
                next_cursor,
            },
            LiveComposition {
                overlay: if offline_cached {
                    json!({"status": "unavailable", "reason": "editor_offline"})
                } else {
                    live_overlay
                },
                conflicts: live_conflicts,
            },
            offline_cached,
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
        let offline_cached =
            scene_generation_is_offline(&self.scene_index.status(), &generation.generation_id);
        let selected = match resolve_node_selection(
            generation,
            input.node_id.as_deref(),
            input.scene.as_deref(),
            input.node_path.as_deref(),
        ) {
            Ok(selected) => selected,
            Err((code, message)) => return structured_error(code, message, false),
        };
        let live_snapshot = match offline_cached {
            true => None,
            false => match self.live_snapshot_for_project(&generation.project_id) {
                Ok(snapshot) => snapshot,
                Err(error) => return error,
            },
        };
        let live_scene_revision = live_snapshot.as_ref().and_then(|snapshot| {
            let composer = LiveOverlay::bind(snapshot, &generation.project_id).ok()?;
            let live_scene = composer.scene_for_path(&selected.scene.comparison_path)?;
            live_scene.get("scene_revision").and_then(Value::as_u64)
        });
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
            editor_session_id: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.editor_session_id.as_str()),
            snapshot_id: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.snapshot_id.as_str()),
            event_seq: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.revisions.event_seq),
            scene_revision: live_scene_revision,
        };
        let now = unix_seconds();
        let offset = match cursor_offset(input.cursor.as_deref(), &self.cursor_codec, &binding, now)
        {
            Ok(offset) => offset,
            Err(result) => return result,
        };
        // Definition properties remain keyed by their canonical definition for
        // direct scene occurrences. Only materialized instance-chain
        // properties are keyed by the occurrence ID.
        let property_subject_id = selected
            .occurrence
            .filter(|occurrence| {
                occurrence
                    .attributes
                    .get("instance_chain")
                    .and_then(Value::as_array)
                    .is_some_and(|chain| !chain.is_empty())
            })
            .map_or(selected.definition.node_entity_id.as_str(), |_| {
                selected.subject_id.as_str()
            });
        let mut properties: Vec<_> = generation
            .scene
            .properties
            .iter()
            .filter(|property| {
                property.scene_entity_id == selected.scene.scene_entity_id
                    && property.subject_entity_id == property_subject_id
            })
            .collect();
        properties.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.property_id.cmp(&right.property_id))
        });
        let mut composed_properties: Vec<_> = properties
            .iter()
            .map(|property| property_view(property))
            .collect();
        let effective_node_path = selected
            .occurrence
            .map(relation_path)
            .unwrap_or(&selected.definition.node_path);
        let mut live_overlay = json!({"status": "unavailable"});
        let mut live_conflicts = Vec::new();
        if let Some(live_snapshot) = live_snapshot.as_ref() {
            let composer = LiveOverlay::bind(live_snapshot, &generation.project_id)
                .expect("project binding was validated before cursor construction");
            if let Some(composed) = composer.compose_node_properties(
                &selected.scene.comparison_path,
                effective_node_path,
                composed_properties.clone(),
            ) {
                composed_properties = composed.properties;
                live_conflicts = composed.conflicts;
                live_overlay = overlay_metadata(composer.snapshot(), Some(&composed.live_scene));
                if let Some(object) = live_overlay.as_object_mut() {
                    object.insert("status".to_owned(), json!("composed"));
                    object.insert("node".to_owned(), composed.live_node);
                    object.insert(
                        "properties_coverage".to_owned(),
                        json!(if composed.properties_complete {
                            "complete"
                        } else {
                            "partial"
                        }),
                    );
                }
            } else {
                live_overlay = overlay_metadata(
                    composer.snapshot(),
                    composer.scene_for_path(&selected.scene.comparison_path),
                );
                live_overlay["status"] = json!("node_not_observed");
            }
        }
        if offset > composed_properties.len() || offset >= MAX_TOTAL_RESULTS {
            return structured_error(
                "stale_cursor",
                "cursor offset is outside the current scene generation",
                false,
            );
        }
        let has_more = offset
            .checked_add(input.limit)
            .is_some_and(|end| end < composed_properties.len());
        let page: Vec<_> = composed_properties
            .iter()
            .skip(offset)
            .take(input.limit)
            .cloned()
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
            ValuePage {
                limit: input.limit,
                offset,
                items: page,
                has_more,
                next_cursor,
            },
            LiveComposition {
                overlay: if offline_cached {
                    json!({"status": "unavailable", "reason": "editor_offline"})
                } else {
                    live_overlay
                },
                conflicts: live_conflicts,
            },
            offline_cached,
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
        let offline_cached =
            script_generation_is_offline(&self.script_index.status(), &generation.generation_id);
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
        let live_snapshot = match offline_cached {
            true => None,
            false => match self.live_snapshot_for_project(&generation.project_id) {
                Ok(snapshot) => snapshot,
                Err(error) => return error,
            },
        };
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
            editor_session_id: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.editor_session_id.as_str()),
            snapshot_id: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.snapshot_id.as_str()),
            event_seq: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.revisions.event_seq),
            scene_revision: None,
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
        let dirty_scripts = Self::dirty_scripts_from(live_snapshot.as_ref());
        search_symbols_success(
            generation,
            &input,
            script_filter.as_ref(),
            result,
            dirty_scripts,
            StaticPage {
                limit: input.limit,
                offset,
                next_cursor,
                offline_cached,
            },
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
        let offline_cached =
            script_generation_is_offline(&self.script_index.status(), &generation.generation_id);
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
        let live_snapshot = match offline_cached {
            true => None,
            false => match self.live_snapshot_for_project(&generation.project_id) {
                Ok(snapshot) => snapshot,
                Err(error) => return error,
            },
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
            editor_session_id: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.editor_session_id.as_str()),
            snapshot_id: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.snapshot_id.as_str()),
            event_seq: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.revisions.event_seq),
            scene_revision: None,
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
        let dirty_scripts = Self::dirty_scripts_from(live_snapshot.as_ref());
        inspect_symbol_success(
            generation,
            &selector_binding,
            result,
            dirty_scripts,
            StaticPage {
                limit: input.limit,
                offset,
                next_cursor,
                offline_cached,
            },
        )
    }

    fn find_usages_query(&self, mut input: FindUsagesInput) -> CallToolResult {
        if !(1..=MAX_RESOURCE_LIMIT).contains(&input.limit) {
            return structured_error("invalid_limit", "limit must be between 1 and 200", false);
        }
        input.source_kinds.sort();
        input.source_kinds.dedup();
        input.confidence.sort();
        input.confidence.dedup();
        let snapshot = match self.semantic_index.pin_current() {
            Ok(snapshot) => snapshot,
            Err(error) => return semantic_index_error(error),
        };
        let generation = snapshot.generation();
        let query_index = snapshot.query_index();
        let offline_cached = snapshot.freshness() == SemanticIndexFreshness::OfflineCached;
        let (target_entity_id, target_binding) =
            match resolve_find_usages_target(generation, query_index, &input.target) {
                Ok(target) => target,
                Err((code, message)) => return structured_error(code, message, false),
            };
        let (scope, scope_binding) =
            match resolve_find_usages_scope(generation, query_index, &input.scope) {
                Ok(scope) => scope,
                Err((code, message)) => return structured_error(code, message, false),
            };
        let source_kinds = input
            .source_kinds
            .iter()
            .copied()
            .map(SemanticEntityKind::from)
            .collect::<Vec<_>>();
        let confidence = input
            .confidence
            .iter()
            .copied()
            .map(SemanticConfidence::from)
            .collect::<Vec<_>>();
        let selector_binding = json!({
            "target": target_binding,
            "source_kinds": input.source_kinds,
            "confidence": input.confidence,
            "scope": scope_binding,
        })
        .to_string();
        let revisions = query_index.revisions();
        let live_snapshot = match offline_cached {
            true => None,
            false => match self.live_snapshot_for_project(&generation.project_id) {
                Ok(snapshot) => snapshot,
                Err(error) => return error,
            },
        };
        let binding = CursorBinding {
            project_id: &generation.project_id,
            tool: CursorTool::FindUsages,
            selector: &selector_binding,
            limit: input.limit,
            generation_id: query_index.generation_id(),
            index_revision: revisions.index_revision,
            resource_revision: revisions.resource_revision,
            scene_graph_revision: revisions.scene_graph_revision,
            script_graph_revision: revisions.script_graph_revision,
            editor_session_id: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.editor_session_id.as_str()),
            snapshot_id: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.snapshot_id.as_str()),
            event_seq: live_snapshot
                .as_ref()
                .map(|snapshot| snapshot.revisions.event_seq),
            scene_revision: None,
        };
        let now = unix_seconds();
        let offset = match cursor_offset(input.cursor.as_deref(), &self.cursor_codec, &binding, now)
        {
            Ok(offset) => offset,
            Err(result) => return result,
        };
        let result = match query_index.find_usages(&FindUsagesQuery {
            target_entity_id,
            source_kinds,
            confidence,
            scope,
            offset,
            limit: input.limit,
        }) {
            Ok(result) => result,
            Err(error) => return find_usages_error(error),
        };
        let next_cursor = match result.next_offset {
            Some(next_offset) => match self.cursor_codec.issue(&binding, next_offset, now) {
                Ok(cursor) => Some(cursor),
                Err(()) => {
                    return structured_error(
                        "index_not_current",
                        "the current semantic index page could not be pinned",
                        true,
                    );
                }
            },
            None => None,
        };
        let dirty_scripts = Self::dirty_scripts_from(live_snapshot.as_ref());
        let partial_reasons = snapshot
            .partial_reasons()
            .iter()
            .map(partial_reason_view)
            .collect::<Vec<_>>();
        let status = if partial_reasons.is_empty() && result.conflicts.is_empty() {
            "exact"
        } else {
            "partial"
        };
        let mut validated_checkpoint = resource_checkpoint(generation, offline_cached);
        if let Some(checkpoint) = validated_checkpoint.as_object_mut() {
            checkpoint.insert(
                "scene_graph_revision".to_owned(),
                json!(revisions.scene_graph_revision),
            );
            checkpoint.insert(
                "script_graph_revision".to_owned(),
                json!(revisions.script_graph_revision),
            );
        }
        static_success(
            json!({
                "project_id": generation.project_id,
                "schema_version": generation.schema_version,
                "generation_id": query_index.generation_id(),
                "index_revision": revisions.index_revision,
                "resource_revision": revisions.resource_revision,
                "scene_graph_revision": revisions.scene_graph_revision,
                "script_graph_revision": revisions.script_graph_revision,
                "freshness": if offline_cached { "offline_cached" } else { "current" },
                "offline_cached": offline_cached,
                "status": status,
                "target": {
                    "entity_id": result.target_entity_id,
                    "kind": result.target_kind,
                    "selector": input.target,
                },
                "scope": input.scope,
                "source_kinds": input.source_kinds,
                "confidence": input.confidence,
                "limit": input.limit,
                "offset": offset,
                "total_matches": result.total_matches,
                "truncated": result.truncated,
                "usages": result.usages,
                "conflicts": result.conflicts,
                "diagnostics": [],
                "partial_reasons": partial_reasons,
                "may_be_stale_for_editor": !dirty_scripts.is_empty(),
                "dirty_open_scripts": dirty_scripts,
                "next_cursor": next_cursor,
                "validated_checkpoint": validated_checkpoint,
                "evidence": {
                    "source": "persistent_semantic_index",
                    "source_complete": snapshot.partial_reasons().is_empty(),
                    "authorities": ["resource_graph", "scene_state", "script_semantics"],
                },
            }),
            offline_cached,
        )
    }

    fn summary_resources() -> Vec<Resource> {
        vec![
            Resource::new(PROJECT_SUMMARY_URI, "godot_project_summary")
                .with_title("Godot project semantic summary")
                .with_description(
                    "Deterministic bounded project context with revisions and evidence IDs",
                )
                .with_mime_type("application/json"),
            Resource::new(EDITOR_SUMMARY_URI, "godot_editor_summary")
                .with_title("Godot live editor summary")
                .with_description(
                    "Deterministic bounded live editor context including dirty and revision state",
                )
                .with_mime_type("application/json"),
            Resource::new(RUNTIME_SUMMARY_URI, "godot_runtime_summary")
                .with_title("Godot runtime diagnostic summary")
                .with_description(
                    "Bounded memory-only lifecycle and diagnostic summary for the local game",
                )
                .with_mime_type("application/json"),
            Resource::new(CONNECTION_STATUS_URI, "godot_connection_status")
                .with_title("Godot connection status")
                .with_description(
                    "Bounded sidecar-authoritative connection, compatibility, cache, and remediation status",
                )
                .with_mime_type("application/json"),
        ]
    }

    fn summary_resource_templates() -> Vec<ResourceTemplate> {
        vec![
            ResourceTemplate::new("godot://scene/{scene_id}/summary", "godot_scene_summary")
                .with_title("Godot scene semantic summary")
                .with_description(
                    "Deterministic bounded context for one percent-encoded canonical scene ID",
                )
                .with_mime_type("application/json"),
        ]
    }

    fn read_summary_resource(&self, uri: &str) -> Result<ReadResourceResult, McpError> {
        if uri == CONNECTION_STATUS_URI {
            let text = serde_json::to_string(&self.connection_status()).map_err(|_| {
                McpError::internal_error(
                    "connection status could not be serialized",
                    Some(json!({"code": "status_unavailable"})),
                )
            })?;
            if text.len() > CONNECTION_STATUS_MAX_BYTES {
                return Err(McpError::internal_error(
                    "connection status exceeded its byte budget",
                    Some(json!({"code": "status_unavailable"})),
                ));
            }
            return Ok(ReadResourceResult::new(vec![
                ResourceContents::text(text, uri).with_mime_type("application/json"),
            ]));
        }
        if uri == RUNTIME_SUMMARY_URI {
            if let Some(error) = self.unavailable_error(AvailabilityDomain::Runtime) {
                let error = error
                    .structured_content
                    .and_then(|value| value.get("error").cloned())
                    .unwrap_or_else(|| {
                        json!({
                            "code": "runtime_unavailable",
                            "message": "The Godot runtime is unavailable.",
                            "retryable": true,
                        })
                    });
                let text = json!({
                    "status": "unavailable",
                    "freshness": "offline",
                    "error": error,
                })
                .to_string();
                return Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(text, uri).with_mime_type("application/json"),
                ]));
            }
            let text = serde_json::to_string(&self.runtime_overlay.summary()).map_err(|_| {
                McpError::internal_error(
                    "runtime summary could not be serialized",
                    Some(json!({"code": "summary_unavailable"})),
                )
            })?;
            if text.len() > EDITOR_SUMMARY_MAX_BYTES {
                return Err(McpError::internal_error(
                    "runtime summary exceeded its byte budget",
                    Some(json!({"code": "summary_unavailable"})),
                ));
            }
            return Ok(ReadResourceResult::new(vec![
                ResourceContents::text(text, uri).with_mime_type("application/json"),
            ]));
        }
        if uri == EDITOR_SUMMARY_URI {
            if let Some(error) = self.unavailable_error(AvailabilityDomain::Editor) {
                let data = error
                    .structured_content
                    .and_then(|value| value.get("error").cloned())
                    .unwrap_or_else(|| {
                        json!({
                            "code": "editor_state_unavailable",
                            "retryable": true,
                        })
                    });
                return Err(McpError::resource_not_found(
                    "live editor summary is unavailable until the exact editor binding is ready",
                    Some(data),
                ));
            }
            let snapshot = self.replicator.read().map_err(|error| {
                McpError::resource_not_found(
                    "live editor summary is not currently available",
                    Some(json!({
                        "code": "editor_state_unavailable",
                        "retryable": true,
                        "replica_status": error.status,
                    })),
                )
            })?;
            let text = build_editor_summary(&snapshot)?;
            return Ok(ReadResourceResult::new(vec![
                ResourceContents::text(text, uri).with_mime_type("application/json"),
            ]));
        }
        let snapshot = self
            .semantic_index
            .pin_current()
            .map_err(summary_index_error)?;
        let offline_cached = snapshot.freshness() == SemanticIndexFreshness::OfflineCached;
        let text = if uri == PROJECT_SUMMARY_URI {
            build_project_summary(snapshot.generation(), snapshot.query_index())
                .map_err(summary_build_error)?
        } else {
            let scene_id = parse_scene_summary_uri(uri)?;
            build_scene_summary(snapshot.generation(), snapshot.query_index(), &scene_id)
                .map_err(summary_build_error)?
        };
        let text = mark_summary_freshness(&text, offline_cached)?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, uri).with_mime_type("application/json"),
        ]))
    }
}

fn mark_summary_freshness(text: &str, offline_cached: bool) -> Result<String, McpError> {
    if !offline_cached {
        return Ok(text.to_owned());
    }
    let mut summary: Value = serde_json::from_str(text).map_err(|_| {
        McpError::internal_error(
            "semantic summary could not be decoded",
            Some(json!({"code": "summary_unavailable"})),
        )
    })?;
    let object = summary.as_object_mut().ok_or_else(|| {
        McpError::internal_error(
            "semantic summary has an invalid shape",
            Some(json!({"code": "summary_unavailable"})),
        )
    })?;
    object.insert(
        "freshness".to_owned(),
        json!(static_freshness_label(offline_cached)),
    );
    object.insert("offline_cached".to_owned(), json!(offline_cached));
    let byte_limit = match summary.get("kind").and_then(Value::as_str) {
        Some("godot_project_summary") => 4_096,
        Some("godot_scene_summary") => 2_048,
        _ => 2_048,
    };
    let reduction_order = [
        "evidence_ids",
        "high_degree_relations",
        "diagnostics",
        "important_scripts",
        "important_resources",
        "important_scenes",
        "nodes",
        "connections",
        "groups",
        "facts",
    ];
    loop {
        let encoded = serde_json::to_string(&summary).map_err(|_| {
            McpError::internal_error(
                "semantic summary could not be serialized",
                Some(json!({"code": "summary_unavailable"})),
            )
        })?;
        if encoded.len() <= byte_limit {
            return Ok(encoded);
        }
        let mut reduced = false;
        for key in reduction_order {
            if let Some(values) = summary.get_mut(key).and_then(Value::as_array_mut)
                && !values.is_empty()
            {
                values.pop();
                reduced = true;
                break;
            }
        }
        if !reduced {
            return Err(McpError::internal_error(
                "semantic summary could not be bounded",
                Some(json!({"code": "summary_unavailable"})),
            ));
        }
    }
}

fn resource_index_observation(status: ResourceIndexStatus) -> IndexObservation {
    match status {
        ResourceIndexStatus::ProjectNotBound => IndexObservation::Offline,
        ResourceIndexStatus::NotReady => IndexObservation::Syncing,
        ResourceIndexStatus::Current {
            project_id,
            generation_id,
            index_revision,
            resource_revision,
            ..
        }
        | ResourceIndexStatus::OfflineCurrent {
            project_id,
            generation_id,
            index_revision,
            resource_revision,
            ..
        } => IndexObservation::Current(IndexCoordinates {
            project_id,
            generation_id,
            index_revision,
            resource_revision,
            scene_graph_revision: 0,
            script_graph_revision: 0,
        }),
        ResourceIndexStatus::NotCurrent { reason } => match reason {
            ResourceIndexStaleReason::BridgeDisconnected => IndexObservation::Disconnected,
            ResourceIndexStaleReason::StartupValidation
            | ResourceIndexStaleReason::JournalGap
            | ResourceIndexStaleReason::Rebuilding
            | ResourceIndexStaleReason::StoreUnavailable => IndexObservation::Stale,
        },
        ResourceIndexStatus::CapabilityUnavailable => IndexObservation::Degraded,
    }
}

fn resource_generation_is_offline(status: &ResourceIndexStatus, generation_id: &str) -> bool {
    matches!(
        status,
        ResourceIndexStatus::OfflineCurrent {
            generation_id: current,
            ..
        } if current == generation_id
    )
}

fn scene_generation_is_offline(status: &SceneIndexStatus, generation_id: &str) -> bool {
    matches!(
        status,
        SceneIndexStatus::OfflineCurrent {
            generation_id: current,
            ..
        } if current == generation_id
    )
}

fn script_generation_is_offline(status: &ScriptIndexStatus, generation_id: &str) -> bool {
    matches!(
        status,
        ScriptIndexStatus::OfflineCurrent {
            generation_id: current,
            ..
        } if current == generation_id
    )
}

fn static_freshness_label(offline_cached: bool) -> &'static str {
    if offline_cached {
        "offline_cached"
    } else {
        "current"
    }
}

fn resource_checkpoint(
    generation: &godot_codex_index_store::IndexGeneration,
    offline_cached: bool,
) -> Value {
    let mut checkpoint = serde_json::to_value(&generation.checkpoint)
        .expect("validated index checkpoint is serializable");
    if offline_cached {
        checkpoint
            .as_object_mut()
            .expect("checkpoint is an object")
            .remove("editor_session_id");
    }
    checkpoint
}

fn scene_index_observation(status: SceneIndexStatus) -> IndexObservation {
    match status {
        SceneIndexStatus::ProjectNotBound => IndexObservation::Offline,
        SceneIndexStatus::NotReady => IndexObservation::Syncing,
        SceneIndexStatus::Current {
            project_id,
            generation_id,
            index_revision,
            resource_revision,
            scene_graph_revision,
            ..
        }
        | SceneIndexStatus::OfflineCurrent {
            project_id,
            generation_id,
            index_revision,
            resource_revision,
            scene_graph_revision,
            ..
        } => IndexObservation::Current(IndexCoordinates {
            project_id,
            generation_id,
            index_revision,
            resource_revision,
            scene_graph_revision,
            script_graph_revision: 0,
        }),
        SceneIndexStatus::NotCurrent { reason } => match reason {
            SceneIndexStaleReason::BridgeDisconnected => IndexObservation::Disconnected,
            SceneIndexStaleReason::StartupValidation
            | SceneIndexStaleReason::JournalGap
            | SceneIndexStaleReason::ResourceChanged
            | SceneIndexStaleReason::Rebuilding
            | SceneIndexStaleReason::StoreUnavailable => IndexObservation::Stale,
        },
        SceneIndexStatus::CapabilityUnavailable => IndexObservation::Degraded,
    }
}

fn script_index_observation(status: ScriptIndexStatus) -> IndexObservation {
    match status {
        ScriptIndexStatus::ProjectNotBound => IndexObservation::Offline,
        ScriptIndexStatus::NotReady => IndexObservation::Syncing,
        ScriptIndexStatus::Current {
            project_id,
            generation_id,
            index_revision,
            resource_revision,
            scene_graph_revision,
            script_graph_revision,
            ..
        }
        | ScriptIndexStatus::OfflineCurrent {
            project_id,
            generation_id,
            index_revision,
            resource_revision,
            scene_graph_revision,
            script_graph_revision,
            ..
        } => IndexObservation::Current(IndexCoordinates {
            project_id,
            generation_id,
            index_revision,
            resource_revision,
            scene_graph_revision,
            script_graph_revision,
        }),
        ScriptIndexStatus::NotCurrent { reason } => match reason {
            ScriptIndexStaleReason::BridgeDisconnected => IndexObservation::Disconnected,
            ScriptIndexStaleReason::StartupValidation
            | ScriptIndexStaleReason::JournalGap
            | ScriptIndexStaleReason::ResourceChanged
            | ScriptIndexStaleReason::SceneChanged
            | ScriptIndexStaleReason::Rebuilding
            | ScriptIndexStaleReason::CompositionFailed
            | ScriptIndexStaleReason::StoreUnavailable => IndexObservation::Stale,
        },
        ScriptIndexStatus::CapabilityUnavailable => IndexObservation::Degraded,
    }
}

fn cache_age_seconds(
    project_root: Option<&std::path::Path>,
    offline_verified: bool,
    resource: &ResourceIndexStatus,
    scene: &SceneIndexStatus,
    script: &ScriptIndexStatus,
) -> Option<u64> {
    let project_root = project_root?;
    let resource_current = matches!(
        resource,
        ResourceIndexStatus::Current { .. } | ResourceIndexStatus::OfflineCurrent { .. }
    );
    let scene_current = matches!(
        scene,
        SceneIndexStatus::Current { .. } | SceneIndexStatus::OfflineCurrent { .. }
    );
    let script_current = matches!(
        script,
        ScriptIndexStatus::Current { .. } | ScriptIndexStatus::OfflineCurrent { .. }
    );
    if !(resource_current && scene_current && script_current) {
        return None;
    }
    let modified = if offline_verified {
        safe_file_modified(
            &project_root
                .join(".godot")
                .join("codex")
                .join("offline-authority-v1.json"),
        )?
    } else {
        latest_commit_modified(
            &project_root
                .join(".godot")
                .join("codex")
                .join("index")
                .join("commits"),
        )?
    };
    Some(
        SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::ZERO)
            .as_secs(),
    )
}

fn latest_commit_modified(directory: &std::path::Path) -> Option<SystemTime> {
    let metadata = fs::symlink_metadata(directory).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    let mut latest = None;
    let mut count = 0_usize;
    for entry in fs::read_dir(directory).ok()? {
        count = count.checked_add(1)?;
        if count > 16 {
            return None;
        }
        let entry = entry.ok()?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let modified = safe_file_modified(&path)?;
        latest = Some(latest.map_or(modified, |current: SystemTime| current.max(modified)));
    }
    latest
}

fn safe_file_modified(path: &std::path::Path) -> Option<SystemTime> {
    let metadata = fs::symlink_metadata(path).ok()?;
    (!metadata.file_type().is_symlink() && metadata.is_file())
        .then(|| metadata.modified().ok())
        .flatten()
}

fn runtime_observation(summary: &Value, replica_status: ReplicaStatus) -> ComponentCondition {
    if replica_status == ReplicaStatus::Disconnected {
        return ComponentCondition::Offline;
    }
    if summary.get("freshness").and_then(Value::as_str) == Some("stale")
        || summary.get("status").and_then(Value::as_str) == Some("invalidated")
    {
        return ComponentCondition::Stale;
    }
    if summary.get("state").and_then(Value::as_str) == Some("disconnected") {
        return ComponentCondition::Disconnected;
    }
    match (
        summary.get("status").and_then(Value::as_str),
        summary.get("freshness").and_then(Value::as_str),
    ) {
        (Some("ready"), Some("current")) => ComponentCondition::Ready,
        (Some("unavailable"), _) => ComponentCondition::Unavailable,
        _ => ComponentCondition::Degraded,
    }
}

fn build_editor_summary(snapshot: &SemanticSnapshot) -> Result<String, McpError> {
    let mut scenes = snapshot
        .scenes
        .values()
        .map(|scene| {
            json!({
                "scene_id": scene.get("entity_id"),
                "path": scene.get("path"),
                "title": scene.get("title"),
                "current": scene.get("current"),
                "dirty": scene.get("dirty"),
                "scene_revision": scene.get("scene_revision"),
            })
        })
        .collect::<Vec<_>>();
    scenes.sort_by(|left, right| {
        left.get("scene_id")
            .and_then(Value::as_str)
            .cmp(&right.get("scene_id").and_then(Value::as_str))
    });
    let mut scripts = snapshot
        .script_tabs
        .values()
        .map(|script| {
            json!({
                "script_id": script.get("entity_id"),
                "path": script.get("path"),
                "active": script.get("active"),
                "dirty": script.get("dirty"),
                "selection_count": script.get("selections").and_then(Value::as_array).map(Vec::len),
            })
        })
        .collect::<Vec<_>>();
    scripts.sort_by(|left, right| {
        left.get("script_id")
            .and_then(Value::as_str)
            .cmp(&right.get("script_id").and_then(Value::as_str))
    });
    let mut histories = snapshot
        .histories
        .values()
        .map(|history| {
            json!({
                "history_id": history.get("entity_id"),
                "scene_id": history.get("scene_id"),
                "action_name": history.get("action_name"),
                "saved_state": history.get("saved_state"),
                "can_undo": history.get("can_undo"),
                "can_redo": history.get("can_redo"),
                "transition_kind": history.get("transition_kind"),
                "last_operation_seq": history.get("last_operation_seq"),
            })
        })
        .collect::<Vec<_>>();
    histories.sort_by(|left, right| {
        left.get("history_id")
            .and_then(Value::as_str)
            .cmp(&right.get("history_id").and_then(Value::as_str))
    });
    let dirty_scene_count = scenes
        .iter()
        .filter(|scene| scene.get("dirty").and_then(Value::as_bool) == Some(true))
        .count();
    let dirty_script_count = scripts
        .iter()
        .filter(|script| script.get("dirty").and_then(Value::as_bool) == Some(true))
        .count();
    let mut summary = json!({
        "schema_version": "editor/1.0",
        "project_id": snapshot.project_id,
        "editor_session_id": snapshot.editor_session_id,
        "snapshot_id": snapshot.snapshot_id,
        "revision_vector": snapshot.revisions,
        "current_scene_id": snapshot.current_scene_id,
        "selected_node_ids": snapshot.selected_node_ids,
        "inspector_object_id": snapshot.inspector_state.as_ref().and_then(|state| state.get("object_id")),
        "active_script_id": snapshot.script_state.as_ref().and_then(|state| state.get("active_script_id")),
        "dirty_scene_count": dirty_scene_count,
        "dirty_script_count": dirty_script_count,
        "diagnostic_count": snapshot.diagnostics.len(),
        "viewport_kind": snapshot.viewport_state.as_ref().and_then(|state| state.get("active_kind")),
        "scenes": scenes,
        "scripts": scripts,
        "histories": histories,
        "truncated": snapshot.truncated,
        "evidence": [{"source": "live_editor_snapshot", "freshness": "current"}],
    });
    let reduction_order = ["histories", "scripts", "scenes", "selected_node_ids"];
    let mut text = serde_json::to_string(&summary).map_err(|_| {
        McpError::internal_error(
            "live editor summary serialization failed".to_owned(),
            Some(json!({"code": "summary_unavailable"})),
        )
    })?;
    while text.len() > EDITOR_SUMMARY_MAX_BYTES {
        let mut reduced = false;
        for key in reduction_order {
            if let Some(values) = summary.get_mut(key).and_then(Value::as_array_mut)
                && !values.is_empty()
            {
                values.pop();
                summary["truncated"] = json!(true);
                reduced = true;
                break;
            }
        }
        if !reduced {
            return Err(McpError::internal_error(
                "live editor summary could not be bounded".to_owned(),
                Some(json!({"code": "summary_unavailable"})),
            ));
        }
        text = serde_json::to_string(&summary).map_err(|_| {
            McpError::internal_error(
                "live editor summary serialization failed".to_owned(),
                Some(json!({"code": "summary_unavailable"})),
            )
        })?;
    }
    Ok(text)
}

fn resolve_find_usages_target(
    generation: &godot_codex_index_store::IndexGeneration,
    query_index: &SemanticQueryIndex,
    target: &FindUsagesTargetInput,
) -> Result<(String, String), (&'static str, &'static str)> {
    match target {
        FindUsagesTargetInput::EntityId { entity_id } => {
            if entity_id.is_empty()
                || entity_id.len() > 256
                || entity_id.chars().any(char::is_control)
                || query_index.entity_kind(entity_id).is_none()
            {
                return Err((
                    "target_not_found",
                    "entity ID was not found in the current semantic index",
                ));
            }
            Ok((entity_id.clone(), format!("entity_id:{entity_id}")))
        }
        FindUsagesTargetInput::Resource { selector } => {
            let (selector, canonical, _) = parse_selector(selector)?;
            let resource = match &selector {
                ResourceSelector::EntityId(entity_id) => generation
                    .resources
                    .iter()
                    .find(|resource| resource.entity_id == *entity_id),
                ResourceSelector::Uid(uid) => generation
                    .resources
                    .iter()
                    .find(|resource| resource.uid.as_ref() == Some(uid)),
                ResourceSelector::ComparisonPath(path) => generation
                    .resources
                    .iter()
                    .find(|resource| resource.comparison_path == *path),
            }
            .ok_or((
                "resource_not_found",
                "resource was not found in the current index",
            ))?;
            Ok((resource.entity_id.clone(), format!("resource:{canonical}")))
        }
        FindUsagesTargetInput::Scene { selector } => {
            require_semantic_scene(query_index)?;
            let (scene, canonical) = resolve_scene(&generation.scene.scenes, selector)?;
            Ok((scene.scene_entity_id.clone(), format!("scene:{canonical}")))
        }
        FindUsagesTargetInput::NodePath { scene, node_path } => {
            require_semantic_scene(query_index)?;
            if !valid_node_path(node_path) {
                return Err(("invalid_query", "node_path is invalid or unsafe"));
            }
            let selected = resolve_node_selection(generation, None, Some(scene), Some(node_path))?;
            Ok((
                selected.subject_id,
                format!("node_path:{}", selected.canonical_selector),
            ))
        }
        FindUsagesTargetInput::ScriptSymbol {
            script,
            qualified_name,
        } => {
            require_semantic_script(query_index)?;
            if !valid_qualified_name(qualified_name) {
                return Err(("invalid_query", "qualified_name is invalid"));
            }
            let (document, canonical) = resolve_script(generation, script)?;
            let symbol = generation
                .script
                .symbols
                .iter()
                .find(|symbol| {
                    symbol.script_resource_id == document.script_resource_id
                        && symbol.qualified_key == *qualified_name
                })
                .ok_or((
                    "symbol_not_found",
                    "symbol was not found in the current script index",
                ))?;
            Ok((
                symbol.symbol_id.clone(),
                format!("script_symbol:{canonical}\0qualified_name:{qualified_name}"),
            ))
        }
        FindUsagesTargetInput::Signal {
            scene,
            emitter_node_path,
            signal,
        } => {
            require_semantic_scene(query_index)?;
            require_semantic_script(query_index)?;
            if !valid_node_path(emitter_node_path) || !valid_signal_name(signal) {
                return Err(("invalid_query", "signal selector is invalid or unsafe"));
            }
            let selected =
                resolve_node_selection(generation, None, Some(scene), Some(emitter_node_path))?;
            let occurrence_signal = signal_entity_id(
                &selected.scene.scene_entity_id,
                &selected.subject_id,
                signal,
            );
            let definition_signal = signal_entity_id(
                &selected.scene.scene_entity_id,
                &selected.definition.node_entity_id,
                signal,
            );
            let entity_id = [definition_signal, occurrence_signal]
                .into_iter()
                .find(|candidate| query_index.entity_kind(candidate).is_some())
                .ok_or((
                    "signal_not_found",
                    "signal was not found in the current semantic index",
                ))?;
            Ok((
                entity_id,
                format!("signal:{}\0name:{signal}", selected.canonical_selector),
            ))
        }
    }
}

fn resolve_find_usages_scope(
    generation: &godot_codex_index_store::IndexGeneration,
    query_index: &SemanticQueryIndex,
    scope: &FindUsagesScopeInput,
) -> Result<(FindUsagesScope, String), (&'static str, &'static str)> {
    match scope {
        FindUsagesScopeInput::Project => Ok((FindUsagesScope::Project, "project".to_owned())),
        FindUsagesScopeInput::Scene { scene } => {
            require_semantic_scene(query_index)?;
            let (scene, canonical) = resolve_scene(&generation.scene.scenes, scene)?;
            Ok((
                FindUsagesScope::Scene {
                    scene_entity_id: scene.scene_entity_id.clone(),
                },
                format!("scene:{canonical}"),
            ))
        }
        FindUsagesScopeInput::Script { script } => {
            require_semantic_script(query_index)?;
            let (script, canonical) = resolve_script(generation, script)?;
            Ok((
                FindUsagesScope::Script {
                    script_resource_id: script.script_resource_id.clone(),
                },
                format!("script:{canonical}"),
            ))
        }
    }
}

fn require_semantic_scene(
    query_index: &SemanticQueryIndex,
) -> Result<(), (&'static str, &'static str)> {
    query_index
        .revisions()
        .scene_graph_revision
        .ok_or((
            "scene_index_not_current",
            "scene target or scope requires a current scene index",
        ))
        .map(|_| ())
}

fn require_semantic_script(
    query_index: &SemanticQueryIndex,
) -> Result<(), (&'static str, &'static str)> {
    query_index
        .revisions()
        .script_graph_revision
        .ok_or((
            "script_index_not_current",
            "script target or scope requires a current script index",
        ))
        .map(|_| ())
}

fn valid_signal_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value == value.trim()
        && !value.chars().any(char::is_control)
        && !value.contains(['/', '\\', ':'])
}

fn partial_reason_view(reason: &SemanticPartialReason) -> Value {
    let domain = match reason.domain {
        SemanticPartialDomain::Scene => "scene",
        SemanticPartialDomain::Script => "script",
    };
    let code = match reason.code {
        SemanticPartialCode::ProjectNotBound => "project_not_bound",
        SemanticPartialCode::NotReady => "not_ready",
        SemanticPartialCode::NotCurrent => "not_current",
        SemanticPartialCode::CapabilityUnavailable => "capability_unavailable",
    };
    json!({"domain": domain, "code": code})
}

fn semantic_index_error(error: SemanticIndexReadError) -> CallToolResult {
    match error {
        SemanticIndexReadError::ProjectNotBound => structured_error(
            "project_not_bound",
            "no project is bound to the semantic index",
            true,
        ),
        SemanticIndexReadError::NotReady => structured_error(
            "index_not_ready",
            "semantic index has no committed resource generation",
            true,
        ),
        SemanticIndexReadError::CapabilityUnavailable => structured_error(
            "capability_unavailable",
            "semantic indexing is unavailable for this project",
            false,
        ),
        SemanticIndexReadError::NotCurrent | SemanticIndexReadError::TornGeneration => {
            structured_error(
                "index_not_current",
                "semantic domains could not be pinned to one immutable generation",
                true,
            )
        }
    }
}

fn find_usages_error(error: FindUsagesQueryError) -> CallToolResult {
    match error {
        FindUsagesQueryError::TargetNotFound => structured_error(
            "target_not_found",
            "target was not found in the current semantic index",
            false,
        ),
        FindUsagesQueryError::InvalidLimit => {
            structured_error("invalid_limit", "limit must be between 1 and 200", false)
        }
        FindUsagesQueryError::ResultWindowExceeded => structured_error(
            "result_limit_exceeded",
            "usage query exceeded the bounded result window",
            false,
        ),
    }
}

fn parse_scene_summary_uri(uri: &str) -> Result<String, McpError> {
    let segment = uri
        .strip_prefix(SCENE_SUMMARY_PREFIX)
        .and_then(|value| value.strip_suffix(SCENE_SUMMARY_SUFFIX))
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 768
                && !value.contains(['/', '?', '#'])
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| summary_not_found("invalid scene summary URI"))?;
    let mut decoded = Vec::with_capacity(segment.len());
    let bytes = segment.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let high = uppercase_hex(bytes[index + 1])
                    .ok_or_else(|| summary_not_found("invalid scene summary URI"))?;
                let low = uppercase_hex(bytes[index + 2])
                    .ok_or_else(|| summary_not_found("invalid scene summary URI"))?;
                decoded.push((high << 4) | low);
                index += 3;
            }
            byte if is_unreserved(byte) => {
                decoded.push(byte);
                index += 1;
            }
            _ => return Err(summary_not_found("invalid scene summary URI")),
        }
    }
    let scene_id =
        String::from_utf8(decoded).map_err(|_| summary_not_found("invalid scene summary URI"))?;
    if !scene_id.starts_with("godot:scene:") || encode_uri_segment(&scene_id) != segment {
        return Err(summary_not_found("invalid scene summary URI"));
    }
    Ok(scene_id)
}

fn encode_uri_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if is_unreserved(byte) {
            encoded.push(char::from(byte));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn uppercase_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn summary_not_found(message: &'static str) -> McpError {
    McpError::resource_not_found(message, Some(json!({"code": "resource_not_found"})))
}

fn summary_index_error(error: SemanticIndexReadError) -> McpError {
    let code = match error {
        SemanticIndexReadError::ProjectNotBound => "project_not_bound",
        SemanticIndexReadError::NotReady => "index_not_ready",
        SemanticIndexReadError::NotCurrent | SemanticIndexReadError::TornGeneration => {
            "index_not_current"
        }
        SemanticIndexReadError::CapabilityUnavailable => "capability_unavailable",
    };
    McpError::resource_not_found(
        "semantic summary is not currently available",
        Some(json!({"code": code, "retryable": code != "capability_unavailable"})),
    )
}

fn summary_build_error(error: ContextSummaryError) -> McpError {
    match error {
        ContextSummaryError::SceneNotFound => summary_not_found("scene summary was not found"),
        ContextSummaryError::BudgetTooSmall | ContextSummaryError::Serialization => {
            McpError::internal_error(
                "semantic summary could not be bounded".to_owned(),
                Some(json!({"code": "summary_unavailable"})),
            )
        }
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

struct StaticPage {
    limit: usize,
    offset: usize,
    next_cursor: Option<String>,
    offline_cached: bool,
}

fn search_symbols_success(
    generation: &godot_codex_index_store::IndexGeneration,
    input: &SearchSymbolsInput,
    script_filter: Option<&(String, String, String)>,
    result: ScriptSymbolQueryResult,
    dirty_scripts: Vec<Value>,
    page: StaticPage,
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
    let relevant_paths: BTreeSet<_> = generation
        .script
        .documents
        .iter()
        .filter(|document| script_ids.contains(document.script_resource_id.as_str()))
        .map(|document| document.path.as_str())
        .collect();
    let dirty_scripts = dirty_scripts
        .into_iter()
        .filter(|script| {
            script
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(|path| relevant_paths.contains(path))
        })
        .collect::<Vec<_>>();
    if !dirty_scripts.is_empty() {
        partial_reasons.insert("dirty_open_script_not_parsed".to_owned());
    }
    let symbols = result
        .symbols
        .iter()
        .map(|symbol| script_symbol_view(generation, symbol))
        .collect::<Vec<_>>();
    static_success(
        json!({
            "project_id": generation.project_id,
            "schema_version": generation.schema_version,
            "generation_id": generation.generation_id,
            "index_revision": generation.index_revision,
            "resource_revision": generation.script.resource_revision,
            "scene_graph_revision": generation.script.scene_graph_revision,
            "script_graph_revision": generation.script.script_graph_revision,
            "freshness": static_freshness_label(page.offline_cached),
            "offline_cached": page.offline_cached,
            "status": if partial_reasons.is_empty() { "exact" } else { "partial" },
            "query": {
                "query": input.query,
                "match": input.match_mode,
                "language": input.language,
                "kind": input.kind,
                "script": script_filter.map(|(_, _, path)| path),
                "limit": page.limit,
                "offset": page.offset,
            },
            "symbols": symbols,
            "total_matches": result.total_matches,
            "diagnostics": diagnostics,
            "partial_reasons": partial_reasons,
            "truncated": result.has_more,
            "next_cursor": page.next_cursor,
            "may_be_stale_for_editor": !dirty_scripts.is_empty(),
            "dirty_open_scripts": dirty_scripts,
            "validated_checkpoint": script_checkpoint(generation, page.offline_cached),
            "evidence": {
                "source": "persistent_segment_index",
                "source_complete": generation.script.source_complete,
                "authorities": ["gdscript_parser_analyzer", "resource_graph", "scene_state"],
            },
        }),
        page.offline_cached,
    )
}

fn inspect_symbol_success(
    generation: &godot_codex_index_store::IndexGeneration,
    selector: &str,
    result: ScriptSymbolInspectionResult,
    dirty_scripts: Vec<Value>,
    page: StaticPage,
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
    let dirty_scripts = dirty_scripts
        .into_iter()
        .filter(|script| {
            script.get("path").and_then(Value::as_str) == Some(result.document.path.as_str())
        })
        .collect::<Vec<_>>();
    if !dirty_scripts.is_empty() {
        partial_reasons.insert("dirty_open_script_not_parsed".to_owned());
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
    static_success(
        json!({
            "project_id": generation.project_id,
            "schema_version": generation.schema_version,
            "generation_id": generation.generation_id,
            "index_revision": generation.index_revision,
            "resource_revision": generation.script.resource_revision,
            "scene_graph_revision": generation.script.scene_graph_revision,
            "script_graph_revision": generation.script.script_graph_revision,
            "freshness": static_freshness_label(page.offline_cached),
            "offline_cached": page.offline_cached,
            "status": if partial_reasons.is_empty() { "exact" } else { "partial" },
            "query": {"selector": selector, "limit": page.limit, "offset": page.offset},
            "document": script_document_view(&result.document),
            "declaration": script_symbol_view(generation, &result.symbol),
            "owner": result.owner.as_ref().map(|owner| script_symbol_view(generation, owner)),
            "outgoing_relations": outgoing_relations,
            "scene_attachments": scene_attachments,
            "total_relations": result.total_relations,
            "diagnostics": diagnostics,
            "partial_reasons": partial_reasons,
            "truncated": result.has_more,
            "next_cursor": page.next_cursor,
            "may_be_stale_for_editor": !dirty_scripts.is_empty(),
            "dirty_open_scripts": dirty_scripts,
            "validated_checkpoint": script_checkpoint(generation, page.offline_cached),
            "evidence": {
                "source": "persistent_segment_index",
                "source_complete": generation.script.source_complete,
                "authorities": ["gdscript_parser_analyzer", "resource_graph", "scene_state"],
            },
        }),
        page.offline_cached,
    )
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

fn script_checkpoint(
    generation: &godot_codex_index_store::IndexGeneration,
    offline_cached: bool,
) -> Value {
    let mut checkpoint = json!({
        "editor_session_id": generation.script.editor_session_id,
        "resource_revision": generation.script.resource_revision,
        "scene_graph_revision": generation.script.scene_graph_revision,
        "script_graph_revision": generation.script.script_graph_revision,
        "source_complete": generation.script.source_complete,
        "snapshot_checksum": generation.script.snapshot_checksum,
        "semantic_digest": generation.script.semantic_digest,
    });
    if offline_cached {
        checkpoint
            .as_object_mut()
            .expect("checkpoint is an object")
            .remove("editor_session_id");
    }
    checkpoint
}

struct ValuePage {
    limit: usize,
    offset: usize,
    items: Vec<Value>,
    has_more: bool,
    next_cursor: Option<String>,
}

struct LiveComposition {
    overlay: Value,
    conflicts: Vec<Value>,
}

fn scene_graph_success(
    generation: &godot_codex_index_store::IndexGeneration,
    scene: &SceneEntity,
    page: ValuePage,
    live: LiveComposition,
    offline_cached: bool,
) -> CallToolResult {
    let subjects: BTreeSet<_> = page
        .items
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
    static_success(
        json!({
            "project_id": generation.project_id,
            "schema_version": generation.schema_version,
            "generation_id": generation.generation_id,
            "index_revision": generation.index_revision,
            "resource_revision": generation.scene.resource_revision,
            "scene_graph_revision": generation.scene.scene_graph_revision,
            "freshness": static_freshness_label(offline_cached),
            "offline_cached": offline_cached,
            "status": if partial_reasons.is_empty() { "exact" } else { "partial" },
            "scene": scene_view(scene),
            "query": {"scene": scene.comparison_path, "limit": page.limit, "offset": page.offset},
            "nodes": page.items,
            "project_context": project_context,
            "project_context_truncated": project_context_truncated,
            "diagnostics": diagnostics,
            "partial_reasons": partial_reasons,
            "truncated": page.has_more,
            "next_cursor": page.next_cursor,
            "live_overlay": live.overlay,
            "conflicts": live.conflicts,
            "validated_checkpoint": scene_checkpoint(generation, offline_cached),
            "evidence": {
                "source": "persistent_segment_index",
                "source_complete": generation.scene.source_complete,
                "authorities": ["packed_scene_state", "scene_composer"],
            },
        }),
        offline_cached,
    )
}

fn inspect_node_success(
    generation: &godot_codex_index_store::IndexGeneration,
    selected: &NodeSelection<'_>,
    page: ValuePage,
    live: LiveComposition,
    offline_cached: bool,
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
    if page.items.iter().any(|property| {
        property
            .pointer("/value/truncated")
            .and_then(Value::as_bool)
            == Some(true)
    }) {
        partial_reasons.insert("projected_value_truncated".to_owned());
    }
    static_success(
        json!({
            "project_id": generation.project_id,
            "schema_version": generation.schema_version,
            "generation_id": generation.generation_id,
            "index_revision": generation.index_revision,
            "resource_revision": generation.scene.resource_revision,
            "scene_graph_revision": generation.scene.scene_graph_revision,
            "freshness": static_freshness_label(offline_cached),
            "offline_cached": offline_cached,
            "status": if partial_reasons.is_empty() { "exact" } else { "partial" },
            "scene": scene_view(selected.scene),
            "node": node,
            "query": {"selector": selected.canonical_selector, "limit": page.limit, "offset": page.offset},
            "properties": page.items,
            "attached_script_resource_id": selected.definition.attached_script_entity_id,
            "resources": relations,
            "groups": groups,
            "connections": connections,
            "animation_references": animations,
            "diagnostics": diagnostics,
            "partial_reasons": partial_reasons,
            "truncated": page.has_more,
            "next_cursor": page.next_cursor,
            "live_overlay": live.overlay,
            "conflicts": live.conflicts,
            "validated_checkpoint": scene_checkpoint(generation, offline_cached),
            "evidence": {
                "source": "persistent_segment_index",
                "source_complete": generation.scene.source_complete,
                "authorities": ["packed_scene_state", "scene_composer"],
            },
        }),
        offline_cached,
    )
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

fn scene_checkpoint(
    generation: &godot_codex_index_store::IndexGeneration,
    offline_cached: bool,
) -> Value {
    let mut checkpoint = json!({
        "editor_session_id": generation.scene.editor_session_id,
        "resource_revision": generation.scene.resource_revision,
        "scene_graph_revision": generation.scene.scene_graph_revision,
        "source_complete": generation.scene.source_complete,
        "snapshot_checksum": generation.scene.snapshot_checksum,
    });
    if offline_cached {
        checkpoint
            .as_object_mut()
            .expect("checkpoint is an object")
            .remove("editor_session_id");
    }
    checkpoint
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
    result: godot_codex_index_store::ResourceQueryResult,
    page: StaticPage,
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
            | CursorTool::InspectSymbol
            | CursorTool::FindUsages
            | CursorTool::OpenScenes
            | CursorTool::OpenScripts
            | CursorTool::EditorHistory
            | CursorTool::Diagnostics
            | CursorTool::RuntimeTree => unreachable!("resource tool"),
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
        "validated_checkpoint": resource_checkpoint(generation, page.offline_cached),
        "query": {
            "resource": selector,
            "limit": page.limit,
            "offset": page.offset,
        },
        "resource": resource_view(&result.resource),
        "status": if result.exact { "exact" } else { "partial" },
        "freshness": static_freshness_label(page.offline_cached),
        "offline_cached": page.offline_cached,
        "diagnostics": diagnostics,
        "truncated": result.has_more,
        "next_cursor": page.next_cursor,
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
        | CursorTool::InspectSymbol
        | CursorTool::FindUsages
        | CursorTool::OpenScenes
        | CursorTool::OpenScripts
        | CursorTool::EditorHistory
        | CursorTool::Diagnostics
        | CursorTool::RuntimeTree => unreachable!("resource tool"),
    }] = Value::Array(related);
    static_success(response, page.offline_cached)
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

fn static_success(mut value: Value, offline_cached: bool) -> CallToolResult {
    if offline_cached {
        sanitize_offline_static_value(&mut value);
    }
    CallToolResult::structured(value)
}

fn sanitize_offline_static_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for key in [
                "editor_session_id",
                "runtime_session_id",
                "snapshot_id",
                "native_handle",
            ] {
                object.remove(key);
            }
            if object.get("freshness").and_then(Value::as_str) == Some("current") {
                object.insert("freshness".to_owned(), json!("offline_cached"));
            }
            for child in object.values_mut() {
                sanitize_offline_static_value(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                sanitize_offline_static_value(child);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn runtime_bridge_error(error: BridgeError) -> CallToolResult {
    let failure = error.failure_class();
    let validation_stage = match &error {
        BridgeError::Invalid(message) if message == "runtime capture result is invalid" => {
            "capture_result"
        }
        BridgeError::Invalid(message) if message == "runtime screenshot encoding is invalid" => {
            "capture_encoding"
        }
        BridgeError::Invalid(message) if message == "runtime screenshot payload is invalid" => {
            "capture_payload"
        }
        BridgeError::Invalid(_) => "bridge_payload",
        _ => "request",
    };
    eprintln!("{}", runtime_bridge_debug_line(&error, validation_stage));
    if matches!(
        failure,
        BridgeFailureClass::AuthenticationFailed
            | BridgeFailureClass::ProjectBindingMismatch
            | BridgeFailureClass::ProtocolVersionIncompatible
    ) {
        let (code, retryable) = match failure {
            BridgeFailureClass::AuthenticationFailed => ("bridge_authentication_failed", true),
            BridgeFailureClass::ProjectBindingMismatch => ("project_binding_mismatch", false),
            BridgeFailureClass::ProtocolVersionIncompatible => {
                ("bridge_version_incompatible", false)
            }
            BridgeFailureClass::TransportUnavailable | BridgeFailureClass::TimedOut => {
                unreachable!("guarded by the proven-failure match")
            }
        };
        return CallToolResult::structured_error(json!({
            "error": {
                "code": code,
                "message": failure.safe_summary(),
                "retryable": retryable,
            }
        }));
    }
    match error {
        BridgeError::Rpc {
            code,
            retryable,
            data,
            ..
        } => {
            let known_runtime_code = known_runtime_rpc_code(&code);
            let (code, retryable, current) = known_runtime_code.map_or_else(
                || ("runtime_unavailable", true, json!({})),
                |code| (code, retryable, safe_runtime_error_coordinates(&data)),
            );
            CallToolResult::structured_error(json!({
                "error": {
                    "code": code,
                    "message": "Godot runtime request failed",
                    "retryable": retryable,
                    "current": current,
                }
            }))
        }
        error => CallToolResult::structured_error(json!({
            "error": {
                "code": "runtime_unavailable",
                "message": error.failure_class().safe_summary(),
                "retryable": true,
            }
        })),
    }
}

fn runtime_bridge_debug_line(error: &BridgeError, validation_stage: &str) -> String {
    let failure = error.failure_class();
    format!(
        "[godot-codex-runtime] class={} summary={} ({validation_stage})",
        failure.as_str(),
        failure.safe_summary()
    )
}

fn known_runtime_rpc_code(code: &str) -> Option<&str> {
    matches!(
        code,
        "runtime_inactive"
            | "runtime_already_active"
            | "runtime_ambiguous"
            | "runtime_unsaved_changes"
            | "runtime_start_failed"
            | "runtime_start_timeout"
            | "stale_runtime_session"
            | "stale_runtime_state"
            | "runtime_not_running"
            | "runtime_not_paused"
            | "runtime_disconnected"
            | "runtime_crashed"
            | "runtime_request_timeout"
            | "runtime_control_timeout"
            | "runtime_object_not_found"
            | "runtime_object_stale"
            | "runtime_data_retired"
            | "runtime_capture_unavailable"
            | "runtime_capture_rate_limited"
            | "runtime_capture_too_large"
    )
    .then_some(code)
}

fn safe_runtime_error_coordinates(data: &Value) -> Value {
    let Some(object) = data.as_object() else {
        return json!({});
    };
    let Some(runtime_session_id) = object
        .get("runtime_session_id")
        .and_then(Value::as_str)
        .filter(|value| valid_runtime_session_id(value))
    else {
        return json!({});
    };
    let Some(runtime_event_seq) = object
        .get("runtime_event_seq")
        .and_then(Value::as_u64)
        .filter(|value| (1..=MAX_SAFE_RUNTIME_SEQUENCE).contains(value))
    else {
        return json!({});
    };
    let state = object.get("state").and_then(Value::as_str).filter(|state| {
        matches!(
            *state,
            "starting"
                | "running"
                | "paused"
                | "stopping"
                | "disconnected"
                | "stopped"
                | "crashed"
                | "failed"
                | "timed_out"
        )
    });
    json!({
        "runtime_session_id": runtime_session_id,
        "runtime_event_seq": runtime_event_seq,
        "state": state,
    })
}

fn valid_runtime_session_id(value: &str) -> bool {
    value.strip_prefix("runtime:").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn runtime_capture_result(capture: RuntimeViewportCapture) -> Result<CallToolResult, BridgeError> {
    let png = capture.decode_png()?;
    let metadata = json!({
        "schema_version": capture.schema_version,
        "runtime_session_id": capture.runtime_session_id,
        "runtime_event_seq": capture.runtime_event_seq,
        "state": capture.state,
        "mime_type": capture.mime_type,
        "width": capture.width,
        "height": capture.height,
        "byte_length": capture.byte_length,
        "sha256": capture.sha256,
    });
    let mut result = CallToolResult::success(vec![
        ContentBlock::text(metadata.to_string()),
        ContentBlock::image(
            base64::engine::general_purpose::STANDARD.encode(png),
            "image/png",
        ),
    ]);
    result.structured_content = Some(metadata);
    Ok(result)
}

struct RuntimeSourceContext<'a> {
    generation: &'a godot_codex_index_store::IndexGeneration,
    live: &'a SemanticSnapshot,
    launched_scene_path: &'a str,
    runtime_root_path: &'a str,
}

fn runtime_node_view(
    node: &RuntimeNode,
    context: Option<&RuntimeSourceContext<'_>>,
    unavailable_reason: &str,
) -> Value {
    let mut value = json!(node);
    let mapping = if let Some(context) = context {
        runtime_source_mapping(node, context)
    } else {
        json!({
            "confidence": "unmapped",
            "diagnostic": unavailable_reason,
            "evidence": [{"source": "runtime_debugger_tree", "runtime_object_id": node.runtime_object_id}],
        })
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("source_mapping".to_owned(), mapping);
    }
    value
}

fn runtime_source_mapping(node: &RuntimeNode, context: &RuntimeSourceContext<'_>) -> Value {
    let Some(source_hint) = &node.source_hint else {
        return unmapped_runtime_node(node, "runtime_source_hint_absent");
    };
    let Some(observed_scene_path) = source_hint.scene_path.as_deref() else {
        return unmapped_runtime_node(node, "runtime_source_scene_absent");
    };
    let effective_path = if node.runtime_node_path == context.runtime_root_path {
        "."
    } else {
        let Some(relative) = node
            .runtime_node_path
            .strip_prefix(&format!("{}/", context.runtime_root_path))
        else {
            return unmapped_runtime_node(node, "runtime_path_outside_launched_root");
        };
        relative
    };
    let Some(scene) = context
        .generation
        .scene
        .scenes
        .iter()
        .find(|scene| scene.comparison_path == context.launched_scene_path)
    else {
        return unmapped_runtime_node(node, "launched_scene_not_indexed");
    };
    let persistent_matches = context
        .generation
        .scene
        .relations
        .iter()
        .filter(|relation| {
            relation.relation == "occurrence"
                && relation.scene_entity_id.as_deref() == Some(&scene.scene_entity_id)
                && relation_path(relation) == effective_path
        })
        .filter_map(|occurrence| {
            let definition = occurrence.target.as_ref().and_then(|definition_id| {
                context
                    .generation
                    .scene
                    .nodes
                    .iter()
                    .find(|candidate| candidate.node_entity_id == *definition_id)
            })?;
            let defining_scene_path = context
                .generation
                .scene
                .scenes
                .iter()
                .find(|candidate| candidate.scene_entity_id == definition.scene_entity_id)
                .map(|candidate| candidate.comparison_path.as_str())?;
            (definition.name == node.name
                && definition.godot_type == node.godot_type
                && defining_scene_path == observed_scene_path)
                .then_some((occurrence, definition))
        })
        .collect::<Vec<_>>();
    if persistent_matches.len() != 1 {
        return unmapped_runtime_node(
            node,
            if persistent_matches.is_empty() {
                "persistent_node_occurrence_not_observed"
            } else {
                "ambiguous_persistent_node_occurrence"
            },
        );
    }
    let live_scenes = context
        .live
        .scenes
        .values()
        .filter(|candidate| {
            candidate.get("path").and_then(Value::as_str) == Some(context.launched_scene_path)
                && candidate.get("coverage").and_then(Value::as_str) == Some("complete")
                && candidate.get("nodes_truncated").and_then(Value::as_bool) != Some(true)
        })
        .collect::<Vec<_>>();
    if context.live.truncated || live_scenes.len() != 1 {
        return unmapped_runtime_node(node, "live_editor_scene_incomplete");
    }
    let Some(live_scene_id) = live_scenes[0].get("entity_id").and_then(Value::as_str) else {
        return unmapped_runtime_node(node, "live_editor_scene_invalid");
    };
    let live_matches = context
        .live
        .nodes
        .values()
        .filter(|candidate| {
            candidate.get("scene_id").and_then(Value::as_str) == Some(live_scene_id)
                && candidate.get("node_path").and_then(Value::as_str) == Some(effective_path)
                && candidate.get("name").and_then(Value::as_str) == Some(node.name.as_str())
                && candidate.get("godot_type").and_then(Value::as_str)
                    == Some(node.godot_type.as_str())
        })
        .collect::<Vec<_>>();
    if live_matches.len() != 1 {
        return unmapped_runtime_node(
            node,
            if live_matches.is_empty() {
                "live_editor_node_not_observed"
            } else {
                "ambiguous_live_editor_node"
            },
        );
    }
    let (occurrence, _) = persistent_matches[0];
    json!({
        "scene_path": context.launched_scene_path,
        "relative_node_path": effective_path,
        "node_occurrence_id": occurrence.source,
        "editor_node_id": live_matches[0].get("entity_id"),
        "confidence": "runtime_confirmed",
        "evidence": [
            {"source": "runtime_debugger_tree", "runtime_object_id": node.runtime_object_id},
            {
                "source": "persistent_scene_state",
                "generation_id": context.generation.generation_id,
                "scene_graph_revision": context.generation.scene.scene_graph_revision,
                "scene_id": scene.scene_entity_id,
                "node_occurrence_id": occurrence.source,
            },
            {
                "source": "live_editor_scene_state",
                "snapshot_id": context.live.snapshot_id,
                "editor_node_id": live_matches[0].get("entity_id"),
                "event_seq": context.live.revisions.event_seq,
            }
        ],
    })
}

fn unmapped_runtime_node(node: &RuntimeNode, diagnostic: &str) -> Value {
    json!({
        "confidence": "unmapped",
        "diagnostic": diagnostic,
        "evidence": [{"source": "runtime_debugger_tree", "runtime_object_id": node.runtime_object_id}],
    })
}

fn stale_live_error(code: &str, snapshot: &SemanticSnapshot) -> CallToolResult {
    let current_scene_revision = snapshot
        .current_scene_id
        .as_ref()
        .and_then(|scene_id| snapshot.revisions.scene_revisions.get(scene_id))
        .copied();
    CallToolResult::structured_error(json!({
        "error": {
            "code": code,
            "message": "requested editor revision is stale",
            "retryable": true,
            "current": {
                "editor_session_id": snapshot.editor_session_id,
                "snapshot_id": snapshot.snapshot_id,
                "event_seq": snapshot.revisions.event_seq,
                "current_scene_id": snapshot.current_scene_id,
                "scene_revision": current_scene_revision,
            }
        }
    }))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn safe_digest(value: &Value) -> String {
    let encoded = serde_json::to_vec(value).unwrap_or_default();
    format!("sha256:{:x}", Sha256::digest(encoded))
}

fn change_set_operation_flags(preview: &Value) -> (bool, bool, bool) {
    let mut scene = false;
    let mut resource = false;
    let mut script = false;
    for operation in preview
        .get("operations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match operation.get("kind").and_then(Value::as_str) {
            Some("create_resource" | "update_resource") => resource = true,
            Some("update_gdscript") => script = true,
            Some(_) => scene = true,
            None => {}
        }
    }
    (scene, resource, script)
}

fn change_set_saves_scene(preview: &Value) -> bool {
    preview
        .get("save_scope")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|path| path.ends_with(".tscn"))
}

fn live_scene_projection_complete(snapshot: &SemanticSnapshot, scene_id: &str) -> bool {
    snapshot.scenes.get(scene_id).is_some_and(|scene| {
        scene.get("coverage").and_then(Value::as_str) == Some("complete")
            && scene.get("nodes_truncated").and_then(Value::as_bool) == Some(false)
    })
}

fn live_diagnostics_complete(snapshot: &SemanticSnapshot) -> bool {
    snapshot
        .diagnostic_state
        .as_ref()
        .and_then(|state| state.get("omitted_count"))
        .and_then(Value::as_u64)
        == Some(0)
        && snapshot.diagnostics.values().all(|diagnostic| {
            diagnostic.get("message_truncated").and_then(Value::as_bool) == Some(false)
        })
}

fn projected_u64(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    if let Some(integer) = value.as_u64() {
        return Some(integer);
    }
    let number = value.as_f64()?;
    if number.is_finite() && number >= 0.0 && number.fract() == 0.0 && number <= u64::MAX as f64 {
        Some(number as u64)
    } else {
        None
    }
}

fn live_diagnostics_delta_complete(
    baseline: &SemanticSnapshot,
    postimage: &SemanticSnapshot,
) -> bool {
    let messages_complete = |snapshot: &SemanticSnapshot| {
        snapshot.diagnostics.values().all(|diagnostic| {
            diagnostic.get("message_truncated").and_then(Value::as_bool) == Some(false)
        })
    };
    if !messages_complete(baseline) || !messages_complete(postimage) {
        return false;
    }
    if live_diagnostics_complete(postimage) {
        return true;
    }
    let Some(before) = baseline.diagnostic_state.as_ref() else {
        return false;
    };
    let Some(after) = postimage.diagnostic_state.as_ref() else {
        return false;
    };
    if after.get("omitted_before_first").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    let Some(before_last) = projected_u64(before.get("last_output_seq")) else {
        return false;
    };
    let Some(after_first) = projected_u64(after.get("first_output_seq")) else {
        return false;
    };
    let Some(after_last) = projected_u64(after.get("last_output_seq")) else {
        return false;
    };
    after_last >= before_last && after_first <= before_last.saturating_add(1)
}

fn change_set_paths(preview: &Value) -> BTreeSet<String> {
    preview
        .get("operations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|operation| operation.get("path").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn live_diagnostic_fingerprints(snapshot: &SemanticSnapshot) -> BTreeSet<DiagnosticFingerprint> {
    snapshot
        .diagnostics
        .values()
        .filter_map(|diagnostic| {
            let severity = match diagnostic.get("severity").and_then(Value::as_str) {
                Some("error") => DiagnosticSeverity::Error,
                Some("warning") => DiagnosticSeverity::Warning,
                _ => return None,
            };
            let id = diagnostic.get("entity_id")?.as_str()?.to_owned();
            let source = diagnostic
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("editor")
                .to_owned();
            Some(DiagnosticFingerprint {
                id,
                severity,
                source,
                entity_id: diagnostic
                    .get("target_entity_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                message_digest: safe_digest(diagnostic.get("message").unwrap_or(&Value::Null)),
            })
        })
        .collect()
}

fn semantic_delta_for_change_set(
    baseline: &ValidationSemanticSnapshot,
    postimage: &ValidationSemanticSnapshot,
    preview: &Value,
    scene_id: &str,
) -> ExpectedSemanticDelta {
    let mut affected = BTreeSet::from([scene_id.to_owned()]);
    let mut create_specs = Vec::new();
    let paths = change_set_paths(preview);
    for operation in preview
        .get("operations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for key in [
            "node_id",
            "parent_node_id",
            "new_parent_node_id",
            "emitter_node_id",
            "receiver_node_id",
        ] {
            if let Some(id) = operation.get(key).and_then(Value::as_str) {
                affected.insert(id.to_owned());
            }
        }
        if operation.get("kind").and_then(Value::as_str) == Some("create_node")
            && let Some(name) = operation.get("name").and_then(Value::as_str)
        {
            create_specs.push(name.to_owned());
        }
    }
    for (id, entity) in &postimage.entities {
        let path_matches = entity
            .get("display_path")
            .or_else(|| entity.get("path"))
            .and_then(Value::as_str)
            .is_some_and(|path| paths.contains(path));
        let created_node_matches = !baseline.entities.contains_key(id)
            && entity.get("scene_id").and_then(Value::as_str) == Some(scene_id)
            && entity
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| create_specs.iter().any(|expected| expected == name));
        if path_matches || created_node_matches {
            affected.insert(id.clone());
        }
    }

    let baseline_ids: BTreeSet<_> = baseline.entities.keys().cloned().collect();
    let postimage_ids: BTreeSet<_> = postimage.entities.keys().cloned().collect();
    let added = postimage_ids.difference(&baseline_ids).cloned();
    let removed = baseline_ids.difference(&postimage_ids).cloned();
    let changed = baseline_ids
        .intersection(&postimage_ids)
        .filter(|id| baseline.entities.get(*id) != postimage.entities.get(*id))
        .cloned();
    ExpectedSemanticDelta {
        affected_closure: affected.clone(),
        added_entities: added.filter(|id| affected.contains(id)).collect(),
        removed_entities: removed.filter(|id| affected.contains(id)).collect(),
        changed_entities: changed.filter(|id| affected.contains(id)).collect(),
        added_relations: BTreeSet::new(),
        removed_relations: BTreeSet::new(),
    }
}

#[tool_router]
impl GodotMcpServer {
    #[tool(
        description = "Report bounded project-scoped Godot connection, compatibility, cache, and remediation state even when the editor is offline",
        output_schema = rmcp::handler::server::tool::schema_for_type::<ConnectionHealth>(),
        annotations(
            title = "Godot connection status",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_get_connection_status(
        &self,
        Parameters(_input): Parameters<ConnectionStatusInput>,
    ) -> CallToolResult {
        CallToolResult::structured(
            serde_json::to_value(self.connection_status())
                .expect("connection status contains only serializable bounded fields"),
        )
    }

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
        Parameters(input): Parameters<LiveGuardInput>,
    ) -> CallToolResult {
        self.editor_state_query(&input)
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
        Parameters(input): Parameters<LiveGuardInput>,
    ) -> CallToolResult {
        self.query(Query::CurrentScene, &input)
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
        Parameters(input): Parameters<LiveGuardInput>,
    ) -> CallToolResult {
        self.query(Query::SelectedNodes, &input)
    }

    #[tool(
        description = "Return every open Godot scene tab with live identity, dirty state, selection, and revisions",
        annotations(
            title = "Godot open scenes",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_get_open_scenes(
        &self,
        Parameters(input): Parameters<LiveListInput>,
    ) -> CallToolResult {
        self.live_list_query(input, LiveListQuery::OpenScenes)
    }

    #[tool(
        description = "Return the current Godot Inspector object and bounded live editable properties",
        annotations(
            title = "Godot Inspector state",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_get_inspector_state(
        &self,
        Parameters(input): Parameters<LiveGuardInput>,
    ) -> CallToolResult {
        let snapshot = match self.live_snapshot(&input) {
            Ok(snapshot) => snapshot,
            Err(error) => return error,
        };
        CallToolResult::structured(snapshot.inspector_state_result())
    }

    #[tool(
        description = "Return open Godot script tabs, the active script, dirty hashes, and bounded selections without source text",
        annotations(
            title = "Godot open scripts",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_get_open_scripts(
        &self,
        Parameters(input): Parameters<LiveListInput>,
    ) -> CallToolResult {
        self.live_list_query(input, LiveListQuery::OpenScripts)
    }

    #[tool(
        description = "Return bounded native Godot Undo/Redo history summaries with opaque operation payloads",
        annotations(
            title = "Godot editor history",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_get_editor_history(
        &self,
        Parameters(input): Parameters<LiveListInput>,
    ) -> CallToolResult {
        self.live_list_query(input, LiveListQuery::EditorHistory)
    }

    #[tool(
        description = "Return the bounded redacted snapshot of Godot editor Output diagnostics without runtime inference",
        annotations(
            title = "Godot editor diagnostics",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_get_diagnostics(
        &self,
        Parameters(input): Parameters<DiagnosticInput>,
    ) -> CallToolResult {
        match input.scope {
            DiagnosticScopeInput::Editor => self.live_list_query(
                LiveListInput {
                    limit: input.limit,
                    cursor: input.cursor,
                    expected_editor_session_id: input.expected_editor_session_id,
                    expected_event_seq: input.expected_event_seq,
                    expected_scene_revision: input.expected_scene_revision,
                },
                LiveListQuery::Diagnostics,
            ),
            DiagnosticScopeInput::Runtime => self.runtime_diagnostics_query(input).await,
        }
    }

    #[tool(
        description = "Return bounded Godot editor viewport metadata; screenshot pixels are unavailable in this milestone",
        annotations(
            title = "Godot viewport state",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_get_viewport_state(
        &self,
        Parameters(input): Parameters<LiveGuardInput>,
    ) -> CallToolResult {
        let snapshot = match self.live_snapshot(&input) {
            Ok(snapshot) => snapshot,
            Err(error) => return error,
        };
        CallToolResult::structured(snapshot.viewport_state_result())
    }

    #[tool(
        description = "Run the saved Godot project through the bound editor debugger without modifying scenes or scripts",
        annotations(
            title = "Run Godot project",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn godot_run_project(
        &self,
        Parameters(_input): Parameters<RuntimeRunInput>,
    ) -> CallToolResult {
        self.runtime_control(RuntimeControl::Run(RuntimeTarget::Project), None)
            .await
    }

    #[tool(
        description = "Run the saved current Godot scene through the bound editor debugger without modifying scenes or scripts",
        annotations(
            title = "Run current Godot scene",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn godot_run_current_scene(
        &self,
        Parameters(_input): Parameters<RuntimeRunInput>,
    ) -> CallToolResult {
        self.runtime_control(RuntimeControl::Run(RuntimeTarget::CurrentScene), None)
            .await
    }

    #[tool(
        description = "Stop exactly the guarded local Godot runtime session",
        annotations(
            title = "Stop Godot runtime",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn godot_stop_project(
        &self,
        Parameters(input): Parameters<RuntimeGuardInput>,
    ) -> CallToolResult {
        self.runtime_control(RuntimeControl::Stop, Some(&input))
            .await
    }

    #[tool(
        description = "Pause exactly the guarded local Godot runtime session",
        annotations(
            title = "Pause Godot runtime",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn godot_pause_project(
        &self,
        Parameters(input): Parameters<RuntimeGuardInput>,
    ) -> CallToolResult {
        self.runtime_control(RuntimeControl::Pause, Some(&input))
            .await
    }

    #[tool(
        description = "Continue exactly the guarded paused local Godot runtime session",
        annotations(
            title = "Continue Godot runtime",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn godot_continue_project(
        &self,
        Parameters(input): Parameters<RuntimeGuardInput>,
    ) -> CallToolResult {
        self.runtime_control(RuntimeControl::Continue, Some(&input))
            .await
    }

    #[tool(
        description = "Return a signed, bounded page from the checksum-verified remote runtime scene tree",
        annotations(
            title = "Godot runtime tree",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_get_runtime_tree(
        &self,
        Parameters(input): Parameters<RuntimeTreeInput>,
    ) -> CallToolResult {
        self.runtime_tree_query(input).await
    }

    #[tool(
        description = "Inspect bounded read-only properties of one opaque object from the current runtime tree snapshot",
        annotations(
            title = "Inspect Godot runtime object",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_inspect_runtime_object(
        &self,
        Parameters(input): Parameters<RuntimeObjectInput>,
    ) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Runtime) {
            return error;
        }
        let mut client = match self.runtime_overlay.connect().await {
            Ok(client) => client,
            Err(error) => return runtime_bridge_error(error),
        };
        match client
            .inspect_runtime_object(
                &input.runtime_session_id,
                &input.runtime_object_id,
                input.expected_runtime_event_seq,
            )
            .await
        {
            Ok(result) => CallToolResult::structured(json!(result)),
            Err(error) => runtime_bridge_error(error),
        }
    }

    #[tool(
        description = "Return one bounded runtime stack trace by its opaque stack ID",
        annotations(
            title = "Godot runtime stack trace",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_get_stack_trace(
        &self,
        Parameters(input): Parameters<RuntimeStackInput>,
    ) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Runtime) {
            return error;
        }
        let mut client = match self.runtime_overlay.connect().await {
            Ok(client) => client,
            Err(error) => return runtime_bridge_error(error),
        };
        match client
            .get_runtime_stack(
                &input.runtime_session_id,
                &input.runtime_stack_id,
                input.expected_runtime_event_seq,
            )
            .await
        {
            Ok(result) => CallToolResult::structured(json!(result)),
            Err(error) => runtime_bridge_error(error),
        }
    }

    #[tool(
        description = "Capture a bounded PNG from the running game viewport without exposing native handles or filesystem paths",
        annotations(
            title = "Capture Godot runtime viewport",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn godot_capture_viewport(
        &self,
        Parameters(input): Parameters<RuntimeCaptureInput>,
    ) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Runtime) {
            return error;
        }
        let mut client = match self.runtime_overlay.connect().await {
            Ok(client) => client,
            Err(error) => return runtime_bridge_error(error),
        };
        let capture = match client
            .capture_runtime_viewport(
                &input.runtime_session_id,
                input.expected_runtime_event_seq,
                input.max_width,
                input.max_height,
            )
            .await
        {
            Ok(capture) => capture,
            Err(error) => return runtime_bridge_error(error),
        };
        match runtime_capture_result(capture) {
            Ok(result) => result,
            Err(error) => runtime_bridge_error(error),
        }
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

    #[tool(
        description = "Find evidence-backed resource, scene, and saved-script usages of one canonical Godot entity",
        annotations(
            title = "Godot find usages",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_find_usages(&self, Parameters(input): Parameters<FindUsagesInput>) -> CallToolResult {
        self.find_usages_query(input)
    }

    #[tool(
        description = "Prepare an immutable bounded preview for creating one node in the current open Godot scene; this allocates transaction state but does not mutate the scene",
        annotations(
            title = "Prepare Godot node creation",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_prepare_create_node(
        &self,
        Parameters(input): Parameters<PrepareCreateNodeInput>,
    ) -> CallToolResult {
        self.prepare_transaction(input.into_command()).await
    }

    #[tool(
        description = "Prepare an immutable bounded preview for deleting one editable node from the current open Godot scene; this does not mutate the scene",
        annotations(
            title = "Prepare Godot node deletion",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_prepare_delete_node(
        &self,
        Parameters(input): Parameters<PrepareDeleteNodeInput>,
    ) -> CallToolResult {
        self.prepare_transaction(input.into_command()).await
    }

    #[tool(
        description = "Prepare an immutable bounded preview for reparenting one editable node inside the same current Godot scene; this does not mutate the scene",
        annotations(
            title = "Prepare Godot node reparent",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_prepare_reparent_node(
        &self,
        Parameters(input): Parameters<PrepareReparentNodeInput>,
    ) -> CallToolResult {
        self.prepare_transaction(input.into_command()).await
    }

    #[tool(
        description = "Prepare an immutable redacted preview for setting one safely projected property on an editable node; this does not mutate the scene",
        annotations(
            title = "Prepare Godot property change",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_prepare_set_property(
        &self,
        Parameters(input): Parameters<PrepareSetPropertyInput>,
    ) -> CallToolResult {
        self.prepare_transaction(input.into_command()).await
    }

    #[tool(
        description = "Prepare an immutable bounded preview for attaching an existing project script to one editable node; this does not mutate the scene or script source",
        annotations(
            title = "Prepare Godot script attachment",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_prepare_attach_script(
        &self,
        Parameters(input): Parameters<PrepareAttachScriptInput>,
    ) -> CallToolResult {
        self.prepare_transaction(input.into_command()).await
    }

    #[tool(
        description = "Prepare an immutable bounded preview for detaching the current script from one editable node; this does not mutate the scene or script source",
        annotations(
            title = "Prepare Godot script detachment",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_prepare_detach_script(
        &self,
        Parameters(input): Parameters<PrepareDetachScriptInput>,
    ) -> CallToolResult {
        self.prepare_transaction(input.into_command()).await
    }

    #[tool(
        description = "Prepare an immutable bounded preview for connecting one same-scene Godot signal; this does not mutate the scene",
        annotations(
            title = "Prepare Godot signal connection",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_prepare_connect_signal(
        &self,
        Parameters(input): Parameters<PrepareConnectSignalInput>,
    ) -> CallToolResult {
        self.prepare_transaction(input.into_command()).await
    }

    #[tool(
        description = "Prepare an immutable bounded preview for disconnecting one exact same-scene Godot signal connection; this does not mutate the scene",
        annotations(
            title = "Prepare Godot signal disconnection",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_prepare_disconnect_signal(
        &self,
        Parameters(input): Parameters<PrepareDisconnectSignalInput>,
    ) -> CallToolResult {
        self.prepare_transaction(input.into_command()).await
    }

    #[tool(
        description = "Prepare one immutable bounded multi-operation Godot change-set preview with explicit save, validation, and rollback policy; this never accepts approval material and performs no mutation",
        annotations(
            title = "Prepare Godot change set",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_prepare_change_set(
        &self,
        Parameters(input): Parameters<PrepareChangeSetInput>,
    ) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Editor) {
            return error;
        }
        if let Err(message) = input.validate() {
            return structured_error("change_set_invalid", message, false);
        }
        let Some(project_root) = &self.project_root else {
            return structured_error(
                "change_set_coordinator_unavailable",
                "The project-scoped compound coordinator is unavailable.",
                true,
            );
        };
        let mut client = match godot_codex_bridge_client::BridgeClient::connect(project_root).await
        {
            Ok(client) => client,
            Err(error) => return runtime_bridge_error(error),
        };
        if client.project_id() != input.project_id
            || client.editor_session_id() != input.coordinates.editor_session_id
        {
            return structured_error(
                "stale_editor_state",
                "The change-set binding does not match the active Bridge session.",
                true,
            );
        }
        let mut params = input.bridge_params();
        if let Err((code, message)) = self.bind_change_set_resource_paths(&mut params) {
            return structured_error(&code, &message, code == "resource_index_unavailable");
        }
        match client.prepare_change_set(params).await {
            Ok(result) => {
                self.retain_change_set_baseline(&input, &result).await;
                transaction_result(result)
            }
            Err(error) => runtime_bridge_error(error),
        }
    }

    #[tool(
        description = "Read one immutable bounded page of a retained automatic validation report by opaque report identifier",
        annotations(
            title = "Godot validation report",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_get_validation_report(
        &self,
        Parameters(input): Parameters<ValidationReportInput>,
    ) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Editor) {
            return error;
        }
        let coordinator = self.validation_coordinator.lock().await;
        match coordinator.page(&input.report_id, input.page) {
            Ok(page) => CallToolResult::structured(report_page_value(page)),
            Err(error) => CallToolResult::structured_error(validation_report_error(error)),
        }
    }

    #[tool(
        description = "Return the effective host-owned Godot confirmation policy without exposing policy grants, nonces, or approval receipts",
        annotations(
            title = "Godot confirmation policy",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_get_confirmation_policy(
        &self,
        Parameters(_input): Parameters<ConfirmationPolicyInput>,
    ) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Editor) {
            return error;
        }
        let snapshot = self.replicator.read().ok();
        CallToolResult::structured(
            self.confirmation_policy.snapshot(
                snapshot.as_ref().map(|value| value.project_id.as_str()),
                snapshot
                    .as_ref()
                    .map(|value| value.editor_session_id.as_str()),
                unix_millis(),
            ),
        )
    }

    #[tool(
        description = "Immediately revoke the memory-only host confirmation grant; this operation can only narrow write authority",
        annotations(
            title = "Reset Godot confirmation policy",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn godot_reset_confirmation_policy(
        &self,
        Parameters(_input): Parameters<ConfirmationPolicyInput>,
    ) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Editor) {
            return error;
        }
        CallToolResult::structured(json!({
            "mode": "always_ask",
            "revoked": self.confirmation_policy.reset(),
            "grant_active": false,
        }))
    }

    #[tool(
        description = "Apply one previously prepared Godot editor transaction after an exact standard MCP form approval; never replay this tool after an uncertain outcome",
        annotations(
            title = "Apply Godot editor transaction",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn godot_apply_transaction(
        &self,
        Parameters(input): Parameters<ApplyTransactionInput>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Editor) {
            return error;
        }
        if input.transaction_id.starts_with("change-set:") {
            return self.apply_compound_change_set(input, context).await;
        }
        let Some(coordinator) = &self.transaction_coordinator else {
            return coordinator_unavailable();
        };
        let approval = McpApprovalProvider::new(context);
        match coordinator
            .apply(
                ApplyCommand {
                    transaction_id: input.transaction_id,
                    preview_digest: input.preview_digest,
                    expected_scene_revision: input.expected_scene_revision,
                    expected_operation_seq: input.expected_operation_seq,
                },
                &approval,
            )
            .await
        {
            Ok(result) => transaction_result(result),
            Err(error) => transaction_error(error),
        }
    }

    #[tool(
        description = "Return the bounded reconciled state of one opaque Godot editor transaction without exposing native history identifiers or mutation payloads",
        annotations(
            title = "Godot transaction status",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn godot_get_transaction_status(
        &self,
        Parameters(input): Parameters<TransactionStatusInput>,
    ) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Editor) {
            return error;
        }
        if input.transaction_id.starts_with("change-set:") {
            self.change_set_status(&input.transaction_id).await
        } else {
            self.transaction_status(&input.transaction_id).await
        }
    }

    #[tool(
        description = "Undo one committed Godot editor transaction only when it is still the newest eligible action in its exact native history",
        annotations(
            title = "Undo Godot editor transaction",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn godot_undo_transaction(
        &self,
        Parameters(input): Parameters<UndoTransactionInput>,
    ) -> CallToolResult {
        if let Some(error) = self.unavailable_error(AvailabilityDomain::Editor) {
            return error;
        }
        if input.transaction_id.starts_with("change-set:") {
            let Some(project_root) = &self.project_root else {
                return structured_error(
                    "change_set_coordinator_unavailable",
                    "The project-scoped compound coordinator is unavailable.",
                    true,
                );
            };
            let mut client =
                match godot_codex_bridge_client::BridgeClient::connect(project_root).await {
                    Ok(client) => client,
                    Err(error) => return runtime_bridge_error(error),
                };
            return match client
                .undo_change_set(&input.transaction_id, input.expected_transaction_seq)
                .await
            {
                Ok(result) => transaction_result(result),
                Err(error) => runtime_bridge_error(error),
            };
        }
        let Some(coordinator) = &self.transaction_coordinator else {
            return coordinator_unavailable();
        };
        match coordinator
            .undo(UndoCommand {
                transaction_id: input.transaction_id,
                expected_transaction_seq: input.expected_transaction_seq,
                expected_scene_revision: input.expected_scene_revision,
                expected_operation_seq: input.expected_operation_seq,
            })
            .await
        {
            Ok(result) => transaction_result(result),
            Err(error) => transaction_error(error),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GodotMcpServer {
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(
            Self::summary_resources(),
        ))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult::with_all_items(
            Self::summary_resource_templates(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        self.read_summary_resource(&request.uri)
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
        .with_instructions(
            include_str!("../../../product/server-instructions.v1.txt").trim_end_matches('\n'),
        )
    }
}

pub fn structured_content(result: &CallToolResult) -> Option<&Value> {
    result.structured_content.as_ref()
}

#[cfg(test)]
mod tests {
    use std::fs;

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
    use godot_codex_operations::BridgeProbeObservation;
    use godot_codex_product::{
        BridgeCondition, CacheCondition, ConfigurationCondition, Diagnostic as ProductDiagnostic,
        DiagnosticCode, FIXED_RESOURCE_URIS, FULL_BETA_TOOLS, PackageCondition,
        ProductCompatibilityBasis, ProductStartupObservation, RESOURCE_TEMPLATE_URIS,
        embedded_compatibility_matrix,
    };
    use godot_codex_resource_indexer::{
        ResourceIndexCoordinator, ResourceIndexReader, SceneIndexReader, ScriptIndexReader,
        persist_offline_authority,
    };
    use godot_codex_semantic_model::{
        NegotiatedBridgeMetadata, SnapshotChunk, SnapshotEnd, SnapshotMetadata,
    };
    use rmcp::{ServiceExt, model::CallToolRequestParams};
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    fn assert_closed_schema_objects(value: &Value) {
        if value.get("type").and_then(Value::as_str) == Some("object") {
            assert_eq!(
                value.get("additionalProperties").and_then(Value::as_bool),
                Some(false),
                "object schema is not closed: {value}"
            );
        }
        match value {
            Value::Array(values) => {
                for value in values {
                    assert_closed_schema_objects(value);
                }
            }
            Value::Object(values) => {
                for value in values.values() {
                    assert_closed_schema_objects(value);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    fn assert_tool_result_contract(tool_name: &str, result: CallToolResult) {
        assert!(
            output_schema::matches_top_level_contract(tool_name, &result),
            "{tool_name} returned a value outside its advertised top-level output contract: {:?}",
            result.structured_content
        );
        let structured = result
            .structured_content
            .as_ref()
            .expect("advertised output schema requires structuredContent");
        let ContentBlock::Text(text) = &result.content[0] else {
            panic!("{tool_name} must return equivalent JSON as its first text block");
        };
        assert_eq!(
            serde_json::from_str::<Value>(&text.text).expect("tool text block must be JSON"),
            *structured,
            "{tool_name} text and structuredContent diverged"
        );
    }

    fn ready_product_startup() -> ProductStartupObservation {
        let matrix = embedded_compatibility_matrix().unwrap();
        ProductStartupObservation {
            package_version: matrix.package.version.clone(),
            package: PackageCondition::Ready,
            configuration: ConfigurationCondition::Ready,
            compatibility_basis: Some(ProductCompatibilityBasis {
                package_version: matrix.package.version.clone(),
                package_manifest_verified: true,
                target: matrix.package.target,
                godot_source_commit: matrix.godot.source_commit.clone(),
                godot_build_id: matrix.godot.build_id.clone(),
                godot_artifact_sha256: matrix.godot.artifact_sha256.clone(),
                mcp_protocol: matrix.protocols.mcp_protocol.clone(),
                index_schema: matrix.schemas.index.clone(),
                transaction_journal_schema: matrix.schemas.transaction_journal.clone(),
                validation_report_schema: matrix.schemas.validation_report.clone(),
            }),
        }
    }

    fn config_missing_product_startup() -> ProductStartupObservation {
        let mut observation = ready_product_startup();
        observation.configuration = ConfigurationCondition::Missing;
        observation
    }

    fn current_negotiated_bridge() -> NegotiatedBridgeMetadata {
        let matrix = embedded_compatibility_matrix().unwrap();
        let profile = matrix
            .bridge_profiles
            .iter()
            .find(|profile| profile.bridge_minor == matrix.protocols.bridge_current_minor)
            .unwrap();
        NegotiatedBridgeMetadata::new(
            format!(
                "{}.{}",
                matrix.protocols.bridge_major, matrix.protocols.bridge_current_minor
            ),
            profile.capabilities.iter().cloned(),
        )
        .unwrap()
    }

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
        replicator.mark_bridge_negotiated(current_negotiated_bridge());
        replicator.begin(metadata).unwrap();
        replicator.push_chunk(chunk).unwrap();
        replicator.end(end).unwrap();
        replicator
    }

    #[test]
    fn validation_uses_domain_coverage_instead_of_unrelated_global_truncation() {
        let replica = canonical_ready_replica();
        let mut snapshot = (*replica.read().unwrap()).clone();
        let scene_id = snapshot.scenes.keys().next().unwrap().clone();
        let scene = snapshot.scenes.get_mut(&scene_id).unwrap();
        scene["coverage"] = json!("complete");
        scene["nodes_truncated"] = json!(false);
        snapshot.diagnostic_state = Some(json!({"omitted_count": 0}));
        snapshot.diagnostics.clear();
        snapshot.truncated = true;

        assert!(live_scene_projection_complete(&snapshot, &scene_id));
        assert!(live_diagnostics_complete(&snapshot));

        snapshot.scenes.get_mut(&scene_id).unwrap()["nodes_truncated"] = json!(true);
        assert!(!live_scene_projection_complete(&snapshot, &scene_id));
        snapshot.diagnostic_state = Some(json!({"omitted_count": 1}));
        assert!(!live_diagnostics_complete(&snapshot));

        let mut baseline = snapshot.clone();
        baseline.diagnostic_state = Some(json!({
            "omitted_count": 2,
            "first_output_seq": 20,
            "last_output_seq": 30,
            "omitted_before_first": true
        }));
        let mut postimage = baseline.clone();
        postimage.diagnostic_state = Some(json!({
            "omitted_count": 3,
            "first_output_seq": 28.0,
            "last_output_seq": 33.0,
            "omitted_before_first": true
        }));
        assert!(live_diagnostics_delta_complete(&baseline, &postimage));
        postimage.diagnostic_state.as_mut().unwrap()["first_output_seq"] = json!(32);
        assert!(!live_diagnostics_delta_complete(&baseline, &postimage));
    }

    fn indexed_resource_server() -> GodotMcpServer {
        indexed_server_fixture(false, false, false, false)
    }

    fn indexed_resource_server_fixture(include_missing: bool) -> GodotMcpServer {
        indexed_server_fixture(include_missing, false, false, false)
    }

    fn indexed_semantic_server() -> GodotMcpServer {
        indexed_server_fixture(false, true, false, false)
    }

    fn indexed_script_server() -> GodotMcpServer {
        indexed_server_fixture(false, true, true, false)
    }

    fn canonical_ready_server() -> GodotMcpServer {
        indexed_server_fixture(false, true, true, true)
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
                    subject_entity_id: CHILD.to_owned(),
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
                    subject_entity_id: CHILD.to_owned(),
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
        live_ready: bool,
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
            project_id:
                "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd"
                    .to_owned(),
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
        let mut store = SegmentStore::open(
            temp.path(),
            "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd",
        )
        .unwrap();
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
        if live_ready {
            GodotMcpServer::with_all_indexes_and_project_root(
                canonical_ready_replica(),
                reader,
                scene_reader,
                script_reader,
                PathBuf::from("fixture-project"),
            )
            .with_product_startup_observation(ready_product_startup())
        } else {
            GodotMcpServer::with_all_indexes(
                SnapshotReplicator::new(),
                reader,
                scene_reader,
                script_reader,
            )
        }
    }

    fn sha256_bytes(bytes: &[u8]) -> String {
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    fn offline_indexed_server_fixture() -> (GodotMcpServer, TempDir) {
        offline_indexed_server_with_fault(OfflineAuthorityFault::None)
    }

    #[derive(Clone, Copy)]
    enum OfflineAuthorityFault {
        None,
        Missing,
        Corrupt,
        StaleSource,
    }

    fn offline_indexed_server_with_fault(
        fault: OfflineAuthorityFault,
    ) -> (GodotMcpServer, TempDir) {
        let temp = TempDir::new().unwrap();
        let project_bytes = b"[application]\nconfig/name=\"Offline Fixture\"\n";
        let a_bytes = b"[gd_resource]\nresource_name = \"a\"\n";
        let b_bytes = b"[gd_resource]\nresource_name = \"b\"\n";
        let c_bytes = b"[gd_resource]\nresource_name = \"c\"\n";
        let scene_bytes = b"[gd_scene]\n[node name=\"Main\" type=\"Node\"]\n";
        let script_bytes = b"extends Node\nclass_name Player\nfunc attack():\n    pass\n";
        fs::write(temp.path().join("project.godot"), project_bytes).unwrap();
        fs::write(temp.path().join("a.tres"), a_bytes).unwrap();
        fs::write(temp.path().join("b.tres"), b_bytes).unwrap();
        fs::write(temp.path().join("c.tres"), c_bytes).unwrap();
        fs::write(temp.path().join("main.tscn"), scene_bytes).unwrap();
        fs::create_dir_all(temp.path().join("scripts")).unwrap();
        fs::write(temp.path().join("scripts/player.gd"), script_bytes).unwrap();
        let project_id = godot_codex_bridge_client::project_id_for_path(temp.path()).unwrap();

        let resource =
            |id: &str, uid: &str, path: &str, resource_type: &str, bytes: &[u8]| ResourceEntity {
                entity_id: id.to_owned(),
                identity_input: uid.to_owned(),
                uid: Some(uid.to_owned()),
                display_path: path.to_owned(),
                comparison_path: path.to_owned(),
                identity_strength: IdentityStrength::ResourceUid,
                resource_type: resource_type.to_owned(),
                source_kind: "source".to_owned(),
                import_state: "not_imported".to_owned(),
                authority: "editor_file_system".to_owned(),
                content_generation: Some(sha256_bytes(bytes)),
                mtime_ns: 1,
                byte_size: bytes.len() as u64,
                validity: RecordValidity::Valid,
                resource_revision: 1,
            };
        let resources = vec![
            resource("entity-a", "uid://a", "res://a.tres", "Resource", a_bytes),
            resource("entity-b", "uid://b", "res://b.tres", "Resource", b_bytes),
            resource("entity-c", "uid://c", "res://c.tres", "Resource", c_bytes),
            resource(
                "entity-scene",
                "uid://scene",
                "res://main.tscn",
                "PackedScene",
                scene_bytes,
            ),
            resource(
                &script_resource_id(),
                "uid://player-script",
                "res://scripts/player.gd",
                "GDScript",
                script_bytes,
            ),
        ];
        let source_documents = resources
            .iter()
            .map(|resource| SourceDocument {
                entity_id: resource.entity_id.clone(),
                comparison_path: resource.comparison_path.clone(),
                size_before: resource.byte_size,
                size_after: resource.byte_size,
                mtime_before_ns: 1,
                mtime_after_ns: 1,
                content_generation: resource.content_generation.clone(),
                ingest_state: "ready".to_owned(),
            })
            .collect();
        let dependencies = vec![DependencyEdge {
            edge_id: "edge-a-b".to_owned(),
            source_entity_id: "entity-a".to_owned(),
            target_uid: Some("uid://b".to_owned()),
            target_comparison_path: Some("res://b.tres".to_owned()),
            target_display_path: Some("res://b.tres".to_owned()),
            target_entity_id: Some("entity-b".to_owned()),
            resolved_target_path: Some("res://b.tres".to_owned()),
            relation: "references".to_owned(),
            declared_type: Some("Resource".to_owned()),
            authority: "godot_resource_loader".to_owned(),
            resolution: DependencyResolution::Resolved,
            resource_revision: 1,
        }];
        let mut scene = scene_domain_fixture();
        scene.scenes[0].content_generation = sha256_bytes(scene_bytes);
        scene.validation_digest.clear();
        scene.validation_digest = scene.compute_validation_digest();
        let mut script = script_domain_fixture();
        script.documents[0].content_sha256 = sha256_bytes(script_bytes);
        for symbol in &mut script.symbols {
            symbol.declaration_range.content_sha256 = sha256_bytes(script_bytes);
        }
        for relation in &mut script.relations {
            if let Some(range) = &mut relation.evidence_range {
                range.content_sha256 = sha256_bytes(script_bytes);
            }
        }
        for diagnostic in &mut script.diagnostics {
            diagnostic.content_sha256 = sha256_bytes(script_bytes);
            if let Some(range) = &mut diagnostic.range {
                range.content_sha256 = sha256_bytes(script_bytes);
            }
        }
        script.validation_digest.clear();
        script.validation_digest = script.compute_validation_digest();
        let mut generation = IndexGeneration {
            generation_id: format!("generation:sha256:{}", "8".repeat(64)),
            parent_generation_id: None,
            schema_version: LOGICAL_SCHEMA_V1,
            project_id: project_id.clone(),
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
            diagnostics: Vec::new(),
            tombstones: Vec::new(),
            scene,
            script,
            validation_digest: String::new(),
        };
        generation.canonicalize();
        generation.validation_digest = generation.compute_validation_digest();
        generation.validate().unwrap();
        {
            let mut store = SegmentStore::open(temp.path(), &project_id).unwrap();
            store.activate(&generation, None).unwrap();
        }
        persist_offline_authority(temp.path(), &generation).unwrap();
        let authority_path = temp.path().join(".godot/codex/offline-authority-v1.json");
        match fault {
            OfflineAuthorityFault::None => {}
            OfflineAuthorityFault::Missing => fs::remove_file(authority_path).unwrap(),
            OfflineAuthorityFault::Corrupt => fs::write(authority_path, b"{").unwrap(),
            OfflineAuthorityFault::StaleSource => {
                fs::write(
                    temp.path().join("a.tres"),
                    b"[gd_resource]\nresource_name = \"changed\"\n",
                )
                .unwrap();
            }
        }
        let (coordinator, resource, scene, script) =
            ResourceIndexCoordinator::new_semantic(temp.path()).unwrap();
        assert_eq!(
            resource_generation_is_offline(&resource.status(), &generation.generation_id),
            matches!(fault, OfflineAuthorityFault::None)
        );
        drop(coordinator);
        let replicator = SnapshotReplicator::new();
        replicator.mark_disconnected(ReplicaFailure::TransportDisconnected);
        (
            GodotMcpServer::with_all_indexes_and_project_root(
                replicator,
                resource,
                scene,
                script,
                temp.path().to_owned(),
            )
            .with_product_startup_observation(ready_product_startup()),
            temp,
        )
    }

    #[test]
    fn unconfigured_server_fails_with_the_canonical_diagnostic() {
        let server = GodotMcpServer::new(SnapshotReplicator::new())
            .with_product_startup_observation(config_missing_product_startup());
        let result = server.godot_get_editor_state(Parameters(LiveGuardInput::default()));
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            structured_content(&result)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("project_config_missing")
        );
        assert_eq!(
            structured_content(&result)
                .and_then(|value| value.pointer("/error/retryable"))
                .and_then(Value::as_bool),
            Some(false)
        );
    }

    #[tokio::test]
    async fn verified_offline_cache_serves_only_static_facts_and_redacts_editor_identity() {
        let (server, _project) = offline_indexed_server_fixture();
        assert!(server.offline_cache_active());

        let status =
            server.godot_get_connection_status(Parameters(ConnectionStatusInput::default()));
        let status = structured_content(&status).unwrap();
        assert_eq!(status["status"], "offline_cached");
        assert_eq!(
            status.pointer("/static_cache/source_hashes_verified"),
            Some(&json!(true))
        );

        let dependencies = server.godot_get_resource_dependencies(Parameters(ResourceInput {
            resource: "res://a.tres".to_owned(),
            limit: 50,
            cursor: None,
        }));
        let dependencies = structured_content(&dependencies).unwrap();
        assert_eq!(dependencies["freshness"], "offline_cached");
        assert_eq!(dependencies["offline_cached"], true);
        assert_eq!(dependencies["dependencies"].as_array().unwrap().len(), 1);
        assert!(
            dependencies
                .pointer("/validated_checkpoint/editor_session_id")
                .is_none()
        );
        let owners = server.godot_find_resource_owners(Parameters(ResourceInput {
            resource: "res://b.tres".to_owned(),
            limit: 50,
            cursor: None,
        }));
        let owners = structured_content(&owners).unwrap();
        assert_eq!(owners["freshness"], "offline_cached");
        assert_eq!(owners["owners"].as_array().unwrap().len(), 1);

        let scene = server.godot_get_scene_graph(Parameters(SceneGraphInput {
            scene: "res://main.tscn".to_owned(),
            limit: 50,
            cursor: None,
        }));
        let scene = structured_content(&scene).unwrap();
        assert_eq!(scene["freshness"], "offline_cached");
        assert_eq!(scene["offline_cached"], true);
        assert_eq!(
            scene.pointer("/live_overlay/reason"),
            Some(&json!("editor_offline"))
        );
        assert!(
            scene
                .pointer("/validated_checkpoint/editor_session_id")
                .is_none()
        );
        let node_id = scene["nodes"][0]["node_id"].as_str().unwrap().to_owned();
        let scene_id = scene["scene"]["scene_id"].as_str().unwrap().to_owned();
        let node = server.godot_inspect_node(Parameters(InspectNodeInput {
            node_id: Some(node_id),
            scene: None,
            node_path: None,
            limit: 50,
            cursor: None,
        }));
        let node = structured_content(&node).unwrap();
        assert_eq!(node["freshness"], "offline_cached");
        assert!(
            !serde_json::to_string(node)
                .unwrap()
                .contains("editor_session_id")
        );

        let symbols = server.godot_search_symbols(Parameters(SearchSymbolsInput {
            query: "attack".to_owned(),
            match_mode: SymbolMatchInput::Prefix,
            language: Some(ScriptLanguageInput::Gdscript),
            kind: None,
            script: None,
            limit: 50,
            cursor: None,
        }));
        let symbols = structured_content(&symbols).unwrap();
        assert_eq!(symbols["freshness"], "offline_cached");
        assert_eq!(symbols["offline_cached"], true);
        assert!(
            symbols
                .pointer("/validated_checkpoint/editor_session_id")
                .is_none()
        );
        let symbol_id = symbols["symbols"][0]["symbol_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let symbol = server.godot_inspect_symbol(Parameters(InspectSymbolInput {
            symbol_id: Some(symbol_id),
            script: None,
            qualified_name: None,
            limit: 50,
            cursor: None,
        }));
        let symbol = structured_content(&symbol).unwrap();
        assert_eq!(symbol["freshness"], "offline_cached");
        assert!(
            !serde_json::to_string(symbol)
                .unwrap()
                .contains("editor_session_id")
        );

        let usages = server.godot_find_usages(Parameters(FindUsagesInput {
            target: FindUsagesTargetInput::Resource {
                selector: "uid://b".to_owned(),
            },
            source_kinds: Vec::new(),
            confidence: vec![SemanticConfidenceInput::Exact],
            scope: FindUsagesScopeInput::Project,
            limit: 50,
            cursor: None,
        }));
        let usages = structured_content(&usages).unwrap();
        assert_eq!(usages["freshness"], "offline_cached");
        assert_eq!(usages["offline_cached"], true);
        let usages_text = serde_json::to_string(usages).unwrap();
        assert!(!usages_text.contains("editor_session_id"));
        assert!(!usages_text.contains("\"freshness\":\"current\""));

        let project = server
            .read_summary_resource(PROJECT_SUMMARY_URI)
            .expect("offline project summary");
        let ResourceContents::TextResourceContents { text, .. } = &project.contents[0] else {
            panic!("project summary must be text");
        };
        let summary: Value = serde_json::from_str(text).unwrap();
        assert_eq!(summary["freshness"], "offline_cached");
        assert_eq!(summary["offline_cached"], true);
        assert!(!text.contains("editor_session_id"));
        let scene_summary_uri = format!("godot://scene/{}/summary", scene_id.replace(':', "%3A"));
        let scene_summary = server
            .read_summary_resource(&scene_summary_uri)
            .expect("offline scene summary");
        let ResourceContents::TextResourceContents { text, .. } = &scene_summary.contents[0] else {
            panic!("scene summary must be text");
        };
        let scene_summary: Value = serde_json::from_str(text).unwrap();
        assert_eq!(scene_summary["freshness"], "offline_cached");
        assert_eq!(scene_summary["offline_cached"], true);
        assert!(!text.contains("editor_session_id"));

        let live = server.godot_get_editor_state(Parameters(LiveGuardInput::default()));
        assert_eq!(
            structured_content(&live).unwrap().pointer("/error/code"),
            Some(&json!("editor_offline"))
        );
        let runtime = server
            .godot_get_runtime_tree(Parameters(RuntimeTreeInput {
                runtime_session_id: format!("runtime:{}", "1".repeat(32)),
                expected_runtime_event_seq: None,
                limit: 50,
                cursor: None,
            }))
            .await;
        assert_eq!(
            structured_content(&runtime).unwrap().pointer("/error/code"),
            Some(&json!("runtime_unavailable"))
        );
        let run = server
            .godot_run_project(Parameters(RuntimeRunInput::default()))
            .await;
        assert_eq!(
            structured_content(&run).unwrap().pointer("/error/code"),
            Some(&json!("runtime_unavailable"))
        );
        let runtime_object = server
            .godot_inspect_runtime_object(Parameters(RuntimeObjectInput {
                runtime_session_id: format!("runtime:{}", "1".repeat(32)),
                runtime_object_id: format!("runtime-object:{}", "a".repeat(43)),
                expected_runtime_event_seq: None,
            }))
            .await;
        assert_eq!(
            structured_content(&runtime_object)
                .unwrap()
                .pointer("/error/code"),
            Some(&json!("runtime_unavailable"))
        );
        let runtime_stack = server
            .godot_get_stack_trace(Parameters(RuntimeStackInput {
                runtime_session_id: format!("runtime:{}", "1".repeat(32)),
                runtime_stack_id: format!("runtime-stack:{}", "b".repeat(43)),
                expected_runtime_event_seq: None,
            }))
            .await;
        assert_eq!(
            structured_content(&runtime_stack)
                .unwrap()
                .pointer("/error/code"),
            Some(&json!("runtime_unavailable"))
        );
        let capture = server
            .godot_capture_viewport(Parameters(RuntimeCaptureInput {
                runtime_session_id: format!("runtime:{}", "1".repeat(32)),
                expected_runtime_event_seq: None,
                max_width: 320,
                max_height: 180,
            }))
            .await;
        assert_eq!(
            structured_content(&capture).unwrap().pointer("/error/code"),
            Some(&json!("runtime_unavailable"))
        );
        let prepare = server
            .godot_prepare_create_node(Parameters(PrepareCreateNodeInput {
                project_id: format!("project:sha256:{}", "1".repeat(64)),
                editor_session_id: format!("editor:{}", "2".repeat(32)),
                scene_id: format!("scene:{}", "3".repeat(32)),
                history_id: format!("history:{}", "4".repeat(32)),
                scene_revision: 1,
                operation_seq: 1,
                idempotency_key: format!("idempotency:{}", "5".repeat(32)),
                parent_node_id: format!("node:{}", "6".repeat(32)),
                godot_type: "Node".to_owned(),
                name: "Child".to_owned(),
                insertion_index: None,
            }))
            .await;
        assert_eq!(
            structured_content(&prepare).unwrap().pointer("/error/code"),
            Some(&json!("editor_offline"))
        );
        let status = server
            .godot_get_transaction_status(Parameters(TransactionStatusInput {
                transaction_id: format!("transaction:{}", "2".repeat(32)),
            }))
            .await;
        assert_eq!(
            structured_content(&status).unwrap().pointer("/error/code"),
            Some(&json!("editor_offline"))
        );
        let policy =
            server.godot_get_confirmation_policy(Parameters(ConfirmationPolicyInput::default()));
        assert_eq!(
            structured_content(&policy).unwrap().pointer("/error/code"),
            Some(&json!("editor_offline"))
        );
        let reset =
            server.godot_reset_confirmation_policy(Parameters(ConfirmationPolicyInput::default()));
        assert_eq!(
            structured_content(&reset).unwrap().pointer("/error/code"),
            Some(&json!("editor_offline"))
        );
        let undo = server
            .godot_undo_transaction(Parameters(UndoTransactionInput {
                transaction_id: format!("transaction:{}", "2".repeat(32)),
                expected_transaction_seq: 1,
                expected_scene_revision: 1,
                expected_operation_seq: 1,
            }))
            .await;
        assert_eq!(
            structured_content(&undo).unwrap().pointer("/error/code"),
            Some(&json!("editor_offline"))
        );
        let validation = server
            .godot_get_validation_report(Parameters(ValidationReportInput {
                report_id: format!("validation-report:{}", "3".repeat(32)),
                page: 0,
            }))
            .await;
        assert_eq!(
            structured_content(&validation)
                .unwrap()
                .pointer("/error/code"),
            Some(&json!("editor_offline"))
        );
    }

    #[test]
    fn connection_status_preserves_safe_authentication_failure() {
        let replicator = SnapshotReplicator::new();
        replicator.mark_disconnected(ReplicaFailure::AuthenticationFailed);
        let server = GodotMcpServer::with_all_indexes_and_project_root(
            replicator,
            ResourceIndexReader::new(),
            SceneIndexReader::new(),
            ScriptIndexReader::new(),
            PathBuf::from("fixture-project"),
        )
        .with_product_startup_observation(ready_product_startup());
        let result =
            server.godot_get_connection_status(Parameters(ConnectionStatusInput::default()));
        let status = structured_content(&result).expect("connection status");
        assert_eq!(status["status"], "auth_failed");
        assert_eq!(
            status.pointer("/diagnostic/code"),
            Some(&json!("bridge_authentication_failed"))
        );
        assert_eq!(
            status.pointer("/bridge/condition"),
            Some(&json!("authentication_failed"))
        );
    }

    #[test]
    fn change_set_resource_binding_is_index_owned_and_hash_exact() {
        let server = indexed_resource_server();
        let mut params = json!({
            "operations": [{
                "kind": "update_resource",
                "resource": "entity-a",
                "expected_hash": format!("sha256:{}", "a".repeat(64)),
                "properties": [{"name": "offset", "value": {"type": "int", "value": 2}}]
            }],
            "save_scope": {"paths": ["res://a.tres"]}
        });
        server.bind_change_set_resource_paths(&mut params).unwrap();
        assert_eq!(
            params.pointer("/operations/0/resolved_path"),
            Some(&json!("res://a.tres"))
        );

        params["operations"][0]["expected_hash"] = json!(format!("sha256:{}", "f".repeat(64)));
        let error = server
            .bind_change_set_resource_paths(&mut params)
            .unwrap_err();
        assert_eq!(error.0, "stale_resource_state");
    }

    #[test]
    fn change_set_resource_binding_cannot_escape_explicit_save_scope() {
        let server = indexed_resource_server();
        let mut params = json!({
            "operations": [{
                "kind": "update_resource",
                "resource": "entity-a",
                "expected_hash": format!("sha256:{}", "a".repeat(64)),
                "properties": [{"name": "offset", "value": {"type": "int", "value": 2}}]
            }],
            "save_scope": {"paths": ["res://b.tres"]}
        });
        let error = server
            .bind_change_set_resource_paths(&mut params)
            .unwrap_err();
        assert_eq!(error.0, "save_scope_mismatch");
        assert!(params.pointer("/operations/0/resolved_path").is_none());
    }

    #[test]
    fn live_reads_fail_closed_on_stale_revision_guards() {
        let server = canonical_ready_server();
        let current = server.replicator.read().unwrap();
        let stale = server.godot_get_open_scenes(Parameters(LiveListInput {
            limit: 50,
            cursor: None,
            expected_editor_session_id: Some(current.editor_session_id.clone()),
            expected_event_seq: Some(current.revisions.event_seq + 1),
            expected_scene_revision: None,
        }));
        assert_eq!(stale.is_error, Some(true));
        assert_eq!(
            structured_content(&stale)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("stale_editor_state")
        );
        assert_eq!(
            structured_content(&stale)
                .and_then(|value| value.pointer("/error/retryable"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            structured_content(&stale)
                .and_then(|value| value.pointer("/error/current/event_seq"))
                .and_then(Value::as_u64),
            Some(current.revisions.event_seq)
        );

        let ready = server.godot_get_open_scenes(Parameters(LiveListInput {
            limit: 1,
            cursor: None,
            expected_editor_session_id: Some(current.editor_session_id.clone()),
            expected_event_seq: Some(current.revisions.event_seq),
            expected_scene_revision: None,
        }));
        assert_ne!(ready.is_error, Some(true));
        let content = structured_content(&ready).unwrap();
        assert_eq!(content["editor_session_id"], current.editor_session_id);
        assert!(content["scenes"].is_array());

        let current_scene_revision = current
            .current_scene_id
            .as_ref()
            .and_then(|scene_id| current.revisions.scene_revisions.get(scene_id))
            .copied()
            .unwrap_or(0);
        let stale_scene = server.godot_get_inspector_state(Parameters(LiveGuardInput {
            expected_editor_session_id: Some(current.editor_session_id.clone()),
            expected_event_seq: Some(current.revisions.event_seq),
            expected_scene_revision: Some(current_scene_revision + 1),
        }));
        assert_eq!(stale_scene.is_error, Some(true));
        assert_eq!(
            structured_content(&stale_scene)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("stale_scene_revision")
        );
        assert_eq!(
            structured_content(&stale_scene)
                .and_then(|value| value.pointer("/error/retryable"))
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn server_pins_the_2025_11_25_protocol() {
        let server = GodotMcpServer::new(SnapshotReplicator::new());
        let info = server.get_info();
        assert_eq!(info.protocol_version, ProtocolVersion::V_2025_11_25);
        let instructions = info.instructions.expect("server instructions");
        assert!(instructions.len() <= 4_096);
        let first_window = &instructions[..instructions.len().min(512)];
        assert!(first_window.contains("bound to the trusted project"));
        assert!(first_window.contains("godot_get_connection_status"));
        assert!(first_window.contains("godot://connection/status"));
        assert!(first_window.contains("Offline data is saved-project cache only"));
        assert!(first_window.contains("never claim live editor/runtime state"));
        assert!(first_window.contains("writes are unavailable"));
        assert!(first_window.contains("immutable preview"));
        assert!(first_window.contains("Codex sandbox approval"));
        assert!(first_window.contains("MCP action-only form approval"));
        assert!(first_window.contains("independent and both required"));
        assert!(first_window.contains("neither substitutes for the other"));
    }

    #[test]
    fn exactly_forty_one_tools_have_closed_schemas_and_transaction_annotations() {
        let server = GodotMcpServer::new(SnapshotReplicator::new());
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 41);
        let names: BTreeSet<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        let expected = FULL_BETA_TOOLS.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(names.len(), tools.len(), "tool names must be unique");
        assert_eq!(names, expected);
        for tool in &tools {
            let title = tool
                .title
                .as_deref()
                .unwrap_or_else(|| panic!("{} has no stable title", tool.name));
            assert!(!title.trim().is_empty(), "{} has an empty title", tool.name);
            assert!(
                title.chars().count() <= 128,
                "{} title exceeds the product budget",
                tool.name
            );
            let description = tool
                .description
                .as_deref()
                .unwrap_or_else(|| panic!("{} has no description", tool.name));
            assert!(
                !description.trim().is_empty() && description.chars().count() <= 1_024,
                "{} description is empty or exceeds the product budget",
                tool.name
            );
            let annotations = tool.annotations.as_ref().expect("annotations");
            assert_eq!(annotations.open_world_hint, Some(false));
            let input_schema = Value::Object(tool.input_schema.as_ref().clone());
            assert_eq!(
                input_schema.get("type").and_then(Value::as_str),
                Some("object")
            );
            assert_eq!(
                input_schema
                    .get("additionalProperties")
                    .and_then(Value::as_bool),
                Some(false)
            );
            assert!(
                jsonschema::draft202012::meta::is_valid(&input_schema),
                "{} publishes an invalid Draft 2020-12 input schema",
                tool.name
            );
            jsonschema::draft202012::options()
                .build(&input_schema)
                .unwrap_or_else(|error| {
                    panic!(
                        "{} input schema does not compile as Draft 2020-12: {error}",
                        tool.name
                    )
                });
            let output_schema = Value::Object(
                tool.output_schema
                    .as_ref()
                    .unwrap_or_else(|| panic!("{} has no output schema", tool.name))
                    .as_ref()
                    .clone(),
            );
            assert_eq!(
                output_schema.get("type").and_then(Value::as_str),
                Some("object"),
                "{} output root must be an object",
                tool.name
            );
            assert_eq!(
                output_schema
                    .get("additionalProperties")
                    .and_then(Value::as_bool),
                Some(false),
                "{} output root must be closed",
                tool.name
            );
            assert_closed_schema_objects(&output_schema);
            assert!(
                jsonschema::draft202012::meta::is_valid(&output_schema),
                "{} publishes an invalid Draft 2020-12 output schema",
                tool.name
            );
            let output_validator = jsonschema::draft202012::options()
                .build(&output_schema)
                .unwrap_or_else(|error| {
                    panic!(
                        "{} output schema does not compile as Draft 2020-12: {error}",
                        tool.name
                    )
                });
            for invalid in [Value::Null, json!({"unknown": true}), json!({"error": {}})] {
                assert!(
                    !output_validator.is_valid(&invalid),
                    "{} output schema accepts a malformed wire result: {invalid}",
                    tool.name
                );
            }
        }
        let connection_status = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "godot_get_connection_status")
            .expect("connection status tool");
        assert!(
            connection_status
                .description
                .as_deref()
                .is_some_and(|description| description.len() <= 200)
        );
        for name in [
            "godot_run_project",
            "godot_run_current_scene",
            "godot_stop_project",
            "godot_pause_project",
            "godot_continue_project",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool.name.as_ref() == name)
                .expect("runtime control tool");
            assert_eq!(
                tool.annotations.as_ref().unwrap().read_only_hint,
                Some(false)
            );
            assert_eq!(
                tool.annotations.as_ref().unwrap().idempotent_hint,
                Some(false)
            );
            assert_eq!(
                tool.annotations.as_ref().unwrap().destructive_hint,
                Some(name == "godot_stop_project")
            );
        }
        let stop = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "godot_stop_project")
            .unwrap();
        assert_eq!(
            stop.annotations.as_ref().unwrap().destructive_hint,
            Some(true)
        );
        for name in [
            "godot_get_runtime_tree",
            "godot_inspect_runtime_object",
            "godot_get_stack_trace",
            "godot_capture_viewport",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool.name.as_ref() == name)
                .expect("runtime observation tool");
            assert_eq!(
                tool.annotations.as_ref().unwrap().read_only_hint,
                Some(true)
            );
            assert_eq!(
                tool.annotations.as_ref().unwrap().destructive_hint,
                Some(false)
            );
            assert_eq!(
                tool.annotations.as_ref().unwrap().idempotent_hint,
                Some(name != "godot_capture_viewport")
            );
        }
        let capture = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "godot_capture_viewport")
            .expect("runtime capture tool");
        assert_eq!(
            capture.input_schema["properties"]["runtime_session_id"]["pattern"],
            "^runtime:[0-9a-f]{32}$"
        );
        assert_eq!(
            capture.input_schema["properties"]["max_width"]["minimum"],
            1
        );
        assert_eq!(
            capture.input_schema["properties"]["max_width"]["maximum"],
            1_280
        );
        assert_eq!(
            capture.input_schema["properties"]["max_height"]["maximum"],
            720
        );
        let runtime_tree = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "godot_get_runtime_tree")
            .expect("runtime tree tool");
        assert_eq!(
            runtime_tree.input_schema["properties"]["limit"]["minimum"],
            1
        );
        assert_eq!(
            runtime_tree.input_schema["properties"]["limit"]["maximum"],
            200
        );
        assert_eq!(
            DiagnosticScopeInput::default() as u8,
            DiagnosticScopeInput::Editor as u8
        );
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
        let find = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "godot_find_usages")
            .expect("find usages tool");
        assert!(
            find.input_schema["required"]
                .as_array()
                .is_some_and(|required| required.contains(&json!("target")))
        );
        assert_eq!(
            find.input_schema["$defs"]["SemanticConfidenceInput"]["enum"],
            json!(["dynamic", "probable", "runtime_confirmed", "exact"])
        );
        for name in [
            "godot_prepare_create_node",
            "godot_prepare_delete_node",
            "godot_prepare_reparent_node",
            "godot_prepare_set_property",
            "godot_prepare_attach_script",
            "godot_prepare_detach_script",
            "godot_prepare_connect_signal",
            "godot_prepare_disconnect_signal",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool.name.as_ref() == name)
                .expect("transaction prepare tool");
            let annotations = tool.annotations.as_ref().unwrap();
            assert_eq!(annotations.read_only_hint, Some(false));
            assert_eq!(annotations.destructive_hint, Some(false));
            assert_eq!(annotations.idempotent_hint, Some(true));
            let required = tool
                .input_schema
                .get("required")
                .and_then(Value::as_array)
                .expect("prepare required fields");
            for field in [
                "project_id",
                "editor_session_id",
                "scene_id",
                "history_id",
                "scene_revision",
                "operation_seq",
                "idempotency_key",
            ] {
                assert!(required.iter().any(|value| value.as_str() == Some(field)));
            }
        }
        let status = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "godot_get_transaction_status")
            .unwrap()
            .annotations
            .as_ref()
            .unwrap();
        assert_eq!(status.read_only_hint, Some(true));
        assert_eq!(status.destructive_hint, Some(false));
        assert_eq!(status.idempotent_hint, Some(true));
        for name in ["godot_apply_transaction", "godot_undo_transaction"] {
            let annotations = tools
                .iter()
                .find(|tool| tool.name.as_ref() == name)
                .unwrap()
                .annotations
                .as_ref()
                .unwrap();
            assert_eq!(annotations.read_only_hint, Some(false));
            assert_eq!(annotations.destructive_hint, Some(true));
            assert_eq!(annotations.idempotent_hint, Some(false));
        }
        let apply = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "godot_apply_transaction")
            .unwrap();
        assert_eq!(
            apply.input_schema["properties"]["transaction_id"]["pattern"],
            "^(?:transaction|change-set):[0-9a-f]{32}$"
        );
        assert_eq!(
            apply.input_schema["properties"]["preview_digest"]["pattern"],
            "^sha256:[0-9a-f]{64}$"
        );
        assert!(apply.input_schema["properties"].get("approval").is_none());
        assert!(apply.input_schema["properties"].get("receipt").is_none());
        assert!(apply.input_schema["properties"].get("approved").is_none());

        let prepare_change_set = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "godot_prepare_change_set")
            .expect("change-set prepare tool");
        let annotations = prepare_change_set.annotations.as_ref().unwrap();
        assert_eq!(annotations.read_only_hint, Some(false));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(true));
        for forbidden in [
            "approval",
            "receipt",
            "approved",
            "grant",
            "confirmation_policy",
            "validation_report",
            "rollback_proof",
        ] {
            assert!(
                prepare_change_set.input_schema["properties"]
                    .get(forbidden)
                    .is_none()
            );
        }
        assert_eq!(
            prepare_change_set.input_schema["properties"]["operations"]["minItems"],
            1
        );
        assert_eq!(
            prepare_change_set.input_schema["properties"]["operations"]["maxItems"],
            16
        );

        for name in [
            "godot_get_validation_report",
            "godot_get_confirmation_policy",
        ] {
            let annotations = tools
                .iter()
                .find(|tool| tool.name.as_ref() == name)
                .unwrap()
                .annotations
                .as_ref()
                .unwrap();
            assert_eq!(annotations.read_only_hint, Some(true));
            assert_eq!(annotations.destructive_hint, Some(false));
            assert_eq!(annotations.idempotent_hint, Some(true));
        }
        let reset = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "godot_reset_confirmation_policy")
            .unwrap()
            .annotations
            .as_ref()
            .unwrap();
        assert_eq!(reset.read_only_hint, Some(false));
        assert_eq!(reset.destructive_hint, Some(false));
        assert_eq!(reset.idempotent_hint, Some(true));
    }

    #[tokio::test]
    async fn wire_tools_list_uses_the_installed_output_contract_router() {
        let (server_transport, client_transport) = tokio::io::duplex(1_048_576);
        let server_task = tokio::spawn(async move {
            GodotMcpServer::new(SnapshotReplicator::new())
                .with_product_startup_observation(config_missing_product_startup())
                .serve(server_transport)
                .await
                .expect("serve MCP contract fixture")
                .waiting()
                .await
                .expect("wait for MCP contract fixture");
        });
        let client = ().serve(client_transport).await.expect("serve MCP client");
        let tools = client.list_all_tools().await.expect("list tools over MCP");
        assert_eq!(tools.len(), FULL_BETA_TOOLS.len());
        for tool in &tools {
            let schema = tool
                .output_schema
                .as_ref()
                .unwrap_or_else(|| panic!("{} omitted outputSchema on the wire", tool.name));
            assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
            assert_eq!(
                schema.get("additionalProperties").and_then(Value::as_bool),
                Some(false)
            );
        }

        let result = client
            .call_tool(CallToolRequestParams::new("godot_get_editor_state"))
            .await
            .expect("call guarded tool over MCP");
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/error/code")),
            Some(&json!("project_config_missing"))
        );
        let ContentBlock::Text(text) = &result.content[0] else {
            panic!("wire result must start with canonical JSON text");
        };
        assert_eq!(
            serde_json::from_str::<Value>(&text.text).unwrap(),
            result.structured_content.unwrap()
        );

        client.cancel().await.expect("cancel MCP client");
        server_task.await.expect("join MCP contract fixture");
    }

    fn unavailable_server(failure: Option<ReplicaFailure>) -> GodotMcpServer {
        let replicator = SnapshotReplicator::new();
        if let Some(failure) = failure {
            replicator.mark_disconnected(failure);
        }
        GodotMcpServer::with_all_indexes_and_project_root(
            replicator,
            ResourceIndexReader::new(),
            SceneIndexReader::new(),
            ScriptIndexReader::new(),
            PathBuf::from("fixture-project"),
        )
        .with_product_startup_observation(ready_product_startup())
    }

    async fn assert_wire_forbidden_matrix(
        server: GodotMcpServer,
        expected_status: &str,
        expected_diagnostic: &str,
    ) {
        let (server_transport, client_transport) = tokio::io::duplex(1_048_576);
        let server_task = tokio::spawn(async move {
            server
                .serve(server_transport)
                .await
                .expect("serve unavailable MCP fixture")
                .waiting()
                .await
                .expect("wait for unavailable MCP fixture");
        });
        let client = ().serve(client_transport).await.expect("serve MCP client");
        let status = client
            .call_tool(CallToolRequestParams::new("godot_get_connection_status"))
            .await
            .expect("read connection status");
        let status_value = status.structured_content.expect("structured status");
        assert_eq!(status_value["status"], expected_status);
        assert_eq!(
            status_value.pointer("/diagnostic/code"),
            Some(&json!(expected_diagnostic))
        );

        for tool_name in FULL_BETA_TOOLS {
            let Some(domain) = output_schema::availability_domain(tool_name) else {
                continue;
            };
            // The router-level guard executes before input deserialization.
            // Empty arguments therefore exercise every prohibited route
            // without maintaining a second copy of 33 input fixtures.
            let result = client
                .call_tool(CallToolRequestParams::new(*tool_name))
                .await
                .unwrap_or_else(|error| panic!("{tool_name} escaped availability guard: {error}"));
            assert_eq!(result.is_error, Some(true), "{tool_name}");
            let value = result
                .structured_content
                .as_ref()
                .unwrap_or_else(|| panic!("{tool_name} returned no structured error"));
            let expected_code = if matches!(expected_status, "offline_cached" | "offline_empty") {
                match domain {
                    AvailabilityDomain::Editor => "editor_offline",
                    AvailabilityDomain::Runtime => "runtime_unavailable",
                }
            } else {
                expected_diagnostic
            };
            assert_eq!(
                value.pointer("/error/code"),
                Some(&json!(expected_code)),
                "{tool_name}"
            );
            assert_eq!(
                value.pointer("/error/status"),
                Some(&json!(expected_status)),
                "{tool_name}"
            );
            assert_eq!(
                value.pointer("/error/diagnostic_code"),
                Some(&json!(expected_diagnostic)),
                "{tool_name}"
            );
            let ContentBlock::Text(text) = &result.content[0] else {
                panic!("{tool_name} did not return canonical JSON text");
            };
            assert_eq!(
                serde_json::from_str::<Value>(&text.text).unwrap(),
                *value,
                "{tool_name}"
            );
        }

        client.cancel().await.expect("cancel MCP client");
        server_task.await.expect("join unavailable MCP fixture");
    }

    #[tokio::test]
    async fn every_forbidden_wire_tool_fails_closed_in_each_connection_failure_class() {
        let (offline_cached, offline_project) = offline_indexed_server_fixture();
        assert_wire_forbidden_matrix(offline_cached, "offline_cached", "editor_offline").await;
        drop(offline_project);

        for (failure, expected_status, expected_diagnostic) in [
            (
                ReplicaFailure::AuthenticationFailed,
                "auth_failed",
                "bridge_authentication_failed",
            ),
            (
                ReplicaFailure::ProjectBindingMismatch,
                "auth_failed",
                "project_binding_mismatch",
            ),
            (
                ReplicaFailure::ProtocolVersionIncompatible,
                "incompatible",
                "bridge_version_incompatible",
            ),
        ] {
            let (server, project) = offline_indexed_server_fixture();
            server.replicator.mark_disconnected(failure);
            assert_wire_forbidden_matrix(server, expected_status, expected_diagnostic).await;
            drop(project);
        }

        assert_wire_forbidden_matrix(
            unavailable_server(Some(ReplicaFailure::TransportDisconnected)),
            "offline_empty",
            "static_cache_unavailable",
        )
        .await;
        for fault in [
            OfflineAuthorityFault::Missing,
            OfflineAuthorityFault::Corrupt,
            OfflineAuthorityFault::StaleSource,
        ] {
            let (server, project) = offline_indexed_server_with_fault(fault);
            assert_wire_forbidden_matrix(server, "offline_empty", "static_cache_stale").await;
            drop(project);
        }
        assert_wire_forbidden_matrix(
            unavailable_server(Some(ReplicaFailure::AuthenticationFailed)),
            "auth_failed",
            "bridge_authentication_failed",
        )
        .await;
        assert_wire_forbidden_matrix(
            unavailable_server(Some(ReplicaFailure::ProjectBindingMismatch)),
            "auth_failed",
            "project_binding_mismatch",
        )
        .await;
        assert_wire_forbidden_matrix(
            unavailable_server(Some(ReplicaFailure::ProtocolVersionIncompatible)),
            "incompatible",
            "bridge_version_incompatible",
        )
        .await;
        assert_wire_forbidden_matrix(unavailable_server(None), "connecting", "bridge_connecting")
            .await;

        assert_wire_forbidden_matrix(
            GodotMcpServer::new(SnapshotReplicator::new())
                .with_product_startup_observation(config_missing_product_startup()),
            "misconfigured",
            "project_config_missing",
        )
        .await;
    }

    #[test]
    fn static_cache_rebuild_does_not_revoke_live_bridge_authority() {
        let server = indexed_server_fixture(false, true, true, true);
        let mut health = server.connection_status();
        assert_eq!(health.status, ConnectionStatus::Ready);

        health.status = ConnectionStatus::Syncing;
        health.static_cache.condition = CacheCondition::Rebuilding;
        health.diagnostic =
            ProductDiagnostic::new(DiagnosticCode::StaticCacheRebuilding, None, None);

        assert!(has_tool_domain_authority(
            &health,
            AvailabilityDomain::Editor
        ));
        assert!(has_tool_domain_authority(
            &health,
            AvailabilityDomain::Runtime
        ));

        health.bridge.condition = BridgeCondition::ProjectBindingMismatch;
        assert!(!has_tool_domain_authority(
            &health,
            AvailabilityDomain::Editor
        ));
        assert!(!has_tool_domain_authority(
            &health,
            AvailabilityDomain::Runtime
        ));
    }

    #[test]
    fn stale_semantic_replica_blocks_editor_reads_but_allows_runtime_reauthentication() {
        let syncing = canonical_ready_replica();
        syncing.invalidate("runtime launch requires a fresh semantic snapshot");
        let server = GodotMcpServer::with_all_indexes_and_project_root(
            syncing,
            ResourceIndexReader::new(),
            SceneIndexReader::new(),
            ScriptIndexReader::new(),
            PathBuf::from("fixture-project"),
        )
        .with_product_startup_observation(ready_product_startup());
        let health = server.connection_status();

        assert_eq!(health.status, ConnectionStatus::Syncing);
        assert_eq!(health.diagnostic.code, DiagnosticCode::BridgeDiscoveryStale);
        assert!(
            server
                .unavailable_error(AvailabilityDomain::Editor)
                .is_some()
        );
        assert!(
            server
                .unavailable_error(AvailabilityDomain::Runtime)
                .is_none()
        );
        assert!(
            output_schema::ToolAvailabilityGuard::unavailable_tool_result(
                &server,
                "godot_get_diagnostics",
            )
            .is_none(),
            "the mixed-scope router guard must defer to the diagnostics scope"
        );
    }

    #[test]
    fn doctor_and_mcp_share_exact_failure_diagnostics_and_remediation() {
        let cases = [
            (
                BridgeError::Authentication,
                DiagnosticCode::BridgeAuthenticationFailed,
            ),
            (
                BridgeError::Rpc {
                    code: "unauthenticated".to_owned(),
                    message: "/Users/alice/private token=top-secret".to_owned(),
                    retryable: false,
                    data: json!({"native_handle": 42}),
                },
                DiagnosticCode::BridgeAuthenticationFailed,
            ),
            (
                BridgeError::ProjectBindingMismatch,
                DiagnosticCode::ProjectBindingMismatch,
            ),
            (
                BridgeError::Rpc {
                    code: "project_not_bound".to_owned(),
                    message: "/Users/alice/private token=top-secret".to_owned(),
                    retryable: false,
                    data: json!({"native_handle": 42}),
                },
                DiagnosticCode::ProjectBindingMismatch,
            ),
            (
                BridgeError::ProtocolVersionMismatch,
                DiagnosticCode::BridgeVersionIncompatible,
            ),
            (
                BridgeError::Rpc {
                    code: "protocol_mismatch".to_owned(),
                    message: "/Users/alice/private token=top-secret".to_owned(),
                    retryable: false,
                    data: json!({"native_handle": 42}),
                },
                DiagnosticCode::BridgeVersionIncompatible,
            ),
        ];

        for (error, expected_code) in cases {
            let doctor_observation = BridgeProbeObservation::failed(&error);
            let doctor_code = doctor_observation
                .status
                .diagnostic_code()
                .expect("failure must have a doctor diagnostic");
            assert_eq!(doctor_code, expected_code);
            let doctor_diagnostic = ProductDiagnostic::new(doctor_code, None, None);

            let server = unavailable_server(Some(error.failure_class().replica_failure()));
            let mcp_health = server.connection_status();
            assert_eq!(mcp_health.diagnostic.code, doctor_code);
            assert_eq!(
                mcp_health.diagnostic.remediation_id,
                doctor_diagnostic.remediation_id
            );
            assert_eq!(mcp_health.diagnostic.retryable, doctor_diagnostic.retryable);

            let serialized = serde_json::to_string(&mcp_health).unwrap();
            assert!(!serialized.contains("/Users/"));
            assert!(!serialized.contains("top-secret"));
            assert!(!serialized.contains("native_handle"));
        }
    }

    #[tokio::test]
    async fn proven_failure_survives_retries_on_wire_until_authenticated_negotiation() {
        let replicator = SnapshotReplicator::new();
        replicator.mark_disconnected(ReplicaFailure::ProjectBindingMismatch);
        let reconnect_control = replicator.clone();
        let server = GodotMcpServer::with_all_indexes_and_project_root(
            replicator,
            ResourceIndexReader::new(),
            SceneIndexReader::new(),
            ScriptIndexReader::new(),
            PathBuf::from("fixture-project"),
        )
        .with_product_startup_observation(ready_product_startup());
        let (server_transport, client_transport) = tokio::io::duplex(1_048_576);
        let server_task = tokio::spawn(async move {
            server
                .serve(server_transport)
                .await
                .expect("serve reconnect MCP fixture")
                .waiting()
                .await
                .expect("wait for reconnect MCP fixture");
        });
        let client = ().serve(client_transport).await.expect("serve MCP client");

        let failed = client
            .call_tool(CallToolRequestParams::new("godot_get_connection_status"))
            .await
            .expect("read failed connection status")
            .structured_content
            .expect("structured failed status");
        assert_eq!(failed["status"], "auth_failed");
        assert_eq!(
            failed.pointer("/diagnostic/code"),
            Some(&json!("project_binding_mismatch"))
        );

        for _ in 0..3 {
            reconnect_control.mark_connecting();
            reconnect_control.mark_disconnected(ReplicaFailure::TransportDisconnected);
            let reconnecting = client
                .call_tool(CallToolRequestParams::new("godot_get_connection_status"))
                .await
                .expect("read reconnecting status")
                .structured_content
                .expect("structured reconnecting status");
            assert_eq!(reconnecting["status"], "auth_failed");
            assert_eq!(
                reconnecting.pointer("/diagnostic/code"),
                Some(&json!("project_binding_mismatch"))
            );
            assert_eq!(
                reconnecting.pointer("/bridge/condition"),
                Some(&json!("project_binding_mismatch"))
            );
        }

        reconnect_control.mark_disconnected(ReplicaFailure::AuthenticationFailed);
        let replaced = client
            .call_tool(CallToolRequestParams::new("godot_get_connection_status"))
            .await
            .expect("read replacement fault")
            .structured_content
            .expect("structured replacement status");
        assert_eq!(
            replaced.pointer("/diagnostic/code"),
            Some(&json!("bridge_authentication_failed"))
        );

        reconnect_control.mark_bridge_negotiated(current_negotiated_bridge());
        let negotiated = client
            .call_tool(CallToolRequestParams::new("godot_get_connection_status"))
            .await
            .expect("read authenticated reconnect")
            .structured_content
            .expect("structured negotiated status");
        assert_eq!(negotiated["status"], "syncing");
        assert_eq!(
            negotiated.pointer("/diagnostic/code"),
            Some(&json!("bridge_syncing"))
        );
        assert_eq!(
            negotiated.pointer("/bridge/condition"),
            Some(&json!("syncing"))
        );

        client.cancel().await.expect("cancel MCP client");
        server_task.await.expect("join reconnect MCP fixture");
    }

    #[tokio::test]
    async fn representative_success_results_match_closed_schemas_and_equivalent_text() {
        let live = canonical_ready_server();
        let live_list_input = || LiveListInput {
            limit: 50,
            cursor: None,
            expected_editor_session_id: None,
            expected_event_seq: None,
            expected_scene_revision: None,
        };
        assert_tool_result_contract(
            "godot_get_connection_status",
            live.godot_get_connection_status(Parameters(ConnectionStatusInput::default())),
        );
        assert_tool_result_contract(
            "godot_get_editor_state",
            live.godot_get_editor_state(Parameters(LiveGuardInput::default())),
        );
        assert_tool_result_contract(
            "godot_get_current_scene",
            live.godot_get_current_scene(Parameters(LiveGuardInput::default())),
        );
        assert_tool_result_contract(
            "godot_get_selected_nodes",
            live.godot_get_selected_nodes(Parameters(LiveGuardInput::default())),
        );
        assert_tool_result_contract(
            "godot_get_open_scenes",
            live.godot_get_open_scenes(Parameters(live_list_input())),
        );
        assert_tool_result_contract(
            "godot_get_inspector_state",
            live.godot_get_inspector_state(Parameters(LiveGuardInput::default())),
        );
        assert_tool_result_contract(
            "godot_get_open_scripts",
            live.godot_get_open_scripts(Parameters(live_list_input())),
        );
        assert_tool_result_contract(
            "godot_get_editor_history",
            live.godot_get_editor_history(Parameters(live_list_input())),
        );
        assert_tool_result_contract(
            "godot_get_diagnostics",
            live.godot_get_diagnostics(Parameters(DiagnosticInput {
                scope: DiagnosticScopeInput::Editor,
                limit: 50,
                cursor: None,
                expected_editor_session_id: None,
                expected_event_seq: None,
                expected_scene_revision: None,
                runtime_session_id: None,
                expected_runtime_event_seq: None,
            }))
            .await,
        );
        assert_tool_result_contract(
            "godot_get_viewport_state",
            live.godot_get_viewport_state(Parameters(LiveGuardInput::default())),
        );
        assert_tool_result_contract(
            "godot_get_confirmation_policy",
            live.godot_get_confirmation_policy(Parameters(ConfirmationPolicyInput::default())),
        );
        assert_tool_result_contract(
            "godot_reset_confirmation_policy",
            live.godot_reset_confirmation_policy(Parameters(ConfirmationPolicyInput::default())),
        );

        let resources = indexed_resource_server();
        assert_tool_result_contract(
            "godot_get_resource_dependencies",
            resources.godot_get_resource_dependencies(Parameters(ResourceInput {
                resource: "uid://a".to_owned(),
                limit: 50,
                cursor: None,
            })),
        );
        assert_tool_result_contract(
            "godot_find_resource_owners",
            resources.godot_find_resource_owners(Parameters(ResourceInput {
                resource: "res://b.tres".to_owned(),
                limit: 50,
                cursor: None,
            })),
        );

        let scenes = indexed_semantic_server();
        assert_tool_result_contract(
            "godot_get_scene_graph",
            scenes.godot_get_scene_graph(Parameters(SceneGraphInput {
                scene: "res://main.tscn".to_owned(),
                limit: 50,
                cursor: None,
            })),
        );
        assert_tool_result_contract(
            "godot_inspect_node",
            scenes.godot_inspect_node(Parameters(InspectNodeInput {
                node_id: Some("godot:node-occurrence:v1:player".to_owned()),
                scene: None,
                node_path: None,
                limit: 50,
                cursor: None,
            })),
        );
        let scripts = indexed_script_server();
        let signal_usages = scripts.godot_find_usages(Parameters(FindUsagesInput {
            target: FindUsagesTargetInput::Signal {
                scene: "uid://scene".to_owned(),
                emitter_node_path: ".".to_owned(),
                signal: "ready".to_owned(),
            },
            source_kinds: Vec::new(),
            confidence: vec![SemanticConfidenceInput::Exact],
            scope: FindUsagesScopeInput::Project,
            limit: 50,
            cursor: None,
        }));
        assert_ne!(
            signal_usages.is_error,
            Some(true),
            "signal usage fixture must resolve: {:?}",
            signal_usages.structured_content
        );
        assert_tool_result_contract("godot_find_usages", signal_usages);
        assert_tool_result_contract(
            "godot_search_symbols",
            scripts.godot_search_symbols(Parameters(SearchSymbolsInput {
                query: "attack".to_owned(),
                match_mode: SymbolMatchInput::Prefix,
                language: Some(ScriptLanguageInput::Gdscript),
                kind: None,
                script: None,
                limit: 50,
                cursor: None,
            })),
        );
        assert_tool_result_contract(
            "godot_inspect_symbol",
            scripts.godot_inspect_symbol(Parameters(InspectSymbolInput {
                symbol_id: Some(attack_symbol_id()),
                script: None,
                qualified_name: None,
                limit: 50,
                cursor: None,
            })),
        );
        assert_tool_result_contract(
            "godot_find_usages",
            scripts.godot_find_usages(Parameters(FindUsagesInput {
                target: FindUsagesTargetInput::Resource {
                    selector: "uid://b".to_owned(),
                },
                source_kinds: Vec::new(),
                confidence: vec![SemanticConfidenceInput::Exact],
                scope: FindUsagesScopeInput::Project,
                limit: 50,
                cursor: None,
            })),
        );

        for (tool_name, response) in [
            (
                "godot_run_project",
                serde_json::from_str::<Value>(include_str!(
                    "../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-run-response.json"
                ))
                .unwrap(),
            ),
            (
                "godot_inspect_runtime_object",
                serde_json::from_str::<Value>(include_str!(
                    "../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-object-inspect-response.json"
                ))
                .unwrap(),
            ),
            (
                "godot_get_stack_trace",
                serde_json::from_str::<Value>(include_str!(
                    "../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-stack-response.json"
                ))
                .unwrap(),
            ),
        ] {
            assert_tool_result_contract(
                tool_name,
                CallToolResult::structured(response["result"].clone()),
            );
        }
        let capture: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-viewport-capture-response.json"
        ))
        .unwrap();
        let capture: RuntimeViewportCapture =
            serde_json::from_value(capture["result"].clone()).unwrap();
        assert_tool_result_contract(
            "godot_capture_viewport",
            runtime_capture_result(capture).unwrap(),
        );
    }

    #[tokio::test]
    async fn transaction_tools_remain_registered_but_fail_stably_without_a_coordinator() {
        let server = canonical_ready_server();
        let result = server
            .prepare_transaction(PrepareCommand {
                project_id: format!("project:sha256:{}", "1".repeat(64)),
                editor_session_id: format!("editor:{}", "2".repeat(32)),
                idempotency_key: format!("idempotency:{}", "3".repeat(32)),
                coordinates: godot_codex_bridge_client::RevisionCoordinates {
                    scene_id: format!("scene:{}", "4".repeat(32)),
                    history_id: format!("history:{}", "5".repeat(32)),
                    scene_revision: 1,
                    operation_seq: 1,
                },
                operation: godot_codex_bridge_client::TransactionOperation::DeleteNode {
                    node_id: format!("node:{}", "6".repeat(32)),
                },
            })
            .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            structured_content(&result).unwrap()["error"]["code"],
            "transaction_coordinator_unavailable"
        );
        let status = server
            .transaction_status(&format!("transaction:{}", "7".repeat(32)))
            .await;
        assert_eq!(status.is_error, Some(true));
        assert_eq!(
            structured_content(&status).unwrap()["error"]["code"],
            "transaction_coordinator_unavailable"
        );
    }

    #[test]
    fn runtime_error_projection_exposes_only_safe_coordinates() {
        let session = format!("runtime:{}", "a".repeat(32));
        let projected = runtime_bridge_error(BridgeError::Rpc {
            code: "stale_runtime_state".to_owned(),
            message: "failed at /Users/private/project".to_owned(),
            retryable: true,
            data: json!({
                "runtime_session_id": session,
                "runtime_event_seq": 7,
                "state": "paused",
                "path": "/Users/private/project/main.tscn",
                "native_handle": 42,
            }),
        });
        let error = &structured_content(&projected).unwrap()["error"];
        assert_eq!(error["code"], "stale_runtime_state");
        assert_eq!(error["retryable"], true);
        assert_eq!(
            error["current"],
            json!({
                "runtime_session_id": format!("runtime:{}", "a".repeat(32)),
                "runtime_event_seq": 7,
                "state": "paused",
            })
        );
        let serialized = serde_json::to_string(error).unwrap();
        assert!(!serialized.contains("/Users/"));
        assert!(!serialized.contains("native_handle"));

        let malformed = runtime_bridge_error(BridgeError::Rpc {
            code: "/Users/private".to_owned(),
            message: "private".to_owned(),
            retryable: false,
            data: json!({"runtime_session_id": "/tmp/session", "runtime_event_seq": 1}),
        });
        let error = &structured_content(&malformed).unwrap()["error"];
        assert_eq!(error["code"], "runtime_unavailable");
        assert_eq!(error["current"], json!({}));

        let malformed_json =
            serde_json::from_str::<Value>("{").expect_err("fixture must be malformed");
        for transport_error in [
            BridgeError::Json(malformed_json),
            BridgeError::Invalid("server path /Users/private/project token=top-secret".to_owned()),
            BridgeError::Replica(godot_codex_semantic_model::ReplicaError::InvalidSnapshot(
                "malformed fixture",
            )),
            BridgeError::Rpc {
                code: "unknown_private_failure".to_owned(),
                message: "server path /Users/private/project token=top-secret".to_owned(),
                retryable: false,
                data: json!({
                    "runtime_session_id": format!("runtime:{}", "a".repeat(32)),
                    "runtime_event_seq": 99,
                    "native_handle": 42,
                }),
            },
            BridgeError::Rpc {
                code: "session_mismatch".to_owned(),
                message: "stale /Users/private/project token=top-secret".to_owned(),
                retryable: false,
                data: json!({"native_handle": 42}),
            },
        ] {
            let debug_line = runtime_bridge_debug_line(&transport_error, "request");
            assert!(!debug_line.contains("/Users/"));
            assert!(!debug_line.contains("top-secret"));
            assert!(!debug_line.contains("native_handle"));
            assert!(!debug_line.contains("unknown_private_failure"));
            assert!(!debug_line.contains("session_mismatch"));
            let projected = runtime_bridge_error(transport_error);
            let error = &structured_content(&projected).unwrap()["error"];
            assert_eq!(error["code"], "runtime_unavailable");
            assert_eq!(error["retryable"], true);
            let serialized = serde_json::to_string(error).unwrap();
            assert!(!serialized.contains("/Users/"));
            assert!(!serialized.contains("top-secret"));
            assert!(!serialized.contains("native_handle"));
            assert!(!serialized.contains("unknown_private_failure"));
            assert!(!serialized.contains("session_mismatch"));
        }

        for (rpc_code, public_code, retryable) in [
            ("unauthenticated", "bridge_authentication_failed", true),
            ("project_not_bound", "project_binding_mismatch", false),
            ("protocol_mismatch", "bridge_version_incompatible", false),
        ] {
            let projected = runtime_bridge_error(BridgeError::Rpc {
                code: rpc_code.to_owned(),
                message: "/Users/private/project token=top-secret".to_owned(),
                retryable: false,
                data: json!({"native_handle": 42}),
            });
            let error = &structured_content(&projected).unwrap()["error"];
            assert_eq!(error["code"], public_code);
            assert_eq!(error["retryable"], retryable);
            let serialized = serde_json::to_string(error).unwrap();
            assert!(!serialized.contains("/Users/"));
            assert!(!serialized.contains("top-secret"));
            assert!(!serialized.contains("native_handle"));
        }

        assert_tool_result_contract(
            "godot_get_runtime_tree",
            runtime_bridge_error(BridgeError::Invalid("bounded transport fixture".to_owned())),
        );
        assert_tool_result_contract(
            "godot_get_runtime_tree",
            runtime_bridge_error(BridgeError::Rpc {
                code: "runtime_request_timeout".to_owned(),
                message: "bounded runtime fixture".to_owned(),
                retryable: true,
                data: json!({
                    "runtime_session_id": format!("runtime:{}", "a".repeat(32)),
                    "runtime_event_seq": 99,
                    "state": "running",
                }),
            }),
        );
    }

    #[test]
    fn runtime_capture_returns_one_image_copy_and_metadata_only_structured_content() {
        let response: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-viewport-capture-response.json"
        ))
        .unwrap();
        let capture: RuntimeViewportCapture =
            serde_json::from_value(response["result"].clone()).unwrap();
        let result = runtime_capture_result(capture).unwrap();
        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.content.len(), 2);
        let structured = result.structured_content.as_ref().unwrap();
        assert_eq!(structured["mime_type"], "image/png");
        assert_eq!(structured["width"], 2);
        assert_eq!(structured["height"], 2);
        assert!(structured.get("data_base64url").is_none());
        assert!(structured.get("path").is_none());
        assert!(structured.get("native_handle").is_none());

        let ContentBlock::Text(text) = &result.content[0] else {
            panic!("capture metadata must be text");
        };
        let text_value: Value = serde_json::from_str(&text.text).unwrap();
        assert_eq!(&text_value, structured);
        let ContentBlock::Image(image) = &result.content[1] else {
            panic!("capture pixels must be image content");
        };
        assert_eq!(image.mime_type, "image/png");
        let png = base64::engine::general_purpose::STANDARD
            .decode(&image.data)
            .unwrap();
        assert_eq!(
            png.len(),
            structured["byte_length"].as_u64().unwrap() as usize
        );
        assert_eq!(
            Sha256::digest(&png)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            structured["sha256"].as_str().unwrap()
        );
    }

    #[test]
    fn find_usages_aggregates_evidence_pages_and_binds_filters() {
        let server = indexed_script_server();
        let first = server.godot_find_usages(Parameters(FindUsagesInput {
            target: FindUsagesTargetInput::Resource {
                selector: "uid://b".to_owned(),
            },
            source_kinds: vec![],
            confidence: vec![SemanticConfidenceInput::Exact],
            scope: FindUsagesScopeInput::Project,
            limit: 1,
            cursor: None,
        }));
        assert_ne!(first.is_error, Some(true));
        let first = structured_content(&first).expect("first usage page");
        assert!(
            first["total_matches"]
                .as_u64()
                .is_some_and(|total| total >= 2)
        );
        assert_eq!(first["usages"].as_array().map(Vec::len), Some(1));
        assert!(
            first["usages"][0]["evidence"]
                .as_array()
                .is_some_and(|evidence| !evidence.is_empty())
        );
        assert_eq!(first["partial_reasons"], json!([]));
        let cursor = first["next_cursor"].as_str().expect("next cursor");

        let second = server.godot_find_usages(Parameters(FindUsagesInput {
            target: FindUsagesTargetInput::Resource {
                selector: "uid://b".to_owned(),
            },
            source_kinds: vec![],
            confidence: vec![SemanticConfidenceInput::Exact],
            scope: FindUsagesScopeInput::Project,
            limit: 1,
            cursor: Some(cursor.to_owned()),
        }));
        assert_ne!(second.is_error, Some(true));
        assert_eq!(
            structured_content(&second).and_then(|value| value["offset"].as_u64()),
            Some(1)
        );

        let cross_filter = server.godot_find_usages(Parameters(FindUsagesInput {
            target: FindUsagesTargetInput::Resource {
                selector: "uid://b".to_owned(),
            },
            source_kinds: vec![SemanticEntityKindInput::Node],
            confidence: vec![SemanticConfidenceInput::Exact],
            scope: FindUsagesScopeInput::Project,
            limit: 1,
            cursor: Some(cursor.to_owned()),
        }));
        assert_eq!(cross_filter.is_error, Some(true));
        assert_eq!(
            structured_content(&cross_filter)
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("stale_cursor")
        );
    }

    #[test]
    fn find_usages_reports_partial_domains_without_serving_stale_facts() {
        let server = indexed_resource_server();
        let result = server.godot_find_usages(Parameters(FindUsagesInput {
            target: FindUsagesTargetInput::Resource {
                selector: "uid://b".to_owned(),
            },
            source_kinds: vec![],
            confidence: vec![],
            scope: FindUsagesScopeInput::Project,
            limit: 50,
            cursor: None,
        }));
        assert_ne!(result.is_error, Some(true));
        assert!(
            output_schema::matches_top_level_contract("godot_find_usages", &result),
            "partial find-usages result must match its output schema: {:?}",
            result.structured_content
        );
        let result = structured_content(&result).expect("partial usage response");
        assert_eq!(result["scene_graph_revision"], Value::Null);
        assert_eq!(result["script_graph_revision"], Value::Null);
        assert_eq!(result["partial_reasons"].as_array().map(Vec::len), Some(2));
        assert!(
            result["usages"]
                .as_array()
                .expect("usages")
                .iter()
                .all(|usage| usage["domain"] == "resource")
        );
    }

    #[test]
    fn find_usages_resolves_a_canonical_script_symbol() {
        let server = indexed_script_server();
        let result = server.godot_find_usages(Parameters(FindUsagesInput {
            target: FindUsagesTargetInput::ScriptSymbol {
                script: "res://scripts/player.gd".to_owned(),
                qualified_name: "class:Player/method:attack_special".to_owned(),
            },
            source_kinds: vec![SemanticEntityKindInput::Symbol],
            confidence: vec![SemanticConfidenceInput::Exact],
            scope: FindUsagesScopeInput::Script {
                script: "res://scripts/player.gd".to_owned(),
            },
            limit: 50,
            cursor: None,
        }));
        assert_ne!(result.is_error, Some(true));
        let usages = structured_content(&result)
            .and_then(|value| value["usages"].as_array())
            .expect("symbol usages");
        assert!(usages.iter().any(|usage| usage["predicate"] == "calls"));
    }

    #[test]
    fn summary_resources_are_stable_bounded_and_do_not_enable_subscriptions() {
        let server = indexed_script_server();
        let info = server.get_info();
        let resources = info.capabilities.resources.expect("resources capability");
        assert_eq!(resources.subscribe, None);
        assert_eq!(resources.list_changed, None);
        let fixed_resources = GodotMcpServer::summary_resources();
        assert_eq!(fixed_resources.len(), 4);
        let resource_uris: BTreeSet<_> = fixed_resources
            .iter()
            .map(|resource| resource.uri.as_str())
            .collect();
        let resource_names: BTreeSet<_> = fixed_resources
            .iter()
            .map(|resource| resource.name.as_str())
            .collect();
        assert_eq!(resource_uris.len(), fixed_resources.len());
        assert_eq!(resource_names.len(), fixed_resources.len());
        assert_eq!(
            resource_uris,
            FIXED_RESOURCE_URIS.iter().copied().collect::<BTreeSet<_>>()
        );
        assert_eq!(fixed_resources[0].uri, PROJECT_SUMMARY_URI);
        assert_eq!(fixed_resources[1].uri, EDITOR_SUMMARY_URI);
        assert_eq!(fixed_resources[2].uri, RUNTIME_SUMMARY_URI);
        assert_eq!(fixed_resources[3].uri, CONNECTION_STATUS_URI);
        assert_eq!(GodotMcpServer::summary_resource_templates().len(), 1);
        assert_eq!(
            GodotMcpServer::summary_resource_templates()[0].uri_template,
            "godot://scene/{scene_id}/summary"
        );
        assert_eq!(
            GodotMcpServer::summary_resource_templates()[0].uri_template,
            RESOURCE_TEMPLATE_URIS[0]
        );

        let project = server
            .read_summary_resource(PROJECT_SUMMARY_URI)
            .expect("project summary");
        let ResourceContents::TextResourceContents {
            text, mime_type, ..
        } = &project.contents[0]
        else {
            panic!("project summary must be text");
        };
        assert_eq!(mime_type.as_deref(), Some("application/json"));
        assert!(text.len() <= 4_096);
        let value: Value = serde_json::from_str(text).expect("project summary JSON");
        assert_eq!(value["kind"], "godot_project_summary");
        assert_eq!(value["budget"]["method"], "utf8_byte_upper_bound_v1");
        assert!(value["evidence_ids"].is_array());

        let live_server = canonical_ready_server();
        let editor = live_server
            .read_summary_resource(EDITOR_SUMMARY_URI)
            .expect("editor summary");
        let ResourceContents::TextResourceContents { text, .. } = &editor.contents[0] else {
            panic!("editor summary must be text");
        };
        assert!(text.len() <= EDITOR_SUMMARY_MAX_BYTES);
        let value: Value = serde_json::from_str(text).expect("editor summary JSON");
        assert_eq!(value["schema_version"], "editor/1.0");
        assert!(value["revision_vector"].is_object());

        let runtime = live_server
            .read_summary_resource(RUNTIME_SUMMARY_URI)
            .expect("runtime summary");
        let ResourceContents::TextResourceContents { text, .. } = &runtime.contents[0] else {
            panic!("runtime summary must be text");
        };
        assert!(text.len() <= EDITOR_SUMMARY_MAX_BYTES);
        let value: Value = serde_json::from_str(text).expect("runtime summary JSON");
        assert_eq!(value["schema_version"], "runtime-summary/1.0");

        let connection = live_server
            .read_summary_resource(CONNECTION_STATUS_URI)
            .expect("connection status");
        let ResourceContents::TextResourceContents { text, .. } = &connection.contents[0] else {
            panic!("connection status must be text");
        };
        assert!(text.len() <= CONNECTION_STATUS_MAX_BYTES);
        let resource_value: Value =
            serde_json::from_str(text).expect("connection status resource JSON");
        let tool =
            live_server.godot_get_connection_status(Parameters(ConnectionStatusInput::default()));
        let tool_value = structured_content(&tool).expect("structured connection status");
        assert_eq!(&resource_value, tool_value);
        assert_eq!(
            resource_value["schema_version"],
            "godot-connection-status/1.0"
        );
        assert_eq!(resource_value["status"], "ready");
        assert_eq!(
            resource_value.pointer("/diagnostic/code"),
            Some(&json!("ready"))
        );
        let ContentBlock::Text(text_content) = &tool.content[0] else {
            panic!("connection status tool must include equivalent JSON text");
        };
        let tool_text_value: Value =
            serde_json::from_str(&text_content.text).expect("connection status tool text JSON");
        assert_eq!(tool_text_value, *tool_value);

        let scene_id = "godot:scene:uid:v1:testscene";
        let uri = format!(
            "{SCENE_SUMMARY_PREFIX}{}{SCENE_SUMMARY_SUFFIX}",
            encode_uri_segment(scene_id)
        );
        let scene = server.read_summary_resource(&uri).expect("scene summary");
        let ResourceContents::TextResourceContents { text, .. } = &scene.contents[0] else {
            panic!("scene summary must be text");
        };
        assert!(text.len() <= 2_048);
        let value: Value = serde_json::from_str(text).expect("scene summary JSON");
        assert_eq!(value["scene_entity_id"], scene_id);
        assert!(value["root_structure"].is_array());
    }

    #[test]
    fn scene_summary_uri_parser_is_canonical_and_filesystem_independent() {
        let scene_id = "godot:scene:uid:v1:testscene";
        let encoded = encode_uri_segment(scene_id);
        assert_eq!(
            parse_scene_summary_uri(&format!(
                "{SCENE_SUMMARY_PREFIX}{encoded}{SCENE_SUMMARY_SUFFIX}"
            )),
            Ok(scene_id.to_owned())
        );
        assert!(parse_scene_summary_uri("godot://scene/..%2Fsecret/summary").is_err());
        assert!(parse_scene_summary_uri("godot://scene/godot%3ascene%3auid/summary").is_err());
        assert!(parse_scene_summary_uri("file:///tmp/project.godot").is_err());
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
        let server = canonical_ready_server();
        let result = server.godot_get_selected_nodes(Parameters(LiveGuardInput::default()));
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

    #[path = "success_contract_tests.rs"]
    mod success_contract_tests;
}
