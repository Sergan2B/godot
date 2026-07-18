//! Storage-neutral semantic-index contracts.
//!
//! This crate deliberately contains no SQL, filesystem layout, or backend cursor.
//! Physical stores implement these interfaces without changing the observable
//! generation, revision, validation, and query rules.

mod segment;

use std::collections::{BTreeMap, BTreeSet};

pub use segment::{
    IndexReadSnapshot, SEGMENT_PHYSICAL_VERSION, SegmentFaultInjection, SegmentFaultMode,
    SegmentFaultPoint, SegmentIndexReader, SegmentIndexStore, SegmentStore, SegmentTransaction,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Current logical semantic-index schema.
pub const LOGICAL_SCHEMA_V1: SchemaVersion = SchemaVersion { major: 1, minor: 2 };

/// Last resource-only logical schema written by `segment-v1`.
pub const LOGICAL_SCHEMA_RESOURCE_V1: SchemaVersion = SchemaVersion { major: 1, minor: 1 };

/// Version of the storage-neutral logical schema.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaVersion {
    /// Breaking format generation.
    pub major: u16,
    /// Backward-compatible additive generation.
    pub minor: u16,
}

/// Lifecycle state of a persisted index.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildState {
    /// No compatible generation exists.
    Empty,
    /// A replacement generation is being built.
    Building,
    /// A complete compatible generation is active.
    Ready,
    /// Freshness was lost and the active data cannot be labeled current.
    Invalidated,
    /// Startup recovery or quarantine is in progress.
    Recovering,
}

/// Lifecycle state of one immutable generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationState {
    /// Records may still be written.
    Staging,
    /// Invariants are being checked.
    Validating,
    /// The generation is the atomic read target.
    Active,
    /// A newer generation replaced this one.
    Retired,
    /// Validation or corruption made this generation unusable.
    Quarantined,
    /// Work stopped before activation.
    Cancelled,
}

/// Durable metadata shared by every physical backend.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexMetadata {
    /// Logical semantic schema.
    pub schema_version: SchemaVersion,
    /// Lowest compatible reader minor for the same major.
    pub reader_min_minor: u16,
    /// Highest compatible reader minor for the same major.
    pub reader_max_minor: u16,
    /// Opaque project binding from Bridge RPC.
    pub project_id: String,
    /// Active generation, when one exists.
    pub active_generation_id: Option<String>,
    /// Last durable logical commit.
    pub index_revision: u64,
    /// Current store lifecycle state.
    pub build_state: BuildState,
    /// Frozen hash identifier.
    pub hash_algorithm: String,
    /// Number of full snapshots activated.
    pub full_rebuild_count: u64,
    /// Number of incremental batches activated.
    pub incremental_commit_count: u64,
    /// Number of cache artifacts quarantined.
    pub quarantine_count: u64,
    /// Number of failed physical/logical migrations.
    pub failed_migration_count: u64,
}

/// Source coordinates represented by an index revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestionCheckpoint {
    /// Editor session that produced the observation.
    pub editor_session_id: String,
    /// Last complete resource journal revision.
    pub resource_revision: u64,
    /// Project-wide Bridge revision.
    pub project_revision: u64,
    /// Durable index revision assigned at activation.
    pub index_revision: u64,
    /// Whether the snapshot covers the whole resource domain.
    pub source_complete: bool,
    /// SHA-256 of the normalized input snapshot.
    pub snapshot_checksum: String,
    /// Last applied Bridge batch identity, absent for full snapshots.
    #[serde(default)]
    pub last_batch_id: Option<String>,
    /// Last applied Bridge batch checksum used for idempotence defense.
    #[serde(default)]
    pub last_batch_checksum: Option<String>,
}

/// Persistent identity strength of a resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityStrength {
    /// Godot supplied a stable ResourceUID.
    ResourceUid,
    /// Identity is scoped to normalized path and content generation.
    PathContentGeneration,
}

/// Validity of one resource observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordValidity {
    /// All required facts are authoritative and complete.
    Valid,
    /// The record is queryable with diagnostics but is not exact.
    Partial,
    /// The record must not be returned as exact.
    Invalid,
    /// A bounded deletion record retained for reconciliation.
    Deleted,
}

/// One normalized file-backed Godot resource.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceEntity {
    /// Opaque deterministic entity identifier.
    pub entity_id: String,
    /// Canonical identity input retained for collision defense.
    pub identity_input: String,
    /// Canonical Godot UID, when present.
    pub uid: Option<String>,
    /// Normalized user-facing `res://` spelling.
    pub display_path: String,
    /// NFC comparison key without case folding.
    pub comparison_path: String,
    /// Persistent or content-scoped identity mechanism.
    pub identity_strength: IdentityStrength,
    /// Godot resource type.
    pub resource_type: String,
    /// `source` or `imported_source` as reported by Godot.
    #[serde(default)]
    pub source_kind: String,
    /// Import state reported by Godot.
    pub import_state: String,
    /// Authoritative producer of the resource observation.
    #[serde(default)]
    pub authority: String,
    /// SHA-256 content generation when hashing succeeded.
    pub content_generation: Option<String>,
    /// Observed source modification time in nanoseconds.
    pub mtime_ns: u64,
    /// Observed source byte size.
    pub byte_size: u64,
    /// Current record validity.
    pub validity: RecordValidity,
    /// Authoritative source resource revision.
    pub resource_revision: u64,
}

/// Hash/ingestion observation for a resource source document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDocument {
    /// Owning resource entity.
    pub entity_id: String,
    /// Normalized project-relative path.
    pub comparison_path: String,
    /// Size before bounded hashing.
    pub size_before: u64,
    /// Size after bounded hashing.
    pub size_after: u64,
    /// Modification time before bounded hashing.
    pub mtime_before_ns: u64,
    /// Modification time after bounded hashing.
    pub mtime_after_ns: u64,
    /// Content generation when hashing succeeded.
    pub content_generation: Option<String>,
    /// Stable ingest state or diagnostic code.
    pub ingest_state: String,
}

/// Resolution state of a dependency target.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyResolution {
    /// Target resolved to an indexed entity.
    Resolved,
    /// Path-only target was absent.
    Missing,
    /// UID was canonical but absent or conflicted with its fallback path.
    StaleUid,
}

/// One normalized direct resource dependency.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyEdge {
    /// Opaque deterministic edge identifier.
    pub edge_id: String,
    /// Source resource entity.
    pub source_entity_id: String,
    /// Canonical UID reference, when present.
    pub target_uid: Option<String>,
    /// Canonical fallback comparison path, when present.
    pub target_comparison_path: Option<String>,
    /// Normalized user-facing fallback path.
    #[serde(default)]
    pub target_display_path: Option<String>,
    /// Resolved target entity in this generation.
    pub target_entity_id: Option<String>,
    /// Current resolved target path, when available.
    #[serde(default)]
    pub resolved_target_path: Option<String>,
    /// Frozen relationship kind (`references`).
    #[serde(default)]
    pub relation: String,
    /// Optional Godot-declared target type.
    #[serde(default)]
    pub declared_type: Option<String>,
    /// Authority that produced this declaration.
    #[serde(default)]
    pub authority: String,
    /// Resolution status retained even for unresolved edges.
    pub resolution: DependencyResolution,
    /// Source resource revision.
    pub resource_revision: u64,
}

/// Stable index diagnostic.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    /// Stable identifier derived from code, subject, and first generation.
    pub diagnostic_id: String,
    /// Stable machine-readable code.
    pub code: String,
    /// Safe bounded subject identifier.
    pub subject: String,
    /// Optional safe project-relative detail such as a target reference.
    #[serde(default)]
    pub detail: Option<String>,
    /// First observed durable revision.
    pub first_index_revision: u64,
    /// Most recent durable revision.
    pub last_index_revision: u64,
    /// Whether the diagnostic remains active.
    pub active: bool,
}

/// Bounded deletion record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tombstone {
    /// Deleted entity.
    pub entity_id: String,
    /// Revision that observed the deletion.
    pub deleted_index_revision: u64,
    /// Last generation that may retain this tombstone.
    pub retain_through_index_revision: u64,
}

/// Persistence scope of a canonical scene-domain identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneIdentityScope {
    /// Godot supplied a stable UID or scene-unique identifier.
    Persistent,
    /// Identity is valid only for one resource content generation.
    ContentRevision,
}

/// Provenance of an effective serialized scene property.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenePropertyOrigin {
    /// Declared by a node owned by the queried scene.
    Local,
    /// Inherited from a base scene without a later override.
    Inherited,
    /// Declared as an override of an instantiated scene node.
    InstanceOverride,
}

/// One normalized scene definition.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneEntity {
    /// Canonical D-06 scene entity identifier.
    pub scene_entity_id: String,
    /// Resource-index entity that owns the scene, when resolved.
    pub source_resource_entity_id: Option<String>,
    /// Canonical Godot resource UID, when present.
    pub uid: Option<String>,
    /// Normalized `res://` comparison path.
    pub comparison_path: String,
    /// Scene content generation used by weak identities.
    pub content_generation: String,
    /// Persistence scope of this scene identity.
    pub identity_scope: SceneIdentityScope,
    /// Canonical base-scene entity, when this scene is inherited.
    pub base_scene_entity_id: Option<String>,
    /// Godot authority that produced the record.
    pub authority: String,
    /// Resource graph revision represented by this record.
    pub resource_revision: u64,
    /// Scene graph revision represented by this record.
    pub scene_graph_revision: u64,
}

/// One normalized saved node definition.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneNode {
    /// Canonical D-06 node-definition identifier.
    pub node_entity_id: String,
    /// Defining scene entity.
    pub scene_entity_id: String,
    /// Canonical relative node-only path; `.` selects the root.
    pub node_path: String,
    /// Persistence scope of this node identity.
    pub identity_scope: SceneIdentityScope,
    /// Godot scene-unique node ID, when usable.
    pub unique_scene_id: Option<u32>,
    /// Parent node definition, when present in the same defining scene.
    pub parent_node_entity_id: Option<String>,
    /// Owner node definition, when present in the same defining scene.
    pub owner_node_entity_id: Option<String>,
    /// Serialized node name.
    pub name: String,
    /// Godot node type.
    pub godot_type: String,
    /// Stable serialized sibling index.
    pub node_index: i32,
    /// Whether the node is owned by the defining scene.
    pub owned: bool,
    /// Whether Godot reports the definition as internal.
    pub internal: bool,
    /// Resolved attached-script resource entity, when available.
    pub attached_script_entity_id: Option<String>,
    /// Godot authority that produced the record.
    pub authority: String,
    /// Resource graph revision represented by this record.
    pub resource_revision: u64,
    /// Scene graph revision represented by this record.
    pub scene_graph_revision: u64,
}

/// One serialized or effective property fact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneProperty {
    /// Deterministic semantic fact identifier.
    pub property_id: String,
    /// Query/owning scene entity.
    pub scene_entity_id: String,
    /// Node definition or occurrence that receives the value.
    pub subject_entity_id: String,
    /// Serialized property name.
    pub name: String,
    /// Bounded projected Godot Variant type.
    pub value_type: String,
    /// Bounded projected value; never a raw imported payload.
    pub value: serde_json::Value,
    /// Whether bounded projection truncated the value.
    pub truncated: bool,
    /// Scene that declared this fact.
    pub declaring_scene_entity_id: String,
    /// Node that declared this fact.
    pub declaring_node_entity_id: String,
    /// Effective provenance of the value.
    pub origin: ScenePropertyOrigin,
    /// Fact replaced by this override, when applicable.
    pub overridden_property_id: Option<String>,
    /// Godot authority that produced the record.
    pub authority: String,
    /// Resource graph revision represented by this record.
    pub resource_revision: u64,
    /// Scene graph revision represented by this record.
    pub scene_graph_revision: u64,
}

/// One normalized structural relation not covered by a dedicated shard.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneRelation {
    /// Deterministic semantic relation identifier.
    pub relation_id: String,
    /// Query/owning scene entity, absent only for project context.
    pub scene_entity_id: Option<String>,
    /// Frozen relation kind such as `instance`, `occurrence`, or `subresource`.
    pub relation: String,
    /// Canonical source entity or project-context key.
    pub source: String,
    /// Canonical target entity, when resolved.
    pub target: Option<String>,
    /// Declaration scope retained for provenance.
    pub declaration_scope: Option<String>,
    /// Bounded additive relation attributes.
    pub attributes: BTreeMap<String, serde_json::Value>,
    /// Godot or composition authority that produced the record.
    pub authority: String,
    /// Resource graph revision represented by this record.
    pub resource_revision: u64,
    /// Scene graph revision represented by this record.
    pub scene_graph_revision: u64,
}

/// One saved signal connection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneConnection {
    /// Deterministic semantic connection identifier.
    pub connection_id: String,
    /// Query/owning scene entity.
    pub scene_entity_id: String,
    /// Canonical emitter node entity.
    pub emitter_node_entity_id: String,
    /// Serialized signal name.
    pub signal: String,
    /// Canonical receiver node entity, when resolved.
    pub receiver_node_entity_id: Option<String>,
    /// Serialized target method name without source-code resolution.
    pub method: String,
    /// Godot connection flags.
    pub flags: u32,
    /// Serialized unbind count.
    pub unbinds: usize,
    /// Bounded projected bind values.
    pub binds: Vec<serde_json::Value>,
    /// Declaration scope retained for provenance.
    pub declaration_scope: String,
    /// Godot authority that produced the record.
    pub authority: String,
    /// Resource graph revision represented by this record.
    pub resource_revision: u64,
    /// Scene graph revision represented by this record.
    pub scene_graph_revision: u64,
}

/// One saved group membership.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneGroupMembership {
    /// Deterministic semantic membership identifier.
    pub membership_id: String,
    /// Query/owning scene entity.
    pub scene_entity_id: String,
    /// Canonical member node entity.
    pub member_node_entity_id: String,
    /// Serialized group name.
    pub group: String,
    /// Declaration scope retained for provenance.
    pub declaration_scope: String,
    /// Godot authority that produced the record.
    pub authority: String,
    /// Resource graph revision represented by this record.
    pub resource_revision: u64,
    /// Scene graph revision represented by this record.
    pub scene_graph_revision: u64,
}

/// Resolution status of an animation track NodePath.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneAnimationResolution {
    /// The node component resolved to one canonical entity.
    Resolved,
    /// The saved path is syntactically valid but its target is missing.
    Broken,
    /// Resolution is unavailable without inventing dynamic semantics.
    Unresolved,
}

/// One animation track NodePath reference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneAnimationReference {
    /// Deterministic semantic track-reference identifier.
    pub animation_reference_id: String,
    /// Query/owning scene entity.
    pub scene_entity_id: String,
    /// Canonical mixer/player node entity.
    pub mixer_node_entity_id: String,
    /// Animation library name.
    pub library: String,
    /// Animation name.
    pub animation: String,
    /// Serialized track index.
    pub track_index: usize,
    /// Original bounded NodePath spelling.
    pub node_path: String,
    /// Resolved node entity, when exact.
    pub target_node_entity_id: Option<String>,
    /// Deterministic resolution status.
    pub resolution: SceneAnimationResolution,
    /// Godot authority that produced the record.
    pub authority: String,
    /// Resource graph revision represented by this record.
    pub resource_revision: u64,
    /// Scene graph revision represented by this record.
    pub scene_graph_revision: u64,
}

/// Independently checkpointed scene-domain records inside one semantic generation.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneDomainGeneration {
    /// Editor session that produced the observation.
    pub editor_session_id: String,
    /// Resource revision joined by the scene snapshot.
    pub resource_revision: u64,
    /// Project-wide Bridge scene graph revision.
    pub scene_graph_revision: u64,
    /// Whether the scene domain is a complete current snapshot.
    pub source_complete: bool,
    /// SHA-256 of the normalized scene input.
    pub snapshot_checksum: String,
    /// Normalized scene definitions.
    pub scenes: Vec<SceneEntity>,
    /// Normalized node definitions and composed occurrences.
    pub nodes: Vec<SceneNode>,
    /// Serialized and effective property facts.
    pub properties: Vec<SceneProperty>,
    /// Instances, occurrences, resources, project context, and diagnostics.
    pub relations: Vec<SceneRelation>,
    /// Saved signal connections.
    pub connections: Vec<SceneConnection>,
    /// Saved group memberships.
    pub groups: Vec<SceneGroupMembership>,
    /// Animation track references.
    pub animations: Vec<SceneAnimationReference>,
    /// Digest of the canonical normalized scene domain.
    pub validation_digest: String,
}

impl SceneDomainGeneration {
    /// Returns whether this is the intentionally empty scene domain produced by
    /// a resource-only `segment-v1` migration.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.editor_session_id.is_empty()
            && self.resource_revision == 0
            && self.scene_graph_revision == 0
            && !self.source_complete
            && self.snapshot_checksum.is_empty()
            && self.scenes.is_empty()
            && self.nodes.is_empty()
            && self.properties.is_empty()
            && self.relations.is_empty()
            && self.connections.is_empty()
            && self.groups.is_empty()
            && self.animations.is_empty()
    }

    /// Sorts every scene-domain collection by its deterministic semantic key.
    pub fn canonicalize(&mut self) {
        self.scenes
            .sort_by(|left, right| left.scene_entity_id.cmp(&right.scene_entity_id));
        self.nodes
            .sort_by(|left, right| left.node_entity_id.cmp(&right.node_entity_id));
        self.properties
            .sort_by(|left, right| left.property_id.cmp(&right.property_id));
        self.relations
            .sort_by(|left, right| left.relation_id.cmp(&right.relation_id));
        self.connections
            .sort_by(|left, right| left.connection_id.cmp(&right.connection_id));
        self.groups
            .sort_by(|left, right| left.membership_id.cmp(&right.membership_id));
        self.animations.sort_by(|left, right| {
            left.animation_reference_id
                .cmp(&right.animation_reference_id)
        });
    }

    /// Computes the layout-independent digest of all normalized scene facts.
    #[must_use]
    pub fn compute_validation_digest(&self) -> String {
        let mut canonical = self.clone();
        canonical.canonicalize();
        canonical.validation_digest.clear();
        let bytes = serde_json::to_vec(&canonical).expect("scene domain is serializable");
        let mut hasher = Sha256::new();
        hasher.update(b"godot-codex/scene-domain/v1\0");
        hasher.update(bytes);
        format!("sha256:{:x}", hasher.finalize())
    }

    /// Validates scene identities, references, bounds, and the semantic digest.
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.is_empty() {
            if self.validation_digest.is_empty()
                || self.validation_digest == self.compute_validation_digest()
            {
                return Ok(());
            }
            return Err(StoreError::ValidationFailed(
                "scene domain validation digest mismatch".to_owned(),
            ));
        }
        if self.editor_session_id.is_empty()
            || self.resource_revision == 0
            || self.scene_graph_revision == 0
            || !self.source_complete
            || self.snapshot_checksum.is_empty()
        {
            return Err(StoreError::ValidationFailed(
                "scene domain checkpoint is incomplete".to_owned(),
            ));
        }

        let mut scene_ids = BTreeSet::new();
        let mut scene_paths = BTreeSet::new();
        for scene in &self.scenes {
            validate_scene_record_coordinates(
                &scene.scene_entity_id,
                &scene.authority,
                scene.resource_revision,
                scene.scene_graph_revision,
                self,
            )?;
            if scene.comparison_path.is_empty() || scene.content_generation.is_empty() {
                return Err(StoreError::ValidationFailed(
                    "scene identity input is incomplete".to_owned(),
                ));
            }
            if !scene_ids.insert(scene.scene_entity_id.as_str()) {
                return Err(StoreError::ValidationFailed(
                    "duplicate scene entity".to_owned(),
                ));
            }
            if !scene_paths.insert(scene.comparison_path.as_str()) {
                return Err(StoreError::ValidationFailed(
                    "duplicate scene comparison path".to_owned(),
                ));
            }
        }

        let mut node_ids = BTreeSet::new();
        let mut node_paths = BTreeSet::new();
        for node in &self.nodes {
            validate_scene_record_coordinates(
                &node.node_entity_id,
                &node.authority,
                node.resource_revision,
                node.scene_graph_revision,
                self,
            )?;
            if !scene_ids.contains(node.scene_entity_id.as_str())
                || node.node_path.is_empty()
                || node.name.is_empty()
                || node.godot_type.is_empty()
            {
                return Err(StoreError::ValidationFailed(
                    "scene node identity or owner is incomplete".to_owned(),
                ));
            }
            if !node_ids.insert(node.node_entity_id.as_str())
                || !node_paths.insert((node.scene_entity_id.as_str(), node.node_path.as_str()))
            {
                return Err(StoreError::ValidationFailed(
                    "duplicate scene node identity or path".to_owned(),
                ));
            }
        }

        let mut property_ids = BTreeSet::new();
        for property in &self.properties {
            validate_scene_record_coordinates(
                &property.property_id,
                &property.authority,
                property.resource_revision,
                property.scene_graph_revision,
                self,
            )?;
            if !scene_ids.contains(property.scene_entity_id.as_str())
                || property.subject_entity_id.is_empty()
                || property.name.is_empty()
                || property.value_type.is_empty()
                || !property_ids.insert(property.property_id.as_str())
                || encoded_len(&property.value)? > 65_536
            {
                return Err(StoreError::ValidationFailed(
                    "invalid scene property record".to_owned(),
                ));
            }
        }

        validate_scene_records(
            &self.relations,
            |record| {
                (
                    &record.relation_id,
                    &record.authority,
                    record.resource_revision,
                    record.scene_graph_revision,
                )
            },
            self,
        )?;
        validate_scene_records(
            &self.connections,
            |record| {
                (
                    &record.connection_id,
                    &record.authority,
                    record.resource_revision,
                    record.scene_graph_revision,
                )
            },
            self,
        )?;
        validate_scene_records(
            &self.groups,
            |record| {
                (
                    &record.membership_id,
                    &record.authority,
                    record.resource_revision,
                    record.scene_graph_revision,
                )
            },
            self,
        )?;
        validate_scene_records(
            &self.animations,
            |record| {
                (
                    &record.animation_reference_id,
                    &record.authority,
                    record.resource_revision,
                    record.scene_graph_revision,
                )
            },
            self,
        )?;
        for relation in &self.relations {
            if relation.relation.is_empty()
                || relation.source.is_empty()
                || encoded_len(&relation.attributes)? > 65_536
                || relation
                    .scene_entity_id
                    .as_deref()
                    .is_some_and(|scene_id| !scene_ids.contains(scene_id))
            {
                return Err(StoreError::ValidationFailed(
                    "invalid scene relation record".to_owned(),
                ));
            }
        }
        if self.connections.iter().any(|record| {
            !scene_ids.contains(record.scene_entity_id.as_str())
                || record.emitter_node_entity_id.is_empty()
                || record.signal.is_empty()
                || record.method.is_empty()
                || record.declaration_scope.is_empty()
        }) || self.groups.iter().any(|record| {
            !scene_ids.contains(record.scene_entity_id.as_str())
                || record.member_node_entity_id.is_empty()
                || record.group.is_empty()
                || record.declaration_scope.is_empty()
        }) || self.animations.iter().any(|record| {
            !scene_ids.contains(record.scene_entity_id.as_str())
                || record.mixer_node_entity_id.is_empty()
                || record.animation.is_empty()
                || record.node_path.is_empty()
        }) {
            return Err(StoreError::ValidationFailed(
                "scene relation owner or required field is missing".to_owned(),
            ));
        }

        let expected = self.compute_validation_digest();
        if !self.validation_digest.is_empty() && self.validation_digest != expected {
            return Err(StoreError::ValidationFailed(
                "scene domain validation digest mismatch".to_owned(),
            ));
        }
        Ok(())
    }
}

fn validate_scene_record_coordinates(
    record_id: &str,
    authority: &str,
    resource_revision: u64,
    scene_graph_revision: u64,
    domain: &SceneDomainGeneration,
) -> Result<(), StoreError> {
    if record_id.is_empty()
        || record_id.len() > 1_024
        || authority.is_empty()
        || resource_revision > domain.resource_revision
        || scene_graph_revision > domain.scene_graph_revision
    {
        return Err(StoreError::ValidationFailed(
            "scene record coordinates are invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_scene_records<'a, T, F>(
    records: &'a [T],
    identity: F,
    domain: &SceneDomainGeneration,
) -> Result<(), StoreError>
where
    F: Fn(&'a T) -> (&'a String, &'a String, u64, u64),
{
    let mut ids = BTreeSet::new();
    for record in records {
        let (record_id, authority, resource_revision, scene_graph_revision) = identity(record);
        validate_scene_record_coordinates(
            record_id,
            authority,
            resource_revision,
            scene_graph_revision,
            domain,
        )?;
        if !ids.insert(record_id.as_str()) {
            return Err(StoreError::ValidationFailed(
                "duplicate scene semantic record".to_owned(),
            ));
        }
    }
    Ok(())
}

fn encoded_len<T: Serialize>(value: &T) -> Result<usize, StoreError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|error| StoreError::ValidationFailed(error.to_string()))
}

/// Immutable logical generation shared by both spike backends.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexGeneration {
    /// Opaque generation identifier.
    pub generation_id: String,
    /// Previous active generation, when applicable.
    pub parent_generation_id: Option<String>,
    /// Logical schema written by this generation.
    pub schema_version: SchemaVersion,
    /// Project binding.
    pub project_id: String,
    /// Durable revision assigned on activation.
    pub index_revision: u64,
    /// Lifecycle state represented by this serialized record.
    pub state: GenerationState,
    /// Full snapshot, incremental update, or migration.
    pub creation_reason: String,
    /// Source checkpoint.
    pub checkpoint: IngestionCheckpoint,
    /// Resources keyed by entity ID.
    pub resources: Vec<ResourceEntity>,
    /// Source observations keyed by entity ID.
    pub source_documents: Vec<SourceDocument>,
    /// Direct dependency declarations.
    pub dependencies: Vec<DependencyEdge>,
    /// Diagnostics visible at this revision.
    pub diagnostics: Vec<Diagnostic>,
    /// Bounded deletion records.
    pub tombstones: Vec<Tombstone>,
    /// Independently checkpointed scene domain added by logical schema 1.2.
    #[serde(default)]
    pub scene: SceneDomainGeneration,
    /// Digest of the canonical normalized generation.
    pub validation_digest: String,
}

/// Atomic normalized delta applied to one exact committed generation.
///
/// The batch contains only logical records. Physical row IDs, segment numbers,
/// backend cursors, and filesystem coordinates deliberately cannot cross this
/// boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncrementalBatch {
    /// Project binding shared with the base generation.
    pub project_id: String,
    /// Exact generation that the delta was computed from.
    pub base_generation_id: String,
    /// Generation created when the batch commits.
    pub generation_id: String,
    /// Monotonically increasing durable index revision.
    pub index_revision: u64,
    /// Stable reason such as `incremental_rename` or `incremental_delete`.
    pub creation_reason: String,
    /// Source checkpoint activated atomically with the records.
    pub checkpoint: IngestionCheckpoint,
    /// Resources inserted or replaced by entity ID.
    pub upsert_resources: Vec<ResourceEntity>,
    /// Resource entity IDs removed by the batch.
    pub remove_resource_entity_ids: Vec<String>,
    /// Source documents inserted or replaced by entity ID.
    pub upsert_source_documents: Vec<SourceDocument>,
    /// Source-document entity IDs removed by the batch.
    pub remove_source_document_entity_ids: Vec<String>,
    /// Dependency edges inserted or replaced by edge ID.
    pub upsert_dependencies: Vec<DependencyEdge>,
    /// Dependency edge IDs removed by the batch.
    pub remove_dependency_edge_ids: Vec<String>,
    /// Diagnostics inserted or replaced by diagnostic ID.
    pub upsert_diagnostics: Vec<Diagnostic>,
    /// Diagnostic IDs removed by the batch.
    pub remove_diagnostic_ids: Vec<String>,
    /// Tombstones inserted or replaced by entity ID.
    pub upsert_tombstones: Vec<Tombstone>,
    /// Tombstone entity IDs removed by the batch.
    pub remove_tombstone_entity_ids: Vec<String>,
}

/// Selector accepted by storage-neutral direct queries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ResourceSelector {
    /// Select by opaque entity ID.
    EntityId(String),
    /// Select by canonical UID.
    Uid(String),
    /// Select by normalized comparison path.
    ComparisonPath(String),
}

/// Storage-neutral direct query.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceQuery {
    /// Resource to inspect.
    pub selector: ResourceSelector,
    /// Maximum records, already validated against the public hard limit.
    pub limit: usize,
    /// Canonical result offset carried only by a signed public cursor.
    #[serde(default)]
    pub offset: usize,
}

/// One committed query result bound to a generation and revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceQueryResult {
    /// Generation used for all returned records.
    pub generation_id: String,
    /// Durable revision used for all returned records.
    pub index_revision: u64,
    /// Resolved selector entity.
    pub resource: ResourceEntity,
    /// Deterministically ordered direct or reverse edges.
    pub edges: Vec<DependencyEdge>,
    /// Whether the result is complete and exact for its direct domain.
    pub exact: bool,
    /// Whether another deterministic page exists in this generation.
    pub has_more: bool,
}

/// Stable storage-neutral failures.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum StoreError {
    /// Another process owns the project store writer lease.
    #[error("store_busy")]
    StoreBusy,
    /// Durable data failed integrity validation.
    #[error("corrupt_store: {0}")]
    CorruptStore(String),
    /// Logical or physical schema cannot be read safely.
    #[error("incompatible_schema")]
    IncompatibleSchema,
    /// Work stopped before activation.
    #[error("cancelled")]
    Cancelled,
    /// A complete generation failed an invariant.
    #[error("validation_failed: {0}")]
    ValidationFailed(String),
    /// Durable data belongs to another project.
    #[error("project_mismatch")]
    ProjectMismatch,
    /// No compatible active generation exists.
    #[error("index_not_ready")]
    NotReady,
    /// Selector does not identify a resource.
    #[error("resource_not_found")]
    ResourceNotFound,
    /// Physical backend I/O failed.
    #[error("storage_io: {0}")]
    StorageIo(String),
}

/// Read operations over exactly one active committed generation.
pub trait IndexRead {
    /// Returns durable index metadata.
    fn metadata(&self) -> Result<IndexMetadata, StoreError>;
    /// Resolves a resource selector.
    fn resource(&self, selector: &ResourceSelector) -> Result<ResourceEntity, StoreError>;
    /// Returns direct dependencies in canonical order.
    fn direct_dependencies(&self, query: &ResourceQuery)
    -> Result<ResourceQueryResult, StoreError>;
    /// Returns direct reverse owners in canonical order.
    fn reverse_owners(&self, query: &ResourceQuery) -> Result<ResourceQueryResult, StoreError>;
}

/// Atomic incremental mutation over the active generation.
pub trait IndexWriteTransaction {
    /// Applies a complete normalized generation replacement for the spike.
    fn replace_generation(&mut self, generation: IndexGeneration) -> Result<(), StoreError>;
    /// Applies one logical incremental batch against the transaction generation.
    fn apply_incremental_batch(&mut self, batch: IncrementalBatch) -> Result<(), StoreError>;
    /// Durably activates all transaction changes exactly once.
    fn commit(self) -> Result<IndexMetadata, StoreError>;
    /// Abandons all changes before activation.
    fn cancel(self) -> Result<(), StoreError>;
}

/// Full-generation staging and activation boundary.
pub trait GenerationBuilder {
    /// Backend transaction/build handle borrowing its writer lease.
    type Transaction<'a>: IndexWriteTransaction
    where
        Self: 'a;
    /// Begins one isolated staging generation.
    fn begin_generation<'a>(
        &'a mut self,
        generation: &IndexGeneration,
    ) -> Result<Self::Transaction<'a>, StoreError>;
}

/// Physical format migration that preserves logical results.
pub trait MigrationRunner {
    /// Migrates to a new physical format through a staging generation.
    fn migrate(&mut self, target_physical_version: u32) -> Result<IndexMetadata, StoreError>;
}

impl IndexGeneration {
    /// Applies a storage-neutral incremental batch and validates the complete result.
    pub fn apply_incremental_batch(&self, batch: &IncrementalBatch) -> Result<Self, StoreError> {
        if batch.project_id != self.project_id {
            return Err(StoreError::ProjectMismatch);
        }
        if batch.base_generation_id != self.generation_id {
            return Err(StoreError::ValidationFailed(
                "incremental base generation mismatch".to_owned(),
            ));
        }
        if batch.generation_id.is_empty() || batch.generation_id == self.generation_id {
            return Err(StoreError::ValidationFailed(
                "incremental generation identity is not new".to_owned(),
            ));
        }
        if batch.index_revision <= self.index_revision
            || batch.checkpoint.index_revision != batch.index_revision
        {
            return Err(StoreError::ValidationFailed(
                "incremental revision is not monotonic".to_owned(),
            ));
        }

        let mut resources: BTreeMap<_, _> = self
            .resources
            .iter()
            .cloned()
            .map(|record| (record.entity_id.clone(), record))
            .collect();
        for entity_id in &batch.remove_resource_entity_ids {
            resources.remove(entity_id);
        }
        for record in &batch.upsert_resources {
            resources.insert(record.entity_id.clone(), record.clone());
        }

        let mut source_documents: BTreeMap<_, _> = self
            .source_documents
            .iter()
            .cloned()
            .map(|record| (record.entity_id.clone(), record))
            .collect();
        for entity_id in &batch.remove_source_document_entity_ids {
            source_documents.remove(entity_id);
        }
        for record in &batch.upsert_source_documents {
            source_documents.insert(record.entity_id.clone(), record.clone());
        }

        let mut dependencies: BTreeMap<_, _> = self
            .dependencies
            .iter()
            .cloned()
            .map(|record| (record.edge_id.clone(), record))
            .collect();
        for edge_id in &batch.remove_dependency_edge_ids {
            dependencies.remove(edge_id);
        }
        for record in &batch.upsert_dependencies {
            dependencies.insert(record.edge_id.clone(), record.clone());
        }

        let mut diagnostics: BTreeMap<_, _> = self
            .diagnostics
            .iter()
            .cloned()
            .map(|record| (record.diagnostic_id.clone(), record))
            .collect();
        for diagnostic_id in &batch.remove_diagnostic_ids {
            diagnostics.remove(diagnostic_id);
        }
        for record in &batch.upsert_diagnostics {
            diagnostics.insert(record.diagnostic_id.clone(), record.clone());
        }

        let mut tombstones: BTreeMap<_, _> = self
            .tombstones
            .iter()
            .cloned()
            .map(|record| (record.entity_id.clone(), record))
            .collect();
        for entity_id in &batch.remove_tombstone_entity_ids {
            tombstones.remove(entity_id);
        }
        for record in &batch.upsert_tombstones {
            tombstones.insert(record.entity_id.clone(), record.clone());
        }

        let mut next = Self {
            generation_id: batch.generation_id.clone(),
            parent_generation_id: Some(self.generation_id.clone()),
            schema_version: self.schema_version,
            project_id: self.project_id.clone(),
            index_revision: batch.index_revision,
            state: GenerationState::Active,
            creation_reason: batch.creation_reason.clone(),
            checkpoint: batch.checkpoint.clone(),
            resources: resources.into_values().collect(),
            source_documents: source_documents.into_values().collect(),
            dependencies: dependencies.into_values().collect(),
            diagnostics: diagnostics.into_values().collect(),
            tombstones: tombstones.into_values().collect(),
            scene: self.scene.clone(),
            validation_digest: String::new(),
        };
        next.canonicalize();
        next.validation_digest = next.compute_validation_digest();
        next.validate()?;
        Ok(next)
    }

    /// Validates frozen project/schema/identity/direct-reverse invariants.
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.schema_version.major != LOGICAL_SCHEMA_V1.major
            || self.schema_version.minor > LOGICAL_SCHEMA_V1.minor
        {
            return Err(StoreError::IncompatibleSchema);
        }
        if self.project_id.is_empty() || self.generation_id.is_empty() {
            return Err(StoreError::ValidationFailed(
                "empty project or generation identity".to_owned(),
            ));
        }
        if self.checkpoint.index_revision != self.index_revision {
            return Err(StoreError::ValidationFailed(
                "checkpoint/index revision mismatch".to_owned(),
            ));
        }
        if self.checkpoint.last_batch_id.is_some() != self.checkpoint.last_batch_checksum.is_some()
        {
            return Err(StoreError::ValidationFailed(
                "partial batch checkpoint identity".to_owned(),
            ));
        }

        let mut entity_inputs = BTreeMap::new();
        let mut paths = BTreeSet::new();
        let mut uids = BTreeSet::new();
        for resource in &self.resources {
            if resource.source_kind.is_empty()
                || resource.authority.is_empty()
                || (resource.identity_strength == IdentityStrength::PathContentGeneration
                    && resource.content_generation.is_none())
            {
                return Err(StoreError::ValidationFailed(
                    "resource authority or content identity missing".to_owned(),
                ));
            }
            if let Some(previous) = entity_inputs.insert(
                resource.entity_id.as_str(),
                resource.identity_input.as_str(),
            ) {
                if previous != resource.identity_input {
                    return Err(StoreError::ValidationFailed(
                        "entity_id_collision".to_owned(),
                    ));
                }
                return Err(StoreError::ValidationFailed(
                    "duplicate entity_id".to_owned(),
                ));
            }
            if !paths.insert(resource.comparison_path.as_str()) {
                return Err(StoreError::ValidationFailed(
                    "path_normalization_collision".to_owned(),
                ));
            }
            if let Some(uid) = resource.uid.as_deref()
                && !uids.insert(uid)
            {
                return Err(StoreError::ValidationFailed(
                    "duplicate_resource_uid".to_owned(),
                ));
            }
        }

        let known_entities: BTreeSet<_> = self
            .resources
            .iter()
            .map(|resource| resource.entity_id.as_str())
            .collect();
        let mut source_entities = BTreeSet::new();
        for document in &self.source_documents {
            if !known_entities.contains(document.entity_id.as_str()) {
                return Err(StoreError::ValidationFailed(
                    "source document entity missing".to_owned(),
                ));
            }
            if !source_entities.insert(document.entity_id.as_str()) {
                return Err(StoreError::ValidationFailed(
                    "duplicate source document".to_owned(),
                ));
            }
        }
        let mut edge_ids = BTreeSet::new();
        for edge in &self.dependencies {
            if edge.relation != "references" || edge.authority.is_empty() {
                return Err(StoreError::ValidationFailed(
                    "dependency authority or relation missing".to_owned(),
                ));
            }
            if !known_entities.contains(edge.source_entity_id.as_str()) {
                return Err(StoreError::ValidationFailed(
                    "dependency source missing".to_owned(),
                ));
            }
            if !edge_ids.insert(edge.edge_id.as_str()) {
                return Err(StoreError::ValidationFailed(
                    "duplicate dependency edge".to_owned(),
                ));
            }
            if let Some(target) = edge.target_entity_id.as_deref()
                && !known_entities.contains(target)
            {
                return Err(StoreError::ValidationFailed(
                    "resolved dependency target missing".to_owned(),
                ));
            }
        }
        if self.diagnostics.iter().any(|diagnostic| {
            diagnostic.subject.len() > 1_024
                || diagnostic
                    .detail
                    .as_ref()
                    .is_some_and(|detail| detail.len() > 1_024)
        }) {
            return Err(StoreError::ValidationFailed(
                "diagnostic detail exceeds limit".to_owned(),
            ));
        }
        if self.schema_version.minor < 2 {
            if !self.scene.is_empty() {
                return Err(StoreError::IncompatibleSchema);
            }
        } else {
            self.scene.validate()?;
        }

        let expected = self.compute_validation_digest();
        if !self.validation_digest.is_empty() && self.validation_digest != expected {
            return Err(StoreError::ValidationFailed(
                "generation validation digest mismatch".to_owned(),
            ));
        }
        Ok(())
    }

    /// Computes a deterministic digest independent of physical storage layout.
    #[must_use]
    pub fn compute_validation_digest(&self) -> String {
        let mut resources: Vec<_> = self.resources.iter().collect();
        resources.sort_by(|left, right| left.entity_id.cmp(&right.entity_id));
        let mut dependencies: Vec<_> = self.dependencies.iter().collect();
        dependencies.sort_by(|left, right| left.edge_id.cmp(&right.edge_id));

        let mut hasher = Sha256::new();
        hasher.update(b"godot-codex/index-generation/v1\0");
        hasher.update(self.schema_version.major.to_be_bytes());
        hasher.update(self.schema_version.minor.to_be_bytes());
        update_field(&mut hasher, &self.project_id);
        update_field(&mut hasher, &self.generation_id);
        update_field(
            &mut hasher,
            self.parent_generation_id.as_deref().unwrap_or_default(),
        );
        hasher.update(self.index_revision.to_be_bytes());
        update_field(&mut hasher, &self.creation_reason);
        update_field(&mut hasher, &self.checkpoint.editor_session_id);
        hasher.update(self.checkpoint.resource_revision.to_be_bytes());
        hasher.update(self.checkpoint.project_revision.to_be_bytes());
        hasher.update(self.checkpoint.index_revision.to_be_bytes());
        hasher.update([u8::from(self.checkpoint.source_complete)]);
        update_field(&mut hasher, &self.checkpoint.snapshot_checksum);
        update_field(
            &mut hasher,
            self.checkpoint.last_batch_id.as_deref().unwrap_or_default(),
        );
        update_field(
            &mut hasher,
            self.checkpoint
                .last_batch_checksum
                .as_deref()
                .unwrap_or_default(),
        );
        for resource in resources {
            update_field(&mut hasher, &resource.entity_id);
            update_field(&mut hasher, &resource.identity_input);
            update_field(&mut hasher, resource.uid.as_deref().unwrap_or_default());
            update_field(&mut hasher, &resource.display_path);
            update_field(&mut hasher, &resource.comparison_path);
            hasher.update([match resource.identity_strength {
                IdentityStrength::ResourceUid => 1,
                IdentityStrength::PathContentGeneration => 2,
            }]);
            update_field(&mut hasher, &resource.resource_type);
            update_field(&mut hasher, &resource.source_kind);
            update_field(&mut hasher, &resource.import_state);
            update_field(&mut hasher, &resource.authority);
            update_field(
                &mut hasher,
                resource.content_generation.as_deref().unwrap_or_default(),
            );
            hasher.update(resource.mtime_ns.to_be_bytes());
            hasher.update(resource.byte_size.to_be_bytes());
            hasher.update([match resource.validity {
                RecordValidity::Valid => 1,
                RecordValidity::Partial => 2,
                RecordValidity::Invalid => 3,
                RecordValidity::Deleted => 4,
            }]);
            hasher.update(resource.resource_revision.to_be_bytes());
        }
        let mut source_documents: Vec<_> = self.source_documents.iter().collect();
        source_documents.sort_by(|left, right| left.entity_id.cmp(&right.entity_id));
        for document in source_documents {
            update_field(&mut hasher, &document.entity_id);
            update_field(&mut hasher, &document.comparison_path);
            hasher.update(document.size_before.to_be_bytes());
            hasher.update(document.size_after.to_be_bytes());
            hasher.update(document.mtime_before_ns.to_be_bytes());
            hasher.update(document.mtime_after_ns.to_be_bytes());
            update_field(
                &mut hasher,
                document.content_generation.as_deref().unwrap_or_default(),
            );
            update_field(&mut hasher, &document.ingest_state);
        }
        for edge in dependencies {
            update_field(&mut hasher, &edge.edge_id);
            update_field(&mut hasher, &edge.source_entity_id);
            update_field(&mut hasher, edge.target_uid.as_deref().unwrap_or_default());
            update_field(
                &mut hasher,
                edge.target_comparison_path.as_deref().unwrap_or_default(),
            );
            update_field(
                &mut hasher,
                edge.target_display_path.as_deref().unwrap_or_default(),
            );
            update_field(
                &mut hasher,
                edge.target_entity_id.as_deref().unwrap_or_default(),
            );
            update_field(
                &mut hasher,
                edge.resolved_target_path.as_deref().unwrap_or_default(),
            );
            update_field(&mut hasher, &edge.relation);
            update_field(
                &mut hasher,
                edge.declared_type.as_deref().unwrap_or_default(),
            );
            update_field(&mut hasher, &edge.authority);
            hasher.update([match edge.resolution {
                DependencyResolution::Resolved => 1,
                DependencyResolution::Missing => 2,
                DependencyResolution::StaleUid => 3,
            }]);
            hasher.update(edge.resource_revision.to_be_bytes());
        }
        let mut diagnostics: Vec<_> = self.diagnostics.iter().collect();
        diagnostics.sort_by(|left, right| left.diagnostic_id.cmp(&right.diagnostic_id));
        for diagnostic in diagnostics {
            update_field(&mut hasher, &diagnostic.diagnostic_id);
            update_field(&mut hasher, &diagnostic.code);
            update_field(&mut hasher, &diagnostic.subject);
            update_field(
                &mut hasher,
                diagnostic.detail.as_deref().unwrap_or_default(),
            );
            hasher.update(diagnostic.first_index_revision.to_be_bytes());
            hasher.update(diagnostic.last_index_revision.to_be_bytes());
            hasher.update([u8::from(diagnostic.active)]);
        }
        let mut tombstones: Vec<_> = self.tombstones.iter().collect();
        tombstones.sort_by(|left, right| left.entity_id.cmp(&right.entity_id));
        for tombstone in tombstones {
            update_field(&mut hasher, &tombstone.entity_id);
            hasher.update(tombstone.deleted_index_revision.to_be_bytes());
            hasher.update(tombstone.retain_through_index_revision.to_be_bytes());
        }
        if self.schema_version.minor >= 2 {
            update_field(&mut hasher, &self.scene.compute_validation_digest());
        }
        format!("sha256:{:x}", hasher.finalize())
    }

    /// Computes the current resource-graph identity without editor-session or
    /// durable-generation coordinates.
    ///
    /// A fresh editor session restarts its resource revision sequence and a
    /// validating full snapshot receives a provisional index revision. Neither
    /// changes the graph represented by an otherwise compatible persistent
    /// generation. Diagnostic history and deletion tombstones are likewise
    /// generation history rather than current graph facts.
    #[must_use]
    pub fn compute_graph_compatibility_digest(&self) -> String {
        let mut resources: Vec<_> = self.resources.iter().collect();
        resources.sort_by(|left, right| left.entity_id.cmp(&right.entity_id));
        let mut source_documents: Vec<_> = self.source_documents.iter().collect();
        source_documents.sort_by(|left, right| left.entity_id.cmp(&right.entity_id));
        let mut dependencies: Vec<_> = self.dependencies.iter().collect();
        dependencies.sort_by(|left, right| left.edge_id.cmp(&right.edge_id));
        let mut diagnostics: Vec<_> = self
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.active)
            .collect();
        diagnostics.sort_by(|left, right| {
            left.code
                .cmp(&right.code)
                .then_with(|| left.subject.cmp(&right.subject))
                .then_with(|| left.detail.cmp(&right.detail))
        });

        let mut hasher = Sha256::new();
        hasher.update(b"godot-codex/index-graph-compatibility/v1\0");
        hasher.update(self.schema_version.major.to_be_bytes());
        hasher.update(self.schema_version.minor.to_be_bytes());
        update_field(&mut hasher, &self.project_id);
        hasher.update(b"resources\0");
        for resource in resources {
            update_field(&mut hasher, &resource.entity_id);
            update_field(&mut hasher, &resource.identity_input);
            update_field(&mut hasher, resource.uid.as_deref().unwrap_or_default());
            update_field(&mut hasher, &resource.display_path);
            update_field(&mut hasher, &resource.comparison_path);
            hasher.update([match resource.identity_strength {
                IdentityStrength::ResourceUid => 1,
                IdentityStrength::PathContentGeneration => 2,
            }]);
            update_field(&mut hasher, &resource.resource_type);
            update_field(&mut hasher, &resource.source_kind);
            update_field(&mut hasher, &resource.import_state);
            update_field(&mut hasher, &resource.authority);
            update_field(
                &mut hasher,
                resource.content_generation.as_deref().unwrap_or_default(),
            );
            hasher.update(resource.mtime_ns.to_be_bytes());
            hasher.update(resource.byte_size.to_be_bytes());
            hasher.update([match resource.validity {
                RecordValidity::Valid => 1,
                RecordValidity::Partial => 2,
                RecordValidity::Invalid => 3,
                RecordValidity::Deleted => 4,
            }]);
        }
        hasher.update(b"source-documents\0");
        for document in source_documents {
            update_field(&mut hasher, &document.entity_id);
            update_field(&mut hasher, &document.comparison_path);
            hasher.update(document.size_before.to_be_bytes());
            hasher.update(document.size_after.to_be_bytes());
            hasher.update(document.mtime_before_ns.to_be_bytes());
            hasher.update(document.mtime_after_ns.to_be_bytes());
            update_field(
                &mut hasher,
                document.content_generation.as_deref().unwrap_or_default(),
            );
            update_field(&mut hasher, &document.ingest_state);
        }
        hasher.update(b"dependencies\0");
        for edge in dependencies {
            update_field(&mut hasher, &edge.edge_id);
            update_field(&mut hasher, &edge.source_entity_id);
            update_field(&mut hasher, edge.target_uid.as_deref().unwrap_or_default());
            update_field(
                &mut hasher,
                edge.target_comparison_path.as_deref().unwrap_or_default(),
            );
            update_field(
                &mut hasher,
                edge.target_display_path.as_deref().unwrap_or_default(),
            );
            update_field(
                &mut hasher,
                edge.target_entity_id.as_deref().unwrap_or_default(),
            );
            update_field(
                &mut hasher,
                edge.resolved_target_path.as_deref().unwrap_or_default(),
            );
            update_field(&mut hasher, &edge.relation);
            update_field(
                &mut hasher,
                edge.declared_type.as_deref().unwrap_or_default(),
            );
            update_field(&mut hasher, &edge.authority);
            hasher.update([match edge.resolution {
                DependencyResolution::Resolved => 1,
                DependencyResolution::Missing => 2,
                DependencyResolution::StaleUid => 3,
            }]);
        }
        hasher.update(b"active-diagnostics\0");
        for diagnostic in diagnostics {
            update_field(&mut hasher, &diagnostic.code);
            update_field(&mut hasher, &diagnostic.subject);
            update_field(
                &mut hasher,
                diagnostic.detail.as_deref().unwrap_or_default(),
            );
        }
        format!("sha256:{:x}", hasher.finalize())
    }

    /// Returns whether a validating full snapshot represents the same current
    /// resource graph, independent of editor-session and index history.
    #[must_use]
    pub fn graph_is_compatible_with(&self, observed: &Self) -> bool {
        self.compute_graph_compatibility_digest() == observed.compute_graph_compatibility_digest()
    }

    /// Sorts all logical collections into canonical physical-ingest order.
    pub fn canonicalize(&mut self) {
        self.resources.sort_by(|left, right| {
            left.comparison_path
                .cmp(&right.comparison_path)
                .then_with(|| left.entity_id.cmp(&right.entity_id))
        });
        self.source_documents
            .sort_by(|left, right| left.entity_id.cmp(&right.entity_id));
        self.dependencies.sort_by(|left, right| {
            left.source_entity_id
                .cmp(&right.source_entity_id)
                .then_with(|| dependency_target_key(left).cmp(dependency_target_key(right)))
                .then_with(|| left.edge_id.cmp(&right.edge_id))
        });
        self.diagnostics
            .sort_by(|left, right| left.diagnostic_id.cmp(&right.diagnostic_id));
        self.tombstones
            .sort_by(|left, right| left.entity_id.cmp(&right.entity_id));
        self.scene.canonicalize();
    }
}

/// Resolves a selector against one immutable generation.
pub fn resolve_resource<'a>(
    generation: &'a IndexGeneration,
    selector: &ResourceSelector,
) -> Result<&'a ResourceEntity, StoreError> {
    generation
        .resources
        .iter()
        .find(|resource| match selector {
            ResourceSelector::EntityId(value) => resource.entity_id == *value,
            ResourceSelector::Uid(value) => resource.uid.as_ref() == Some(value),
            ResourceSelector::ComparisonPath(value) => resource.comparison_path == *value,
        })
        .ok_or(StoreError::ResourceNotFound)
}

/// Executes a direct or reverse query over one immutable generation.
pub fn query_generation(
    generation: &IndexGeneration,
    query: &ResourceQuery,
    reverse: bool,
) -> Result<ResourceQueryResult, StoreError> {
    if !(1..=200).contains(&query.limit) {
        return Err(StoreError::ValidationFailed(
            "invalid query limit".to_owned(),
        ));
    }
    let resource = resolve_resource(generation, &query.selector)?.clone();
    let mut edges: Vec<_> = generation
        .dependencies
        .iter()
        .filter(|edge| {
            if reverse {
                edge.target_entity_id.as_ref() == Some(&resource.entity_id)
                    || edge
                        .target_uid
                        .as_ref()
                        .is_some_and(|uid| resource.uid.as_ref() == Some(uid))
                    || edge.target_comparison_path.as_ref() == Some(&resource.comparison_path)
            } else {
                edge.source_entity_id == resource.entity_id
            }
        })
        .cloned()
        .collect();
    edges.sort_by(|left, right| compare_dependency_edges(generation, left, right, reverse));
    let exact = edges.iter().all(|edge| {
        edge.resolution == DependencyResolution::Resolved && edge.target_entity_id.is_some()
    });
    let has_more = query
        .offset
        .checked_add(query.limit)
        .is_some_and(|end| end < edges.len());
    let edges = edges
        .into_iter()
        .skip(query.offset)
        .take(query.limit)
        .collect();
    Ok(ResourceQueryResult {
        generation_id: generation.generation_id.clone(),
        index_revision: generation.index_revision,
        resource,
        edges,
        exact,
        has_more,
    })
}

pub(crate) fn compare_dependency_edges(
    generation: &IndexGeneration,
    left: &DependencyEdge,
    right: &DependencyEdge,
    reverse: bool,
) -> std::cmp::Ordering {
    let related = |edge: &DependencyEdge| {
        let entity_id = if reverse {
            Some(edge.source_entity_id.as_str())
        } else {
            edge.target_entity_id.as_deref()
        };
        entity_id.and_then(|entity_id| {
            generation
                .resources
                .iter()
                .find(|resource| resource.entity_id == entity_id)
        })
    };
    let left_resource = related(left);
    let right_resource = related(right);
    let left_path = left_resource.map_or_else(
        || left.target_comparison_path.as_deref().unwrap_or_default(),
        |resource| resource.comparison_path.as_str(),
    );
    let right_path = right_resource.map_or_else(
        || right.target_comparison_path.as_deref().unwrap_or_default(),
        |resource| resource.comparison_path.as_str(),
    );
    let left_uid = left_resource.and_then(|resource| resource.uid.as_deref());
    let right_uid = right_resource.and_then(|resource| resource.uid.as_deref());
    let left_entity = left_resource.map_or_else(
        || left.target_entity_id.as_deref().unwrap_or_default(),
        |resource| resource.entity_id.as_str(),
    );
    let right_entity = right_resource.map_or_else(
        || right.target_entity_id.as_deref().unwrap_or_default(),
        |resource| resource.entity_id.as_str(),
    );
    left_path
        .cmp(right_path)
        .then_with(|| left_uid.is_none().cmp(&right_uid.is_none()))
        .then_with(|| {
            left_uid
                .unwrap_or_default()
                .cmp(right_uid.unwrap_or_default())
        })
        .then_with(|| left_entity.cmp(right_entity))
        .then_with(|| left.edge_id.cmp(&right.edge_id))
}

fn dependency_target_key(edge: &DependencyEdge) -> &str {
    edge.target_uid
        .as_deref()
        .or(edge.target_comparison_path.as_deref())
        .unwrap_or_default()
}

fn update_field(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;
    use tempfile::TempDir;

    fn generation() -> IndexGeneration {
        let resource = |id: &str, uid: &str, path: &str| ResourceEntity {
            entity_id: id.to_owned(),
            identity_input: uid.to_owned(),
            uid: Some(uid.to_owned()),
            display_path: path.to_owned(),
            comparison_path: path.to_owned(),
            identity_strength: IdentityStrength::ResourceUid,
            resource_type: "Resource".to_owned(),
            source_kind: "source".to_owned(),
            import_state: "ready".to_owned(),
            authority: "editor_file_system".to_owned(),
            content_generation: Some(format!("sha256:{id}")),
            mtime_ns: 1,
            byte_size: 1,
            validity: RecordValidity::Valid,
            resource_revision: 1,
        };
        let mut generation = IndexGeneration {
            generation_id: "generation-1".to_owned(),
            parent_generation_id: None,
            schema_version: LOGICAL_SCHEMA_V1,
            project_id: "project-1".to_owned(),
            index_revision: 1,
            state: GenerationState::Active,
            creation_reason: "full_snapshot".to_owned(),
            checkpoint: IngestionCheckpoint {
                editor_session_id: "session-1".to_owned(),
                resource_revision: 1,
                project_revision: 1,
                index_revision: 1,
                source_complete: true,
                snapshot_checksum: "sha256:snapshot".to_owned(),
                last_batch_id: None,
                last_batch_checksum: None,
            },
            resources: vec![
                resource("entity-a", "uid://a", "res://a.tres"),
                resource("entity-b", "uid://b", "res://b.tres"),
            ],
            source_documents: vec![SourceDocument {
                entity_id: "entity-a".to_owned(),
                comparison_path: "res://a.tres".to_owned(),
                size_before: 1,
                size_after: 1,
                mtime_before_ns: 1,
                mtime_after_ns: 1,
                content_generation: Some("sha256:entity-a".to_owned()),
                ingest_state: "ready".to_owned(),
            }],
            dependencies: vec![DependencyEdge {
                edge_id: "edge-a-b".to_owned(),
                source_entity_id: "entity-a".to_owned(),
                target_uid: Some("uid://b".to_owned()),
                target_comparison_path: Some("res://b.tres".to_owned()),
                target_display_path: Some("res://b.tres".to_owned()),
                target_entity_id: Some("entity-b".to_owned()),
                resolved_target_path: Some("res://b.tres".to_owned()),
                relation: "references".to_owned(),
                declared_type: None,
                authority: "godot_resource_loader".to_owned(),
                resolution: DependencyResolution::Resolved,
                resource_revision: 1,
            }],
            diagnostics: Vec::new(),
            tombstones: Vec::new(),
            scene: SceneDomainGeneration::default(),
            validation_digest: String::new(),
        };
        generation.validation_digest = generation.compute_validation_digest();
        generation
    }

    fn scene_domain() -> SceneDomainGeneration {
        let scene_id = "godot:scene:test".to_owned();
        let root_id = "godot:node:root".to_owned();
        let child_id = "godot:node:child".to_owned();
        let mut domain = SceneDomainGeneration {
            editor_session_id: "session-1".to_owned(),
            resource_revision: 1,
            scene_graph_revision: 3,
            source_complete: true,
            snapshot_checksum: "sha256:scene-snapshot".to_owned(),
            scenes: vec![SceneEntity {
                scene_entity_id: scene_id.clone(),
                source_resource_entity_id: Some("entity-a".to_owned()),
                uid: Some("uid://scene-test".to_owned()),
                comparison_path: "res://main.tscn".to_owned(),
                content_generation: "sha256:scene-content".to_owned(),
                identity_scope: SceneIdentityScope::Persistent,
                base_scene_entity_id: None,
                authority: "godot_scene_state".to_owned(),
                resource_revision: 1,
                scene_graph_revision: 3,
            }],
            nodes: vec![
                SceneNode {
                    node_entity_id: root_id.clone(),
                    scene_entity_id: scene_id.clone(),
                    node_path: ".".to_owned(),
                    identity_scope: SceneIdentityScope::Persistent,
                    unique_scene_id: Some(1001),
                    parent_node_entity_id: None,
                    owner_node_entity_id: None,
                    name: "Main".to_owned(),
                    godot_type: "Node2D".to_owned(),
                    node_index: 0,
                    owned: true,
                    internal: false,
                    attached_script_entity_id: None,
                    authority: "godot_scene_state".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 3,
                },
                SceneNode {
                    node_entity_id: child_id.clone(),
                    scene_entity_id: scene_id.clone(),
                    node_path: "Player".to_owned(),
                    identity_scope: SceneIdentityScope::Persistent,
                    unique_scene_id: Some(1002),
                    parent_node_entity_id: Some(root_id.clone()),
                    owner_node_entity_id: Some(root_id.clone()),
                    name: "Player".to_owned(),
                    godot_type: "CharacterBody2D".to_owned(),
                    node_index: 0,
                    owned: true,
                    internal: false,
                    attached_script_entity_id: Some("entity-b".to_owned()),
                    authority: "godot_scene_state".to_owned(),
                    resource_revision: 1,
                    scene_graph_revision: 3,
                },
            ],
            properties: vec![SceneProperty {
                property_id: "property:player-speed".to_owned(),
                scene_entity_id: scene_id.clone(),
                subject_entity_id: child_id.clone(),
                name: "speed".to_owned(),
                value_type: "float".to_owned(),
                value: serde_json::json!(240.0),
                truncated: false,
                declaring_scene_entity_id: scene_id.clone(),
                declaring_node_entity_id: child_id.clone(),
                origin: ScenePropertyOrigin::Local,
                overridden_property_id: None,
                authority: "godot_scene_state".to_owned(),
                resource_revision: 1,
                scene_graph_revision: 3,
            }],
            relations: vec![SceneRelation {
                relation_id: "relation:main-scene".to_owned(),
                scene_entity_id: None,
                relation: "project_context".to_owned(),
                source: "application/run/main_scene".to_owned(),
                target: Some(scene_id.clone()),
                declaration_scope: None,
                attributes: BTreeMap::from([(
                    "value".to_owned(),
                    serde_json::json!("uid://scene-test"),
                )]),
                authority: "godot_project_settings".to_owned(),
                resource_revision: 1,
                scene_graph_revision: 3,
            }],
            connections: vec![SceneConnection {
                connection_id: "connection:ready".to_owned(),
                scene_entity_id: scene_id.clone(),
                emitter_node_entity_id: child_id.clone(),
                signal: "ready".to_owned(),
                receiver_node_entity_id: Some(root_id.clone()),
                method: "_on_player_ready".to_owned(),
                flags: 1,
                unbinds: 0,
                binds: Vec::new(),
                declaration_scope: "local".to_owned(),
                authority: "godot_scene_state".to_owned(),
                resource_revision: 1,
                scene_graph_revision: 3,
            }],
            groups: vec![SceneGroupMembership {
                membership_id: "group:actors:player".to_owned(),
                scene_entity_id: scene_id.clone(),
                member_node_entity_id: child_id.clone(),
                group: "actors".to_owned(),
                declaration_scope: "local".to_owned(),
                authority: "godot_scene_state".to_owned(),
                resource_revision: 1,
                scene_graph_revision: 3,
            }],
            animations: vec![SceneAnimationReference {
                animation_reference_id: "animation:walk:0".to_owned(),
                scene_entity_id: scene_id,
                mixer_node_entity_id: root_id,
                library: String::new(),
                animation: "walk".to_owned(),
                track_index: 0,
                node_path: "Player:position".to_owned(),
                target_node_entity_id: Some(child_id),
                resolution: SceneAnimationResolution::Resolved,
                authority: "godot_animation".to_owned(),
                resource_revision: 1,
                scene_graph_revision: 3,
            }],
            validation_digest: String::new(),
        };
        domain.canonicalize();
        domain.validation_digest = domain.compute_validation_digest();
        domain
    }

    fn generation_with_scene(base: &IndexGeneration, generation_id: &str) -> IndexGeneration {
        let mut generation = base.clone();
        generation.parent_generation_id = Some(base.generation_id.clone());
        generation.generation_id = generation_id.to_owned();
        generation.index_revision = base.index_revision + 1;
        generation.checkpoint.index_revision = generation.index_revision;
        generation.creation_reason = "scene_full_snapshot".to_owned();
        generation.scene = scene_domain();
        generation.canonicalize();
        generation.validation_digest.clear();
        generation.validation_digest = generation.compute_validation_digest();
        generation
    }

    fn renamed_batch(base: &IndexGeneration) -> IncrementalBatch {
        let mut resource = base.resources[0].clone();
        resource.display_path = "res://renamed-a.tres".to_owned();
        resource.comparison_path = resource.display_path.clone();
        resource.resource_revision = 2;
        let mut document = base.source_documents[0].clone();
        document.comparison_path = resource.comparison_path.clone();
        IncrementalBatch {
            project_id: base.project_id.clone(),
            base_generation_id: base.generation_id.clone(),
            generation_id: "generation-2".to_owned(),
            index_revision: 2,
            creation_reason: "incremental_rename".to_owned(),
            checkpoint: IngestionCheckpoint {
                editor_session_id: "session-1".to_owned(),
                resource_revision: 2,
                project_revision: 2,
                index_revision: 2,
                source_complete: true,
                snapshot_checksum: "sha256:snapshot-2".to_owned(),
                last_batch_id: Some("resource-batch:2".to_owned()),
                last_batch_checksum: Some("sha256:batch-2".to_owned()),
            },
            upsert_resources: vec![resource],
            remove_resource_entity_ids: Vec::new(),
            upsert_source_documents: vec![document],
            remove_source_document_entity_ids: Vec::new(),
            upsert_dependencies: Vec::new(),
            remove_dependency_edge_ids: Vec::new(),
            upsert_diagnostics: Vec::new(),
            remove_diagnostic_ids: Vec::new(),
            upsert_tombstones: Vec::new(),
            remove_tombstone_entity_ids: Vec::new(),
        }
    }

    fn reopened_snapshot(base: &IndexGeneration) -> IndexGeneration {
        let mut observed = base.clone();
        observed.generation_id = "validation-generation-2".to_owned();
        observed.parent_generation_id = None;
        observed.index_revision = 2;
        observed.creation_reason = "full_snapshot".to_owned();
        observed.checkpoint = IngestionCheckpoint {
            editor_session_id: "session-2".to_owned(),
            resource_revision: 7,
            project_revision: 11,
            index_revision: 2,
            source_complete: true,
            snapshot_checksum: "sha256:session-2-snapshot".to_owned(),
            last_batch_id: None,
            last_batch_checksum: None,
        };
        for resource in &mut observed.resources {
            resource.resource_revision = 7;
        }
        for edge in &mut observed.dependencies {
            edge.resource_revision = 7;
        }
        observed.validation_digest.clear();
        observed.validation_digest = observed.compute_validation_digest();
        observed
    }

    fn file_contents(path: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(path)
            .expect("artifact directory")
            .map(|entry| {
                let entry = entry.expect("artifact entry");
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read(entry.path()).expect("artifact bytes"),
                )
            })
            .collect()
    }

    #[test]
    fn validates_and_queries_direct_reverse_parity() {
        let generation = generation();
        generation.validate().expect("valid generation");
        let direct = query_generation(
            &generation,
            &ResourceQuery {
                selector: ResourceSelector::Uid("uid://a".to_owned()),
                limit: 50,
                offset: 0,
            },
            false,
        )
        .expect("direct query");
        let reverse = query_generation(
            &generation,
            &ResourceQuery {
                selector: ResourceSelector::Uid("uid://b".to_owned()),
                limit: 50,
                offset: 0,
            },
            true,
        )
        .expect("reverse query");
        assert_eq!(direct.edges, reverse.edges);
        assert!(direct.exact && reverse.exact);
    }

    #[test]
    fn rejects_identity_collision_and_digest_tampering() {
        let mut collision = generation();
        let mut duplicate = collision.resources[0].clone();
        duplicate.identity_input = "uid://different".to_owned();
        duplicate.comparison_path = "res://different.tres".to_owned();
        collision.resources.push(duplicate);
        assert!(matches!(
            collision.validate(),
            Err(StoreError::ValidationFailed(reason)) if reason == "entity_id_collision"
        ));

        let mut tampered = generation();
        tampered.resources[0].content_generation = Some("sha256:tampered".to_owned());
        assert!(matches!(
            tampered.validate(),
            Err(StoreError::ValidationFailed(reason)) if reason.contains("digest")
        ));

        let mut tampered_source = generation();
        tampered_source.source_documents[0].ingest_state = "tampered".to_owned();
        assert!(matches!(
            tampered_source.validate(),
            Err(StoreError::ValidationFailed(reason)) if reason.contains("digest")
        ));
    }

    #[test]
    fn applies_incremental_batch_against_exact_base_generation() {
        let base = generation();
        let batch = renamed_batch(&base);
        let next = base
            .apply_incremental_batch(&batch)
            .expect("incremental batch");
        assert_eq!(next.parent_generation_id.as_deref(), Some("generation-1"));
        assert_eq!(
            next.resources
                .iter()
                .find(|record| record.entity_id == "entity-a")
                .expect("renamed entity")
                .comparison_path,
            "res://renamed-a.tres"
        );
        assert_eq!(next.checkpoint.index_revision, 2);

        let mut wrong_base = batch;
        wrong_base.base_generation_id = "generation-other".to_owned();
        assert!(matches!(
            base.apply_incremental_batch(&wrong_base),
            Err(StoreError::ValidationFailed(reason)) if reason.contains("base generation")
        ));
    }

    #[test]
    fn protocol_generation_ids_use_portable_physical_filenames() {
        let temp = TempDir::new().expect("temp");
        let mut base = generation();
        base.generation_id = format!("generation:sha256:{}", "a".repeat(64));
        base.validation_digest = base.compute_validation_digest();
        {
            let mut store = SegmentStore::open(temp.path(), &base.project_id).expect("store");
            store.activate(&base, None).expect("activate base");
            assert_eq!(
                store.active_generation().expect("active").generation_id,
                base.generation_id
            );
        }

        let index_root = temp.path().join(".godot/codex/index");
        for directory in ["generations", "commits"] {
            for entry in fs::read_dir(index_root.join(directory)).expect("artifact directory") {
                let name = entry
                    .expect("artifact entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned();
                assert!(!name.contains(':'), "non-portable artifact name: {name}");
                assert!(
                    name.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
                    }),
                    "unsafe artifact name: {name}"
                );
            }
        }

        let reopened = SegmentStore::open(temp.path(), &base.project_id).expect("reopen");
        assert_eq!(
            reopened
                .active_generation()
                .expect("reopened generation")
                .generation_id,
            base.generation_id
        );
    }

    #[test]
    fn legacy_safe_generation_artifact_names_remain_readable() {
        let temp = TempDir::new().expect("temp");
        let base = generation();
        {
            let mut store = SegmentStore::open(temp.path(), &base.project_id).expect("store");
            store.activate(&base, None).expect("activate base");
        }

        let generation_directory = temp.path().join(".godot/codex/index/generations");
        for entry in fs::read_dir(&generation_directory).expect("generation directory") {
            let entry = entry.expect("generation artifact");
            let name = entry.file_name().to_string_lossy().into_owned();
            let suffix = if name.ends_with(".generation.json") {
                ".generation.json"
            } else {
                ".json"
            };
            fs::rename(
                entry.path(),
                generation_directory.join(format!("{}{suffix}", base.generation_id)),
            )
            .expect("legacy artifact rename");
        }

        let reopened = SegmentStore::open(temp.path(), &base.project_id).expect("legacy reopen");
        assert_eq!(
            reopened
                .active_generation()
                .expect("legacy generation")
                .generation_id,
            base.generation_id
        );
    }

    #[test]
    fn segment_store_publishes_atomic_reader_snapshots() {
        let temp = TempDir::new().expect("temp");
        let base = generation();
        let mut store = SegmentStore::open(temp.path(), &base.project_id).expect("store");
        store.activate(&base, None).expect("activate base");
        let reader = store.reader();
        let pinned = reader.snapshot().expect("pinned base");

        let next = base
            .apply_incremental_batch(&renamed_batch(&base))
            .expect("next generation");
        store.activate(&next, None).expect("activate next");

        assert_eq!(pinned.generation().generation_id, "generation-1");
        assert_eq!(
            reader
                .snapshot()
                .expect("current")
                .generation()
                .generation_id,
            "generation-2"
        );
        assert_eq!(
            pinned
                .resource(&ResourceSelector::Uid("uid://a".to_owned()))
                .expect("old entity")
                .display_path,
            "res://a.tres"
        );
    }

    #[test]
    fn segment_store_cancel_and_writer_contention_preserve_active_generation() {
        let temp = TempDir::new().expect("temp");
        let base = generation();
        let mut store = SegmentStore::open(temp.path(), &base.project_id).expect("store");
        store.activate(&base, None).expect("activate base");
        assert!(matches!(
            SegmentStore::open(temp.path(), &base.project_id),
            Err(StoreError::StoreBusy)
        ));
        let next = base
            .apply_incremental_batch(&renamed_batch(&base))
            .expect("next generation");
        assert_eq!(
            store.activate(
                &next,
                Some(SegmentFaultInjection {
                    point: SegmentFaultPoint::PreCommit,
                    mode: SegmentFaultMode::Cancel,
                }),
            ),
            Err(StoreError::Cancelled)
        );
        assert_eq!(
            store.active_generation().expect("active").generation_id,
            "generation-1"
        );
    }

    #[test]
    fn reopened_editor_reuses_compatible_segments_and_changed_graph_rebuilds() {
        let temp = TempDir::new().expect("temp");
        let base = generation();
        {
            let mut store = SegmentStore::open(temp.path(), &base.project_id).expect("store");
            store.activate(&base, None).expect("activate base");
        }

        let index_root = temp.path().join(".godot/codex/index");
        let segments_before = file_contents(&index_root.join("segments"));
        let generations_before = file_contents(&index_root.join("generations"));
        let commits_before = file_contents(&index_root.join("commits"));
        let mut store = SegmentStore::open(temp.path(), &base.project_id).expect("sidecar reopen");
        let metadata_before = store.metadata().expect("metadata before reuse");
        let observed = reopened_snapshot(&base);
        let mut older_checkpoint = observed.clone();
        older_checkpoint.index_revision = base.index_revision;
        older_checkpoint.checkpoint.index_revision = base.index_revision;
        older_checkpoint.validation_digest.clear();
        older_checkpoint.validation_digest = older_checkpoint.compute_validation_digest();

        assert!(base.graph_is_compatible_with(&observed));
        assert!(matches!(
            store.reuse_compatible_generation(&older_checkpoint),
            Err(StoreError::ValidationFailed(reason)) if reason.contains("next index revision")
        ));
        assert!(
            store
                .reuse_compatible_generation(&observed)
                .expect("compatible reuse")
        );
        let rebound = store
            .active_generation()
            .expect("rebound active generation");
        assert_eq!(rebound.generation_id, base.generation_id);
        assert_eq!(rebound.index_revision, base.index_revision);
        assert_eq!(rebound.checkpoint.editor_session_id, "session-2");
        assert_eq!(rebound.checkpoint.resource_revision, 7);
        assert!(
            rebound
                .resources
                .iter()
                .all(|resource| resource.resource_revision == 7)
        );
        assert_eq!(
            store.metadata().expect("metadata after reuse"),
            metadata_before
        );
        assert_eq!(file_contents(&index_root.join("segments")), segments_before);
        assert_eq!(
            file_contents(&index_root.join("generations")),
            generations_before
        );
        assert_eq!(file_contents(&index_root.join("commits")), commits_before);
        assert!(matches!(
            store.reuse_compatible_generation(&observed),
            Err(StoreError::ValidationFailed(reason)) if reason.contains("new editor session")
        ));

        let mut changed = reopened_snapshot(&base);
        changed.generation_id = "validation-generation-3".to_owned();
        changed.checkpoint.editor_session_id = "session-3".to_owned();
        changed.checkpoint.snapshot_checksum = "sha256:session-3-snapshot".to_owned();
        changed.resources[0].content_generation = Some("sha256:changed".to_owned());
        changed.source_documents[0].content_generation = Some("sha256:changed".to_owned());
        changed.validation_digest.clear();
        changed.validation_digest = changed.compute_validation_digest();
        assert!(!base.graph_is_compatible_with(&changed));
        assert!(
            !store
                .reuse_compatible_generation(&changed)
                .expect("incompatible observation")
        );
        assert_eq!(
            store
                .active_generation()
                .expect("still reused")
                .generation_id,
            base.generation_id
        );

        store
            .activate(&changed, None)
            .expect("changed graph rebuild");
        let rebuilt = store.active_generation().expect("rebuilt generation");
        assert_eq!(rebuilt.generation_id, changed.generation_id);
        assert_eq!(rebuilt.index_revision, 2);
        let metadata_after = store.metadata().expect("metadata after rebuild");
        assert_eq!(metadata_after.full_rebuild_count, 2);
        assert_eq!(metadata_after.index_revision, 2);
    }

    #[test]
    fn segment_store_reopens_migrates_and_detects_corruption() {
        let temp = TempDir::new().expect("temp");
        let mut base = generation();
        base.schema_version = LOGICAL_SCHEMA_RESOURCE_V1;
        base.scene = SceneDomainGeneration::default();
        base.validation_digest.clear();
        base.validation_digest = base.compute_validation_digest();
        let query = ResourceQuery {
            selector: ResourceSelector::Uid("uid://a".to_owned()),
            limit: 50,
            offset: 0,
        };
        let before = query_generation(&base, &query, false).expect("legacy query");
        {
            let mut store = SegmentStore::open_with_version(temp.path(), &base.project_id, 1)
                .expect("segment-v1");
            store.activate(&base, None).expect("legacy activation");
            assert_eq!(store.physical_version().expect("legacy version"), 1);
            assert_eq!(
                store.migrate_current_with_fault(Some(SegmentFaultInjection {
                    point: SegmentFaultPoint::PreCommit,
                    mode: SegmentFaultMode::Cancel,
                })),
                Err(StoreError::Cancelled)
            );
            assert_eq!(store.physical_version().expect("preserved version"), 1);
            assert_eq!(
                store.active_generation().expect("preserved generation"),
                base
            );
        }

        let index_root = temp.path().join(".godot/codex/index");
        let legacy_generations = file_contents(&index_root.join("generations"));
        let legacy_segments = file_contents(&index_root.join("segments"));
        {
            let mut store = SegmentStore::open(temp.path(), &base.project_id).expect("reopen v1");
            store.migrate_current().expect("migration");
            assert_eq!(
                store.physical_version().expect("current version"),
                SEGMENT_PHYSICAL_VERSION
            );
            let migrated = store.active_generation().expect("migrated generation");
            assert_eq!(migrated.schema_version, LOGICAL_SCHEMA_V1);
            assert!(migrated.scene.is_empty());
            let after = store.direct(&query).expect("migrated query");
            assert_eq!(after.resource, before.resource);
            assert_eq!(after.edges, before.edges);
            assert_eq!(after.exact, before.exact);
            assert_eq!(after.has_more, before.has_more);
            assert_eq!(
                store.metadata().expect("metadata").index_revision,
                base.index_revision + 1
            );
        }
        let current_generations = file_contents(&index_root.join("generations"));
        let current_segments = file_contents(&index_root.join("segments"));
        for (name, bytes) in legacy_generations {
            assert_eq!(current_generations.get(&name), Some(&bytes));
        }
        for (name, bytes) in legacy_segments {
            assert_eq!(current_segments.get(&name), Some(&bytes));
        }

        let segment = fs::read_dir(index_root.join("segments"))
            .expect("segments")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|extension| extension == "seg"))
            .expect("segment file");
        let mut bytes = fs::read(&segment).expect("segment bytes");
        bytes[0] ^= 0xff;
        fs::write(segment, bytes).expect("corrupt segment");
        assert!(matches!(
            SegmentStore::open(temp.path(), &base.project_id),
            Err(StoreError::CorruptStore(_))
        ));
    }

    #[test]
    fn segment_v2_persists_and_validates_all_scene_shard_classes() {
        let temp = TempDir::new().expect("temp");
        let base = generation();
        let generation = generation_with_scene(&base, "generation-scene-2");

        {
            let mut store = SegmentStore::open(temp.path(), &generation.project_id).expect("store");
            store
                .activate(&generation, None)
                .expect("activate scene generation");
        }
        let reopened = SegmentStore::open(temp.path(), &generation.project_id).expect("reopen");
        assert_eq!(reopened.active_generation().expect("active"), generation);
        drop(reopened);

        let generation_directory = temp.path().join(".godot/codex/index/generations");
        let manifest: serde_json::Value = fs::read_dir(&generation_directory)
            .expect("generation directory")
            .filter_map(Result::ok)
            .filter(|entry| !entry.file_name().to_string_lossy().contains(".generation."))
            .find_map(|entry| {
                serde_json::from_slice::<serde_json::Value>(&fs::read(entry.path()).ok()?).ok()
            })
            .expect("segment-v2 manifest");
        for shard_class in [
            "scenes",
            "scene_nodes",
            "scene_properties",
            "scene_relations",
            "scene_connections",
            "scene_groups",
            "scene_animations",
            "scene_lookup",
        ] {
            assert!(
                manifest[shard_class]
                    .as_object()
                    .is_some_and(|shards| !shards.is_empty()),
                "missing {shard_class} shards"
            );
        }

        let node_digest = manifest["scene_nodes"]
            .as_object()
            .and_then(|shards| shards.values().next())
            .and_then(serde_json::Value::as_str)
            .expect("scene node shard digest");
        let node_segment = temp
            .path()
            .join(".godot/codex/index/segments")
            .join(format!("{node_digest}.seg"));
        let mut bytes = fs::read(&node_segment).expect("scene node segment");
        bytes[0] ^= 0xff;
        fs::write(node_segment, bytes).expect("corrupt scene node segment");
        assert!(matches!(
            SegmentStore::open(temp.path(), &generation.project_id),
            Err(StoreError::CorruptStore(_))
        ));
    }

    #[test]
    fn scene_staging_cancel_and_process_crash_preserve_previous_generation() {
        const CHILD_ENV: &str = "CODEX_SCENE_STORE_CRASH_CHILD";
        const ROOT_ENV: &str = "CODEX_SCENE_STORE_CRASH_ROOT";
        if std::env::var_os(CHILD_ENV).is_some() {
            let root = std::env::var_os(ROOT_ENV).expect("child project root");
            let mut store =
                SegmentStore::open(std::path::Path::new(&root), "project-1").expect("child store");
            let base = store.active_generation().expect("child base");
            let scene = generation_with_scene(&base, "generation-scene-crash");
            let _ = store.activate(
                &scene,
                Some(SegmentFaultInjection {
                    point: SegmentFaultPoint::PreCommit,
                    mode: SegmentFaultMode::Crash,
                }),
            );
            panic!("crash fault unexpectedly returned");
        }

        let cancel_temp = TempDir::new().expect("cancel temp");
        let base = generation();
        {
            let mut store =
                SegmentStore::open(cancel_temp.path(), &base.project_id).expect("store");
            store.activate(&base, None).expect("activate base");
            let scene = generation_with_scene(&base, "generation-scene-cancel");
            assert_eq!(
                store.activate(
                    &scene,
                    Some(SegmentFaultInjection {
                        point: SegmentFaultPoint::Staging,
                        mode: SegmentFaultMode::Cancel,
                    }),
                ),
                Err(StoreError::Cancelled)
            );
            assert_eq!(store.active_generation().expect("preserved base"), base);
        }
        assert_eq!(
            SegmentStore::open(cancel_temp.path(), &base.project_id)
                .expect("reopen after cancel")
                .active_generation()
                .expect("active after cancel"),
            base
        );

        let crash_temp = TempDir::new().expect("crash temp");
        {
            let mut store = SegmentStore::open(crash_temp.path(), &base.project_id).expect("store");
            store.activate(&base, None).expect("activate base");
        }
        let ready = crash_temp.path().join("fault-ready");
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "tests::scene_staging_cancel_and_process_crash_preserve_previous_generation",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .env(ROOT_ENV, crash_temp.path())
            .env("CODEX_SEGMENT_STORE_FAULT_READY", &ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn crash worker");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !ready.exists() {
            let status = child.try_wait().expect("poll crash worker");
            let _ = child.kill();
            let _ = child.wait();
            panic!("crash worker did not reach pre-commit boundary: {status:?}");
        }
        child.kill().expect("kill crash worker");
        child.wait().expect("reap crash worker");
        assert_eq!(
            SegmentStore::open(crash_temp.path(), &base.project_id)
                .expect("reopen after crash")
                .active_generation()
                .expect("active after crash"),
            base
        );
    }
}
