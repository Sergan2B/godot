//! Storage-neutral Sprint 6 semantic fact and evidence projection.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    DependencyResolution, IndexGeneration, SceneAnimationResolution, ScriptConfidence,
    ScriptEndpoint, ScriptPredicate, ScriptRelationAuthority, ScriptSourceRange, ScriptSymbolKind,
};

const FACT_DOMAIN: &[u8] = b"godot-codex/semantic-fact/v1\0";
const EVIDENCE_DOMAIN: &[u8] = b"godot-codex/semantic-evidence/v1\0";
const CONFLICT_DOMAIN: &[u8] = b"godot-codex/semantic-conflict/v1\0";
const SIGNAL_DOMAIN: &[u8] = b"godot-codex/signal-entity/v1\0";

/// Entity families exposed by the unified semantic query surface.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticEntityKind {
    Project,
    Resource,
    Scene,
    Node,
    Script,
    Symbol,
    Signal,
}

/// Canonical cross-domain relation vocabulary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticPredicate {
    References,
    Instantiates,
    Inherits,
    Overrides,
    AttachesScript,
    DeclaresSymbol,
    ReferencesSymbol,
    ConnectsSignal,
    BelongsToGroup,
    Preloads,
    Loads,
    Calls,
}

/// Confidence retained independently from freshness.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticConfidence {
    Dynamic,
    Probable,
    RuntimeConfirmed,
    Exact,
}

/// Complete immutable index coordinates represented by one fact.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionVector {
    pub index_revision: u64,
    pub resource_revision: u64,
    pub scene_graph_revision: Option<u64>,
    pub script_graph_revision: Option<u64>,
}

/// One content-bound source range without source bytes.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSourceRange {
    pub path: String,
    pub content_sha256: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

impl From<&ScriptSourceRange> for SemanticSourceRange {
    fn from(value: &ScriptSourceRange) -> Self {
        Self {
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
}

/// One independently attributable basis for a semantic fact.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRecord {
    pub evidence_id: String,
    pub source: String,
    pub authority: String,
    pub path: Option<String>,
    pub node_path: Option<String>,
    pub property: Option<String>,
    pub range: Option<SemanticSourceRange>,
    pub content_sha256: Option<String>,
    pub confidence: SemanticConfidence,
    pub freshness: String,
    pub revisions: SemanticRevisionVector,
}

/// One deduplicated current relation with all retained provenance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticFact {
    pub fact_id: String,
    pub domain: String,
    pub source_entity_id: String,
    pub source_kind: SemanticEntityKind,
    pub predicate: SemanticPredicate,
    pub target_entity_id: String,
    pub target_kind: SemanticEntityKind,
    pub confidence: SemanticConfidence,
    pub freshness: String,
    pub revisions: SemanticRevisionVector,
    pub evidence: Vec<EvidenceRecord>,
}

/// Multiple authoritative values for one declared single-valued slot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConflictDiagnostic {
    pub conflict_id: String,
    pub source_entity_id: String,
    pub predicate: SemanticPredicate,
    pub fact_ids: Vec<String>,
    pub revisions: SemanticRevisionVector,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct FactKey<'a> {
    domain: &'a str,
    source: &'a str,
    predicate: SemanticPredicate,
    target: &'a str,
    revisions: SemanticRevisionVector,
}

#[derive(Clone, Debug)]
struct EvidenceDraft {
    source: String,
    authority: String,
    path: Option<String>,
    node_path: Option<String>,
    property: Option<String>,
    range: Option<SemanticSourceRange>,
    content_sha256: Option<String>,
    confidence: SemanticConfidence,
}

/// Derived, immutable reverse-query cache for one validated generation.
#[derive(Clone, Debug)]
pub struct SemanticQueryIndex {
    generation_id: String,
    revisions: SemanticRevisionVector,
    facts: Vec<SemanticFact>,
    conflicts: Vec<ConflictDiagnostic>,
    by_target: BTreeMap<String, Vec<usize>>,
    entity_kinds: BTreeMap<String, SemanticEntityKind>,
    entity_scenes: BTreeMap<String, String>,
    entity_scripts: BTreeMap<String, String>,
}

impl SemanticQueryIndex {
    /// Builds a deterministic query projection without changing persisted data.
    #[must_use]
    pub fn build(generation: &IndexGeneration) -> Self {
        Self::build_available(generation, true, true)
    }

    /// Builds a projection while excluding domains that are not independently current.
    #[must_use]
    pub fn build_available(
        generation: &IndexGeneration,
        scene_current: bool,
        script_current: bool,
    ) -> Self {
        let revisions = SemanticRevisionVector {
            index_revision: generation.index_revision,
            resource_revision: generation.checkpoint.resource_revision,
            scene_graph_revision: (scene_current && !generation.scene.is_empty())
                .then_some(generation.scene.scene_graph_revision),
            script_graph_revision: (script_current && !generation.script.is_empty())
                .then_some(generation.script.script_graph_revision),
        };
        let mut entity_kinds = BTreeMap::new();
        let mut entity_scenes = BTreeMap::new();
        let mut entity_scripts = BTreeMap::new();
        for resource in &generation.resources {
            let kind = if resource.comparison_path.ends_with(".gd")
                || resource.comparison_path.ends_with(".cs")
            {
                SemanticEntityKind::Script
            } else {
                SemanticEntityKind::Resource
            };
            entity_kinds.insert(resource.entity_id.clone(), kind);
            if kind == SemanticEntityKind::Script {
                entity_scripts.insert(resource.entity_id.clone(), resource.entity_id.clone());
            }
        }
        for scene in generation.scene.scenes.iter().filter(|_| scene_current) {
            entity_kinds.insert(scene.scene_entity_id.clone(), SemanticEntityKind::Scene);
            entity_scenes.insert(scene.scene_entity_id.clone(), scene.scene_entity_id.clone());
        }
        for node in generation.scene.nodes.iter().filter(|_| scene_current) {
            entity_kinds.insert(node.node_entity_id.clone(), SemanticEntityKind::Node);
            entity_scenes.insert(node.node_entity_id.clone(), node.scene_entity_id.clone());
        }
        for symbol in generation.script.symbols.iter().filter(|_| script_current) {
            let kind = if symbol.kind == ScriptSymbolKind::Signal {
                SemanticEntityKind::Signal
            } else {
                SemanticEntityKind::Symbol
            };
            entity_kinds.insert(symbol.symbol_id.clone(), kind);
            entity_scripts.insert(symbol.symbol_id.clone(), symbol.script_resource_id.clone());
        }

        let mut facts = BTreeMap::<String, SemanticFact>::new();
        for edge in &generation.dependencies {
            if edge.resolution != DependencyResolution::Resolved {
                continue;
            }
            let Some(target) = edge.target_entity_id.as_deref() else {
                continue;
            };
            add_fact(
                &mut facts,
                &entity_kinds,
                revisions,
                &edge.source_entity_id,
                ("resource", SemanticPredicate::References),
                target,
                EvidenceDraft {
                    source: "resource_dependency".to_owned(),
                    authority: edge.authority.clone(),
                    path: edge.resolved_target_path.clone(),
                    node_path: None,
                    property: None,
                    range: None,
                    content_sha256: None,
                    confidence: SemanticConfidence::Exact,
                },
            );
        }
        for scene in generation.scene.scenes.iter().filter(|_| scene_current) {
            if let Some(target) = scene.base_scene_entity_id.as_deref() {
                add_fact(
                    &mut facts,
                    &entity_kinds,
                    revisions,
                    &scene.scene_entity_id,
                    ("scene", SemanticPredicate::Inherits),
                    target,
                    scene_evidence(
                        "scene_definition",
                        &scene.authority,
                        SemanticConfidence::Exact,
                    ),
                );
            }
        }
        for node in generation.scene.nodes.iter().filter(|_| scene_current) {
            if let Some(target) = node.instance_scene_entity_id.as_deref() {
                add_fact(
                    &mut facts,
                    &entity_kinds,
                    revisions,
                    &node.node_entity_id,
                    ("scene", SemanticPredicate::Instantiates),
                    target,
                    EvidenceDraft {
                        node_path: Some(node.node_path.clone()),
                        ..scene_evidence(
                            "scene_node_instance",
                            &node.authority,
                            SemanticConfidence::Exact,
                        )
                    },
                );
            }
            if let Some(target) = node.attached_script_entity_id.as_deref() {
                add_fact(
                    &mut facts,
                    &entity_kinds,
                    revisions,
                    &node.node_entity_id,
                    ("composition", SemanticPredicate::AttachesScript),
                    target,
                    EvidenceDraft {
                        node_path: Some(node.node_path.clone()),
                        ..scene_evidence(
                            "scene_node_attachment",
                            &node.authority,
                            SemanticConfidence::Exact,
                        )
                    },
                );
            }
        }
        for relation in generation.scene.relations.iter().filter(|_| scene_current) {
            let Some(target) = relation.target.as_deref() else {
                continue;
            };
            let Some((predicate, domain)) = scene_predicate(&relation.relation) else {
                continue;
            };
            add_fact(
                &mut facts,
                &entity_kinds,
                revisions,
                &relation.source,
                (domain, predicate),
                target,
                scene_evidence(
                    "scene_relation",
                    &relation.authority,
                    SemanticConfidence::Exact,
                ),
            );
        }
        for connection in generation
            .scene
            .connections
            .iter()
            .filter(|_| scene_current)
        {
            let target = signal_entity_id(
                &connection.scene_entity_id,
                &connection.emitter_node_entity_id,
                &connection.signal,
            );
            entity_kinds.insert(target.clone(), SemanticEntityKind::Signal);
            entity_scenes.insert(target.clone(), connection.scene_entity_id.clone());
            add_fact(
                &mut facts,
                &entity_kinds,
                revisions,
                &connection.scene_entity_id,
                ("scene", SemanticPredicate::ConnectsSignal),
                &target,
                scene_evidence(
                    "scene_connection",
                    &connection.authority,
                    SemanticConfidence::Exact,
                ),
            );
        }
        for animation in generation.scene.animations.iter().filter(|_| scene_current) {
            if animation.resolution != SceneAnimationResolution::Resolved {
                continue;
            }
            let Some(target) = animation.target_node_entity_id.as_deref() else {
                continue;
            };
            add_fact(
                &mut facts,
                &entity_kinds,
                revisions,
                &animation.mixer_node_entity_id,
                ("scene", SemanticPredicate::References),
                target,
                EvidenceDraft {
                    node_path: Some(animation.node_path.clone()),
                    ..scene_evidence(
                        "scene_animation",
                        &animation.authority,
                        SemanticConfidence::Exact,
                    )
                },
            );
        }
        for relation in generation
            .script
            .relations
            .iter()
            .filter(|_| script_current)
        {
            let Some(target) = relation.target.as_ref().map(endpoint_id) else {
                continue;
            };
            let predicate = script_predicate(relation.predicate);
            let domain = if predicate == SemanticPredicate::AttachesScript {
                "composition"
            } else {
                "script"
            };
            let source = endpoint_id(&relation.source);
            add_fact(
                &mut facts,
                &entity_kinds,
                revisions,
                source,
                (domain, predicate),
                target,
                EvidenceDraft {
                    source: "script_relation".to_owned(),
                    authority: script_authority(relation.authority).to_owned(),
                    path: relation
                        .evidence_range
                        .as_ref()
                        .map(|range| range.path.clone()),
                    node_path: None,
                    property: None,
                    range: relation
                        .evidence_range
                        .as_ref()
                        .map(SemanticSourceRange::from),
                    content_sha256: relation
                        .evidence_range
                        .as_ref()
                        .map(|range| range.content_sha256.clone()),
                    confidence: script_confidence(relation.confidence),
                },
            );
        }
        for reference in generation
            .script
            .references
            .iter()
            .filter(|_| script_current)
        {
            let source = endpoint_id(&reference.source);
            let target = endpoint_id(&reference.target);
            let predicate = script_predicate(reference.predicate);
            let domain = if predicate == SemanticPredicate::AttachesScript {
                "composition"
            } else {
                "script"
            };
            add_fact(
                &mut facts,
                &entity_kinds,
                revisions,
                source,
                (domain, predicate),
                target,
                EvidenceDraft {
                    source: "materialized_script_reference".to_owned(),
                    authority: script_authority(reference.authority).to_owned(),
                    path: None,
                    node_path: None,
                    property: None,
                    range: None,
                    content_sha256: None,
                    confidence: SemanticConfidence::Exact,
                },
            );
        }

        let mut facts = facts.into_values().collect::<Vec<_>>();
        facts.sort_by(|left, right| {
            left.source_kind
                .cmp(&right.source_kind)
                .then_with(|| left.source_entity_id.cmp(&right.source_entity_id))
                .then_with(|| left.predicate.cmp(&right.predicate))
                .then_with(|| left.fact_id.cmp(&right.fact_id))
        });
        let conflicts = conflicts(&facts, revisions);
        let mut by_target = BTreeMap::<String, Vec<usize>>::new();
        for (index, fact) in facts.iter().enumerate() {
            by_target
                .entry(fact.target_entity_id.clone())
                .or_default()
                .push(index);
        }
        Self {
            generation_id: generation.generation_id.clone(),
            revisions,
            facts,
            conflicts,
            by_target,
            entity_kinds,
            entity_scenes,
            entity_scripts,
        }
    }

    #[must_use]
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    #[must_use]
    pub fn revisions(&self) -> SemanticRevisionVector {
        self.revisions
    }

    #[must_use]
    pub fn facts(&self) -> &[SemanticFact] {
        &self.facts
    }

    #[must_use]
    pub fn conflicts(&self) -> &[ConflictDiagnostic] {
        &self.conflicts
    }

    #[must_use]
    pub fn facts_for_target(&self, target: &str) -> Vec<&SemanticFact> {
        self.by_target
            .get(target)
            .into_iter()
            .flatten()
            .map(|index| &self.facts[*index])
            .collect()
    }

    #[must_use]
    pub fn entity_kind(&self, entity_id: &str) -> Option<SemanticEntityKind> {
        self.entity_kinds.get(entity_id).copied()
    }

    #[must_use]
    pub fn scene_for_entity(&self, entity_id: &str) -> Option<&str> {
        self.entity_scenes.get(entity_id).map(String::as_str)
    }

    #[must_use]
    pub fn script_for_entity(&self, entity_id: &str) -> Option<&str> {
        self.entity_scripts.get(entity_id).map(String::as_str)
    }
}

/// Stable signal identity shared by selectors and scene connection facts.
#[must_use]
pub fn signal_entity_id(scene_id: &str, emitter_id: &str, signal: &str) -> String {
    let digest = digest(SIGNAL_DOMAIN, &(scene_id, emitter_id, signal));
    format!("godot:signal:v1:{digest}")
}

fn add_fact(
    facts: &mut BTreeMap<String, SemanticFact>,
    kinds: &BTreeMap<String, SemanticEntityKind>,
    revisions: SemanticRevisionVector,
    source: &str,
    relation: (&str, SemanticPredicate),
    target: &str,
    evidence: EvidenceDraft,
) {
    if source.is_empty() || target.is_empty() {
        return;
    }
    let (domain, predicate) = relation;
    let key = FactKey {
        domain,
        source,
        predicate,
        target,
        revisions,
    };
    let fact_id = format!("godot:fact:v1:{}", digest(FACT_DOMAIN, &key));
    let evidence_id = format!(
        "godot:evidence:v1:{}",
        digest(
            EVIDENCE_DOMAIN,
            &(
                &fact_id,
                &evidence.source,
                &evidence.authority,
                &evidence.path,
                &evidence.node_path,
                &evidence.property,
                &evidence.range,
                &evidence.content_sha256,
                evidence.confidence,
                revisions,
            ),
        )
    );
    let record = EvidenceRecord {
        evidence_id,
        source: evidence.source,
        authority: evidence.authority,
        path: evidence.path,
        node_path: evidence.node_path,
        property: evidence.property,
        range: evidence.range,
        content_sha256: evidence.content_sha256,
        confidence: evidence.confidence,
        freshness: "current".to_owned(),
        revisions,
    };
    let source_kind = kinds
        .get(source)
        .copied()
        .unwrap_or(SemanticEntityKind::Project);
    let target_kind = kinds
        .get(target)
        .copied()
        .unwrap_or(SemanticEntityKind::Resource);
    match facts.get_mut(&fact_id) {
        Some(fact) => {
            fact.confidence = fact.confidence.max(record.confidence);
            if !fact
                .evidence
                .iter()
                .any(|item| item.evidence_id == record.evidence_id)
            {
                fact.evidence.push(record);
                fact.evidence.sort();
            }
        }
        None => {
            facts.insert(
                fact_id.clone(),
                SemanticFact {
                    fact_id,
                    domain: domain.to_owned(),
                    source_entity_id: source.to_owned(),
                    source_kind,
                    predicate,
                    target_entity_id: target.to_owned(),
                    target_kind,
                    confidence: record.confidence,
                    freshness: "current".to_owned(),
                    revisions,
                    evidence: vec![record],
                },
            );
        }
    }
}

fn conflicts(facts: &[SemanticFact], revisions: SemanticRevisionVector) -> Vec<ConflictDiagnostic> {
    let mut slots = BTreeMap::<(String, SemanticPredicate), BTreeMap<String, String>>::new();
    for fact in facts.iter().filter(|fact| single_valued(fact.predicate)) {
        slots
            .entry((fact.source_entity_id.clone(), fact.predicate))
            .or_default()
            .insert(fact.target_entity_id.clone(), fact.fact_id.clone());
    }
    let mut result = Vec::new();
    for ((source, predicate), targets) in slots {
        if targets.len() < 2 {
            continue;
        }
        let fact_ids = targets.into_values().collect::<Vec<_>>();
        let conflict_id = format!(
            "godot:conflict:v1:{}",
            digest(CONFLICT_DOMAIN, &(&source, predicate, &fact_ids, revisions))
        );
        result.push(ConflictDiagnostic {
            conflict_id,
            source_entity_id: source,
            predicate,
            fact_ids,
            revisions,
        });
    }
    result
}

fn single_valued(predicate: SemanticPredicate) -> bool {
    matches!(
        predicate,
        SemanticPredicate::Inherits | SemanticPredicate::AttachesScript
    )
}

fn scene_evidence(source: &str, authority: &str, confidence: SemanticConfidence) -> EvidenceDraft {
    EvidenceDraft {
        source: source.to_owned(),
        authority: authority.to_owned(),
        path: None,
        node_path: None,
        property: None,
        range: None,
        content_sha256: None,
        confidence,
    }
}

fn scene_predicate(value: &str) -> Option<(SemanticPredicate, &'static str)> {
    match value {
        "instance" | "occurrence" => Some((SemanticPredicate::Instantiates, "scene")),
        "inheritance" => Some((SemanticPredicate::Inherits, "scene")),
        "subresource" | "resource_reference" => Some((SemanticPredicate::References, "scene")),
        "attached_script" => Some((SemanticPredicate::AttachesScript, "composition")),
        _ => None,
    }
}

fn endpoint_id(endpoint: &ScriptEndpoint) -> &str {
    match endpoint {
        ScriptEndpoint::Symbol { symbol_id } => symbol_id,
        ScriptEndpoint::Resource { resource_entity_id } => resource_entity_id,
        ScriptEndpoint::SceneNode { node_entity_id } => node_entity_id,
    }
}

fn script_predicate(value: ScriptPredicate) -> SemanticPredicate {
    match value {
        ScriptPredicate::Contains => SemanticPredicate::References,
        ScriptPredicate::DeclaresSymbol => SemanticPredicate::DeclaresSymbol,
        ScriptPredicate::Inherits => SemanticPredicate::Inherits,
        ScriptPredicate::Overrides => SemanticPredicate::Overrides,
        ScriptPredicate::ReferencesSymbol => SemanticPredicate::ReferencesSymbol,
        ScriptPredicate::Calls => SemanticPredicate::Calls,
        ScriptPredicate::Preloads => SemanticPredicate::Preloads,
        ScriptPredicate::Loads => SemanticPredicate::Loads,
        ScriptPredicate::AttachesScript => SemanticPredicate::AttachesScript,
    }
}

fn script_confidence(value: ScriptConfidence) -> SemanticConfidence {
    match value {
        ScriptConfidence::Exact => SemanticConfidence::Exact,
        ScriptConfidence::Dynamic => SemanticConfidence::Dynamic,
    }
}

fn script_authority(value: ScriptRelationAuthority) -> &'static str {
    match value {
        ScriptRelationAuthority::GdscriptParserAnalyzer => "gdscript_parser_analyzer",
        ScriptRelationAuthority::ResourceGraph => "resource_graph",
        ScriptRelationAuthority::SceneState => "scene_state",
        ScriptRelationAuthority::CsharpAdapter => "csharp_adapter",
    }
}

fn digest<T: Serialize>(domain: &[u8], value: &T) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(serde_json::to_vec(value).expect("semantic identity input is serializable"));
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DependencyEdge, GenerationState, IngestionCheckpoint, LOGICAL_SCHEMA_V1, RecordValidity,
        ResourceEntity, SceneDomainGeneration, ScriptDomainGeneration,
    };

    fn generation() -> IndexGeneration {
        let resource = |id: &str, path: &str| ResourceEntity {
            entity_id: id.to_owned(),
            identity_input: id.to_owned(),
            uid: Some(format!("uid://{id}")),
            display_path: path.to_owned(),
            comparison_path: path.to_owned(),
            identity_strength: crate::IdentityStrength::ResourceUid,
            resource_type: "Resource".to_owned(),
            source_kind: "source".to_owned(),
            import_state: "ready".to_owned(),
            authority: "resource_graph".to_owned(),
            content_generation: Some(format!("sha256:{}", "1".repeat(64))),
            mtime_ns: 1,
            byte_size: 1,
            validity: RecordValidity::Valid,
            resource_revision: 1,
        };
        IndexGeneration {
            generation_id: "generation-1".to_owned(),
            parent_generation_id: None,
            schema_version: LOGICAL_SCHEMA_V1,
            project_id: "project-1".to_owned(),
            index_revision: 1,
            state: GenerationState::Active,
            creation_reason: "test".to_owned(),
            checkpoint: IngestionCheckpoint {
                editor_session_id: "editor-1".to_owned(),
                resource_revision: 1,
                project_revision: 1,
                index_revision: 1,
                source_complete: true,
                snapshot_checksum: format!("sha256:{}", "2".repeat(64)),
                last_batch_id: None,
                last_batch_checksum: None,
            },
            resources: vec![
                resource("owner", "res://owner.tres"),
                resource("target", "res://target.tres"),
            ],
            source_documents: vec![],
            dependencies: vec![DependencyEdge {
                edge_id: "edge-1".to_owned(),
                source_entity_id: "owner".to_owned(),
                target_uid: Some("uid://target".to_owned()),
                target_comparison_path: Some("res://target.tres".to_owned()),
                target_display_path: Some("res://target.tres".to_owned()),
                target_entity_id: Some("target".to_owned()),
                resolved_target_path: Some("res://target.tres".to_owned()),
                relation: "references".to_owned(),
                declared_type: None,
                authority: "resource_graph".to_owned(),
                resolution: DependencyResolution::Resolved,
                resource_revision: 1,
            }],
            diagnostics: vec![],
            tombstones: vec![],
            scene: SceneDomainGeneration::default(),
            script: ScriptDomainGeneration::default(),
            validation_digest: String::new(),
        }
    }

    #[test]
    fn projection_builds_stable_reverse_evidence() {
        let index = SemanticQueryIndex::build(&generation());
        let facts = index.facts_for_target("target");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].predicate, SemanticPredicate::References);
        assert_eq!(facts[0].confidence, SemanticConfidence::Exact);
        assert_eq!(facts[0].evidence.len(), 1);
        assert_eq!(
            index.entity_kind("owner"),
            Some(SemanticEntityKind::Resource)
        );
    }

    #[test]
    fn duplicate_fact_aggregates_distinct_evidence() {
        let mut value = generation();
        let mut duplicate = value.dependencies[0].clone();
        duplicate.edge_id = "edge-2".to_owned();
        duplicate.authority = "secondary_resource_graph".to_owned();
        value.dependencies.push(duplicate);
        let index = SemanticQueryIndex::build(&value);
        let facts = index.facts_for_target("target");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].evidence.len(), 2);
    }
}
