use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use godot_codex_bridge_client as bridge;
use godot_codex_index_store as store;
use sha2::{Digest, Sha256};

use crate::{IndexerError, normalize_resource_path};

const RELATION_DOMAIN: &str = "godot-codex/script-relation/v1";
const REFERENCE_DOMAIN: &str = "godot-codex/script-reference/v1";
const INPUT_DOMAIN: &[u8] = b"godot-codex/normalized-script-input/v1\0";
const GODOT_UID_BASE: u64 = 34;
const GODOT_UID_MASK: u64 = 0x7fff_ffff_ffff_ffff;
const GODOT_UID_ALPHABET: &[u8; GODOT_UID_BASE as usize] = b"abcdefghijklmnopqrstuvwxy012345678";

/// Stateless conversion from a strict Bridge RPC 1.4 script graph to logical
/// schema 1.3 records joined against one immutable resource/scene generation.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScriptNormalizer;

impl ScriptNormalizer {
    /// Resolves resource targets and saved scene attachments without inventing
    /// semantic targets that were not authoritative in Bridge or SceneState.
    pub fn normalize_graph(
        &self,
        base: &store::IndexGeneration,
        editor_session_id: &str,
        graph: &bridge::NormalizedScriptGraph,
    ) -> Result<store::ScriptDomainGeneration, IndexerError> {
        if editor_session_id.is_empty()
            || base.checkpoint.editor_session_id != editor_session_id
            || graph.resource_revision == 0
            || graph.scene_graph_revision == 0
            || graph.script_graph_revision == 0
            || graph.resource_revision != base.checkpoint.resource_revision
        {
            return Err(IndexerError::Script("script_graph_context"));
        }
        let joined_scene_revision = if base.scene.is_empty() {
            graph.scene_graph_revision
        } else {
            if base.scene.editor_session_id != editor_session_id
                || !base.scene.source_complete
                || graph.scene_graph_revision > base.scene.scene_graph_revision
            {
                return Err(IndexerError::Script("script_scene_checkpoint"));
            }
            base.scene.scene_graph_revision
        };

        let resources = ResourceLookup::new(base)?;
        let mut documents = Vec::with_capacity(graph.documents.len());
        let mut document_by_id = BTreeMap::new();
        let mut document_by_source = BTreeMap::new();
        let mut document_by_ref = BTreeMap::new();
        for document in &graph.documents {
            let path = normalize_resource_path(&document.path)?;
            let canonical_id = canonical_script_resource_id(
                &document.script_ref,
                &document.path,
                &document.content_sha256,
            )?;
            let resource = resources.find(&document.script_ref)?;
            let detached_csharp_discovery = resource.is_none()
                && matches!(document.script_ref, bridge::ResourceRef::Path(_))
                && document.language == bridge::ScriptLanguage::Csharp
                && document.adapter_profile == bridge::ScriptAdapterProfile::CsharpDiscoveryOnlyV1
                && document.completeness == bridge::ScriptCompleteness::Unavailable;
            if resource.is_some_and(|resource| {
                path.comparison != resource.comparison_path
                    || resource.content_generation.as_deref()
                        != Some(document.content_sha256.as_str())
                    || canonical_id != resource.entity_id
            }) || (resource.is_none() && !detached_csharp_discovery)
                || document.resource_revision != graph.resource_revision
                || document.script_graph_revision != graph.script_graph_revision
            {
                return Err(IndexerError::Script("script_document_resource_mismatch"));
            }
            let record = store::ScriptDocument {
                script_resource_id: canonical_id,
                path: path.display,
                language: language(document.language),
                content_sha256: document.content_sha256.clone(),
                adapter_profile: adapter_profile(document.adapter_profile),
                completeness: completeness(document.completeness),
                resource_revision: document.resource_revision,
                script_graph_revision: document.script_graph_revision,
            };
            if document_by_id
                .insert(record.script_resource_id.clone(), record.clone())
                .is_some()
                || document_by_source
                    .insert(
                        (record.path.clone(), record.content_sha256.clone()),
                        record.script_resource_id.clone(),
                    )
                    .is_some()
                || document_by_ref
                    .insert(
                        resource_ref_key(&document.script_ref)?,
                        record.script_resource_id.clone(),
                    )
                    .is_some()
            {
                return Err(IndexerError::Script("duplicate_script_document"));
            }
            documents.push(record);
        }

        let mut symbols = Vec::with_capacity(graph.symbols.len());
        let mut symbol_owner = BTreeMap::new();
        for symbol in &graph.symbols {
            let script_resource_id = canonical_script_resource_id(
                &symbol.script_ref,
                &symbol.declaration_range.path,
                &symbol.declaration_range.content_sha256,
            )?;
            if !document_by_id.contains_key(&script_resource_id)
                || symbol.script_graph_revision != graph.script_graph_revision
                || symbol_owner
                    .insert(symbol.symbol_id.clone(), script_resource_id.clone())
                    .is_some()
            {
                return Err(IndexerError::Script("script_symbol_document_mismatch"));
            }
            symbols.push(store::ScriptSymbol {
                symbol_id: symbol.symbol_id.clone(),
                script_resource_id,
                language: language(symbol.language),
                kind: symbol_kind(symbol.kind),
                name: symbol.name.clone(),
                qualified_key: symbol.qualified_key.clone(),
                owner_symbol_id: symbol.owner_symbol_id.clone(),
                identity_scope: identity_scope(symbol.identity_scope),
                signature: symbol.signature.clone(),
                type_name: symbol.type_name.clone(),
                type_state: type_state(symbol.type_state),
                visibility: visibility(symbol.visibility),
                modifiers: symbol.modifiers.iter().copied().map(modifier).collect(),
                declaration_range: source_range(&symbol.declaration_range),
                documentation_present: symbol.documentation_present,
                script_graph_revision: symbol.script_graph_revision,
            });
        }

        let scene_nodes: BTreeSet<_> = base
            .scene
            .nodes
            .iter()
            .map(|node| node.node_entity_id.as_str())
            .collect();
        let mut relations =
            Vec::with_capacity(graph.relations.len().saturating_add(base.scene.nodes.len()));
        for relation in &graph.relations {
            let script_resource_id = relation_owner(
                relation,
                &symbol_owner,
                &document_by_id,
                &document_by_source,
                &resources,
            )?;
            let document = document_by_id
                .get(&script_resource_id)
                .ok_or(IndexerError::Script("script_relation_document_missing"))?;
            if let Some(range) = &relation.evidence_range
                && (range.path != document.path || range.content_sha256 != document.content_sha256)
            {
                return Err(IndexerError::Script("script_relation_evidence_mismatch"));
            }
            let predicate = predicate(relation.predicate);
            let target = relation
                .target
                .as_ref()
                .map(|target| endpoint(target, &resources, &symbol_owner, &scene_nodes))
                .transpose()?;
            let authority = if relation.confidence == bridge::ScriptConfidence::Exact
                && matches!(
                    predicate,
                    store::ScriptPredicate::Preloads | store::ScriptPredicate::Loads
                )
                && matches!(&target, Some(store::ScriptEndpoint::Resource { .. }))
            {
                store::ScriptRelationAuthority::ResourceGraph
            } else {
                relation_authority(relation.authority)
            };
            let record = relation_record(
                script_resource_id,
                endpoint(&relation.source, &resources, &symbol_owner, &scene_nodes)?,
                predicate,
                target,
                confidence(relation.confidence),
                relation.evidence_range.as_ref().map(source_range),
                relation.detail.clone(),
                authority,
                graph.script_graph_revision,
            )?;
            relations.push(record);
        }

        append_scene_attachments(
            base,
            &resources,
            &document_by_id,
            &symbols,
            graph.script_graph_revision,
            &mut relations,
        )?;

        let references = relations
            .iter()
            .filter_map(|relation| {
                relation
                    .target
                    .clone()
                    .map(|target| store::ScriptReference {
                        reference_id: semantic_id(
                            "godot:script-reference:v1:",
                            REFERENCE_DOMAIN,
                            &[&relation.relation_id],
                        ),
                        relation_id: relation.relation_id.clone(),
                        script_resource_id: relation.script_resource_id.clone(),
                        source: relation.source.clone(),
                        predicate: relation.predicate,
                        target,
                        authority: relation.authority,
                        script_graph_revision: relation.script_graph_revision,
                    })
            })
            .collect();

        let mut diagnostics = Vec::with_capacity(graph.diagnostics.len());
        for diagnostic in &graph.diagnostics {
            let script_resource_id = document_by_ref
                .get(&resource_ref_key(&diagnostic.script_ref)?)
                .ok_or(IndexerError::Script("script_diagnostic_document_missing"))?
                .clone();
            if document_by_id
                .get(&script_resource_id)
                .is_none_or(|document| document.content_sha256 != diagnostic.content_sha256)
            {
                return Err(IndexerError::Script("script_diagnostic_document_missing"));
            }
            diagnostics.push(store::ScriptDiagnostic {
                diagnostic_id: diagnostic.diagnostic_id.clone(),
                script_resource_id,
                language: language(diagnostic.language),
                content_sha256: diagnostic.content_sha256.clone(),
                code: diagnostic.code.clone(),
                severity: diagnostic_severity(diagnostic.severity),
                safe_message: diagnostic.safe_message.clone(),
                range: diagnostic.range.as_ref().map(source_range),
                authority: diagnostic_authority(diagnostic.authority),
                script_graph_revision: diagnostic.script_graph_revision,
            });
        }

        let adapter_statuses = graph
            .adapter_statuses
            .iter()
            .map(|status| store::ScriptAdapterStatus {
                language: language(status.language),
                availability: adapter_availability(status.availability),
                profile: status.profile.map(adapter_profile),
                version: status.version.clone(),
                diagnostic: status.diagnostic.clone(),
            })
            .collect();
        let mut domain = store::ScriptDomainGeneration {
            editor_session_id: editor_session_id.to_owned(),
            resource_revision: graph.resource_revision,
            scene_graph_revision: joined_scene_revision,
            script_graph_revision: graph.script_graph_revision,
            source_complete: true,
            snapshot_checksum: input_checksum(graph)?,
            semantic_digest: graph.semantic_digest.clone(),
            documents,
            symbols,
            relations,
            references,
            diagnostics,
            adapter_statuses,
            validation_digest: String::new(),
        };
        domain.canonicalize();
        domain.validation_digest = domain.compute_validation_digest();
        domain.validate()?;
        Ok(domain)
    }
}

struct ResourceLookup<'a> {
    by_uid: BTreeMap<&'a str, &'a store::ResourceEntity>,
    by_path: BTreeMap<&'a str, &'a store::ResourceEntity>,
    by_id: BTreeMap<&'a str, &'a store::ResourceEntity>,
}

impl<'a> ResourceLookup<'a> {
    fn new(base: &'a store::IndexGeneration) -> Result<Self, IndexerError> {
        let mut lookup = Self {
            by_uid: BTreeMap::new(),
            by_path: BTreeMap::new(),
            by_id: BTreeMap::new(),
        };
        for resource in &base.resources {
            if lookup
                .by_path
                .insert(&resource.comparison_path, resource)
                .is_some()
                || lookup.by_id.insert(&resource.entity_id, resource).is_some()
                || resource
                    .uid
                    .as_deref()
                    .is_some_and(|uid| lookup.by_uid.insert(uid, resource).is_some())
            {
                return Err(IndexerError::Script("script_resource_lookup_collision"));
            }
        }
        Ok(lookup)
    }

    fn find(
        &self,
        reference: &bridge::ResourceRef,
    ) -> Result<Option<&'a store::ResourceEntity>, IndexerError> {
        Ok(match reference {
            bridge::ResourceRef::Uid(reference) => self
                .by_uid
                .get(reference.uid.as_str())
                .copied()
                .or_else(|| {
                    canonical_godot_uid(&reference.uid)
                        .and_then(|uid| self.by_uid.get(uid.as_str()).copied())
                }),
            bridge::ResourceRef::Path(reference) => {
                let path = normalize_resource_path(&reference.path)?;
                self.by_path.get(path.comparison.as_str()).copied()
            }
        })
    }

    fn resolve(
        &self,
        reference: &bridge::ResourceRef,
    ) -> Result<&'a store::ResourceEntity, IndexerError> {
        self.find(reference)?
            .ok_or(IndexerError::Script("script_resource_target_missing"))
    }
}

fn resource_ref_key(reference: &bridge::ResourceRef) -> Result<String, IndexerError> {
    match reference {
        bridge::ResourceRef::Uid(reference) => Ok(format!("uid:{}", reference.uid)),
        bridge::ResourceRef::Path(reference) => {
            normalize_resource_path(&reference.path).map(|path| format!("path:{}", path.comparison))
        }
    }
}

/// Reproduces `ResourceUID::text_to_id` followed by `id_to_text`. Godot accepts
/// non-canonical lowercase UID spellings in source files, while the resource
/// catalog always publishes the canonical spelling of the same 63-bit ID.
fn canonical_godot_uid(value: &str) -> Option<String> {
    let payload = value.strip_prefix("uid://")?;
    if payload.is_empty() {
        return None;
    }
    let mut id = 0_u64;
    for byte in payload.bytes() {
        let digit = match byte {
            b'a'..=b'z' => u64::from(byte - b'a'),
            b'0'..=b'9' => u64::from(byte - b'0') + 25,
            _ => return None,
        };
        id = id.wrapping_mul(GODOT_UID_BASE).wrapping_add(digit);
    }
    id &= GODOT_UID_MASK;

    let mut encoded = Vec::with_capacity(13);
    loop {
        let digit = usize::try_from(id % GODOT_UID_BASE).ok()?;
        encoded.push(GODOT_UID_ALPHABET[digit]);
        id /= GODOT_UID_BASE;
        if id == 0 {
            break;
        }
    }
    encoded.reverse();
    String::from_utf8(encoded)
        .ok()
        .map(|payload| format!("uid://{payload}"))
}

fn relation_owner(
    relation: &bridge::ScriptRelation,
    symbol_owner: &BTreeMap<String, String>,
    documents: &BTreeMap<String, store::ScriptDocument>,
    documents_by_source: &BTreeMap<(String, String), String>,
    resources: &ResourceLookup<'_>,
) -> Result<String, IndexerError> {
    if let bridge::ScriptRelationEndpoint::Symbol { symbol_id } = &relation.source
        && let Some(owner) = symbol_owner.get(symbol_id)
    {
        return Ok(owner.clone());
    }
    if let bridge::ScriptRelationEndpoint::Resource { resource_ref } = &relation.source {
        let id = &resources.resolve(resource_ref)?.entity_id;
        if documents.contains_key(id) {
            return Ok((*id).clone());
        }
    }
    if let Some(range) = &relation.evidence_range
        && let Some(owner) =
            documents_by_source.get(&(range.path.clone(), range.content_sha256.clone()))
    {
        return Ok(owner.clone());
    }
    if let Some(bridge::ScriptRelationEndpoint::Symbol { symbol_id }) = &relation.target
        && let Some(owner) = symbol_owner.get(symbol_id)
    {
        return Ok(owner.clone());
    }
    Err(IndexerError::Script("script_relation_owner_missing"))
}

fn endpoint(
    endpoint: &bridge::ScriptRelationEndpoint,
    resources: &ResourceLookup<'_>,
    symbols: &BTreeMap<String, String>,
    scene_nodes: &BTreeSet<&str>,
) -> Result<store::ScriptEndpoint, IndexerError> {
    match endpoint {
        bridge::ScriptRelationEndpoint::Symbol { symbol_id } => {
            if !symbols.contains_key(symbol_id) {
                return Err(IndexerError::Script("script_relation_symbol_missing"));
            }
            Ok(store::ScriptEndpoint::Symbol {
                symbol_id: symbol_id.clone(),
            })
        }
        bridge::ScriptRelationEndpoint::Resource { resource_ref } => {
            Ok(store::ScriptEndpoint::Resource {
                resource_entity_id: resources.resolve(resource_ref)?.entity_id.clone(),
            })
        }
        bridge::ScriptRelationEndpoint::SceneNode { node_id } => {
            if !scene_nodes.contains(node_id.as_str()) {
                return Err(IndexerError::Script("script_relation_scene_node_missing"));
            }
            Ok(store::ScriptEndpoint::SceneNode {
                node_entity_id: node_id.clone(),
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn relation_record(
    script_resource_id: String,
    source: store::ScriptEndpoint,
    predicate: store::ScriptPredicate,
    target: Option<store::ScriptEndpoint>,
    confidence: store::ScriptConfidence,
    evidence_range: Option<store::ScriptSourceRange>,
    detail: Option<String>,
    authority: store::ScriptRelationAuthority,
    script_graph_revision: u64,
) -> Result<store::ScriptRelation, IndexerError> {
    let semantic = serde_json::to_string(&(
        &script_resource_id,
        &source,
        predicate,
        &target,
        confidence,
        &evidence_range,
        &detail,
        authority,
    ))
    .map_err(|_| IndexerError::Serialization)?;
    Ok(store::ScriptRelation {
        relation_id: semantic_id("godot:script-relation:v1:", RELATION_DOMAIN, &[&semantic]),
        script_resource_id,
        source,
        predicate,
        target,
        confidence,
        evidence_range,
        detail,
        authority,
        script_graph_revision,
    })
}

fn append_scene_attachments(
    base: &store::IndexGeneration,
    resources: &ResourceLookup<'_>,
    documents: &BTreeMap<String, store::ScriptDocument>,
    symbols: &[store::ScriptSymbol],
    script_graph_revision: u64,
    output: &mut Vec<store::ScriptRelation>,
) -> Result<(), IndexerError> {
    if base.scene.is_empty() {
        return Ok(());
    }
    let mut root_class = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    for symbol in symbols.iter().filter(|symbol| {
        symbol.kind == store::ScriptSymbolKind::Class && symbol.owner_symbol_id.is_none()
    }) {
        if root_class
            .insert(symbol.script_resource_id.clone(), symbol.symbol_id.clone())
            .is_some()
        {
            ambiguous.insert(symbol.script_resource_id.clone());
        }
    }
    for document in &ambiguous {
        root_class.remove(document);
    }

    for node in &base.scene.nodes {
        let Some(script_resource_id) = node.attached_script_entity_id.as_ref() else {
            continue;
        };
        let Some(document) = documents.get(script_resource_id) else {
            let is_script = resources
                .by_id
                .get(script_resource_id.as_str())
                .is_some_and(|resource| {
                    resource.comparison_path.ends_with(".gd")
                        || resource.comparison_path.ends_with(".cs")
                });
            if is_script {
                return Err(IndexerError::Script("attachment_script_document_missing"));
            }
            continue;
        };
        let target = root_class.get(script_resource_id).map_or_else(
            || store::ScriptEndpoint::Resource {
                resource_entity_id: script_resource_id.clone(),
            },
            |symbol_id| store::ScriptEndpoint::Symbol {
                symbol_id: symbol_id.clone(),
            },
        );
        output.push(relation_record(
            document.script_resource_id.clone(),
            store::ScriptEndpoint::SceneNode {
                node_entity_id: node.node_entity_id.clone(),
            },
            store::ScriptPredicate::AttachesScript,
            Some(target),
            store::ScriptConfidence::Exact,
            None,
            Some("saved_scene_attachment".to_owned()),
            store::ScriptRelationAuthority::SceneState,
            script_graph_revision,
        )?);
    }
    Ok(())
}

fn input_checksum(graph: &bridge::NormalizedScriptGraph) -> Result<String, IndexerError> {
    let bytes = serde_json::to_vec(graph).map_err(|_| IndexerError::Serialization)?;
    let mut hasher = Sha256::new();
    hasher.update(INPUT_DOMAIN);
    hasher.update(bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn semantic_id(prefix: &str, domain: &str, parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            hasher.update([0]);
        }
        hasher.update(part.as_bytes());
    }
    format!(
        "{prefix}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
    )
}

fn canonical_script_resource_id(
    reference: &bridge::ResourceRef,
    path: &str,
    content_sha256: &str,
) -> Result<String, IndexerError> {
    bridge::canonical_script_resource_id(reference, path, content_sha256)
        .map_err(|_| IndexerError::Script("invalid_script_resource_identity"))
}

fn source_range(value: &bridge::SourceRange) -> store::ScriptSourceRange {
    store::ScriptSourceRange {
        path: value.path.clone(),
        content_sha256: value.content_sha256.clone(),
        start_byte: value.start_byte,
        end_byte: value.end_byte,
        start_line: value.start_line,
        start_column: value.start_column,
        end_line: value.end_line,
        end_column: value.end_column,
    }
}

fn language(value: bridge::ScriptLanguage) -> store::ScriptLanguage {
    match value {
        bridge::ScriptLanguage::Gdscript => store::ScriptLanguage::Gdscript,
        bridge::ScriptLanguage::Csharp => store::ScriptLanguage::Csharp,
    }
}

fn adapter_profile(value: bridge::ScriptAdapterProfile) -> store::ScriptAdapterProfile {
    match value {
        bridge::ScriptAdapterProfile::GdscriptParserAnalyzerV1 => {
            store::ScriptAdapterProfile::GdscriptParserAnalyzerV1
        }
        bridge::ScriptAdapterProfile::CsharpDiscoveryOnlyV1 => {
            store::ScriptAdapterProfile::CsharpDiscoveryOnlyV1
        }
    }
}

fn completeness(value: bridge::ScriptCompleteness) -> store::ScriptCompleteness {
    match value {
        bridge::ScriptCompleteness::Complete => store::ScriptCompleteness::Complete,
        bridge::ScriptCompleteness::Partial => store::ScriptCompleteness::Partial,
        bridge::ScriptCompleteness::Invalid => store::ScriptCompleteness::Invalid,
        bridge::ScriptCompleteness::Unavailable => store::ScriptCompleteness::Unavailable,
    }
}

fn identity_scope(value: bridge::ScriptIdentityScope) -> store::ScriptIdentityScope {
    match value {
        bridge::ScriptIdentityScope::Persistent => store::ScriptIdentityScope::Persistent,
        bridge::ScriptIdentityScope::ContentRevision => store::ScriptIdentityScope::ContentRevision,
    }
}

fn symbol_kind(value: bridge::ScriptSymbolKind) -> store::ScriptSymbolKind {
    match value {
        bridge::ScriptSymbolKind::Script => store::ScriptSymbolKind::Script,
        bridge::ScriptSymbolKind::Class => store::ScriptSymbolKind::Class,
        bridge::ScriptSymbolKind::Method => store::ScriptSymbolKind::Method,
        bridge::ScriptSymbolKind::Function => store::ScriptSymbolKind::Function,
        bridge::ScriptSymbolKind::Property => store::ScriptSymbolKind::Property,
        bridge::ScriptSymbolKind::Constant => store::ScriptSymbolKind::Constant,
        bridge::ScriptSymbolKind::Enum => store::ScriptSymbolKind::Enum,
        bridge::ScriptSymbolKind::EnumMember => store::ScriptSymbolKind::EnumMember,
        bridge::ScriptSymbolKind::Signal => store::ScriptSymbolKind::Signal,
        bridge::ScriptSymbolKind::Parameter => store::ScriptSymbolKind::Parameter,
        bridge::ScriptSymbolKind::Local => store::ScriptSymbolKind::Local,
        bridge::ScriptSymbolKind::Lambda => store::ScriptSymbolKind::Lambda,
    }
}

fn type_state(value: bridge::ScriptTypeState) -> store::ScriptTypeState {
    match value {
        bridge::ScriptTypeState::Explicit => store::ScriptTypeState::Explicit,
        bridge::ScriptTypeState::Inferred => store::ScriptTypeState::Inferred,
        bridge::ScriptTypeState::Dynamic => store::ScriptTypeState::Dynamic,
        bridge::ScriptTypeState::Unavailable => store::ScriptTypeState::Unavailable,
    }
}

fn visibility(value: bridge::ScriptVisibility) -> store::ScriptVisibility {
    match value {
        bridge::ScriptVisibility::Public => store::ScriptVisibility::Public,
        bridge::ScriptVisibility::Protected => store::ScriptVisibility::Protected,
        bridge::ScriptVisibility::Private => store::ScriptVisibility::Private,
        bridge::ScriptVisibility::Internal => store::ScriptVisibility::Internal,
    }
}

fn modifier(value: bridge::ScriptModifier) -> store::ScriptModifier {
    match value {
        bridge::ScriptModifier::Abstract => store::ScriptModifier::Abstract,
        bridge::ScriptModifier::Const => store::ScriptModifier::Const,
        bridge::ScriptModifier::Exported => store::ScriptModifier::Exported,
        bridge::ScriptModifier::Override => store::ScriptModifier::Override,
        bridge::ScriptModifier::Static => store::ScriptModifier::Static,
        bridge::ScriptModifier::Tool => store::ScriptModifier::Tool,
        bridge::ScriptModifier::Virtual => store::ScriptModifier::Virtual,
    }
}

fn predicate(value: bridge::ScriptRelationPredicate) -> store::ScriptPredicate {
    match value {
        bridge::ScriptRelationPredicate::Contains => store::ScriptPredicate::Contains,
        bridge::ScriptRelationPredicate::DeclaresSymbol => store::ScriptPredicate::DeclaresSymbol,
        bridge::ScriptRelationPredicate::Inherits => store::ScriptPredicate::Inherits,
        bridge::ScriptRelationPredicate::Overrides => store::ScriptPredicate::Overrides,
        bridge::ScriptRelationPredicate::ReferencesSymbol => {
            store::ScriptPredicate::ReferencesSymbol
        }
        bridge::ScriptRelationPredicate::Calls => store::ScriptPredicate::Calls,
        bridge::ScriptRelationPredicate::Preloads => store::ScriptPredicate::Preloads,
        bridge::ScriptRelationPredicate::Loads => store::ScriptPredicate::Loads,
        bridge::ScriptRelationPredicate::AttachesScript => store::ScriptPredicate::AttachesScript,
    }
}

fn confidence(value: bridge::ScriptConfidence) -> store::ScriptConfidence {
    match value {
        bridge::ScriptConfidence::Exact => store::ScriptConfidence::Exact,
        bridge::ScriptConfidence::Dynamic => store::ScriptConfidence::Dynamic,
    }
}

fn relation_authority(value: bridge::ScriptRelationAuthority) -> store::ScriptRelationAuthority {
    match value {
        bridge::ScriptRelationAuthority::GdscriptParserAnalyzer => {
            store::ScriptRelationAuthority::GdscriptParserAnalyzer
        }
        bridge::ScriptRelationAuthority::ResourceGraph => {
            store::ScriptRelationAuthority::ResourceGraph
        }
        bridge::ScriptRelationAuthority::SceneState => store::ScriptRelationAuthority::SceneState,
        bridge::ScriptRelationAuthority::CsharpAdapter => {
            store::ScriptRelationAuthority::CsharpAdapter
        }
    }
}

fn diagnostic_severity(value: bridge::ScriptDiagnosticSeverity) -> store::ScriptDiagnosticSeverity {
    match value {
        bridge::ScriptDiagnosticSeverity::Error => store::ScriptDiagnosticSeverity::Error,
        bridge::ScriptDiagnosticSeverity::Warning => store::ScriptDiagnosticSeverity::Warning,
        bridge::ScriptDiagnosticSeverity::Info => store::ScriptDiagnosticSeverity::Info,
        bridge::ScriptDiagnosticSeverity::Hint => store::ScriptDiagnosticSeverity::Hint,
    }
}

fn diagnostic_authority(
    value: bridge::ScriptDiagnosticAuthority,
) -> store::ScriptDiagnosticAuthority {
    match value {
        bridge::ScriptDiagnosticAuthority::GdscriptParser => {
            store::ScriptDiagnosticAuthority::GdscriptParser
        }
        bridge::ScriptDiagnosticAuthority::GdscriptAnalyzer => {
            store::ScriptDiagnosticAuthority::GdscriptAnalyzer
        }
        bridge::ScriptDiagnosticAuthority::CsharpAdapter => {
            store::ScriptDiagnosticAuthority::CsharpAdapter
        }
        bridge::ScriptDiagnosticAuthority::ScriptAdapter => {
            store::ScriptDiagnosticAuthority::ScriptAdapter
        }
    }
}

fn adapter_availability(
    value: bridge::ScriptAdapterAvailability,
) -> store::ScriptAdapterAvailability {
    match value {
        bridge::ScriptAdapterAvailability::Available => store::ScriptAdapterAvailability::Available,
        bridge::ScriptAdapterAvailability::DiscoveryOnly => {
            store::ScriptAdapterAvailability::DiscoveryOnly
        }
        bridge::ScriptAdapterAvailability::Unavailable => {
            store::ScriptAdapterAvailability::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use godot_codex_bridge_client::{
        LanguageAdapterStatus, ResourceRef, ScriptAdapterAvailability, ScriptAdapterProfile,
        ScriptCompleteness, ScriptConfidence, ScriptIdentityScope, ScriptLanguage, ScriptModifier,
        ScriptRelation, ScriptRelationAuthority, ScriptRelationEndpoint, ScriptRelationPredicate,
        ScriptSymbol, ScriptSymbolKind, ScriptTypeState, ScriptVisibility, UidResourceRef,
        canonical_named_symbol_id,
    };
    use godot_codex_index_store::{
        GenerationState, IdentityStrength, IndexGeneration, IngestionCheckpoint, RecordValidity,
        ResourceEntity, SceneDomainGeneration, SceneEntity, SceneIdentityScope, SceneNode,
        SchemaVersion, ScriptEndpoint, ScriptPredicate, SegmentStore,
    };
    use tempfile::TempDir;

    const SESSION: &str = "session-script";

    fn resource(uid: &str, path: &str, hash: &str, resource_type: &str) -> ResourceEntity {
        ResourceEntity {
            entity_id: crate::uid_entity_id(uid).expect("resource id"),
            identity_input: uid.to_owned(),
            uid: Some(uid.to_owned()),
            display_path: path.to_owned(),
            comparison_path: path.to_owned(),
            identity_strength: IdentityStrength::ResourceUid,
            resource_type: resource_type.to_owned(),
            source_kind: "source".to_owned(),
            import_state: "valid".to_owned(),
            authority: "editor_file_system".to_owned(),
            content_generation: Some(hash.to_owned()),
            mtime_ns: 1,
            byte_size: 1,
            validity: RecordValidity::Valid,
            resource_revision: 1,
        }
    }

    fn base_generation() -> IndexGeneration {
        let player = resource(
            "uid://player",
            "res://scripts/player.gd",
            &format!("sha256:{}", "a".repeat(64)),
            "GDScript",
        );
        let base = resource(
            "uid://base",
            "res://scripts/base.gd",
            &format!("sha256:{}", "b".repeat(64)),
            "GDScript",
        );
        let damage = resource(
            "uid://damage",
            "res://resources/damage.tres",
            &format!("sha256:{}", "c".repeat(64)),
            "Resource",
        );
        let scene_resource = resource(
            "uid://scene",
            "res://scenes/main.tscn",
            &format!("sha256:{}", "d".repeat(64)),
            "PackedScene",
        );
        let csharp = resource(
            "uid://enemy-csharp",
            "res://scripts/Enemy.cs",
            &format!("sha256:{}", "e".repeat(64)),
            "CSharpScript",
        );
        let scene_id = "godot:scene:uid:v1:test".to_owned();
        let node_id = "godot:node-definition:v1:player".to_owned();
        let mut scene = SceneDomainGeneration {
            editor_session_id: SESSION.to_owned(),
            resource_revision: 1,
            scene_graph_revision: 2,
            source_complete: true,
            snapshot_checksum: format!("sha256:{}", "e".repeat(64)),
            scenes: vec![SceneEntity {
                scene_entity_id: scene_id.clone(),
                source_resource_entity_id: Some(scene_resource.entity_id.clone()),
                uid: Some("uid://scene".to_owned()),
                comparison_path: "res://scenes/main.tscn".to_owned(),
                content_generation: format!("sha256:{}", "d".repeat(64)),
                identity_scope: SceneIdentityScope::Persistent,
                base_scene_entity_id: None,
                authority: "godot_scene_state".to_owned(),
                resource_revision: 1,
                scene_graph_revision: 2,
            }],
            nodes: vec![
                SceneNode {
                    node_entity_id: node_id.clone(),
                    scene_entity_id: scene_id.clone(),
                    node_path: ".".to_owned(),
                    identity_scope: SceneIdentityScope::Persistent,
                    unique_scene_id: Some(1),
                    parent_node_entity_id: None,
                    owner_node_entity_id: None,
                    name: "Player".to_owned(),
                    godot_type: "Node".to_owned(),
                    node_index: 0,
                    owned: true,
                    internal: false,
                    attached_script_entity_id: Some(player.entity_id.clone()),
                    instance_scene_entity_id: None,
                    authority: "godot_scene_state".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 2,
                },
                SceneNode {
                    node_entity_id: "godot:node-definition:v1:enemy".to_owned(),
                    scene_entity_id: scene_id,
                    node_path: "Enemy".to_owned(),
                    identity_scope: SceneIdentityScope::Persistent,
                    unique_scene_id: Some(2),
                    parent_node_entity_id: Some(node_id),
                    owner_node_entity_id: None,
                    name: "Enemy".to_owned(),
                    godot_type: "Node".to_owned(),
                    node_index: 1,
                    owned: true,
                    internal: false,
                    attached_script_entity_id: Some(csharp.entity_id.clone()),
                    instance_scene_entity_id: None,
                    authority: "godot_scene_state".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 2,
                },
            ],
            properties: Vec::new(),
            relations: Vec::new(),
            connections: Vec::new(),
            groups: Vec::new(),
            animations: Vec::new(),
            validation_digest: String::new(),
        };
        scene.validation_digest = scene.compute_validation_digest();
        IndexGeneration {
            generation_id: "generation-script-base".to_owned(),
            parent_generation_id: None,
            schema_version: SchemaVersion { major: 1, minor: 3 },
            project_id: "project-script".to_owned(),
            index_revision: 1,
            state: GenerationState::Active,
            creation_reason: "fixture".to_owned(),
            checkpoint: IngestionCheckpoint {
                editor_session_id: SESSION.to_owned(),
                resource_revision: 1,
                project_revision: 1,
                index_revision: 1,
                source_complete: true,
                snapshot_checksum: format!("sha256:{}", "f".repeat(64)),
                last_batch_id: None,
                last_batch_checksum: None,
            },
            resources: vec![player, base, damage, scene_resource, csharp],
            source_documents: Vec::new(),
            dependencies: Vec::new(),
            diagnostics: Vec::new(),
            tombstones: Vec::new(),
            scene,
            script: store::ScriptDomainGeneration::default(),
            validation_digest: String::new(),
        }
    }

    fn uid(value: &str) -> ResourceRef {
        ResourceRef::Uid(UidResourceRef {
            uid: value.to_owned(),
        })
    }

    fn range(path: &str, hash: &str, start: u64, end: u64) -> bridge::SourceRange {
        bridge::SourceRange {
            path: path.to_owned(),
            content_sha256: hash.to_owned(),
            start_byte: start,
            end_byte: end,
            start_line: 1,
            start_column: u32::try_from(start + 1).expect("column"),
            end_line: 1,
            end_column: u32::try_from(end + 1).expect("column"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn symbol(
        script_ref: ResourceRef,
        script_id: &str,
        path: &str,
        hash: &str,
        kind: ScriptSymbolKind,
        name: &str,
        qualified_key: &str,
        owner: Option<String>,
        start: u64,
    ) -> ScriptSymbol {
        ScriptSymbol {
            symbol_id: canonical_named_symbol_id(
                script_id,
                ScriptLanguage::Gdscript,
                kind,
                qualified_key,
            ),
            script_ref,
            language: ScriptLanguage::Gdscript,
            kind,
            name: Some(name.to_owned()),
            qualified_key: qualified_key.to_owned(),
            owner_symbol_id: owner,
            identity_scope: ScriptIdentityScope::Persistent,
            signature: None,
            type_name: None,
            type_state: ScriptTypeState::Unavailable,
            visibility: ScriptVisibility::Public,
            modifiers: if kind == ScriptSymbolKind::Method {
                vec![ScriptModifier::Virtual]
            } else {
                Vec::new()
            },
            declaration_range: range(path, hash, start, start + 5),
            documentation_present: false,
            script_graph_revision: 3,
        }
    }

    fn normalized_graph(base: &IndexGeneration) -> bridge::NormalizedScriptGraph {
        let player_hash = format!("sha256:{}", "a".repeat(64));
        let base_hash = format!("sha256:{}", "b".repeat(64));
        let player_id = base.resources[0].entity_id.clone();
        let base_id = base.resources[1].entity_id.clone();
        let player_ref = uid("uid://player");
        let base_ref = uid("uid://base");
        let base_class = symbol(
            base_ref.clone(),
            &base_id,
            "res://scripts/base.gd",
            &base_hash,
            ScriptSymbolKind::Class,
            "Base",
            "class:Base",
            None,
            0,
        );
        let base_method = symbol(
            base_ref.clone(),
            &base_id,
            "res://scripts/base.gd",
            &base_hash,
            ScriptSymbolKind::Method,
            "tick",
            "class:Base/method:tick",
            Some(base_class.symbol_id.clone()),
            6,
        );
        let player_class = symbol(
            player_ref.clone(),
            &player_id,
            "res://scripts/player.gd",
            &player_hash,
            ScriptSymbolKind::Class,
            "Player",
            "class:Player",
            None,
            0,
        );
        let player_method = symbol(
            player_ref.clone(),
            &player_id,
            "res://scripts/player.gd",
            &player_hash,
            ScriptSymbolKind::Method,
            "tick",
            "class:Player/method:tick",
            Some(player_class.symbol_id.clone()),
            6,
        );
        let relation = |source: &str,
                        predicate,
                        target: Option<ScriptRelationEndpoint>,
                        confidence,
                        detail: &str|
         -> ScriptRelation {
            ScriptRelation {
                source: ScriptRelationEndpoint::Symbol {
                    symbol_id: source.to_owned(),
                },
                predicate,
                target,
                confidence,
                evidence_range: Some(range("res://scripts/player.gd", &player_hash, 12, 16)),
                detail: Some(detail.to_owned()),
                authority: ScriptRelationAuthority::GdscriptParserAnalyzer,
                script_graph_revision: 3,
            }
        };
        bridge::NormalizedScriptGraph {
            resource_revision: 1,
            scene_graph_revision: 2,
            script_graph_revision: 3,
            documents: vec![
                bridge::ScriptDocument {
                    script_ref: player_ref,
                    path: "res://scripts/player.gd".to_owned(),
                    language: ScriptLanguage::Gdscript,
                    content_sha256: player_hash.clone(),
                    adapter_profile: ScriptAdapterProfile::GdscriptParserAnalyzerV1,
                    completeness: ScriptCompleteness::Complete,
                    resource_revision: 1,
                    script_graph_revision: 3,
                },
                bridge::ScriptDocument {
                    script_ref: base_ref,
                    path: "res://scripts/base.gd".to_owned(),
                    language: ScriptLanguage::Gdscript,
                    content_sha256: base_hash,
                    adapter_profile: ScriptAdapterProfile::GdscriptParserAnalyzerV1,
                    completeness: ScriptCompleteness::Complete,
                    resource_revision: 1,
                    script_graph_revision: 3,
                },
                bridge::ScriptDocument {
                    script_ref: uid("uid://enemy-csharp"),
                    path: "res://scripts/Enemy.cs".to_owned(),
                    language: ScriptLanguage::Csharp,
                    content_sha256: format!("sha256:{}", "e".repeat(64)),
                    adapter_profile: ScriptAdapterProfile::CsharpDiscoveryOnlyV1,
                    completeness: ScriptCompleteness::Unavailable,
                    resource_revision: 1,
                    script_graph_revision: 3,
                },
            ],
            symbols: vec![
                base_class.clone(),
                base_method.clone(),
                player_class.clone(),
                player_method.clone(),
            ],
            relations: vec![
                relation(
                    &player_class.symbol_id,
                    ScriptRelationPredicate::Inherits,
                    Some(ScriptRelationEndpoint::Symbol {
                        symbol_id: base_class.symbol_id,
                    }),
                    ScriptConfidence::Exact,
                    "script_inheritance",
                ),
                relation(
                    &player_method.symbol_id,
                    ScriptRelationPredicate::Calls,
                    Some(ScriptRelationEndpoint::Symbol {
                        symbol_id: base_method.symbol_id,
                    }),
                    ScriptConfidence::Exact,
                    "direct_call",
                ),
                relation(
                    &player_method.symbol_id,
                    ScriptRelationPredicate::Loads,
                    Some(ScriptRelationEndpoint::Resource {
                        resource_ref: uid("uid://damage"),
                    }),
                    ScriptConfidence::Exact,
                    "literal_uid",
                ),
                relation(
                    &player_method.symbol_id,
                    ScriptRelationPredicate::Calls,
                    None,
                    ScriptConfidence::Dynamic,
                    "variant_method_name",
                ),
            ],
            diagnostics: Vec::new(),
            adapter_statuses: vec![
                LanguageAdapterStatus {
                    language: ScriptLanguage::Gdscript,
                    availability: ScriptAdapterAvailability::Available,
                    profile: Some(ScriptAdapterProfile::GdscriptParserAnalyzerV1),
                    version: Some("4.8".to_owned()),
                    diagnostic: None,
                },
                LanguageAdapterStatus {
                    language: ScriptLanguage::Csharp,
                    availability: ScriptAdapterAvailability::DiscoveryOnly,
                    profile: Some(ScriptAdapterProfile::CsharpDiscoveryOnlyV1),
                    version: Some("1".to_owned()),
                    diagnostic: None,
                },
            ],
            semantic_digest: format!("sha256:{}", "9".repeat(64)),
        }
    }

    #[test]
    fn joins_inheritance_calls_loads_and_scene_attachments_exactly() {
        let base = base_generation();
        let graph = normalized_graph(&base);
        let domain = ScriptNormalizer
            .normalize_graph(&base, SESSION, &graph)
            .expect("composed script domain");

        assert_eq!(domain.relations.len(), 6);
        assert_eq!(domain.references.len(), 5);
        assert_eq!(
            domain
                .relations
                .iter()
                .filter(|relation| relation.confidence == store::ScriptConfidence::Dynamic)
                .count(),
            1
        );
        let load = domain
            .relations
            .iter()
            .find(|relation| relation.predicate == ScriptPredicate::Loads)
            .expect("load relation");
        assert_eq!(
            load.target,
            Some(ScriptEndpoint::Resource {
                resource_entity_id: base.resources[2].entity_id.clone()
            })
        );
        assert_eq!(
            load.authority,
            store::ScriptRelationAuthority::ResourceGraph
        );
        let attachment = domain
            .relations
            .iter()
            .find(|relation| {
                relation.predicate == ScriptPredicate::AttachesScript
                    && relation.script_resource_id == base.resources[0].entity_id
            })
            .expect("attachment relation");
        assert!(matches!(
            attachment.source,
            ScriptEndpoint::SceneNode { .. }
        ));
        assert!(matches!(
            attachment.target,
            Some(ScriptEndpoint::Symbol { .. })
        ));
        let csharp_attachment = domain
            .relations
            .iter()
            .find(|relation| {
                relation.predicate == ScriptPredicate::AttachesScript
                    && relation.script_resource_id == base.resources[4].entity_id
            })
            .expect("C# discovery-only attachment");
        assert_eq!(
            csharp_attachment.target,
            Some(ScriptEndpoint::Resource {
                resource_entity_id: base.resources[4].entity_id.clone()
            })
        );
        domain.validate().expect("valid logical script domain");
    }

    #[test]
    fn attachment_replacement_recomposes_without_changing_producer_revision() {
        let mut base = base_generation();
        let graph = normalized_graph(&base);
        let player = ScriptNormalizer
            .normalize_graph(&base, SESSION, &graph)
            .expect("player attachment");
        base.scene.scene_graph_revision = 3;
        base.scene.nodes[0].scene_graph_revision = 3;
        base.scene.nodes[0].attached_script_entity_id = Some(base.resources[1].entity_id.clone());
        base.scene.validation_digest = base.scene.compute_validation_digest();
        let replaced = ScriptNormalizer
            .normalize_graph(&base, SESSION, &graph)
            .expect("base attachment");

        assert_eq!(replaced.script_graph_revision, player.script_graph_revision);
        assert_eq!(replaced.scene_graph_revision, 3);
        let old_target = player
            .relations
            .iter()
            .find(|relation| {
                relation.predicate == ScriptPredicate::AttachesScript
                    && relation.source
                        == (ScriptEndpoint::SceneNode {
                            node_entity_id: base.scene.nodes[0].node_entity_id.clone(),
                        })
            })
            .and_then(|relation| relation.target.clone());
        let new_target = replaced
            .relations
            .iter()
            .find(|relation| {
                relation.predicate == ScriptPredicate::AttachesScript
                    && relation.source
                        == (ScriptEndpoint::SceneNode {
                            node_entity_id: base.scene.nodes[0].node_entity_id.clone(),
                        })
            })
            .and_then(|relation| relation.target.clone());
        assert_ne!(old_target, new_target);
        assert_eq!(
            replaced
                .relations
                .iter()
                .filter(|relation| relation.predicate == ScriptPredicate::AttachesScript)
                .count(),
            2
        );
    }

    #[test]
    fn missing_exact_resource_target_fails_before_activation() {
        let mut base = base_generation();
        let graph = normalized_graph(&base);
        base.resources.remove(2);
        assert!(matches!(
            ScriptNormalizer.normalize_graph(&base, SESSION, &graph),
            Err(IndexerError::Script("script_resource_target_missing"))
        ));
    }

    #[test]
    fn noncanonical_literal_uid_joins_the_canonical_resource_record() {
        assert_eq!(
            canonical_godot_uid("uid://s5damageprofile").as_deref(),
            Some("uid://b3iupk70ub7mc")
        );
        let mut base = base_generation();
        base.resources[2] = resource(
            "uid://b3iupk70ub7mc",
            "res://resources/damage.tres",
            &format!("sha256:{}", "c".repeat(64)),
            "Resource",
        );
        let mut graph = normalized_graph(&base);
        let load = graph
            .relations
            .iter_mut()
            .find(|relation| relation.predicate == ScriptRelationPredicate::Loads)
            .expect("literal UID load");
        load.target = Some(ScriptRelationEndpoint::Resource {
            resource_ref: uid("uid://s5damageprofile"),
        });

        let domain = ScriptNormalizer
            .normalize_graph(&base, SESSION, &graph)
            .expect("canonical UID join");
        let load = domain
            .relations
            .iter()
            .find(|relation| relation.predicate == ScriptPredicate::Loads)
            .expect("composed load");
        assert_eq!(
            load.target,
            Some(ScriptEndpoint::Resource {
                resource_entity_id: base.resources[2].entity_id.clone()
            })
        );
    }

    #[test]
    fn csharp_discovery_document_survives_without_a_resource_catalog_record() {
        let mut base = base_generation();
        let csharp_resource_id = base.resources[4].entity_id.clone();
        base.resources.remove(4);
        base.scene
            .nodes
            .retain(|node| node.attached_script_entity_id.as_deref() != Some(&csharp_resource_id));
        base.scene.validation_digest = base.scene.compute_validation_digest();
        let mut graph = normalized_graph(&base);
        let csharp = graph
            .documents
            .iter_mut()
            .find(|document| document.language == ScriptLanguage::Csharp)
            .expect("C# graph document");
        csharp.script_ref = ResourceRef::Path(bridge::PathResourceRef {
            uid_missing: true,
            path: csharp.path.clone(),
        });

        let domain = ScriptNormalizer
            .normalize_graph(&base, SESSION, &graph)
            .expect("detached C# discovery document");
        let csharp = domain
            .documents
            .iter()
            .find(|document| document.language == store::ScriptLanguage::Csharp)
            .expect("C# discovery document");
        assert_eq!(csharp.path, "res://scripts/Enemy.cs");
        assert_eq!(csharp.completeness, store::ScriptCompleteness::Unavailable);
        assert!(
            base.resources
                .iter()
                .all(|resource| resource.entity_id != csharp.script_resource_id)
        );
        domain.validate().expect("valid detached C# domain");
    }

    #[test]
    fn unavailable_gdscript_keeps_documents_and_exact_scene_attachments_only() {
        let base = base_generation();
        let mut graph = normalized_graph(&base);
        for document in &mut graph.documents {
            if document.language == ScriptLanguage::Gdscript {
                document.completeness = ScriptCompleteness::Unavailable;
            }
        }
        graph.symbols.clear();
        graph.relations.clear();
        let status = graph
            .adapter_statuses
            .iter_mut()
            .find(|status| status.language == ScriptLanguage::Gdscript)
            .expect("GDScript status");
        status.availability = ScriptAdapterAvailability::Unavailable;
        status.profile = None;
        status.version = None;
        status.diagnostic = Some("GDScript semantic adapter is unavailable.".to_owned());
        graph.semantic_digest = format!("sha256:{}", "8".repeat(64));

        let domain = ScriptNormalizer
            .normalize_graph(&base, SESSION, &graph)
            .expect("unavailable GDScript projection");
        assert!(domain.symbols.is_empty());
        assert_eq!(
            domain
                .relations
                .iter()
                .filter(|relation| relation.predicate == ScriptPredicate::AttachesScript)
                .count(),
            2
        );
        assert!(domain.relations.iter().all(|relation| {
            relation.authority == store::ScriptRelationAuthority::SceneState
                && matches!(relation.target, Some(ScriptEndpoint::Resource { .. }))
        }));
        domain.validate().expect("valid unavailable profile domain");
    }

    #[test]
    fn composed_scene_and_script_domains_activate_together_or_not_at_all() {
        let temp = TempDir::new().expect("temp");
        let mut base = base_generation();
        base.validation_digest = base.compute_validation_digest();
        base.validate().expect("valid base generation");
        let graph = normalized_graph(&base);
        let replacement_script_id = base.resources[1].entity_id.clone();
        let player_node_id = base.scene.nodes[0].node_entity_id.clone();
        let initial_script = ScriptNormalizer
            .normalize_graph(&base, SESSION, &graph)
            .expect("initial script domain");
        base.script = initial_script;
        base.validation_digest = base.compute_validation_digest();

        let mut store = SegmentStore::open(temp.path(), &base.project_id).expect("store");
        store.activate(&base, None).expect("activate base");

        let mut next = base.clone();
        next.parent_generation_id = Some(base.generation_id.clone());
        next.generation_id = "generation-attachment-replaced".to_owned();
        next.index_revision = 2;
        next.checkpoint.index_revision = 2;
        next.scene.scene_graph_revision = 3;
        for node in &mut next.scene.nodes {
            node.scene_graph_revision = 3;
        }
        next.scene.nodes[0].attached_script_entity_id = Some(replacement_script_id.clone());
        next.scene.validation_digest = next.scene.compute_validation_digest();
        next.script = ScriptNormalizer
            .normalize_graph(&next, SESSION, &graph)
            .expect("recomposed script domain");
        next.canonicalize();
        next.validation_digest = next.compute_validation_digest();
        store.activate(&next, None).expect("atomic activation");

        let active = store.active_generation().expect("active generation");
        assert_eq!(active.generation_id, next.generation_id);
        assert_eq!(active.scene.scene_graph_revision, 3);
        assert_eq!(active.script.scene_graph_revision, 3);
        let attachment = active
            .script
            .relations
            .iter()
            .find(|relation| {
                relation.predicate == ScriptPredicate::AttachesScript
                    && relation.source
                        == (ScriptEndpoint::SceneNode {
                            node_entity_id: player_node_id.clone(),
                        })
            })
            .expect("current attachment");
        assert_eq!(attachment.script_resource_id, replacement_script_id);

        let mut invalid_base = active.clone();
        invalid_base
            .resources
            .retain(|resource| resource.uid.as_deref() != Some("uid://damage"));
        assert!(
            ScriptNormalizer
                .normalize_graph(&invalid_base, SESSION, &graph)
                .is_err()
        );
        assert_eq!(
            store.active_generation().expect("preserved generation"),
            active
        );
    }
}
