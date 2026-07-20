//! Storage-neutral Sprint 6 semantic fact and evidence projection.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    DependencyResolution, IndexGeneration, SceneAnimationResolution, ScriptConfidence,
    ScriptEndpoint, ScriptPredicate, ScriptRelationAuthority, ScriptSourceRange, ScriptSymbolKind,
};

const FACT_DOMAIN: &[u8] = b"godot-codex/semantic-fact/v1\0";
const EVIDENCE_DOMAIN: &[u8] = b"godot-codex/semantic-evidence/v1\0";
const CONFLICT_DOMAIN: &[u8] = b"godot-codex/semantic-conflict/v1\0";
const SIGNAL_DOMAIN: &[u8] = b"godot-codex/signal-entity/v1\0";
pub const FIND_USAGES_DEFAULT_LIMIT: usize = 50;
pub const FIND_USAGES_MAX_LIMIT: usize = 200;
pub const FIND_USAGES_MAX_WINDOW: usize = 250_000;

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

/// Closed source scope for a reverse semantic query.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum FindUsagesScope {
    Project,
    Scene { scene_entity_id: String },
    Script { script_resource_id: String },
}

/// Storage-neutral normalized reverse query.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindUsagesQuery {
    pub target_entity_id: String,
    #[serde(default)]
    pub source_kinds: Vec<SemanticEntityKind>,
    #[serde(default)]
    pub confidence: Vec<SemanticConfidence>,
    pub scope: FindUsagesScope,
    pub offset: usize,
    pub limit: usize,
}

/// One deterministic bounded page over the reverse semantic index.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindUsagesQueryResult {
    pub target_entity_id: String,
    pub target_kind: SemanticEntityKind,
    pub revisions: SemanticRevisionVector,
    pub usages: Vec<SemanticFact>,
    pub conflicts: Vec<ConflictDiagnostic>,
    pub total_matches: usize,
    pub truncated: bool,
    pub next_offset: Option<usize>,
}

/// Closed validation errors for storage-neutral reverse queries.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum FindUsagesQueryError {
    #[error("target_not_found")]
    TargetNotFound,
    #[error("invalid_limit")]
    InvalidLimit,
    #[error("result_window_exceeded")]
    ResultWindowExceeded,
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
        for occurrence in generation.scene.relations.iter().filter(|relation| {
            scene_current && relation.relation == "occurrence" && relation.scene_entity_id.is_some()
        }) {
            let scene_id = occurrence
                .scene_entity_id
                .as_ref()
                .expect("filtered occurrence scene");
            entity_kinds.insert(occurrence.source.clone(), SemanticEntityKind::Node);
            entity_scenes.insert(occurrence.source.clone(), scene_id.clone());
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
        let signal_aliases = signal_aliases(generation, scene_current, script_current);
        for aliases in signal_aliases.values() {
            for (signal_id, scene_id) in aliases {
                entity_kinds.insert(signal_id.clone(), SemanticEntityKind::Signal);
                entity_scenes.insert(signal_id.clone(), scene_id.clone());
            }
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
            let Some(target_endpoint) = relation.target.as_ref() else {
                continue;
            };
            let predicate = script_predicate(relation.predicate);
            let domain = if predicate == SemanticPredicate::AttachesScript {
                "composition"
            } else {
                "script"
            };
            let source = endpoint_id(&relation.source);
            let evidence = EvidenceDraft {
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
            };
            for target in endpoint_targets(target_endpoint, &signal_aliases) {
                add_fact(
                    &mut facts,
                    &entity_kinds,
                    revisions,
                    source,
                    (domain, predicate),
                    &target,
                    evidence.clone(),
                );
            }
        }
        for reference in generation
            .script
            .references
            .iter()
            .filter(|_| script_current)
        {
            let source = endpoint_id(&reference.source);
            let targets = endpoint_targets(&reference.target, &signal_aliases);
            let predicate = script_predicate(reference.predicate);
            let domain = if predicate == SemanticPredicate::AttachesScript {
                "composition"
            } else {
                "script"
            };
            let evidence = EvidenceDraft {
                source: "materialized_script_reference".to_owned(),
                authority: script_authority(reference.authority).to_owned(),
                path: None,
                node_path: None,
                property: None,
                range: None,
                content_sha256: None,
                confidence: SemanticConfidence::Exact,
            };
            for target in targets {
                add_fact(
                    &mut facts,
                    &entity_kinds,
                    revisions,
                    source,
                    (domain, predicate),
                    &target,
                    evidence.clone(),
                );
            }
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

    /// Executes one deterministic reverse lookup over the immutable projection.
    pub fn find_usages(
        &self,
        query: &FindUsagesQuery,
    ) -> Result<FindUsagesQueryResult, FindUsagesQueryError> {
        let target_kind = self
            .entity_kind(&query.target_entity_id)
            .ok_or(FindUsagesQueryError::TargetNotFound)?;
        if !(1..=FIND_USAGES_MAX_LIMIT).contains(&query.limit) {
            return Err(FindUsagesQueryError::InvalidLimit);
        }
        if query.offset >= FIND_USAGES_MAX_WINDOW && query.offset != 0 {
            return Err(FindUsagesQueryError::ResultWindowExceeded);
        }
        let matches = self
            .facts_for_target(&query.target_entity_id)
            .into_iter()
            .filter(|fact| {
                query.source_kinds.is_empty() || query.source_kinds.contains(&fact.source_kind)
            })
            .filter(|fact| {
                query.confidence.is_empty() || query.confidence.contains(&fact.confidence)
            })
            .filter(|fact| self.fact_in_scope(fact, &query.scope))
            .collect::<Vec<_>>();
        let total_matches = matches.len();
        let bounded_matches = total_matches.min(FIND_USAGES_MAX_WINDOW);
        if query.offset > bounded_matches {
            return Err(FindUsagesQueryError::ResultWindowExceeded);
        }
        let end = query
            .offset
            .saturating_add(query.limit)
            .min(bounded_matches);
        let usages = matches[query.offset..end]
            .iter()
            .map(|fact| (*fact).clone())
            .collect::<Vec<_>>();
        let page_fact_ids = usages
            .iter()
            .map(|fact| fact.fact_id.as_str())
            .collect::<BTreeSet<_>>();
        let conflicts = self
            .conflicts
            .iter()
            .filter(|conflict| {
                conflict
                    .fact_ids
                    .iter()
                    .any(|fact_id| page_fact_ids.contains(fact_id.as_str()))
            })
            .cloned()
            .collect();
        Ok(FindUsagesQueryResult {
            target_entity_id: query.target_entity_id.clone(),
            target_kind,
            revisions: self.revisions,
            usages,
            conflicts,
            total_matches,
            truncated: total_matches > FIND_USAGES_MAX_WINDOW,
            next_offset: (end < bounded_matches).then_some(end),
        })
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

    fn fact_in_scope(&self, fact: &SemanticFact, scope: &FindUsagesScope) -> bool {
        match scope {
            FindUsagesScope::Project => true,
            FindUsagesScope::Scene { scene_entity_id } => self
                .scene_for_entity(&fact.source_entity_id)
                .is_some_and(|value| value == scene_entity_id),
            FindUsagesScope::Script { script_resource_id } => self
                .script_for_entity(&fact.source_entity_id)
                .is_some_and(|value| value == script_resource_id),
        }
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

fn signal_aliases(
    generation: &IndexGeneration,
    scene_current: bool,
    script_current: bool,
) -> BTreeMap<String, Vec<(String, String)>> {
    if !scene_current || !script_current {
        return BTreeMap::new();
    }
    let symbol_scripts = generation
        .script
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.symbol_id.as_str(),
                symbol.script_resource_id.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut parents = BTreeMap::<String, BTreeSet<String>>::new();
    for relation in generation
        .script
        .relations
        .iter()
        .filter(|relation| relation.predicate == ScriptPredicate::Inherits)
    {
        let Some(target) = relation.target.as_ref() else {
            continue;
        };
        let Some(source_script) = endpoint_script(&relation.source, &symbol_scripts) else {
            continue;
        };
        let Some(target_script) = endpoint_script(target, &symbol_scripts) else {
            continue;
        };
        if source_script != target_script {
            parents
                .entry(source_script.to_owned())
                .or_default()
                .insert(target_script.to_owned());
        }
    }

    let mut aliases = BTreeMap::<String, Vec<(String, String)>>::new();
    for symbol in generation
        .script
        .symbols
        .iter()
        .filter(|symbol| symbol.kind == ScriptSymbolKind::Signal)
    {
        let Some(name) = symbol.name.as_deref() else {
            continue;
        };
        for node in generation
            .scene
            .nodes
            .iter()
            .filter(|node| node.attached_script_entity_id.is_some())
        {
            let attached = node
                .attached_script_entity_id
                .as_deref()
                .expect("filtered attached script");
            if script_reaches(attached, &symbol.script_resource_id, &parents) {
                let mut emitters = vec![node.node_entity_id.as_str()];
                emitters.extend(
                    generation
                        .scene
                        .relations
                        .iter()
                        .filter(|relation| {
                            relation.relation == "occurrence"
                                && relation.scene_entity_id.as_deref()
                                    == Some(node.scene_entity_id.as_str())
                                && relation.target.as_deref() == Some(node.node_entity_id.as_str())
                        })
                        .map(|relation| relation.source.as_str()),
                );
                for emitter in emitters {
                    aliases.entry(symbol.symbol_id.clone()).or_default().push((
                        signal_entity_id(&node.scene_entity_id, emitter, name),
                        node.scene_entity_id.clone(),
                    ));
                }
            }
        }
    }
    for values in aliases.values_mut() {
        values.sort();
        values.dedup();
    }
    aliases
}

fn endpoint_script<'a>(
    endpoint: &'a ScriptEndpoint,
    symbol_scripts: &BTreeMap<&str, &'a str>,
) -> Option<&'a str> {
    match endpoint {
        ScriptEndpoint::Symbol { symbol_id } => symbol_scripts.get(symbol_id.as_str()).copied(),
        ScriptEndpoint::Resource { resource_entity_id } => Some(resource_entity_id),
        ScriptEndpoint::SceneNode { .. } => None,
    }
}

fn script_reaches(
    script: &str,
    expected: &str,
    parents: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    let mut pending = vec![script];
    let mut visited = BTreeSet::new();
    while let Some(current) = pending.pop() {
        if current == expected {
            return true;
        }
        if !visited.insert(current) {
            continue;
        }
        if let Some(next) = parents.get(current) {
            pending.extend(next.iter().map(String::as_str));
        }
    }
    false
}

fn endpoint_targets(
    endpoint: &ScriptEndpoint,
    signal_aliases: &BTreeMap<String, Vec<(String, String)>>,
) -> Vec<String> {
    let mut targets = vec![endpoint_id(endpoint).to_owned()];
    if let ScriptEndpoint::Symbol { symbol_id } = endpoint
        && let Some(aliases) = signal_aliases.get(symbol_id)
    {
        targets.extend(aliases.iter().map(|(signal_id, _)| signal_id.clone()));
    }
    targets.sort();
    targets.dedup();
    targets
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

    #[test]
    fn find_usages_filters_pages_and_rejects_invalid_windows() {
        let mut value = generation();
        let mut second = value.dependencies[0].clone();
        second.edge_id = "edge-2".to_owned();
        second.source_entity_id = "target".to_owned();
        second.target_entity_id = Some("owner".to_owned());
        second.target_uid = Some("uid://owner".to_owned());
        second.target_comparison_path = Some("res://owner.tres".to_owned());
        second.target_display_path = Some("res://owner.tres".to_owned());
        second.resolved_target_path = Some("res://owner.tres".to_owned());
        value.dependencies.push(second);
        let index = SemanticQueryIndex::build(&value);
        let result = index
            .find_usages(&FindUsagesQuery {
                target_entity_id: "target".to_owned(),
                source_kinds: vec![SemanticEntityKind::Resource],
                confidence: vec![SemanticConfidence::Exact],
                scope: FindUsagesScope::Project,
                offset: 0,
                limit: 1,
            })
            .expect("valid query");
        assert_eq!(result.total_matches, 1);
        assert_eq!(result.usages.len(), 1);
        assert_eq!(result.next_offset, None);

        assert_eq!(
            index.find_usages(&FindUsagesQuery {
                target_entity_id: "target".to_owned(),
                source_kinds: vec![],
                confidence: vec![],
                scope: FindUsagesScope::Project,
                offset: FIND_USAGES_MAX_WINDOW,
                limit: 1,
            }),
            Err(FindUsagesQueryError::ResultWindowExceeded)
        );
    }

    #[test]
    fn unavailable_semantic_domains_never_leak_stale_revisions() {
        let index = SemanticQueryIndex::build_available(&generation(), false, false);
        assert_eq!(index.revisions().scene_graph_revision, None);
        assert_eq!(index.revisions().script_graph_revision, None);
        assert!(index.facts().iter().all(|fact| fact.domain == "resource"));
    }
}
