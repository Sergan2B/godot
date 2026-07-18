//! Storage-neutral saved-script records introduced by logical schema 1.3.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::StoreError;

const MAX_DOCUMENTS: usize = 250_000;
const MAX_SYMBOLS: usize = 2_000_000;
const MAX_RELATIONS: usize = 4_000_000;
const MAX_DIAGNOSTICS: usize = 2_000_000;
const MAX_PATH_BYTES: usize = 1_024;
const MAX_NAME_BYTES: usize = 1_024;
const MAX_SIGNATURE_BYTES: usize = 4_096;
const MAX_DIAGNOSTIC_BYTES: usize = 2_048;
const MAX_ID_BYTES: usize = 1_024;

/// Language represented by one saved-script record.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptLanguage {
    /// Godot's built-in GDScript language.
    Gdscript,
    /// C# script discovery or authoritative adapter output.
    Csharp,
}

/// Frozen adapter profile that produced a saved-script document.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptAdapterProfile {
    /// Bridge-owned exact-content GDScript parser/analyzer projection.
    GdscriptParserAnalyzerV1,
    /// Discovery-only C# projection with no semantic claims.
    CsharpDiscoveryOnlyV1,
}

/// Completeness of one exact saved-content document projection.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptCompleteness {
    /// All required parse and semantic observations are available.
    Complete,
    /// Safe current facts exist but one or more facts are unresolved.
    Partial,
    /// Only bounded current diagnostics are safe to retain.
    Invalid,
    /// The language adapter cannot produce semantic facts.
    Unavailable,
}

/// Persistence scope of a script declaration identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptIdentityScope {
    /// The declaration is stable while its canonical named identity is stable.
    Persistent,
    /// The declaration identity is bound to one exact content revision.
    ContentRevision,
}

/// Declaration kind supported by Sprint 5.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
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

/// Type confidence retained on one declaration.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptTypeState {
    Explicit,
    Inferred,
    Dynamic,
    Unavailable,
}

/// Saved declaration visibility.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptVisibility {
    Public,
    Protected,
    Private,
    Internal,
}

/// Stable declaration modifier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
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

/// Shared script relation vocabulary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptPredicate {
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

/// Authority confidence of one saved-script relation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptConfidence {
    Exact,
    Dynamic,
}

/// Component that produced one relation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptRelationAuthority {
    GdscriptParserAnalyzer,
    ResourceGraph,
    SceneState,
    CsharpAdapter,
}

/// Stable diagnostic severity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptDiagnosticSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

/// Component that produced one script diagnostic.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptDiagnosticAuthority {
    GdscriptParser,
    GdscriptAnalyzer,
    CsharpAdapter,
    ScriptAdapter,
}

/// Availability of one language adapter in the committed script domain.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptAdapterAvailability {
    Available,
    DiscoveryOnly,
    Unavailable,
}

/// Exact saved-content source span.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptSourceRange {
    pub path: String,
    pub content_sha256: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// One normalized saved-script document.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptDocument {
    pub script_resource_id: String,
    pub path: String,
    pub language: ScriptLanguage,
    pub content_sha256: String,
    pub adapter_profile: ScriptAdapterProfile,
    pub completeness: ScriptCompleteness,
    pub resource_revision: u64,
    pub script_graph_revision: u64,
}

/// One normalized declaration.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptSymbol {
    pub symbol_id: String,
    pub script_resource_id: String,
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
    pub declaration_range: ScriptSourceRange,
    pub documentation_present: bool,
    pub script_graph_revision: u64,
}

/// Canonical endpoint used by a script relation.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScriptEndpoint {
    Symbol { symbol_id: String },
    Resource { resource_entity_id: String },
    SceneNode { node_entity_id: String },
}

/// One normalized semantic relation, including targetless dynamic observations.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptRelation {
    pub relation_id: String,
    pub script_resource_id: String,
    pub source: ScriptEndpoint,
    pub predicate: ScriptPredicate,
    pub target: Option<ScriptEndpoint>,
    pub confidence: ScriptConfidence,
    pub evidence_range: Option<ScriptSourceRange>,
    pub detail: Option<String>,
    pub authority: ScriptRelationAuthority,
    pub script_graph_revision: u64,
}

/// Materialized targetful relation used for deterministic reference lookup.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptReference {
    pub reference_id: String,
    pub relation_id: String,
    pub script_resource_id: String,
    pub source: ScriptEndpoint,
    pub predicate: ScriptPredicate,
    pub target: ScriptEndpoint,
    pub authority: ScriptRelationAuthority,
    pub script_graph_revision: u64,
}

/// One bounded current diagnostic tied to exact saved bytes.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptDiagnostic {
    pub diagnostic_id: String,
    pub script_resource_id: String,
    pub language: ScriptLanguage,
    pub content_sha256: String,
    pub code: String,
    pub severity: ScriptDiagnosticSeverity,
    pub safe_message: String,
    pub range: Option<ScriptSourceRange>,
    pub authority: ScriptDiagnosticAuthority,
    pub script_graph_revision: u64,
}

/// Availability and version of one language adapter.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptAdapterStatus {
    pub language: ScriptLanguage,
    pub availability: ScriptAdapterAvailability,
    pub profile: Option<ScriptAdapterProfile>,
    pub version: Option<String>,
    pub diagnostic: Option<String>,
}

/// Independently checkpointed script-domain records inside one generation.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptDomainGeneration {
    pub editor_session_id: String,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub script_graph_revision: u64,
    pub source_complete: bool,
    pub snapshot_checksum: String,
    pub semantic_digest: String,
    pub documents: Vec<ScriptDocument>,
    pub symbols: Vec<ScriptSymbol>,
    pub relations: Vec<ScriptRelation>,
    pub references: Vec<ScriptReference>,
    pub diagnostics: Vec<ScriptDiagnostic>,
    pub adapter_statuses: Vec<ScriptAdapterStatus>,
    pub validation_digest: String,
}

impl ScriptDomainGeneration {
    /// Returns whether migration intentionally left the script domain non-current.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.editor_session_id.is_empty()
            && self.resource_revision == 0
            && self.scene_graph_revision == 0
            && self.script_graph_revision == 0
            && !self.source_complete
            && self.snapshot_checksum.is_empty()
            && self.semantic_digest.is_empty()
            && self.documents.is_empty()
            && self.symbols.is_empty()
            && self.relations.is_empty()
            && self.references.is_empty()
            && self.diagnostics.is_empty()
            && self.adapter_statuses.is_empty()
    }

    /// Sorts every record family by its canonical semantic identity.
    pub fn canonicalize(&mut self) {
        self.documents.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.script_resource_id.cmp(&right.script_resource_id))
        });
        for symbol in &mut self.symbols {
            symbol.modifiers.sort_unstable();
        }
        self.symbols
            .sort_by(|left, right| left.symbol_id.cmp(&right.symbol_id));
        self.relations
            .sort_by(|left, right| left.relation_id.cmp(&right.relation_id));
        self.references
            .sort_by(|left, right| left.reference_id.cmp(&right.reference_id));
        self.diagnostics
            .sort_by(|left, right| left.diagnostic_id.cmp(&right.diagnostic_id));
        self.adapter_statuses
            .sort_by(|left, right| left.language.cmp(&right.language));
    }

    /// Computes the physical-layout-independent digest of normalized script facts.
    #[must_use]
    pub fn compute_validation_digest(&self) -> String {
        let mut canonical = self.clone();
        canonical.canonicalize();
        canonical.validation_digest.clear();
        let bytes = serde_json::to_vec(&canonical).expect("script domain is serializable");
        let mut hasher = Sha256::new();
        hasher.update(b"godot-codex/script-domain/v1\0");
        hasher.update(bytes);
        format!("sha256:{:x}", hasher.finalize())
    }

    /// Validates checkpoint, identities, references, bounds, and semantic parity.
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.is_empty() {
            if self.validation_digest.is_empty()
                || self.validation_digest == self.compute_validation_digest()
            {
                return Ok(());
            }
            return invalid("script domain validation digest mismatch");
        }
        if self.editor_session_id.is_empty()
            || self.resource_revision == 0
            || self.scene_graph_revision == 0
            || self.script_graph_revision == 0
            || !self.source_complete
            || !valid_hash(&self.snapshot_checksum)
            || !valid_sha256(&self.semantic_digest)
            || self.documents.len() > MAX_DOCUMENTS
            || self.symbols.len() > MAX_SYMBOLS
            || self.relations.len() > MAX_RELATIONS
            || self.references.len() > MAX_RELATIONS
            || self.diagnostics.len() > MAX_DIAGNOSTICS
            || self.adapter_statuses.len() > 2
        {
            return invalid("script domain checkpoint or limits are invalid");
        }

        let mut documents = BTreeMap::new();
        let mut document_paths = BTreeSet::new();
        for document in &self.documents {
            if !valid_id(&document.script_resource_id)
                || !valid_path(&document.path)
                || !valid_sha256(&document.content_sha256)
                || document.resource_revision == 0
                || document.resource_revision > self.resource_revision
                || document.script_graph_revision != self.script_graph_revision
                || !profile_matches_language(document.adapter_profile, document.language)
                || !completeness_matches_profile(document)
                || documents
                    .insert(document.script_resource_id.as_str(), document)
                    .is_some()
                || !document_paths.insert(document.path.as_str())
            {
                return invalid("script document identity or coordinates are invalid");
            }
        }

        let mut symbols = BTreeMap::new();
        for symbol in &self.symbols {
            let Some(document) = documents.get(symbol.script_resource_id.as_str()) else {
                return invalid("script symbol has no owning document");
            };
            if !valid_symbol(symbol, document, self.script_graph_revision)
                || symbols.insert(symbol.symbol_id.as_str(), symbol).is_some()
            {
                return invalid("script symbol identity or coordinates are invalid");
            }
        }
        for symbol in &self.symbols {
            if let Some(owner) = symbol.owner_symbol_id.as_deref() {
                let Some(owner_symbol) = symbols.get(owner) else {
                    return invalid("script symbol owner is missing");
                };
                if owner_symbol.script_resource_id != symbol.script_resource_id {
                    return invalid("script symbol owner belongs to another document");
                }
            }
        }

        let mut relations = BTreeMap::new();
        for relation in &self.relations {
            let Some(document) = documents.get(relation.script_resource_id.as_str()) else {
                return invalid("script relation has no owning document");
            };
            if !valid_id(&relation.relation_id)
                || (matches!(
                    document.completeness,
                    ScriptCompleteness::Invalid | ScriptCompleteness::Unavailable
                ) && relation.authority != ScriptRelationAuthority::SceneState)
                || relation.script_graph_revision != self.script_graph_revision
                || !valid_endpoint(&relation.source, &symbols)
                || !valid_relation_shape(relation, &symbols)
                || relation.detail.as_ref().is_some_and(|detail| {
                    detail.is_empty()
                        || detail.len() > 128
                        || !detail.bytes().enumerate().all(|(index, byte)| {
                            if index == 0 {
                                byte.is_ascii_lowercase()
                            } else {
                                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                            }
                        })
                })
                || relation.evidence_range.as_ref().is_some_and(|range| {
                    !valid_range(range)
                        || range.path != document.path
                        || range.content_sha256 != document.content_sha256
                })
                || relations
                    .insert(relation.relation_id.as_str(), relation)
                    .is_some()
            {
                return invalid("script relation is invalid");
            }
        }

        let mut reference_relations = BTreeSet::new();
        let mut reference_ids = BTreeSet::new();
        for reference in &self.references {
            let Some(relation) = relations.get(reference.relation_id.as_str()) else {
                return invalid("script reference relation is missing");
            };
            if !valid_id(&reference.reference_id)
                || reference.script_graph_revision != self.script_graph_revision
                || reference.script_resource_id != relation.script_resource_id
                || reference.source != relation.source
                || reference.predicate != relation.predicate
                || relation.target.as_ref() != Some(&reference.target)
                || reference.authority != relation.authority
                || relation.confidence != ScriptConfidence::Exact
                || !reference_ids.insert(reference.reference_id.as_str())
                || !reference_relations.insert(reference.relation_id.as_str())
            {
                return invalid("script reference parity is invalid");
            }
        }
        let targetful_relations: BTreeSet<_> = self
            .relations
            .iter()
            .filter(|relation| relation.target.is_some())
            .map(|relation| relation.relation_id.as_str())
            .collect();
        if reference_relations != targetful_relations {
            return invalid("script relation/reference shard parity mismatch");
        }

        let mut diagnostic_ids = BTreeSet::new();
        for diagnostic in &self.diagnostics {
            let Some(document) = documents.get(diagnostic.script_resource_id.as_str()) else {
                return invalid("script diagnostic has no owning document");
            };
            if !valid_id(&diagnostic.diagnostic_id)
                || diagnostic.language != document.language
                || diagnostic.content_sha256 != document.content_sha256
                || diagnostic.script_graph_revision != self.script_graph_revision
                || !valid_diagnostic_code(&diagnostic.code)
                || !valid_safe_message(&diagnostic.safe_message)
                || diagnostic.range.as_ref().is_some_and(|range| {
                    !valid_range(range)
                        || range.path != document.path
                        || range.content_sha256 != document.content_sha256
                })
                || !diagnostic_ids.insert(diagnostic.diagnostic_id.as_str())
            {
                return invalid("script diagnostic is invalid");
            }
        }

        let mut status_languages = BTreeSet::new();
        for status in &self.adapter_statuses {
            if !valid_adapter_status(status) || !status_languages.insert(status.language) {
                return invalid("script adapter status is invalid");
            }
        }
        if !status_languages.contains(&ScriptLanguage::Gdscript)
            || !status_languages.contains(&ScriptLanguage::Csharp)
        {
            return invalid("script adapter status set is incomplete");
        }
        for document in &self.documents {
            let Some(status) = self
                .adapter_statuses
                .iter()
                .find(|status| status.language == document.language)
            else {
                return invalid("script document adapter status is missing");
            };
            let compatible = match status.availability {
                ScriptAdapterAvailability::Available => {
                    status.profile == Some(document.adapter_profile)
                        && document.completeness != ScriptCompleteness::Unavailable
                }
                ScriptAdapterAvailability::DiscoveryOnly => {
                    document.language == ScriptLanguage::Csharp
                        && status.profile == Some(document.adapter_profile)
                        && document.completeness == ScriptCompleteness::Unavailable
                }
                ScriptAdapterAvailability::Unavailable => {
                    document.completeness == ScriptCompleteness::Unavailable
                }
            };
            if !compatible {
                return invalid("script document differs from adapter status");
            }
        }

        let expected = self.compute_validation_digest();
        if !self.validation_digest.is_empty() && self.validation_digest != expected {
            return invalid("script domain validation digest mismatch");
        }
        Ok(())
    }
}

fn completeness_matches_profile(document: &ScriptDocument) -> bool {
    match document.adapter_profile {
        ScriptAdapterProfile::GdscriptParserAnalyzerV1 => {
            document.language == ScriptLanguage::Gdscript
        }
        ScriptAdapterProfile::CsharpDiscoveryOnlyV1 => {
            document.language == ScriptLanguage::Csharp
                && document.completeness == ScriptCompleteness::Unavailable
        }
    }
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

fn valid_symbol(
    symbol: &ScriptSymbol,
    document: &ScriptDocument,
    script_graph_revision: u64,
) -> bool {
    let name_valid = symbol.name.as_ref().is_none_or(|name| {
        !name.is_empty()
            && name.len() <= MAX_NAME_BYTES
            && name.chars().count() <= 512
            && !name.chars().any(char::is_control)
    });
    let optional_text_valid = |value: &Option<String>, limit: usize| {
        value
            .as_ref()
            .is_none_or(|value| value.len() <= limit && !value.chars().any(char::is_control))
    };
    let local_scope_valid = if matches!(
        symbol.kind,
        ScriptSymbolKind::Parameter | ScriptSymbolKind::Local | ScriptSymbolKind::Lambda
    ) {
        symbol.identity_scope == ScriptIdentityScope::ContentRevision
    } else {
        true
    };
    let mut modifiers = BTreeSet::new();
    valid_id(&symbol.symbol_id)
        && symbol.language == document.language
        && !matches!(
            document.completeness,
            ScriptCompleteness::Invalid | ScriptCompleteness::Unavailable
        )
        && name_valid
        && !symbol.qualified_key.is_empty()
        && symbol.qualified_key.len() <= 2_048
        && !symbol.qualified_key.chars().any(char::is_control)
        && optional_text_valid(&symbol.signature, MAX_SIGNATURE_BYTES)
        && optional_text_valid(&symbol.type_name, MAX_NAME_BYTES)
        && local_scope_valid
        && valid_range(&symbol.declaration_range)
        && symbol.declaration_range.start_byte < symbol.declaration_range.end_byte
        && symbol.declaration_range.path == document.path
        && symbol.declaration_range.content_sha256 == document.content_sha256
        && symbol.script_graph_revision == script_graph_revision
        && symbol
            .modifiers
            .iter()
            .all(|modifier| modifiers.insert(*modifier))
}

fn valid_relation_shape(
    relation: &ScriptRelation,
    symbols: &BTreeMap<&str, &ScriptSymbol>,
) -> bool {
    if relation.confidence == ScriptConfidence::Dynamic {
        return relation.target.is_none()
            && matches!(
                relation.predicate,
                ScriptPredicate::ReferencesSymbol | ScriptPredicate::Calls | ScriptPredicate::Loads
            );
    }
    let Some(target) = relation.target.as_ref() else {
        return false;
    };
    if !valid_endpoint(target, symbols) {
        return false;
    }
    match relation.predicate {
        ScriptPredicate::Contains | ScriptPredicate::DeclaresSymbol => {
            matches!(target, ScriptEndpoint::Symbol { .. })
        }
        ScriptPredicate::Preloads | ScriptPredicate::Loads => {
            matches!(target, ScriptEndpoint::Resource { .. })
        }
        ScriptPredicate::Inherits
        | ScriptPredicate::Overrides
        | ScriptPredicate::ReferencesSymbol
        | ScriptPredicate::Calls => matches!(
            target,
            ScriptEndpoint::Symbol { .. } | ScriptEndpoint::Resource { .. }
        ),
        ScriptPredicate::AttachesScript => matches!(
            target,
            ScriptEndpoint::Symbol { .. } | ScriptEndpoint::Resource { .. }
        ),
    }
}

fn valid_endpoint(endpoint: &ScriptEndpoint, symbols: &BTreeMap<&str, &ScriptSymbol>) -> bool {
    match endpoint {
        ScriptEndpoint::Symbol { symbol_id } => symbols.contains_key(symbol_id.as_str()),
        ScriptEndpoint::Resource { resource_entity_id } => valid_id(resource_entity_id),
        ScriptEndpoint::SceneNode { node_entity_id } => valid_id(node_entity_id),
    }
}

fn valid_adapter_status(status: &ScriptAdapterStatus) -> bool {
    let version_valid = status.version.as_ref().is_none_or(|version| {
        !version.is_empty() && version.len() <= 64 && !version.chars().any(char::is_control)
    });
    let diagnostic_valid = status.diagnostic.as_ref().is_none_or(|message| {
        !message.is_empty() && message.len() <= 512 && normalized_safe_message(message) == *message
    });
    version_valid
        && diagnostic_valid
        && match status.availability {
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
        }
}

fn valid_range(range: &ScriptSourceRange) -> bool {
    valid_path(&range.path)
        && valid_sha256(&range.content_sha256)
        && range.start_byte <= range.end_byte
        && range.start_line > 0
        && range.end_line > 0
        && range.start_column > 0
        && range.end_column > 0
        && range.start_line <= i32::MAX as u32
        && range.end_line <= i32::MAX as u32
        && range.start_column <= i32::MAX as u32
        && range.end_column <= i32::MAX as u32
        && (range.start_line, range.start_column) <= (range.end_line, range.end_column)
}

fn valid_path(value: &str) -> bool {
    value.len() <= MAX_PATH_BYTES
        && value
            .strip_prefix("res://")
            .is_some_and(|suffix| !suffix.is_empty() && !suffix.ends_with('/'))
        && !value.contains('\\')
        && !value.contains('?')
        && !value.contains('#')
        && !value.chars().any(char::is_control)
        && value.strip_prefix("res://").is_some_and(|suffix| {
            suffix
                .split('/')
                .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
        })
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && !value.chars().any(char::is_control)
        && !value.contains('/')
        && !value.contains('\\')
}

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_hash(value: &str) -> bool {
    valid_sha256(value)
        || (value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
}

fn valid_diagnostic_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_uppercase()
            } else {
                byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
            }
        })
}

fn valid_safe_message(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_DIAGNOSTIC_BYTES
        && normalized_safe_message(value) == value
        && !contains_absolute_path(value)
}

fn normalized_safe_message(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut pending_space = false;
    for character in value.chars() {
        if character.is_whitespace() {
            pending_space = !result.is_empty();
        } else {
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

fn invalid<T>(message: &str) -> Result<T, StoreError> {
    Err(StoreError::ValidationFailed(message.to_owned()))
}
