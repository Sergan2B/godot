use std::collections::{BTreeMap, BTreeSet};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::protocol::{BridgeError, Session};
use crate::resource::{ResourceRef, RpcContext};

const MAX_DOCUMENTS: usize = 250_000;
const MAX_SYMBOLS: usize = 2_000_000;
const MAX_RELATIONS: usize = 4_000_000;
const MAX_DIAGNOSTICS: usize = 2_000_000;
const MAX_SYMBOLS_PER_DOCUMENT: usize = 65_536;
const MAX_RELATIONS_PER_DOCUMENT: usize = 262_144;
const MAX_DIAGNOSTICS_PER_DOCUMENT: usize = 4_096;
const MAX_ADAPTER_STATUSES: usize = 16;
const MAX_PATH_BYTES: usize = 1_024;
const MAX_NAME_BYTES: usize = 1_024;
const MAX_SIGNATURE_BYTES: usize = 4_096;
const MAX_DIAGNOSTIC_MESSAGE_BYTES: usize = 2_048;
const MAX_SNAPSHOT_CHUNK_BYTES: usize = 256 * 1_024;
const MAX_DELTA_BATCH_BYTES: usize = 512 * 1_024;
const MAX_SAFE_REVISION: u64 = 9_007_199_254_740_991;
const SCRIPT_SNAPSHOT_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const SCRIPT_SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptRevisionVector {
    pub editor_session_id: String,
    pub event_seq: u64,
    pub project_revision: u64,
    pub operation_seq: u64,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub script_graph_revision: u64,
    pub scene_revisions: BTreeMap<String, u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptLanguage {
    Gdscript,
    Csharp,
}

impl ScriptLanguage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Gdscript => "gdscript",
            Self::Csharp => "csharp",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum ScriptAdapterProfile {
    #[serde(rename = "gdscript_parser_analyzer_v1")]
    GdscriptParserAnalyzerV1,
    #[serde(rename = "csharp_discovery_only_v1")]
    CsharpDiscoveryOnlyV1,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptCompleteness {
    Complete,
    Partial,
    Invalid,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRange {
    pub path: String,
    pub content_sha256: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptDocument {
    pub script_ref: ResourceRef,
    pub path: String,
    pub language: ScriptLanguage,
    pub content_sha256: String,
    pub adapter_profile: ScriptAdapterProfile,
    pub completeness: ScriptCompleteness,
    pub resource_revision: u64,
    pub script_graph_revision: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptSymbolKind {
    Script,
    Class,
    Method,
    Function,
    Property,
    Constant,
    Enum,
    EnumMember,
    Signal,
    Parameter,
    Local,
    Lambda,
}

impl ScriptSymbolKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Script => "script",
            Self::Class => "class",
            Self::Method => "method",
            Self::Function => "function",
            Self::Property => "property",
            Self::Constant => "constant",
            Self::Enum => "enum",
            Self::EnumMember => "enum_member",
            Self::Signal => "signal",
            Self::Parameter => "parameter",
            Self::Local => "local",
            Self::Lambda => "lambda",
        }
    }

    fn requires_content_scope(self) -> bool {
        matches!(self, Self::Parameter | Self::Local | Self::Lambda)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptIdentityScope {
    Persistent,
    ContentRevision,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptTypeState {
    Explicit,
    Inferred,
    Dynamic,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptVisibility {
    Public,
    Protected,
    Private,
    Internal,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptModifier {
    Abstract,
    Const,
    Exported,
    Override,
    Static,
    Tool,
    Virtual,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptSymbol {
    pub symbol_id: String,
    pub script_ref: ResourceRef,
    pub language: ScriptLanguage,
    pub kind: ScriptSymbolKind,
    pub name: Option<String>,
    pub qualified_key: String,
    pub owner_symbol_id: Option<String>,
    pub identity_scope: ScriptIdentityScope,
    pub signature: Option<String>,
    pub type_name: Option<String>,
    pub type_state: ScriptTypeState,
    pub visibility: ScriptVisibility,
    pub modifiers: Vec<ScriptModifier>,
    pub declaration_range: SourceRange,
    pub documentation_present: bool,
    pub script_graph_revision: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScriptRelationEndpoint {
    Symbol { symbol_id: String },
    Resource { resource_ref: ResourceRef },
    SceneNode { node_id: String },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptRelationPredicate {
    Contains,
    DeclaresSymbol,
    Inherits,
    Overrides,
    ReferencesSymbol,
    Calls,
    Preloads,
    Loads,
    AttachesScript,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptConfidence {
    Exact,
    Dynamic,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptRelationAuthority {
    GdscriptParserAnalyzer,
    ResourceGraph,
    SceneState,
    CsharpAdapter,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptRelation {
    pub source: ScriptRelationEndpoint,
    pub predicate: ScriptRelationPredicate,
    pub target: Option<ScriptRelationEndpoint>,
    pub confidence: ScriptConfidence,
    pub evidence_range: Option<SourceRange>,
    pub detail: Option<String>,
    pub authority: ScriptRelationAuthority,
    pub script_graph_revision: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptDiagnosticSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

impl ScriptDiagnosticSeverity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Hint => "hint",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptDiagnosticAuthority {
    GdscriptParser,
    GdscriptAnalyzer,
    CsharpAdapter,
    ScriptAdapter,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptDiagnostic {
    pub diagnostic_id: String,
    pub script_ref: ResourceRef,
    pub language: ScriptLanguage,
    pub content_sha256: String,
    pub code: String,
    pub severity: ScriptDiagnosticSeverity,
    pub safe_message: String,
    pub range: Option<SourceRange>,
    pub authority: ScriptDiagnosticAuthority,
    pub script_graph_revision: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct ScriptDiagnosticIdentity<'a> {
    pub script_resource_id: &'a str,
    pub language: ScriptLanguage,
    pub content_sha256: &'a str,
    pub severity: ScriptDiagnosticSeverity,
    pub code: &'a str,
    pub start_byte: u64,
    pub end_byte: u64,
    pub safe_message: &'a str,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptAdapterAvailability {
    Available,
    DiscoveryOnly,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageAdapterStatus {
    pub language: ScriptLanguage,
    pub availability: ScriptAdapterAvailability,
    pub profile: Option<ScriptAdapterProfile>,
    pub version: Option<String>,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptSnapshotPayload {
    pub documents: Vec<ScriptDocument>,
    pub symbols: Vec<ScriptSymbol>,
    pub relations: Vec<ScriptRelation>,
    pub diagnostics: Vec<ScriptDiagnostic>,
    pub adapter_statuses: Vec<LanguageAdapterStatus>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptDocumentBundle {
    pub document: ScriptDocument,
    pub symbols: Vec<ScriptSymbol>,
    pub relations: Vec<ScriptRelation>,
    pub diagnostics: Vec<ScriptDiagnostic>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptSnapshotLimits {
    pub script_documents: usize,
    pub script_symbols: usize,
    pub script_relations: usize,
    pub script_diagnostics: usize,
    pub symbols_per_document: usize,
    pub relations_per_document: usize,
    pub diagnostics_per_document: usize,
    pub script_path_bytes: usize,
    pub script_name_bytes: usize,
    pub script_signature_bytes: usize,
    pub diagnostic_message_bytes: usize,
    pub snapshot_chunk_bytes: usize,
    pub snapshot_window_bytes: usize,
    pub snapshot_timeout_ms: u64,
    pub adapter_status_count: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptSnapshotAccepted {
    pub snapshot_id: String,
    pub domain: String,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub script_graph_revision: u64,
    pub revisions: ScriptRevisionVector,
    pub limits_applied: ScriptSnapshotLimits,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptSnapshotBeginParams {
    pub snapshot_id: String,
    pub domain: String,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub script_graph_revision: u64,
    pub revisions: ScriptRevisionVector,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ScriptSnapshotBeginMessage {
    protocol_version: String,
    kind: String,
    method: String,
    params: ScriptSnapshotBeginParams,
    context: RpcContext,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptSnapshotChunk {
    pub protocol_version: String,
    pub kind: String,
    pub snapshot_id: String,
    pub domain: String,
    pub chunk_index: usize,
    pub payload: ScriptSnapshotPayload,
    pub payload_json: String,
    pub checksum: String,
    pub context: RpcContext,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptSnapshotEndParams {
    pub snapshot_id: String,
    pub domain: String,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub script_graph_revision: u64,
    pub chunk_count: usize,
    pub document_count: usize,
    pub symbol_count: usize,
    pub relation_count: usize,
    pub diagnostic_count: usize,
    pub adapter_status_count: usize,
    pub checksum: String,
    pub revisions: ScriptRevisionVector,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ScriptSnapshotEndMessage {
    protocol_version: String,
    kind: String,
    method: String,
    params: ScriptSnapshotEndParams,
    context: RpcContext,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScriptSnapshotTransfer {
    pub accepted: ScriptSnapshotAccepted,
    pub end: ScriptSnapshotEndParams,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScriptSnapshot {
    pub accepted: ScriptSnapshotAccepted,
    pub begin: ScriptSnapshotBeginParams,
    pub payload: ScriptSnapshotPayload,
    pub end: ScriptSnapshotEndParams,
}

pub trait ScriptSnapshotSink {
    fn begin(
        &mut self,
        accepted: &ScriptSnapshotAccepted,
        begin: &ScriptSnapshotBeginParams,
    ) -> Result<(), BridgeError>;

    fn chunk(&mut self, chunk: &ScriptSnapshotChunk) -> Result<(), BridgeError>;

    fn end(&mut self, end: &ScriptSnapshotEndParams) -> Result<(), BridgeError>;
}

#[derive(Default)]
struct CollectingSink {
    accepted: Option<ScriptSnapshotAccepted>,
    begin: Option<ScriptSnapshotBeginParams>,
    documents: Vec<ScriptDocument>,
    symbols: Vec<ScriptSymbol>,
    relations: Vec<ScriptRelation>,
    diagnostics: Vec<ScriptDiagnostic>,
    adapter_statuses: Vec<LanguageAdapterStatus>,
    end: Option<ScriptSnapshotEndParams>,
}

impl ScriptSnapshotSink for CollectingSink {
    fn begin(
        &mut self,
        accepted: &ScriptSnapshotAccepted,
        begin: &ScriptSnapshotBeginParams,
    ) -> Result<(), BridgeError> {
        self.accepted = Some(accepted.clone());
        self.begin = Some(begin.clone());
        Ok(())
    }

    fn chunk(&mut self, chunk: &ScriptSnapshotChunk) -> Result<(), BridgeError> {
        self.documents.extend(chunk.payload.documents.clone());
        self.symbols.extend(chunk.payload.symbols.clone());
        self.relations.extend(chunk.payload.relations.clone());
        self.diagnostics.extend(chunk.payload.diagnostics.clone());
        self.adapter_statuses
            .extend(chunk.payload.adapter_statuses.clone());
        Ok(())
    }

    fn end(&mut self, end: &ScriptSnapshotEndParams) -> Result<(), BridgeError> {
        self.end = Some(end.clone());
        Ok(())
    }
}

impl CollectingSink {
    fn finish(self) -> Result<ScriptSnapshot, BridgeError> {
        let snapshot = ScriptSnapshot {
            accepted: self.accepted.ok_or_else(|| {
                BridgeError::Invalid("script snapshot acceptance is missing".to_owned())
            })?,
            begin: self.begin.ok_or_else(|| {
                BridgeError::Invalid("script snapshot begin is missing".to_owned())
            })?,
            payload: ScriptSnapshotPayload {
                documents: self.documents,
                symbols: self.symbols,
                relations: self.relations,
                diagnostics: self.diagnostics,
                adapter_statuses: self.adapter_statuses,
            },
            end: self
                .end
                .ok_or_else(|| BridgeError::Invalid("script snapshot end is missing".to_owned()))?,
        };
        normalize_script_snapshot(&snapshot)?;
        Ok(snapshot)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScriptDeltaOperation {
    UpsertDocument {
        value: Box<ScriptDocumentBundle>,
    },
    RemoveDocument {
        script_ref: ResourceRef,
        path: String,
    },
    AdapterStatus {
        value: LanguageAdapterStatus,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptDeltaBatch {
    pub batch_id: String,
    pub previous_script_graph_revision: u64,
    pub script_graph_revision: u64,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub project_revision: u64,
    pub operations: Vec<ScriptDeltaOperation>,
    pub source_complete: bool,
    pub checksum: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScriptDeltaPoll {
    Current {
        current_script_graph_revision: u64,
    },
    Batch {
        current_script_graph_revision: u64,
        batch: ScriptDeltaBatch,
    },
    Gap {
        requested_after_script_graph_revision: u64,
        oldest_available_script_graph_revision: u64,
        current_script_graph_revision: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NormalizedScriptGraph {
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub script_graph_revision: u64,
    pub documents: Vec<ScriptDocument>,
    pub symbols: Vec<ScriptSymbol>,
    pub relations: Vec<ScriptRelation>,
    pub diagnostics: Vec<ScriptDiagnostic>,
    pub adapter_statuses: Vec<LanguageAdapterStatus>,
    pub semantic_digest: String,
}

fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn canonical_json(value: &Value, output: &mut String) -> Result<(), BridgeError> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            output.push_str(&serde_json::to_string(value)?);
        }
        Value::Array(values) => {
            output.push('[');
            for (index, entry) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                canonical_json(entry, output)?;
            }
            output.push(']');
        }
        Value::Object(values) => {
            output.push('{');
            let mut keys: Vec<_> = values.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key)?);
                output.push(':');
                canonical_json(&values[key], output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

fn canonical_serialized<T: Serialize>(value: &T) -> Result<String, BridgeError> {
    let mut output = String::new();
    canonical_json(&serde_json::to_value(value)?, &mut output)?;
    Ok(output)
}

fn domain_digest(domain: &str, parts: &[&str]) -> String {
    let mut input = Vec::new();
    input.extend_from_slice(domain.as_bytes());
    input.push(0);
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            input.push(0);
        }
        input.extend_from_slice(part.as_bytes());
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(input))
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|suffix| {
        suffix.len() == 64
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn valid_uid(value: &str) -> bool {
    value
        .strip_prefix("uid://")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.len() <= 122)
        && value
            .bytes()
            .skip(6)
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_resource_path(value: &str) -> Result<(), BridgeError> {
    if !value.starts_with("res://")
        || value.len() > MAX_PATH_BYTES
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'\\' | b'?' | b'#' | b'\0'))
        || value[6..].split('/').any(|component| {
            component.is_empty() || matches!(component, "." | "..") || component.contains(':')
        })
    {
        return Err(BridgeError::Invalid(
            "script resource path is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_resource_ref(value: &ResourceRef) -> Result<(), BridgeError> {
    match value {
        ResourceRef::Uid(reference) if valid_uid(&reference.uid) => Ok(()),
        ResourceRef::Path(reference) if reference.uid_missing => {
            validate_resource_path(&reference.path)
        }
        _ => Err(BridgeError::Invalid(
            "script resource reference is invalid".to_owned(),
        )),
    }
}

fn resource_ref_key(value: &ResourceRef) -> Result<String, BridgeError> {
    validate_resource_ref(value)?;
    canonical_serialized(value)
}

pub fn canonical_script_resource_id(
    script_ref: &ResourceRef,
    path: &str,
    content_sha256: &str,
) -> Result<String, BridgeError> {
    validate_resource_ref(script_ref)?;
    validate_resource_path(path)?;
    if !valid_digest(content_sha256) {
        return Err(BridgeError::Invalid(
            "script content digest is invalid".to_owned(),
        ));
    }
    match script_ref {
        ResourceRef::Uid(reference) => Ok(format!(
            "godot:resource:uid:v1:{}",
            domain_digest("godot-codex/resource-entity/uid/v1", &[&reference.uid])
        )),
        ResourceRef::Path(reference) if reference.path == path => Ok(format!(
            "godot:resource:path-content:v1:{}",
            domain_digest(
                "godot-codex/resource-entity/path-content/v1",
                &[path, content_sha256]
            )
        )),
        ResourceRef::Path(_) => Err(BridgeError::Invalid(
            "path-only script identity differs from its document path".to_owned(),
        )),
    }
}

#[must_use]
pub fn canonical_named_symbol_id(
    script_resource_id: &str,
    language: ScriptLanguage,
    kind: ScriptSymbolKind,
    qualified_key: &str,
) -> String {
    format!(
        "godot:script-symbol:named:v1:{}",
        domain_digest(
            "godot-codex/script-symbol/named/v1",
            &[
                script_resource_id,
                language.as_str(),
                kind.as_str(),
                qualified_key,
            ],
        )
    )
}

#[must_use]
pub fn canonical_content_symbol_id(
    script_resource_id: &str,
    language: ScriptLanguage,
    content_sha256: &str,
    owner_qualified_key: &str,
    kind: ScriptSymbolKind,
    start_byte: u64,
    end_byte: u64,
) -> String {
    let start = start_byte.to_string();
    let end = end_byte.to_string();
    format!(
        "godot:script-symbol:content-revision:v1:{}",
        domain_digest(
            "godot-codex/script-symbol/content-revision/v1",
            &[
                script_resource_id,
                language.as_str(),
                content_sha256,
                owner_qualified_key,
                kind.as_str(),
                &start,
                &end,
            ],
        )
    )
}

#[must_use]
pub fn canonical_diagnostic_id(identity: ScriptDiagnosticIdentity<'_>) -> String {
    let start = identity.start_byte.to_string();
    let end = identity.end_byte.to_string();
    format!(
        "godot:script-diagnostic:v1:{}",
        domain_digest(
            "godot-codex/script-diagnostic/content-revision/v1",
            &[
                identity.script_resource_id,
                identity.language.as_str(),
                identity.content_sha256,
                identity.severity.as_str(),
                identity.code,
                &start,
                &end,
                identity.safe_message,
            ],
        )
    )
}

fn valid_opaque_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == 43
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    })
}

fn valid_symbol_id(value: &str) -> bool {
    valid_opaque_id(value, "godot:script-symbol:named:v1:")
        || valid_opaque_id(value, "godot:script-symbol:content-revision:v1:")
}

fn validate_source_range(value: &SourceRange) -> Result<(), BridgeError> {
    validate_resource_path(&value.path)?;
    let position_ordered = value.end_line > value.start_line
        || (value.end_line == value.start_line && value.end_column >= value.start_column);
    if !valid_digest(&value.content_sha256)
        || value.start_byte > value.end_byte
        || value.end_byte > MAX_SAFE_REVISION
        || value.start_line == 0
        || value.start_column == 0
        || value.end_line == 0
        || value.end_column == 0
        || value.start_line > i32::MAX as u32
        || value.end_line > i32::MAX as u32
        || value.start_column > i32::MAX as u32
        || value.end_column > i32::MAX as u32
        || !position_ordered
    {
        return Err(BridgeError::Invalid(
            "script source range is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn profile_matches_language(profile: ScriptAdapterProfile, language: ScriptLanguage) -> bool {
    matches!(
        (profile, language),
        (
            ScriptAdapterProfile::GdscriptParserAnalyzerV1,
            ScriptLanguage::Gdscript
        ) | (
            ScriptAdapterProfile::CsharpDiscoveryOnlyV1,
            ScriptLanguage::Csharp
        )
    )
}

fn validate_document(
    document: &ScriptDocument,
    expected_resource_revision: u64,
    expected_script_graph_revision: u64,
) -> Result<(), BridgeError> {
    validate_resource_ref(&document.script_ref)?;
    validate_resource_path(&document.path)?;
    canonical_script_resource_id(
        &document.script_ref,
        &document.path,
        &document.content_sha256,
    )?;
    let extension_matches = match document.language {
        ScriptLanguage::Gdscript => document.path.ends_with(".gd"),
        ScriptLanguage::Csharp => document.path.ends_with(".cs"),
    };
    if !profile_matches_language(document.adapter_profile, document.language)
        || !extension_matches
        || document.resource_revision != expected_resource_revision
        || document.script_graph_revision != expected_script_graph_revision
        || expected_resource_revision > MAX_SAFE_REVISION
        || expected_script_graph_revision > MAX_SAFE_REVISION
    {
        return Err(BridgeError::Invalid(
            "script document is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_symbol(
    symbol: &ScriptSymbol,
    expected_script_graph_revision: u64,
) -> Result<(), BridgeError> {
    validate_resource_ref(&symbol.script_ref)?;
    validate_source_range(&symbol.declaration_range)?;
    let script_resource_id = canonical_script_resource_id(
        &symbol.script_ref,
        &symbol.declaration_range.path,
        &symbol.declaration_range.content_sha256,
    )?;
    let has_valid_name = symbol.name.as_ref().is_some_and(|name| {
        !name.is_empty()
            && name.len() <= MAX_NAME_BYTES
            && name.chars().count() <= 512
            && !name.chars().any(char::is_control)
    });
    if (!matches!(
        symbol.kind,
        ScriptSymbolKind::Script | ScriptSymbolKind::Lambda
    ) && !has_valid_name)
        || symbol.name.as_ref().is_some_and(|_| !has_valid_name)
        || symbol.qualified_key.is_empty()
        || symbol.qualified_key.len() > 2_048
        || symbol.qualified_key.chars().any(char::is_control)
        || symbol.declaration_range.start_byte == symbol.declaration_range.end_byte
        || symbol.signature.as_ref().is_some_and(|value| {
            value.len() > MAX_SIGNATURE_BYTES || value.chars().any(char::is_control)
        })
        || symbol.type_name.as_ref().is_some_and(|value| {
            value.is_empty() || value.len() > MAX_NAME_BYTES || value.chars().any(char::is_control)
        })
        || symbol.script_graph_revision != expected_script_graph_revision
        || expected_script_graph_revision > MAX_SAFE_REVISION
        || symbol.owner_symbol_id.as_deref() == Some(symbol.symbol_id.as_str())
        || symbol
            .owner_symbol_id
            .as_ref()
            .is_some_and(|value| !valid_symbol_id(value))
        || symbol.modifiers.len() > 16
        || symbol.modifiers.iter().collect::<BTreeSet<_>>().len() != symbol.modifiers.len()
        || (symbol.kind.requires_content_scope()
            && symbol.identity_scope != ScriptIdentityScope::ContentRevision)
    {
        return Err(BridgeError::Invalid("script symbol is invalid".to_owned()));
    }
    let expected_id = match symbol.identity_scope {
        ScriptIdentityScope::Persistent => canonical_named_symbol_id(
            &script_resource_id,
            symbol.language,
            symbol.kind,
            &symbol.qualified_key,
        ),
        ScriptIdentityScope::ContentRevision => {
            let owner_key = symbol
                .qualified_key
                .rsplit_once('/')
                .map(|(owner, _)| owner)
                .filter(|owner| !owner.is_empty())
                .ok_or_else(|| {
                    BridgeError::Invalid("content-scoped symbol has no owner coordinate".to_owned())
                })?;
            canonical_content_symbol_id(
                &script_resource_id,
                symbol.language,
                &symbol.declaration_range.content_sha256,
                owner_key,
                symbol.kind,
                symbol.declaration_range.start_byte,
                symbol.declaration_range.end_byte,
            )
        }
    };
    if symbol.symbol_id != expected_id {
        return Err(BridgeError::Invalid(
            "script symbol identity is not canonical".to_owned(),
        ));
    }
    Ok(())
}

fn validate_endpoint(endpoint: &ScriptRelationEndpoint) -> Result<(), BridgeError> {
    match endpoint {
        ScriptRelationEndpoint::Symbol { symbol_id } if valid_symbol_id(symbol_id) => Ok(()),
        ScriptRelationEndpoint::Resource { resource_ref } => validate_resource_ref(resource_ref),
        ScriptRelationEndpoint::SceneNode { node_id }
            if node_id.strip_prefix("node:").is_some_and(|suffix| {
                suffix.len() == 32
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            }) =>
        {
            Ok(())
        }
        _ => Err(BridgeError::Invalid(
            "script relation endpoint is invalid".to_owned(),
        )),
    }
}

fn validate_relation(
    relation: &ScriptRelation,
    expected_script_graph_revision: u64,
) -> Result<(), BridgeError> {
    validate_endpoint(&relation.source)?;
    if let Some(target) = &relation.target {
        validate_endpoint(target)?;
    }
    if let Some(range) = &relation.evidence_range {
        validate_source_range(range)?;
    }
    let detail_valid = relation.detail.as_ref().is_none_or(|detail| {
        !detail.is_empty()
            && detail.len() <= 128
            && detail.bytes().enumerate().all(|(index, byte)| {
                if index == 0 {
                    byte.is_ascii_lowercase()
                } else {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                }
            })
    });
    let target_shape_valid = match relation.confidence {
        ScriptConfidence::Exact => relation.target.is_some(),
        ScriptConfidence::Dynamic => {
            relation.target.is_none()
                && relation.detail.is_some()
                && matches!(
                    relation.predicate,
                    ScriptRelationPredicate::ReferencesSymbol
                        | ScriptRelationPredicate::Calls
                        | ScriptRelationPredicate::Loads
                )
        }
    };
    let exact_target_kind_valid = matches!(
        (relation.confidence, relation.predicate, &relation.target),
        (
            ScriptConfidence::Exact,
            ScriptRelationPredicate::Preloads | ScriptRelationPredicate::Loads,
            Some(ScriptRelationEndpoint::Resource { .. }),
        ) | (
            ScriptConfidence::Exact,
            ScriptRelationPredicate::Contains
                | ScriptRelationPredicate::DeclaresSymbol
                | ScriptRelationPredicate::Inherits
                | ScriptRelationPredicate::Overrides
                | ScriptRelationPredicate::ReferencesSymbol
                | ScriptRelationPredicate::Calls,
            Some(ScriptRelationEndpoint::Symbol { .. } | ScriptRelationEndpoint::Resource { .. }),
        ) | (
            ScriptConfidence::Exact,
            ScriptRelationPredicate::AttachesScript,
            Some(ScriptRelationEndpoint::Symbol { .. } | ScriptRelationEndpoint::Resource { .. }),
        ) | (ScriptConfidence::Dynamic, _, None)
    );
    if relation.script_graph_revision != expected_script_graph_revision
        || expected_script_graph_revision > MAX_SAFE_REVISION
        || !detail_valid
        || !target_shape_valid
        || !exact_target_kind_valid
    {
        return Err(BridgeError::Invalid(
            "script relation is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn normalized_safe_message(value: &str) -> String {
    let mut result = String::new();
    let mut pending_space = false;
    for character in value.chars() {
        if matches!(character, ' ' | '\t' | '\r' | '\n') {
            pending_space = !result.is_empty();
        } else if !character.is_control() {
            if pending_space {
                result.push(' ');
                pending_space = false;
            }
            result.push(character);
        }
    }
    result.trim().to_owned()
}

fn contains_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.contains("file://")
        || value.contains("/Users/")
        || value.contains("/home/")
        || value.contains("/private/")
        || value.contains("\\\\")
        || bytes.windows(3).enumerate().any(|(index, window)| {
            window[0].is_ascii_alphabetic()
                && window[1] == b':'
                && matches!(window[2], b'/' | b'\\')
                && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric())
        })
}

fn validate_diagnostic(
    diagnostic: &ScriptDiagnostic,
    expected_script_graph_revision: u64,
) -> Result<(), BridgeError> {
    validate_resource_ref(&diagnostic.script_ref)?;
    if let Some(range) = &diagnostic.range {
        validate_source_range(range)?;
        if range.content_sha256 != diagnostic.content_sha256 {
            return Err(BridgeError::Invalid(
                "script diagnostic range has a different content digest".to_owned(),
            ));
        }
    }
    let (path, start_byte, end_byte) = diagnostic.range.as_ref().map_or_else(
        || match &diagnostic.script_ref {
            ResourceRef::Uid(_) => ("res://unknown.gd", 0, 0),
            ResourceRef::Path(reference) => (reference.path.as_str(), 0, 0),
        },
        |range| (range.path.as_str(), range.start_byte, range.end_byte),
    );
    let script_resource_id =
        canonical_script_resource_id(&diagnostic.script_ref, path, &diagnostic.content_sha256)?;
    let code_valid = !diagnostic.code.is_empty()
        && diagnostic.code.len() <= 128
        && diagnostic.code.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_uppercase()
            } else {
                byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
            }
        });
    let message_safe = !diagnostic.safe_message.is_empty()
        && diagnostic.safe_message.len() <= MAX_DIAGNOSTIC_MESSAGE_BYTES
        && normalized_safe_message(&diagnostic.safe_message) == diagnostic.safe_message
        && !contains_absolute_path(&diagnostic.safe_message);
    let expected_id = canonical_diagnostic_id(ScriptDiagnosticIdentity {
        script_resource_id: &script_resource_id,
        language: diagnostic.language,
        content_sha256: &diagnostic.content_sha256,
        severity: diagnostic.severity,
        code: &diagnostic.code,
        start_byte,
        end_byte,
        safe_message: &diagnostic.safe_message,
    });
    if !valid_digest(&diagnostic.content_sha256)
        || !code_valid
        || !message_safe
        || diagnostic.script_graph_revision != expected_script_graph_revision
        || expected_script_graph_revision > MAX_SAFE_REVISION
        || diagnostic.diagnostic_id != expected_id
    {
        return Err(BridgeError::Invalid(
            "script diagnostic is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_adapter_status(status: &LanguageAdapterStatus) -> Result<(), BridgeError> {
    let version_valid = status.version.as_ref().is_none_or(|version| {
        !version.is_empty() && version.len() <= 64 && !version.chars().any(char::is_control)
    });
    let diagnostic_valid = status.diagnostic.as_ref().is_none_or(|diagnostic| {
        !diagnostic.is_empty()
            && diagnostic.len() <= 512
            && normalized_safe_message(diagnostic) == *diagnostic
    });
    let state_valid = match status.availability {
        ScriptAdapterAvailability::Available => {
            status
                .profile
                .is_some_and(|profile| profile_matches_language(profile, status.language))
                && status.version.is_some()
                && status.diagnostic.is_none()
        }
        ScriptAdapterAvailability::DiscoveryOnly => {
            status.language == ScriptLanguage::Csharp
                && status.profile == Some(ScriptAdapterProfile::CsharpDiscoveryOnlyV1)
                && status.version.is_some()
                && status.diagnostic.is_none()
        }
        ScriptAdapterAvailability::Unavailable => {
            status.profile.is_none() && status.version.is_none() && status.diagnostic.is_some()
        }
    };
    if !version_valid || !diagnostic_valid || !state_valid {
        return Err(BridgeError::Invalid(
            "script adapter status is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_snapshot_limits(limits: &ScriptSnapshotLimits) -> Result<(), BridgeError> {
    if limits.script_documents != MAX_DOCUMENTS
        || limits.script_symbols != MAX_SYMBOLS
        || limits.script_relations != MAX_RELATIONS
        || limits.script_diagnostics != MAX_DIAGNOSTICS
        || limits.symbols_per_document != MAX_SYMBOLS_PER_DOCUMENT
        || limits.relations_per_document != MAX_RELATIONS_PER_DOCUMENT
        || limits.diagnostics_per_document != MAX_DIAGNOSTICS_PER_DOCUMENT
        || limits.script_path_bytes != MAX_PATH_BYTES
        || limits.script_name_bytes != MAX_NAME_BYTES
        || limits.script_signature_bytes != MAX_SIGNATURE_BYTES
        || limits.diagnostic_message_bytes != MAX_DIAGNOSTIC_MESSAGE_BYTES
        || limits.snapshot_chunk_bytes != MAX_SNAPSHOT_CHUNK_BYTES
        || limits.snapshot_window_bytes != 32 * 1_024 * 1_024
        || limits.snapshot_timeout_ms != 120_000
        || limits.adapter_status_count != MAX_ADAPTER_STATUSES
    {
        return Err(BridgeError::Invalid(
            "script snapshot limits are invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_payload(
    payload: &ScriptSnapshotPayload,
    resource_revision: u64,
    script_graph_revision: u64,
) -> Result<(), BridgeError> {
    if payload.documents.len() > MAX_DOCUMENTS
        || payload.symbols.len() > MAX_SYMBOLS
        || payload.relations.len() > MAX_RELATIONS
        || payload.diagnostics.len() > MAX_DIAGNOSTICS
        || payload.adapter_statuses.len() > MAX_ADAPTER_STATUSES
    {
        return Err(BridgeError::Invalid(
            "script snapshot payload count is invalid".to_owned(),
        ));
    }
    for document in &payload.documents {
        validate_document(document, resource_revision, script_graph_revision)?;
    }
    for symbol in &payload.symbols {
        validate_symbol(symbol, script_graph_revision)?;
    }
    for relation in &payload.relations {
        validate_relation(relation, script_graph_revision)?;
    }
    for diagnostic in &payload.diagnostics {
        validate_diagnostic(diagnostic, script_graph_revision)?;
    }
    for status in &payload.adapter_statuses {
        validate_adapter_status(status)?;
    }
    Ok(())
}

fn validate_snapshot_chunk(
    chunk: &ScriptSnapshotChunk,
    accepted: &ScriptSnapshotAccepted,
    expected_index: usize,
) -> Result<(), BridgeError> {
    let canonical_payload = canonical_serialized(&chunk.payload)?;
    if chunk.protocol_version != "1.4"
        || chunk.kind != "chunk"
        || chunk.domain != "script_graph"
        || chunk.snapshot_id != accepted.snapshot_id
        || chunk.chunk_index != expected_index
        || chunk.payload_json.len() > MAX_SNAPSHOT_CHUNK_BYTES
        || chunk.payload_json != canonical_payload
        || sha256_hex(chunk.payload_json.as_bytes()) != chunk.checksum
    {
        return Err(BridgeError::Invalid(
            "script snapshot chunk envelope is invalid".to_owned(),
        ));
    }
    let payload_from_json: ScriptSnapshotPayload = serde_json::from_str(&chunk.payload_json)
        .map_err(|error| {
            BridgeError::Invalid(format!("script snapshot payload_json is invalid: {error}"))
        })?;
    if payload_from_json != chunk.payload {
        return Err(BridgeError::Invalid(
            "script snapshot payload_json differs from payload".to_owned(),
        ));
    }
    validate_payload(
        &chunk.payload,
        accepted.resource_revision,
        accepted.script_graph_revision,
    )
}

pub(crate) async fn stream_script_snapshot<S: ScriptSnapshotSink>(
    session: &mut Session,
    sink: &mut S,
) -> Result<ScriptSnapshotTransfer, BridgeError> {
    require_script_graph(session)?;
    let response = session
        .request_with_deadline_and_timeout(
            "script.snapshot.get",
            json!({}),
            SCRIPT_SNAPSHOT_REQUEST_TIMEOUT,
            SCRIPT_SNAPSHOT_TIMEOUT,
        )
        .await?;
    let request_id = response
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::Invalid("script snapshot request ID is missing".to_owned()))?
        .to_owned();
    let result = receive_script_snapshot(session, sink, &response).await;
    if result.is_err() {
        let _ = session
            .send_cancel(&request_id, "script snapshot validation failed")
            .await;
    }
    result
}

async fn receive_script_snapshot<S: ScriptSnapshotSink>(
    session: &mut Session,
    sink: &mut S,
    response: &Value,
) -> Result<ScriptSnapshotTransfer, BridgeError> {
    let accepted: ScriptSnapshotAccepted = serde_json::from_value(
        response
            .get("result")
            .cloned()
            .ok_or_else(|| BridgeError::Invalid("script snapshot result is missing".to_owned()))?,
    )
    .map_err(|error| {
        BridgeError::Invalid(format!("script snapshot acceptance is invalid: {error}"))
    })?;
    if accepted.domain != "script_graph"
        || accepted.revisions.resource_revision != accepted.resource_revision
        || accepted.revisions.scene_graph_revision != accepted.scene_graph_revision
        || accepted.revisions.script_graph_revision != accepted.script_graph_revision
        || accepted.resource_revision > MAX_SAFE_REVISION
        || accepted.scene_graph_revision > MAX_SAFE_REVISION
        || accepted.script_graph_revision > MAX_SAFE_REVISION
        || !valid_prefixed_hex(&accepted.snapshot_id, "snapshot:", 32)
    {
        return Err(BridgeError::Invalid(
            "script snapshot acceptance is invalid".to_owned(),
        ));
    }
    validate_snapshot_limits(&accepted.limits_applied)?;

    let begin_message: ScriptSnapshotBeginMessage = serde_json::from_value(
        session
            .receive_non_sync_with_timeout(SCRIPT_SNAPSHOT_TIMEOUT)
            .await?,
    )
    .map_err(|error| BridgeError::Invalid(format!("script snapshot begin is invalid: {error}")))?;
    if begin_message.protocol_version != "1.4"
        || begin_message.kind != "notification"
        || begin_message.method != "snapshot.begin"
        || begin_message.params.snapshot_id != accepted.snapshot_id
        || begin_message.params.domain != "script_graph"
        || begin_message.params.resource_revision != accepted.resource_revision
        || begin_message.params.scene_graph_revision != accepted.scene_graph_revision
        || begin_message.params.script_graph_revision != accepted.script_graph_revision
        || begin_message.params.revisions != accepted.revisions
    {
        return Err(BridgeError::Invalid(
            "script snapshot begin is invalid".to_owned(),
        ));
    }
    sink.begin(&accepted, &begin_message.params)?;

    let mut expected_index = 0_usize;
    let mut checksum_input = String::new();
    let mut document_count = 0_usize;
    let mut symbol_count = 0_usize;
    let mut relation_count = 0_usize;
    let mut diagnostic_count = 0_usize;
    let mut adapter_status_count = 0_usize;
    let end = loop {
        let message = session
            .receive_non_sync_with_timeout(SCRIPT_SNAPSHOT_TIMEOUT)
            .await?;
        if message.get("kind").and_then(Value::as_str) == Some("chunk") {
            let chunk: ScriptSnapshotChunk = serde_json::from_value(message).map_err(|error| {
                BridgeError::Invalid(format!("script snapshot chunk is invalid: {error}"))
            })?;
            validate_snapshot_chunk(&chunk, &accepted, expected_index)?;
            document_count = checked_count(
                document_count,
                chunk.payload.documents.len(),
                MAX_DOCUMENTS,
                "script document",
            )?;
            symbol_count = checked_count(
                symbol_count,
                chunk.payload.symbols.len(),
                MAX_SYMBOLS,
                "script symbol",
            )?;
            relation_count = checked_count(
                relation_count,
                chunk.payload.relations.len(),
                MAX_RELATIONS,
                "script relation",
            )?;
            diagnostic_count = checked_count(
                diagnostic_count,
                chunk.payload.diagnostics.len(),
                MAX_DIAGNOSTICS,
                "script diagnostic",
            )?;
            adapter_status_count = checked_count(
                adapter_status_count,
                chunk.payload.adapter_statuses.len(),
                MAX_ADAPTER_STATUSES,
                "script adapter status",
            )?;
            checksum_input.push_str(&chunk.checksum);
            sink.chunk(&chunk)?;
            session
                .send_ack(&accepted.snapshot_id, Some("script_graph"), expected_index)
                .await?;
            expected_index += 1;
            continue;
        }
        let end_message: ScriptSnapshotEndMessage =
            serde_json::from_value(message).map_err(|error| {
                BridgeError::Invalid(format!("script snapshot end is invalid: {error}"))
            })?;
        break end_message;
    };
    if end.protocol_version != "1.4"
        || end.kind != "notification"
        || end.method != "snapshot.end"
        || end.params.snapshot_id != accepted.snapshot_id
        || end.params.domain != "script_graph"
        || end.params.resource_revision != accepted.resource_revision
        || end.params.scene_graph_revision != accepted.scene_graph_revision
        || end.params.script_graph_revision != accepted.script_graph_revision
        || end.params.revisions != accepted.revisions
        || end.params.chunk_count != expected_index
        || end.params.document_count != document_count
        || end.params.symbol_count != symbol_count
        || end.params.relation_count != relation_count
        || end.params.diagnostic_count != diagnostic_count
        || end.params.adapter_status_count != adapter_status_count
        || end.params.checksum != sha256_hex(checksum_input.as_bytes())
    {
        return Err(BridgeError::Invalid(
            "script snapshot end is invalid".to_owned(),
        ));
    }
    sink.end(&end.params)?;
    Ok(ScriptSnapshotTransfer {
        accepted,
        end: end.params,
    })
}

fn checked_count(
    current: usize,
    added: usize,
    maximum: usize,
    label: &str,
) -> Result<usize, BridgeError> {
    let total = current
        .checked_add(added)
        .ok_or_else(|| BridgeError::Invalid(format!("{label} count overflow")))?;
    if total > maximum {
        return Err(BridgeError::Invalid(format!(
            "{label} count exceeds the negotiated limit"
        )));
    }
    Ok(total)
}

pub(crate) async fn get_script_snapshot(
    session: &mut Session,
) -> Result<ScriptSnapshot, BridgeError> {
    let mut sink = CollectingSink::default();
    stream_script_snapshot(session, &mut sink).await?;
    sink.finish()
}

fn valid_prefixed_hex(value: &str, prefix: &str, hex_length: usize) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == hex_length
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn validate_document_bundle(
    bundle: &ScriptDocumentBundle,
    resource_revision: u64,
    script_graph_revision: u64,
) -> Result<(), BridgeError> {
    validate_document(&bundle.document, resource_revision, script_graph_revision)?;
    if bundle.symbols.len() > MAX_SYMBOLS_PER_DOCUMENT
        || bundle.relations.len() > MAX_RELATIONS_PER_DOCUMENT
        || bundle.diagnostics.len() > MAX_DIAGNOSTICS_PER_DOCUMENT
    {
        return Err(BridgeError::Invalid(
            "script document bundle count is invalid".to_owned(),
        ));
    }
    let document_key = resource_ref_key(&bundle.document.script_ref)?;
    for symbol in &bundle.symbols {
        validate_symbol(symbol, script_graph_revision)?;
        if resource_ref_key(&symbol.script_ref)? != document_key
            || symbol.language != bundle.document.language
            || symbol.declaration_range.path != bundle.document.path
            || symbol.declaration_range.content_sha256 != bundle.document.content_sha256
        {
            return Err(BridgeError::Invalid(
                "script bundle symbol differs from its document".to_owned(),
            ));
        }
    }
    for relation in &bundle.relations {
        validate_relation(relation, script_graph_revision)?;
        if relation.evidence_range.as_ref().is_some_and(|range| {
            range.path != bundle.document.path
                || range.content_sha256 != bundle.document.content_sha256
        }) {
            return Err(BridgeError::Invalid(
                "script bundle relation differs from its document".to_owned(),
            ));
        }
    }
    for diagnostic in &bundle.diagnostics {
        validate_diagnostic(diagnostic, script_graph_revision)?;
        if resource_ref_key(&diagnostic.script_ref)? != document_key
            || diagnostic.language != bundle.document.language
            || diagnostic.content_sha256 != bundle.document.content_sha256
            || diagnostic
                .range
                .as_ref()
                .is_some_and(|range| range.path != bundle.document.path)
        {
            return Err(BridgeError::Invalid(
                "script bundle diagnostic differs from its document".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_delta_batch(
    batch: &ScriptDeltaBatch,
    requested_after: u64,
    current_script_graph_revision: u64,
) -> Result<(), BridgeError> {
    if !valid_prefixed_hex(&batch.batch_id, "script-batch:", 32)
        || batch.previous_script_graph_revision != requested_after
        || requested_after
            .checked_add(1)
            .is_none_or(|next| batch.script_graph_revision != next)
        || batch.script_graph_revision > current_script_graph_revision
        || batch.resource_revision > MAX_SAFE_REVISION
        || batch.scene_graph_revision > MAX_SAFE_REVISION
        || batch.project_revision > MAX_SAFE_REVISION
        || current_script_graph_revision > MAX_SAFE_REVISION
        || batch.operations.is_empty()
        || batch.operations.len() > MAX_DOCUMENTS
        || !batch.source_complete
    {
        return Err(BridgeError::Invalid(
            "script delta continuity is invalid".to_owned(),
        ));
    }
    let operations_json = canonical_serialized(&batch.operations)?;
    if sha256_hex(operations_json.as_bytes()) != batch.checksum
        || serde_json::to_vec(batch)?.len() > MAX_DELTA_BATCH_BYTES
    {
        return Err(BridgeError::Invalid(
            "script delta checksum or size is invalid".to_owned(),
        ));
    }
    let mut document_operations = BTreeSet::new();
    let mut adapter_languages = BTreeSet::new();
    for operation in &batch.operations {
        match operation {
            ScriptDeltaOperation::UpsertDocument { value } => {
                validate_document_bundle(
                    value,
                    batch.resource_revision,
                    batch.script_graph_revision,
                )?;
                if !document_operations.insert(resource_ref_key(&value.document.script_ref)?) {
                    return Err(BridgeError::Invalid(
                        "script delta repeats a document".to_owned(),
                    ));
                }
            }
            ScriptDeltaOperation::RemoveDocument { script_ref, path } => {
                validate_resource_ref(script_ref)?;
                validate_resource_path(path)?;
                if let ResourceRef::Path(reference) = script_ref
                    && reference.path != *path
                {
                    return Err(BridgeError::Invalid(
                        "script removal path differs from its identity".to_owned(),
                    ));
                }
                if !document_operations.insert(resource_ref_key(script_ref)?) {
                    return Err(BridgeError::Invalid(
                        "script delta repeats a document".to_owned(),
                    ));
                }
            }
            ScriptDeltaOperation::AdapterStatus { value } => {
                validate_adapter_status(value)?;
                if !adapter_languages.insert(value.language) {
                    return Err(BridgeError::Invalid(
                        "script delta repeats an adapter status".to_owned(),
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(crate) async fn get_next_script_delta(
    session: &mut Session,
    after_script_graph_revision: u64,
) -> Result<ScriptDeltaPoll, BridgeError> {
    require_script_graph(session)?;
    if after_script_graph_revision > MAX_SAFE_REVISION {
        return Err(BridgeError::Invalid(
            "script delta revision is invalid".to_owned(),
        ));
    }
    let response = match session
        .request(
            "script.delta.get",
            json!({"after_script_graph_revision": after_script_graph_revision}),
        )
        .await
    {
        Ok(response) => response,
        Err(BridgeError::Rpc {
            code,
            retryable,
            data,
            ..
        }) if code == "script_journal_gap" && retryable => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct GapData {
                requested_after: u64,
                oldest_available: u64,
                current_script_graph_revision: u64,
            }
            let gap: GapData = serde_json::from_value(data)?;
            if gap.requested_after != after_script_graph_revision
                || gap.requested_after >= gap.oldest_available
                || gap.oldest_available > gap.current_script_graph_revision
                || gap.current_script_graph_revision > MAX_SAFE_REVISION
            {
                return Err(BridgeError::Invalid(
                    "script journal gap metadata is invalid".to_owned(),
                ));
            }
            return Ok(ScriptDeltaPoll::Gap {
                requested_after_script_graph_revision: gap.requested_after,
                oldest_available_script_graph_revision: gap.oldest_available,
                current_script_graph_revision: gap.current_script_graph_revision,
            });
        }
        Err(error) => return Err(error),
    };
    let result: ScriptDeltaPoll = serde_json::from_value(
        response
            .get("result")
            .cloned()
            .ok_or_else(|| BridgeError::Invalid("script delta result is missing".to_owned()))?,
    )?;
    match &result {
        ScriptDeltaPoll::Current {
            current_script_graph_revision,
        } if *current_script_graph_revision == after_script_graph_revision => {}
        ScriptDeltaPoll::Batch {
            current_script_graph_revision,
            batch,
        } => validate_delta_batch(
            batch,
            after_script_graph_revision,
            *current_script_graph_revision,
        )?,
        ScriptDeltaPoll::Gap { .. } => {
            return Err(BridgeError::Invalid(
                "script gap must use the RPC error envelope".to_owned(),
            ));
        }
        _ => {
            return Err(BridgeError::Invalid(
                "script delta result is inconsistent".to_owned(),
            ));
        }
    }
    Ok(result)
}

fn sort_canonically<T: Serialize>(values: &mut [T]) -> Result<(), BridgeError> {
    let mut error = None;
    values.sort_by_cached_key(|value| match canonical_serialized(value) {
        Ok(serialized) => serialized,
        Err(current) => {
            error = Some(current);
            String::new()
        }
    });
    error.map_or(Ok(()), Err)
}

fn endpoint_symbol_id(endpoint: &ScriptRelationEndpoint) -> Option<&str> {
    match endpoint {
        ScriptRelationEndpoint::Symbol { symbol_id } => Some(symbol_id),
        ScriptRelationEndpoint::Resource { .. } | ScriptRelationEndpoint::SceneNode { .. } => None,
    }
}

fn remove_transport_revisions(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for entry in values {
                remove_transport_revisions(entry);
            }
        }
        Value::Object(values) => {
            values.remove("resource_revision");
            values.remove("scene_graph_revision");
            values.remove("script_graph_revision");
            values.remove("project_revision");
            values.remove("operation_seq");
            values.remove("event_seq");
            for entry in values.values_mut() {
                remove_transport_revisions(entry);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn semantic_digest(
    documents: &[ScriptDocument],
    symbols: &[ScriptSymbol],
    relations: &[ScriptRelation],
    diagnostics: &[ScriptDiagnostic],
    adapter_statuses: &[LanguageAdapterStatus],
) -> Result<String, BridgeError> {
    let mut value = json!({
        "schema": "godot-codex/script-graph/semantic/v1",
        "documents": documents,
        "symbols": symbols,
        "relations": relations,
        "diagnostics": diagnostics,
        "adapter_statuses": adapter_statuses,
    });
    remove_transport_revisions(&mut value);
    let canonical = canonical_serialized(&value)?;
    let mut input = b"godot-codex/script-graph/semantic/v1\0".to_vec();
    input.extend_from_slice(canonical.as_bytes());
    Ok(format!("sha256:{}", sha256_hex(&input)))
}

pub fn normalize_script_snapshot(
    snapshot: &ScriptSnapshot,
) -> Result<NormalizedScriptGraph, BridgeError> {
    if snapshot.begin.snapshot_id != snapshot.accepted.snapshot_id
        || snapshot.end.snapshot_id != snapshot.accepted.snapshot_id
        || snapshot.begin.revisions != snapshot.accepted.revisions
        || snapshot.end.revisions != snapshot.accepted.revisions
        || snapshot.end.document_count != snapshot.payload.documents.len()
        || snapshot.end.symbol_count != snapshot.payload.symbols.len()
        || snapshot.end.relation_count != snapshot.payload.relations.len()
        || snapshot.end.diagnostic_count != snapshot.payload.diagnostics.len()
        || snapshot.end.adapter_status_count != snapshot.payload.adapter_statuses.len()
    {
        return Err(BridgeError::Invalid(
            "script snapshot aggregate is inconsistent".to_owned(),
        ));
    }
    normalize_script_graph(
        &snapshot.payload,
        snapshot.accepted.resource_revision,
        snapshot.accepted.scene_graph_revision,
        snapshot.accepted.script_graph_revision,
    )
}

pub fn normalize_script_graph(
    payload: &ScriptSnapshotPayload,
    resource_revision: u64,
    scene_graph_revision: u64,
    script_graph_revision: u64,
) -> Result<NormalizedScriptGraph, BridgeError> {
    if scene_graph_revision > MAX_SAFE_REVISION {
        return Err(BridgeError::Invalid(
            "script graph scene revision is invalid".to_owned(),
        ));
    }
    validate_payload(payload, resource_revision, script_graph_revision)?;
    let mut documents = payload.documents.clone();
    let mut symbols = payload.symbols.clone();
    let mut relations = payload.relations.clone();
    let mut diagnostics = payload.diagnostics.clone();
    let mut adapter_statuses = payload.adapter_statuses.clone();
    for symbol in &mut symbols {
        symbol.modifiers.sort_unstable();
    }
    sort_canonically(&mut documents)?;
    sort_canonically(&mut symbols)?;
    sort_canonically(&mut relations)?;
    sort_canonically(&mut diagnostics)?;
    sort_canonically(&mut adapter_statuses)?;

    let mut documents_by_ref = BTreeMap::new();
    let mut documents_by_source = BTreeMap::new();
    let mut document_paths = BTreeSet::new();
    for document in &documents {
        let key = resource_ref_key(&document.script_ref)?;
        if documents_by_ref.insert(key, document).is_some()
            || !document_paths.insert(document.path.as_str())
            || documents_by_source
                .insert(
                    (document.path.as_str(), document.content_sha256.as_str()),
                    document,
                )
                .is_some()
        {
            return Err(BridgeError::Invalid(
                "script graph contains a duplicate document".to_owned(),
            ));
        }
    }

    let mut symbols_by_id = BTreeMap::new();
    let mut symbols_per_document = BTreeMap::<String, usize>::new();
    for symbol in &symbols {
        let document_key = resource_ref_key(&symbol.script_ref)?;
        let document = documents_by_ref.get(&document_key).ok_or_else(|| {
            BridgeError::Invalid("script symbol has no current document".to_owned())
        })?;
        if symbol.language != document.language
            || symbol.declaration_range.path != document.path
            || symbol.declaration_range.content_sha256 != document.content_sha256
            || matches!(
                document.completeness,
                ScriptCompleteness::Invalid | ScriptCompleteness::Unavailable
            )
            || symbols_by_id
                .insert(symbol.symbol_id.as_str(), symbol)
                .is_some()
        {
            return Err(BridgeError::Invalid(
                "script symbol does not match its current document".to_owned(),
            ));
        }
        let count = symbols_per_document.entry(document_key).or_default();
        *count += 1;
        if *count > MAX_SYMBOLS_PER_DOCUMENT {
            return Err(BridgeError::Invalid(
                "script document has too many symbols".to_owned(),
            ));
        }
    }
    for symbol in &symbols {
        if let Some(owner_id) = &symbol.owner_symbol_id {
            let owner = symbols_by_id
                .get(owner_id.as_str())
                .ok_or_else(|| BridgeError::Invalid("script symbol owner is missing".to_owned()))?;
            if resource_ref_key(&owner.script_ref)? != resource_ref_key(&symbol.script_ref)? {
                return Err(BridgeError::Invalid(
                    "script symbol owner belongs to another document".to_owned(),
                ));
            }
        }
    }

    let mut relation_keys = BTreeSet::new();
    let mut relations_per_source = BTreeMap::<String, usize>::new();
    for relation in &relations {
        for endpoint in std::iter::once(&relation.source).chain(relation.target.as_ref()) {
            if let Some(symbol_id) = endpoint_symbol_id(endpoint)
                && !symbols_by_id.contains_key(symbol_id)
            {
                return Err(BridgeError::Invalid(
                    "script relation references a missing symbol".to_owned(),
                ));
            }
        }
        if let Some(range) = &relation.evidence_range
            && !documents_by_source
                .contains_key(&(range.path.as_str(), range.content_sha256.as_str()))
        {
            return Err(BridgeError::Invalid(
                "script relation evidence has no current document".to_owned(),
            ));
        }
        let relation_key = canonical_serialized(relation)?;
        if !relation_keys.insert(relation_key) {
            return Err(BridgeError::Invalid(
                "script graph contains a duplicate relation".to_owned(),
            ));
        }
        if let Some(source_id) = endpoint_symbol_id(&relation.source)
            && let Some(source) = symbols_by_id.get(source_id)
        {
            let key = resource_ref_key(&source.script_ref)?;
            let count = relations_per_source.entry(key).or_default();
            *count += 1;
            if *count > MAX_RELATIONS_PER_DOCUMENT {
                return Err(BridgeError::Invalid(
                    "script document has too many relations".to_owned(),
                ));
            }
        }
    }

    let mut diagnostic_ids = BTreeSet::new();
    let mut diagnostics_per_document = BTreeMap::<String, usize>::new();
    for diagnostic in &diagnostics {
        let key = resource_ref_key(&diagnostic.script_ref)?;
        let document = documents_by_ref.get(&key).ok_or_else(|| {
            BridgeError::Invalid("script diagnostic has no current document".to_owned())
        })?;
        if diagnostic.language != document.language
            || diagnostic.content_sha256 != document.content_sha256
            || diagnostic.range.as_ref().is_some_and(|range| {
                range.path != document.path || range.content_sha256 != document.content_sha256
            })
            || !diagnostic_ids.insert(diagnostic.diagnostic_id.as_str())
        {
            return Err(BridgeError::Invalid(
                "script diagnostic does not match its current document".to_owned(),
            ));
        }
        let count = diagnostics_per_document.entry(key).or_default();
        *count += 1;
        if *count > MAX_DIAGNOSTICS_PER_DOCUMENT {
            return Err(BridgeError::Invalid(
                "script document has too many diagnostics".to_owned(),
            ));
        }
    }

    let mut adapter_languages = BTreeSet::new();
    for status in &adapter_statuses {
        if !adapter_languages.insert(status.language) {
            return Err(BridgeError::Invalid(
                "script graph repeats an adapter status".to_owned(),
            ));
        }
    }
    for document in &documents {
        let status = adapter_statuses
            .iter()
            .find(|status| status.language == document.language)
            .ok_or_else(|| {
                BridgeError::Invalid("script document has no adapter status".to_owned())
            })?;
        if status
            .profile
            .is_some_and(|profile| profile != document.adapter_profile)
            || (status.availability == ScriptAdapterAvailability::Unavailable
                && document.completeness != ScriptCompleteness::Unavailable)
        {
            return Err(BridgeError::Invalid(
                "script document differs from its adapter status".to_owned(),
            ));
        }
    }

    let digest = semantic_digest(
        &documents,
        &symbols,
        &relations,
        &diagnostics,
        &adapter_statuses,
    )?;
    Ok(NormalizedScriptGraph {
        resource_revision,
        scene_graph_revision,
        script_graph_revision,
        documents,
        symbols,
        relations,
        diagnostics,
        adapter_statuses,
        semantic_digest: digest,
    })
}

fn require_script_graph(session: &Session) -> Result<(), BridgeError> {
    let required = [
        "script.gdscript_semantics",
        "script.incremental_index",
        "script.diagnostics",
        "script.csharp_discovery",
    ];
    if session.protocol_version() != "1.4"
        || required
            .iter()
            .any(|capability| !session.capabilities().contains(*capability))
    {
        return Err(BridgeError::CapabilityUnavailable {
            capability: "script.incremental_index",
            negotiated_version: session.protocol_version().to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oracle_string<'a>(value: &'a Value, key: &str) -> &'a str {
        value[key].as_str().unwrap()
    }

    fn oracle_map<'a>(value: &'a Value, key: &str) -> BTreeMap<&'a str, &'a Value> {
        value[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| (oracle_string(entry, "oracle_id"), entry))
            .collect()
    }

    fn oracle_language(value: &str) -> ScriptLanguage {
        match value {
            "gdscript" => ScriptLanguage::Gdscript,
            "csharp" => ScriptLanguage::Csharp,
            _ => panic!("unexpected oracle language"),
        }
    }

    fn oracle_profile(value: &str) -> ScriptAdapterProfile {
        match value {
            "gdscript_parser_analyzer_v1" => ScriptAdapterProfile::GdscriptParserAnalyzerV1,
            "csharp_discovery_only_v1" => ScriptAdapterProfile::CsharpDiscoveryOnlyV1,
            _ => panic!("unexpected oracle profile"),
        }
    }

    fn oracle_completeness(value: &str) -> ScriptCompleteness {
        match value {
            "complete" => ScriptCompleteness::Complete,
            "partial" => ScriptCompleteness::Partial,
            "invalid" => ScriptCompleteness::Invalid,
            "unavailable" => ScriptCompleteness::Unavailable,
            _ => panic!("unexpected oracle completeness"),
        }
    }

    fn oracle_symbol_kind(value: &str) -> ScriptSymbolKind {
        match value {
            "script" => ScriptSymbolKind::Script,
            "class" => ScriptSymbolKind::Class,
            "method" => ScriptSymbolKind::Method,
            "function" => ScriptSymbolKind::Function,
            "property" => ScriptSymbolKind::Property,
            "constant" => ScriptSymbolKind::Constant,
            "enum" => ScriptSymbolKind::Enum,
            "enum_member" => ScriptSymbolKind::EnumMember,
            "signal" => ScriptSymbolKind::Signal,
            "parameter" => ScriptSymbolKind::Parameter,
            "local" => ScriptSymbolKind::Local,
            "lambda" => ScriptSymbolKind::Lambda,
            _ => panic!("unexpected oracle symbol kind"),
        }
    }

    fn oracle_type_state(value: &str) -> ScriptTypeState {
        match value {
            "typed" => ScriptTypeState::Explicit,
            "inferred" => ScriptTypeState::Inferred,
            "untyped" => ScriptTypeState::Dynamic,
            "not_applicable" => ScriptTypeState::Unavailable,
            _ => panic!("unexpected oracle type state"),
        }
    }

    fn oracle_predicate(value: &str) -> ScriptRelationPredicate {
        match value {
            "contains" => ScriptRelationPredicate::Contains,
            "declares_symbol" => ScriptRelationPredicate::DeclaresSymbol,
            "inherits" => ScriptRelationPredicate::Inherits,
            "overrides" => ScriptRelationPredicate::Overrides,
            "references_symbol" => ScriptRelationPredicate::ReferencesSymbol,
            "calls" => ScriptRelationPredicate::Calls,
            "preloads" => ScriptRelationPredicate::Preloads,
            "loads" => ScriptRelationPredicate::Loads,
            "attaches_script" => ScriptRelationPredicate::AttachesScript,
            _ => panic!("unexpected oracle predicate"),
        }
    }

    fn oracle_script_ref(document: &Value) -> ResourceRef {
        let suffix = oracle_string(document, "script_id")
            .strip_prefix("godot:resource:uid:v1:")
            .unwrap();
        ResourceRef::Uid(crate::resource::UidResourceRef {
            uid: format!("uid://{suffix}"),
        })
    }

    fn oracle_range(range: &Value, documents: &BTreeMap<&str, &Value>) -> SourceRange {
        let document = documents[oracle_string(range, "document")];
        let bytes = range["bytes"].as_array().unwrap();
        let start = range["start"].as_array().unwrap();
        let end = range["end"].as_array().unwrap();
        SourceRange {
            path: oracle_string(document, "path").to_owned(),
            content_sha256: oracle_string(document, "content_sha256").to_owned(),
            start_byte: bytes[0].as_u64().unwrap(),
            end_byte: bytes[1].as_u64().unwrap(),
            start_line: u32::try_from(start[0].as_u64().unwrap()).unwrap(),
            start_column: u32::try_from(start[1].as_u64().unwrap()).unwrap(),
            end_line: u32::try_from(end[0].as_u64().unwrap()).unwrap(),
            end_column: u32::try_from(end[1].as_u64().unwrap()).unwrap(),
        }
    }

    fn resolve_oracle_qualified_key(
        oracle_id: &str,
        symbols: &BTreeMap<&str, &Value>,
        ranges: &BTreeMap<&str, &Value>,
        documents: &BTreeMap<&str, &Value>,
        cache: &mut BTreeMap<String, String>,
    ) -> String {
        if let Some(value) = cache.get(oracle_id) {
            return value.clone();
        }
        let symbol = symbols[oracle_id];
        if let Some(value) = symbol["qualified_key"].as_str() {
            cache.insert(oracle_id.to_owned(), value.to_owned());
            return value.to_owned();
        }
        let owner = oracle_string(symbol, "owner");
        let owner_key = resolve_oracle_qualified_key(owner, symbols, ranges, documents, cache);
        let range = oracle_range(ranges[oracle_string(symbol, "range")], documents);
        let value = match oracle_string(symbol, "kind") {
            "parameter" => format!(
                "{owner_key}/parameter:{}@{}:{}",
                oracle_string(symbol, "name"),
                range.start_line,
                range.start_column
            ),
            "local" => format!(
                "{owner_key}/local:{}@{}",
                oracle_string(symbol, "name"),
                range.start_byte
            ),
            "lambda" => format!("{owner_key}/lambda@{}", range.start_byte),
            _ => panic!("content-scoped oracle symbol has no canonical key rule"),
        };
        cache.insert(oracle_id.to_owned(), value.clone());
        value
    }

    fn oracle_payload() -> (ScriptSnapshotPayload, BTreeMap<String, String>) {
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../../tests/codex/fixtures/script_semantics_oracle/golden-script-graph.json"
        ))
        .unwrap();
        let oracle_documents = oracle_map(&oracle, "documents");
        let oracle_ranges = oracle_map(&oracle, "ranges");
        let oracle_resources = oracle_map(&oracle, "resources");
        let oracle_symbols = oracle_map(&oracle, "symbols");
        let mut qualified_keys = BTreeMap::new();
        for oracle_id in oracle_symbols.keys() {
            resolve_oracle_qualified_key(
                oracle_id,
                &oracle_symbols,
                &oracle_ranges,
                &oracle_documents,
                &mut qualified_keys,
            );
        }

        let documents: Vec<_> = oracle["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|document| ScriptDocument {
                script_ref: oracle_script_ref(document),
                path: oracle_string(document, "path").to_owned(),
                language: oracle_language(oracle_string(document, "language")),
                content_sha256: oracle_string(document, "content_sha256").to_owned(),
                adapter_profile: oracle_profile(oracle_string(document, "adapter_profile")),
                completeness: oracle_completeness(oracle_string(document, "semantic_state")),
                resource_revision: 7,
                script_graph_revision: 11,
            })
            .collect();

        let mut ids = BTreeMap::new();
        for symbol in oracle["symbols"].as_array().unwrap() {
            let document = oracle_documents[oracle_string(symbol, "document")];
            let range = oracle_range(
                oracle_ranges[oracle_string(symbol, "range")],
                &oracle_documents,
            );
            let script_ref = oracle_script_ref(document);
            let resource_id = canonical_script_resource_id(
                &script_ref,
                oracle_string(document, "path"),
                oracle_string(document, "content_sha256"),
            )
            .unwrap();
            let kind = oracle_symbol_kind(oracle_string(symbol, "kind"));
            let qualified_key = &qualified_keys[oracle_string(symbol, "oracle_id")];
            let identity = match oracle_string(symbol, "identity_scope") {
                "persistent" => canonical_named_symbol_id(
                    &resource_id,
                    oracle_language(oracle_string(document, "language")),
                    kind,
                    qualified_key,
                ),
                "content_revision" => canonical_content_symbol_id(
                    &resource_id,
                    oracle_language(oracle_string(document, "language")),
                    oracle_string(document, "content_sha256"),
                    qualified_key.rsplit_once('/').unwrap().0,
                    kind,
                    range.start_byte,
                    range.end_byte,
                ),
                _ => panic!("unexpected oracle identity scope"),
            };
            ids.insert(oracle_string(symbol, "oracle_id").to_owned(), identity);
        }

        let symbols: Vec<_> = oracle["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|symbol| {
                let document = oracle_documents[oracle_string(symbol, "document")];
                ScriptSymbol {
                    symbol_id: ids[oracle_string(symbol, "oracle_id")].clone(),
                    script_ref: oracle_script_ref(document),
                    language: oracle_language(oracle_string(document, "language")),
                    kind: oracle_symbol_kind(oracle_string(symbol, "kind")),
                    name: if oracle_string(symbol, "kind") == "lambda" {
                        None
                    } else {
                        symbol["name"].as_str().map(str::to_owned)
                    },
                    qualified_key: qualified_keys[oracle_string(symbol, "oracle_id")].clone(),
                    owner_symbol_id: symbol["owner"].as_str().map(|owner| ids[owner].clone()),
                    identity_scope: match oracle_string(symbol, "identity_scope") {
                        "persistent" => ScriptIdentityScope::Persistent,
                        "content_revision" => ScriptIdentityScope::ContentRevision,
                        _ => panic!("unexpected oracle identity scope"),
                    },
                    signature: None,
                    type_name: None,
                    type_state: oracle_type_state(oracle_string(symbol, "type_state")),
                    visibility: ScriptVisibility::Public,
                    modifiers: Vec::new(),
                    declaration_range: oracle_range(
                        oracle_ranges[oracle_string(symbol, "range")],
                        &oracle_documents,
                    ),
                    documentation_present: false,
                    script_graph_revision: 11,
                }
            })
            .collect();

        let relations: Vec<_> = oracle["relations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|relation| {
                let target = relation["target_symbol"]
                    .as_str()
                    .map(|target| ScriptRelationEndpoint::Symbol {
                        symbol_id: ids[target].clone(),
                    })
                    .or_else(|| {
                        relation["target_resource"].as_str().map(|target| {
                            ScriptRelationEndpoint::Resource {
                                resource_ref: ResourceRef::Uid(crate::resource::UidResourceRef {
                                    uid: oracle_string(oracle_resources[target], "uid").to_owned(),
                                }),
                            }
                        })
                    });
                ScriptRelation {
                    source: ScriptRelationEndpoint::Symbol {
                        symbol_id: ids[oracle_string(relation, "source_symbol")].clone(),
                    },
                    predicate: oracle_predicate(oracle_string(relation, "predicate")),
                    target,
                    confidence: match oracle_string(relation, "confidence") {
                        "exact" => ScriptConfidence::Exact,
                        "dynamic" => ScriptConfidence::Dynamic,
                        _ => panic!("unexpected oracle confidence"),
                    },
                    evidence_range: Some(oracle_range(
                        oracle_ranges[oracle_string(relation, "evidence_range")],
                        &oracle_documents,
                    )),
                    detail: Some(oracle_string(relation, "detail").to_owned()),
                    authority: ScriptRelationAuthority::GdscriptParserAnalyzer,
                    script_graph_revision: 11,
                }
            })
            .collect();

        let diagnostics: Vec<_> = oracle["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|diagnostic| {
                let document = oracle_documents[oracle_string(diagnostic, "document")];
                let range = oracle_range(
                    oracle_ranges[oracle_string(diagnostic, "evidence_range")],
                    &oracle_documents,
                );
                let script_ref = oracle_script_ref(document);
                let resource_id = canonical_script_resource_id(
                    &script_ref,
                    oracle_string(document, "path"),
                    oracle_string(document, "content_sha256"),
                )
                .unwrap();
                let code = oracle_string(diagnostic, "code").to_ascii_uppercase();
                let safe_message = code.replace('_', " ");
                let severity = match oracle_string(diagnostic, "severity") {
                    "error" => ScriptDiagnosticSeverity::Error,
                    "warning" => ScriptDiagnosticSeverity::Warning,
                    "info" => ScriptDiagnosticSeverity::Info,
                    _ => panic!("unexpected oracle severity"),
                };
                let diagnostic_id = canonical_diagnostic_id(ScriptDiagnosticIdentity {
                    script_resource_id: &resource_id,
                    language: oracle_language(oracle_string(document, "language")),
                    content_sha256: oracle_string(document, "content_sha256"),
                    severity,
                    code: &code,
                    start_byte: range.start_byte,
                    end_byte: range.end_byte,
                    safe_message: &safe_message,
                });
                ScriptDiagnostic {
                    diagnostic_id,
                    script_ref,
                    language: oracle_language(oracle_string(document, "language")),
                    content_sha256: oracle_string(document, "content_sha256").to_owned(),
                    code,
                    severity,
                    safe_message,
                    range: Some(range),
                    authority: match oracle_string(diagnostic, "authority") {
                        "gdscript_parser" => ScriptDiagnosticAuthority::GdscriptParser,
                        "gdscript_analyzer" => ScriptDiagnosticAuthority::GdscriptAnalyzer,
                        _ => panic!("unexpected oracle diagnostic authority"),
                    },
                    script_graph_revision: 11,
                }
            })
            .collect();

        (
            ScriptSnapshotPayload {
                documents,
                symbols,
                relations,
                diagnostics,
                adapter_statuses: vec![
                    LanguageAdapterStatus {
                        language: ScriptLanguage::Gdscript,
                        availability: ScriptAdapterAvailability::Available,
                        profile: Some(ScriptAdapterProfile::GdscriptParserAnalyzerV1),
                        version: Some("oracle".to_owned()),
                        diagnostic: None,
                    },
                    LanguageAdapterStatus {
                        language: ScriptLanguage::Csharp,
                        availability: ScriptAdapterAvailability::DiscoveryOnly,
                        profile: Some(ScriptAdapterProfile::CsharpDiscoveryOnlyV1),
                        version: Some("oracle".to_owned()),
                        diagnostic: None,
                    },
                ],
            },
            ids,
        )
    }

    fn canonicalize_fixture_ids(payload: &mut ScriptSnapshotPayload) {
        let mut ids = BTreeMap::new();
        for symbol in &payload.symbols {
            let resource_id = canonical_script_resource_id(
                &symbol.script_ref,
                &symbol.declaration_range.path,
                &symbol.declaration_range.content_sha256,
            )
            .unwrap();
            let canonical = canonical_named_symbol_id(
                &resource_id,
                symbol.language,
                symbol.kind,
                &symbol.qualified_key,
            );
            ids.insert(symbol.symbol_id.clone(), canonical);
        }
        for symbol in &mut payload.symbols {
            symbol.symbol_id = ids[&symbol.symbol_id].clone();
            if let Some(owner) = &mut symbol.owner_symbol_id {
                *owner = ids[owner].clone();
            }
        }
        for relation in &mut payload.relations {
            for endpoint in std::iter::once(&mut relation.source).chain(relation.target.as_mut()) {
                if let ScriptRelationEndpoint::Symbol { symbol_id } = endpoint {
                    *symbol_id = ids[symbol_id].clone();
                }
            }
        }
        for diagnostic in &mut payload.diagnostics {
            let range = diagnostic.range.as_ref().unwrap();
            let resource_id = canonical_script_resource_id(
                &diagnostic.script_ref,
                &range.path,
                &diagnostic.content_sha256,
            )
            .unwrap();
            diagnostic.diagnostic_id = canonical_diagnostic_id(ScriptDiagnosticIdentity {
                script_resource_id: &resource_id,
                language: diagnostic.language,
                content_sha256: &diagnostic.content_sha256,
                severity: diagnostic.severity,
                code: &diagnostic.code,
                start_byte: range.start_byte,
                end_byte: range.end_byte,
                safe_message: &diagnostic.safe_message,
            });
        }
    }

    #[test]
    fn script_snapshot_fixture_normalizes_strictly() {
        let response: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/script-snapshot-response.json"
        ))
        .unwrap();
        let accepted: ScriptSnapshotAccepted =
            serde_json::from_value(response["result"].clone()).unwrap();
        let mut chunk: ScriptSnapshotChunk = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/script-snapshot-chunk.json"
        ))
        .unwrap();
        canonicalize_fixture_ids(&mut chunk.payload);
        chunk.payload_json = canonical_serialized(&chunk.payload).unwrap();
        chunk.checksum = sha256_hex(chunk.payload_json.as_bytes());
        validate_snapshot_chunk(&chunk, &accepted, 0).unwrap();

        let mut reordered = chunk.payload.clone();
        reordered.symbols.reverse();
        reordered.relations.reverse();
        let first = normalize_script_graph(&chunk.payload, 7, 5, 11).unwrap();
        let second = normalize_script_graph(&reordered, 7, 5, 11).unwrap();
        assert_eq!(first.semantic_digest, second.semantic_digest);
        assert_eq!(first.symbols, second.symbols);
        assert_eq!(first.relations, second.relations);
    }

    #[test]
    fn script_normalizer_matches_the_independent_oracle() {
        let (payload, oracle_ids) = oracle_payload();
        let normalized = normalize_script_graph(&payload, 7, 5, 11).unwrap();
        assert_eq!(normalized.documents.len(), 8);
        assert_eq!(normalized.symbols.len(), 48);
        assert_eq!(normalized.relations.len(), 14);
        assert_eq!(normalized.diagnostics.len(), 4);
        assert_eq!(
            normalized
                .relations
                .iter()
                .filter(|relation| relation.confidence == ScriptConfidence::Exact)
                .count(),
            9
        );
        assert_eq!(
            normalized
                .relations
                .iter()
                .filter(|relation| relation.confidence == ScriptConfidence::Dynamic)
                .count(),
            5
        );
        assert_eq!(
            normalized
                .symbols
                .iter()
                .map(|symbol| symbol.symbol_id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            oracle_ids.len()
        );
        assert!(
            normalized
                .relations
                .iter()
                .filter(|relation| relation.confidence == ScriptConfidence::Dynamic)
                .all(|relation| relation.target.is_none())
        );
        assert_eq!(
            normalized.semantic_digest,
            "sha256:709dd89c7066f81aaa1d1fd266dba50ab90ae2abe5ddc43c94f91e6028cfde69"
        );
    }

    #[test]
    fn script_identity_range_and_confidence_fail_closed() {
        let mut chunk: ScriptSnapshotChunk = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/script-snapshot-chunk.json"
        ))
        .unwrap();
        canonicalize_fixture_ids(&mut chunk.payload);
        let mut wrong_identity = chunk.payload.symbols[0].clone();
        wrong_identity.symbol_id = chunk.payload.symbols[1].symbol_id.clone();
        assert!(validate_symbol(&wrong_identity, 11).is_err());

        let mut wrong_range = chunk.payload.symbols[0].clone();
        wrong_range.declaration_range.end_byte = wrong_range.declaration_range.start_byte;
        assert!(validate_symbol(&wrong_range, 11).is_err());

        let mut false_exact = chunk.payload.relations[1].clone();
        false_exact.confidence = ScriptConfidence::Exact;
        assert!(validate_relation(&false_exact, 11).is_err());

        let mut dynamic_target = chunk.payload.relations[1].clone();
        dynamic_target.target = Some(chunk.payload.relations[0].source.clone());
        assert!(validate_relation(&dynamic_target, 11).is_err());

        assert!(!contains_absolute_path(
            "Could not resolve super class path \"res://scripts/missing.gd\"."
        ));
        assert!(!contains_absolute_path(
            "Could not load https://example.invalid/script.gd."
        ));
        assert!(contains_absolute_path(
            "Parser failed at C:\\Users\\developer\\project\\script.gd."
        ));
        assert!(contains_absolute_path(
            "Parser failed at /Users/developer/project/script.gd."
        ));
        assert!(contains_absolute_path(
            "Parser failed at file:///tmp/project/script.gd."
        ));
    }

    #[test]
    fn script_delta_checksum_and_continuity_are_strict() {
        let operations = vec![ScriptDeltaOperation::RemoveDocument {
            script_ref: ResourceRef::Uid(crate::resource::UidResourceRef {
                uid: "uid://s5removedscript".to_owned(),
            }),
            path: "res://scripts/removed.gd".to_owned(),
        }];
        let operations_json = canonical_serialized(&operations).unwrap();
        let batch = ScriptDeltaBatch {
            batch_id: "script-batch:0123456789abcdef0123456789abcdef".to_owned(),
            previous_script_graph_revision: 1,
            script_graph_revision: 2,
            resource_revision: 3,
            scene_graph_revision: 4,
            project_revision: 5,
            checksum: sha256_hex(operations_json.as_bytes()),
            operations,
            source_complete: true,
        };
        validate_delta_batch(&batch, 1, 2).unwrap();

        let mut broken = batch;
        broken.previous_script_graph_revision = 0;
        assert!(validate_delta_batch(&broken, 1, 2).is_err());
    }

    #[test]
    fn script_dto_rejects_unknown_fields() {
        let mut value: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/script-snapshot-chunk.json"
        ))
        .unwrap();
        value["payload"]["documents"][0]["absolute_path"] = json!("/tmp/player.gd");
        assert!(serde_json::from_value::<ScriptSnapshotChunk>(value).is_err());
    }
}
