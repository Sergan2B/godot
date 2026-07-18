//! Immutable content-addressed directory/segment candidate.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::{
    BuildState, DependencyEdge, DependencyResolution, Diagnostic, GenerationBuilder,
    IncrementalBatch, IndexGeneration, IndexMetadata, IndexRead, IndexWriteTransaction,
    MigrationRunner, ResourceEntity, ResourceQuery, ResourceQueryResult, ResourceSelector,
    SceneAnimationReference, SceneConnection, SceneEntity, SceneGroupMembership, SceneNode,
    SceneProperty, SceneRelation, SourceDocument, StoreError, Tombstone,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Production physical format selected by D-05.
pub const SEGMENT_PHYSICAL_VERSION: u32 = 2;

const RESOURCE_LOOKUP_PHYSICAL_VERSION: u32 = 1;
const SCENE_SHARDS_PHYSICAL_VERSION: u32 = 2;

/// Named durability boundaries used by the process-level fault harness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentFaultPoint {
    Capture,
    Staging,
    PreCommit,
    PostCommit,
}

/// Behavior injected at one durability boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentFaultMode {
    Cancel,
    Crash,
}

/// Explicit fault injection used only by local recovery tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentFaultInjection {
    pub point: SegmentFaultPoint,
    pub mode: SegmentFaultMode,
}

impl SegmentFaultInjection {
    fn hit(self, point: SegmentFaultPoint) -> Result<(), StoreError> {
        if self.point != point {
            return Ok(());
        }
        match self.mode {
            SegmentFaultMode::Cancel => Err(StoreError::Cancelled),
            SegmentFaultMode::Crash => {
                if let Some(path) = std::env::var_os("CODEX_SEGMENT_STORE_FAULT_READY") {
                    let mut ready = File::create(path).unwrap_or_else(|_| std::process::exit(87));
                    ready
                        .write_all(format!("{point:?}").as_bytes())
                        .unwrap_or_else(|_| std::process::exit(87));
                    ready.sync_all().unwrap_or_else(|_| std::process::exit(87));
                    loop {
                        std::thread::park_timeout(std::time::Duration::from_secs(60));
                    }
                }
                std::process::exit(86)
            }
        }
    }
}

/// Segment backend holding the exclusive project writer lease.
pub struct SegmentStore {
    root: PathBuf,
    project_id: String,
    initial_version: u32,
    active_cache: Option<Arc<SegmentCache>>,
    reader_state: Arc<RwLock<Option<IndexReadSnapshot>>>,
    _lock: File,
}

/// Public production name used by the sidecar.
pub type SegmentIndexStore = SegmentStore;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SegmentManifest {
    physical_version: u32,
    project_id: String,
    metadata: IndexMetadata,
    generation_id: String,
    resources: BTreeMap<u8, String>,
    source_documents: BTreeMap<u8, String>,
    direct_edges: BTreeMap<u8, String>,
    reverse_edges: BTreeMap<u8, String>,
    diagnostics: BTreeMap<u8, String>,
    tombstones: BTreeMap<u8, String>,
    resource_lookup: Option<BTreeMap<u8, String>>,
    #[serde(default)]
    scenes: BTreeMap<u8, String>,
    #[serde(default)]
    scene_nodes: BTreeMap<u8, String>,
    #[serde(default)]
    scene_properties: BTreeMap<u8, String>,
    #[serde(default)]
    scene_relations: BTreeMap<u8, String>,
    #[serde(default)]
    scene_connections: BTreeMap<u8, String>,
    #[serde(default)]
    scene_groups: BTreeMap<u8, String>,
    #[serde(default)]
    scene_animations: BTreeMap<u8, String>,
    #[serde(default)]
    scene_lookup: Option<BTreeMap<u8, String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivationMarker {
    index_revision: u64,
    generation_id: String,
    manifest_digest: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LookupRecord {
    kind: String,
    value: String,
    entity_id: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneLookupRecord {
    kind: String,
    value: String,
    entity_id: String,
}

struct SegmentCache {
    generation: IndexGeneration,
    entity_lookup: BTreeMap<String, usize>,
    uid_lookup: BTreeMap<String, usize>,
    path_lookup: BTreeMap<String, usize>,
    direct: BTreeMap<String, Vec<DependencyEdge>>,
    reverse: BTreeMap<String, Vec<DependencyEdge>>,
}

/// One pinned immutable committed generation.
#[derive(Clone)]
pub struct IndexReadSnapshot {
    metadata: IndexMetadata,
    cache: Arc<SegmentCache>,
}

impl IndexReadSnapshot {
    /// Returns the immutable generation represented by this snapshot.
    #[must_use]
    pub fn generation(&self) -> &IndexGeneration {
        &self.cache.generation
    }
}

/// Cloneable reader that observes only complete atomic activations.
#[derive(Clone)]
pub struct SegmentIndexReader {
    state: Arc<RwLock<Option<IndexReadSnapshot>>>,
}

impl SegmentIndexReader {
    /// Pins the currently active committed generation.
    pub fn snapshot(&self) -> Result<IndexReadSnapshot, StoreError> {
        self.state
            .read()
            .map_err(|_| StoreError::StorageIo("segment reader state poisoned".to_owned()))?
            .clone()
            .ok_or(StoreError::NotReady)
    }
}

impl SegmentCache {
    fn new(generation: IndexGeneration) -> Self {
        let mut entity_lookup = BTreeMap::new();
        let mut uid_lookup = BTreeMap::new();
        let mut path_lookup = BTreeMap::new();
        for (index, resource) in generation.resources.iter().enumerate() {
            entity_lookup.insert(resource.entity_id.clone(), index);
            path_lookup.insert(resource.comparison_path.clone(), index);
            if let Some(uid) = &resource.uid {
                uid_lookup.insert(uid.clone(), index);
            }
        }
        let mut direct: BTreeMap<String, Vec<DependencyEdge>> = BTreeMap::new();
        let mut reverse: BTreeMap<String, Vec<DependencyEdge>> = BTreeMap::new();
        for edge in &generation.dependencies {
            direct
                .entry(edge.source_entity_id.clone())
                .or_default()
                .push(edge.clone());
            let target = edge
                .target_entity_id
                .as_ref()
                .or_else(|| {
                    edge.target_uid
                        .as_ref()
                        .and_then(|uid| uid_lookup.get(uid))
                        .map(|index| &generation.resources[*index].entity_id)
                })
                .or_else(|| {
                    edge.target_comparison_path
                        .as_ref()
                        .and_then(|path| path_lookup.get(path))
                        .map(|index| &generation.resources[*index].entity_id)
                });
            if let Some(target) = target {
                reverse
                    .entry(target.clone())
                    .or_default()
                    .push(edge.clone());
            }
        }
        for edges in direct.values_mut() {
            edges.sort_by(|left, right| {
                compare_cached_edges(&generation, &entity_lookup, left, right, false)
            });
        }
        for edges in reverse.values_mut() {
            edges.sort_by(|left, right| {
                compare_cached_edges(&generation, &entity_lookup, left, right, true)
            });
        }
        Self {
            generation,
            entity_lookup,
            uid_lookup,
            path_lookup,
            direct,
            reverse,
        }
    }

    fn query(
        &self,
        query: &ResourceQuery,
        reverse: bool,
    ) -> Result<ResourceQueryResult, StoreError> {
        if !(1..=200).contains(&query.limit) {
            return Err(StoreError::ValidationFailed(
                "invalid query limit".to_owned(),
            ));
        }
        let index = match &query.selector {
            ResourceSelector::EntityId(value) => self.entity_lookup.get(value),
            ResourceSelector::Uid(value) => self.uid_lookup.get(value),
            ResourceSelector::ComparisonPath(value) => self.path_lookup.get(value),
        }
        .copied()
        .ok_or(StoreError::ResourceNotFound)?;
        let resource = self.generation.resources[index].clone();
        let source = if reverse {
            self.reverse.get(&resource.entity_id)
        } else {
            self.direct.get(&resource.entity_id)
        };
        let has_more = source.is_some_and(|edges| {
            query
                .offset
                .checked_add(query.limit)
                .is_some_and(|end| end < edges.len())
        });
        let edges: Vec<_> = source
            .into_iter()
            .flatten()
            .skip(query.offset)
            .take(query.limit)
            .cloned()
            .collect();
        let exact = source.into_iter().flatten().all(|edge| {
            edge.resolution == DependencyResolution::Resolved && edge.target_entity_id.is_some()
        });
        Ok(ResourceQueryResult {
            generation_id: self.generation.generation_id.clone(),
            index_revision: self.generation.index_revision,
            resource,
            edges,
            exact,
            has_more,
        })
    }
}

fn compare_cached_edges(
    generation: &IndexGeneration,
    entity_lookup: &BTreeMap<String, usize>,
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
        entity_id
            .and_then(|entity_id| entity_lookup.get(entity_id))
            .map(|index| &generation.resources[*index])
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

impl SegmentStore {
    /// Opens or creates the segment candidate with an exclusive process lease.
    pub fn open(project_root: &Path, project_id: &str) -> Result<Self, StoreError> {
        Self::open_with_version(project_root, project_id, SEGMENT_PHYSICAL_VERSION)
    }

    /// Opens a physical-version fixture for migration/recovery tests.
    pub fn open_with_version(
        project_root: &Path,
        project_id: &str,
        initial_version: u32,
    ) -> Result<Self, StoreError> {
        if initial_version > SEGMENT_PHYSICAL_VERSION {
            return Err(StoreError::IncompatibleSchema);
        }
        let codex = project_root.join(".godot").join("codex");
        fs::create_dir_all(&codex).map_err(io_error)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(codex.join("index.lock"))
            .map_err(io_error)?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => StoreError::StoreBusy,
            TryLockError::Error(error) => io_error(error),
        })?;
        let root = codex.join("index");
        for directory in ["segments", "staging", "generations", "commits"] {
            fs::create_dir_all(root.join(directory)).map_err(io_error)?;
        }
        cleanup_staging(&root)?;
        let mut store = Self {
            root,
            project_id: project_id.to_owned(),
            initial_version,
            active_cache: None,
            reader_state: Arc::new(RwLock::new(None)),
            _lock: lock,
        };
        match store.active_manifest() {
            Ok((manifest, _)) => {
                if manifest.project_id != project_id {
                    return Err(StoreError::ProjectMismatch);
                }
                let cache = Arc::new(SegmentCache::new(load_generation(&store.root, &manifest)?));
                let snapshot = IndexReadSnapshot {
                    metadata: manifest.metadata,
                    cache: cache.clone(),
                };
                store.active_cache = Some(cache);
                *store.reader_state.write().map_err(|_| {
                    StoreError::StorageIo("segment reader state poisoned".to_owned())
                })? = Some(snapshot);
            }
            Err(StoreError::NotReady) => {}
            Err(error) => return Err(error),
        }
        store.garbage_collect()?;
        Ok(store)
    }

    /// Opens and validates active data without acquiring the writer lease.
    pub fn read_generation(
        project_root: &Path,
        project_id: &str,
    ) -> Result<IndexGeneration, StoreError> {
        let root = project_root.join(".godot").join("codex").join("index");
        let (manifest, _) = read_active_manifest(&root)?;
        if manifest.project_id != project_id {
            return Err(StoreError::ProjectMismatch);
        }
        load_generation(&root, &manifest)
    }

    fn active_manifest(&self) -> Result<(SegmentManifest, ActivationMarker), StoreError> {
        read_active_manifest(&self.root)
    }

    /// Returns a cloneable reader that is not blocked by staging writes.
    #[must_use]
    pub fn reader(&self) -> SegmentIndexReader {
        SegmentIndexReader {
            state: self.reader_state.clone(),
        }
    }

    fn activate_with_version(
        &mut self,
        generation: &IndexGeneration,
        physical_version: u32,
        fault: Option<SegmentFaultInjection>,
    ) -> Result<IndexMetadata, StoreError> {
        if generation.project_id != self.project_id {
            return Err(StoreError::ProjectMismatch);
        }
        generation.validate()?;
        if (physical_version < SCENE_SHARDS_PHYSICAL_VERSION
            && generation.schema_version.minor >= crate::LOGICAL_SCHEMA_V1.minor)
            || (physical_version >= SCENE_SHARDS_PHYSICAL_VERSION
                && generation.schema_version != crate::LOGICAL_SCHEMA_V1)
        {
            return Err(StoreError::IncompatibleSchema);
        }
        if let Some(active) = &self.active_cache
            && active.generation.generation_id == generation.generation_id
        {
            if active.generation.validation_digest != generation.validation_digest {
                return Err(StoreError::ValidationFailed(
                    "active generation identity reused with different content".to_owned(),
                ));
            }
            return self.metadata();
        }
        hit(fault, SegmentFaultPoint::Capture)?;

        let generation_stem = generation_file_stem(&generation.generation_id);
        let staging_path = self
            .root
            .join("staging")
            .join(format!("{generation_stem}.json"));
        write_atomic_json(
            &staging_path,
            &serde_json::json!({
                "generation_id": generation.generation_id,
                "index_revision": generation.index_revision,
                "state": "staging"
            }),
        )?;

        let logical_records = generation.resources.len()
            + generation.source_documents.len()
            + generation.dependencies.len()
            + generation.diagnostics.len()
            + generation.tombstones.len()
            + generation.scene.scenes.len()
            + generation.scene.nodes.len()
            + generation.scene.properties.len()
            + generation.scene.relations.len()
            + generation.scene.connections.len()
            + generation.scene.groups.len()
            + generation.scene.animations.len();
        let mut progress = StagingProgress::new(logical_records, fault);
        let resources = write_shards(
            &self.root,
            &generation.resources,
            |record| shard(&record.entity_id),
            Some(&mut progress),
        )?;
        let source_documents = write_shards(
            &self.root,
            &generation.source_documents,
            |record| shard(&record.entity_id),
            Some(&mut progress),
        )?;
        let direct_edges = write_shards(
            &self.root,
            &generation.dependencies,
            |record| shard(&record.source_entity_id),
            Some(&mut progress),
        )?;
        let mut reverse_records = generation.dependencies.clone();
        reverse_records.sort_by(|left, right| {
            dependency_target(left)
                .cmp(dependency_target(right))
                .then_with(|| left.source_entity_id.cmp(&right.source_entity_id))
                .then_with(|| left.edge_id.cmp(&right.edge_id))
        });
        let reverse_edges = write_shards(
            &self.root,
            &reverse_records,
            |record| shard(dependency_target(record)),
            None,
        )?;
        let diagnostics = write_shards(
            &self.root,
            &generation.diagnostics,
            |record| shard(&record.subject),
            Some(&mut progress),
        )?;
        let tombstones = write_shards(
            &self.root,
            &generation.tombstones,
            |record| shard(&record.entity_id),
            Some(&mut progress),
        )?;
        let resource_lookup = if physical_version >= RESOURCE_LOOKUP_PHYSICAL_VERSION {
            let mut lookup = Vec::with_capacity(generation.resources.len() * 3);
            for resource in &generation.resources {
                lookup.push(LookupRecord {
                    kind: "entity_id".to_owned(),
                    value: resource.entity_id.clone(),
                    entity_id: resource.entity_id.clone(),
                });
                lookup.push(LookupRecord {
                    kind: "path".to_owned(),
                    value: resource.comparison_path.clone(),
                    entity_id: resource.entity_id.clone(),
                });
                if let Some(uid) = &resource.uid {
                    lookup.push(LookupRecord {
                        kind: "uid".to_owned(),
                        value: uid.clone(),
                        entity_id: resource.entity_id.clone(),
                    });
                }
            }
            lookup.sort();
            Some(write_shards(&self.root, &lookup, lookup_shard, None)?)
        } else {
            None
        };
        let (
            scenes,
            scene_nodes,
            scene_properties,
            scene_relations,
            scene_connections,
            scene_groups,
            scene_animations,
            scene_lookup,
        ) = if physical_version >= SCENE_SHARDS_PHYSICAL_VERSION {
            let scenes = write_shards(
                &self.root,
                &generation.scene.scenes,
                |record| shard(&record.scene_entity_id),
                Some(&mut progress),
            )?;
            let scene_nodes = write_shards(
                &self.root,
                &generation.scene.nodes,
                |record| shard(&record.node_entity_id),
                Some(&mut progress),
            )?;
            let scene_properties = write_shards(
                &self.root,
                &generation.scene.properties,
                |record| shard(&record.property_id),
                Some(&mut progress),
            )?;
            let scene_relations = write_shards(
                &self.root,
                &generation.scene.relations,
                |record| shard(&record.relation_id),
                Some(&mut progress),
            )?;
            let scene_connections = write_shards(
                &self.root,
                &generation.scene.connections,
                |record| shard(&record.connection_id),
                Some(&mut progress),
            )?;
            let scene_groups = write_shards(
                &self.root,
                &generation.scene.groups,
                |record| shard(&record.membership_id),
                Some(&mut progress),
            )?;
            let scene_animations = write_shards(
                &self.root,
                &generation.scene.animations,
                |record| shard(&record.animation_reference_id),
                Some(&mut progress),
            )?;
            let mut lookup = Vec::with_capacity(
                generation.scene.scenes.len() * 2 + generation.scene.nodes.len() * 2,
            );
            for scene in &generation.scene.scenes {
                lookup.push(SceneLookupRecord {
                    kind: "scene_id".to_owned(),
                    value: scene.scene_entity_id.clone(),
                    entity_id: scene.scene_entity_id.clone(),
                });
                lookup.push(SceneLookupRecord {
                    kind: "scene_path".to_owned(),
                    value: scene.comparison_path.clone(),
                    entity_id: scene.scene_entity_id.clone(),
                });
            }
            for node in &generation.scene.nodes {
                lookup.push(SceneLookupRecord {
                    kind: "node_id".to_owned(),
                    value: node.node_entity_id.clone(),
                    entity_id: node.node_entity_id.clone(),
                });
                lookup.push(SceneLookupRecord {
                    kind: "node_path".to_owned(),
                    value: format!("{}\0{}", node.scene_entity_id, node.node_path),
                    entity_id: node.node_entity_id.clone(),
                });
            }
            lookup.sort();
            let scene_lookup = Some(write_shards(&self.root, &lookup, scene_lookup_shard, None)?);
            (
                scenes,
                scene_nodes,
                scene_properties,
                scene_relations,
                scene_connections,
                scene_groups,
                scene_animations,
                scene_lookup,
            )
        } else {
            (
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                None,
            )
        };
        progress.finish()?;

        let previous = self
            .active_manifest()
            .ok()
            .map(|(manifest, _)| manifest.metadata);
        let metadata = next_metadata(previous.as_ref(), generation);
        let manifest = SegmentManifest {
            physical_version,
            project_id: self.project_id.clone(),
            metadata: metadata.clone(),
            generation_id: generation.generation_id.clone(),
            resources,
            source_documents,
            direct_edges,
            reverse_edges,
            diagnostics,
            tombstones,
            resource_lookup,
            scenes,
            scene_nodes,
            scene_properties,
            scene_relations,
            scene_connections,
            scene_groups,
            scene_animations,
            scene_lookup,
        };
        let manifest_bytes = canonical_json(&manifest)?;
        let manifest_digest = sha256(&manifest_bytes);
        let staged_manifest = self
            .root
            .join("staging")
            .join(format!("{generation_stem}.manifest"));
        write_new_synced(&staged_manifest, &manifest_bytes)?;
        let mut generation_header = generation.clone();
        generation_header.resources.clear();
        generation_header.source_documents.clear();
        generation_header.dependencies.clear();
        generation_header.diagnostics.clear();
        generation_header.tombstones.clear();
        generation_header.scene.scenes.clear();
        generation_header.scene.nodes.clear();
        generation_header.scene.properties.clear();
        generation_header.scene.relations.clear();
        generation_header.scene.connections.clear();
        generation_header.scene.groups.clear();
        generation_header.scene.animations.clear();
        let staged_generation = self
            .root
            .join("staging")
            .join(format!("{generation_stem}.generation"));
        write_new_synced(&staged_generation, &canonical_json(&generation_header)?)?;
        hit(fault, SegmentFaultPoint::PreCommit)?;

        let generation_directory = self.root.join("generations");
        let generation_manifest =
            generation_artifact_path(&generation_directory, &generation.generation_id, ".json");
        fs::rename(&staged_manifest, &generation_manifest).map_err(io_error)?;
        let generation_header_path = generation_artifact_path(
            &generation_directory,
            &generation.generation_id,
            ".generation.json",
        );
        fs::rename(&staged_generation, &generation_header_path).map_err(io_error)?;
        sync_parent(&generation_manifest)?;
        let marker = ActivationMarker {
            index_revision: generation.index_revision,
            generation_id: generation.generation_id.clone(),
            manifest_digest,
        };
        let marker_path = self.root.join("commits").join(format!(
            "{:020}-{generation_stem}.json",
            generation.index_revision
        ));
        write_atomic_json(&marker_path, &marker)?;
        sync_parent(&marker_path)?;
        let cache = Arc::new(SegmentCache::new(generation.clone()));
        let snapshot = IndexReadSnapshot {
            metadata: metadata.clone(),
            cache: cache.clone(),
        };
        self.active_cache = Some(cache);
        *self
            .reader_state
            .write()
            .map_err(|_| StoreError::StorageIo("segment reader state poisoned".to_owned()))? =
            Some(snapshot);
        hit(fault, SegmentFaultPoint::PostCommit)?;
        let _ = fs::remove_file(staging_path);
        self.garbage_collect()?;
        Ok(metadata)
    }

    fn garbage_collect(&self) -> Result<(), StoreError> {
        let commits_dir = self.root.join("commits");
        let mut commits: Vec<_> = fs::read_dir(&commits_dir)
            .map_err(io_error)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.path())
            .collect();
        commits.sort();
        let retained = if commits.len() > 2 {
            commits.split_off(commits.len() - 2)
        } else {
            std::mem::take(&mut commits)
        };
        let mut retained_generations = BTreeSet::new();
        let mut retained_segments = BTreeSet::new();
        for marker_path in &retained {
            let marker: ActivationMarker = read_json(marker_path)?;
            retained_generations.insert(marker.generation_id.clone());
            let manifest_path = existing_generation_artifact_path(
                &self.root.join("generations"),
                &marker.generation_id,
                ".json",
            );
            let manifest: SegmentManifest = read_json(&manifest_path)?;
            for digest in manifest
                .resources
                .values()
                .chain(manifest.source_documents.values())
                .chain(manifest.direct_edges.values())
                .chain(manifest.reverse_edges.values())
                .chain(manifest.diagnostics.values())
                .chain(manifest.tombstones.values())
                .chain(manifest.scenes.values())
                .chain(manifest.scene_nodes.values())
                .chain(manifest.scene_properties.values())
                .chain(manifest.scene_relations.values())
                .chain(manifest.scene_connections.values())
                .chain(manifest.scene_groups.values())
                .chain(manifest.scene_animations.values())
            {
                retained_segments.insert(digest.clone());
            }
            if let Some(lookup) = manifest.resource_lookup {
                retained_segments.extend(lookup.into_values());
            }
            if let Some(lookup) = manifest.scene_lookup {
                retained_segments.extend(lookup.into_values());
            }
        }
        for path in commits {
            let marker: ActivationMarker = read_json(&path)?;
            fs::remove_file(path).map_err(io_error)?;
            if !retained_generations.contains(&marker.generation_id) {
                for generation_file in [
                    existing_generation_artifact_path(
                        &self.root.join("generations"),
                        &marker.generation_id,
                        ".json",
                    ),
                    existing_generation_artifact_path(
                        &self.root.join("generations"),
                        &marker.generation_id,
                        ".generation.json",
                    ),
                ] {
                    if generation_file.exists() {
                        fs::remove_file(generation_file).map_err(io_error)?;
                    }
                }
            }
        }
        for entry in fs::read_dir(self.root.join("segments")).map_err(io_error)? {
            let path = entry.map_err(io_error)?.path();
            let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            if !retained_segments.contains(name) {
                fs::remove_file(path).map_err(io_error)?;
            }
        }
        Ok(())
    }
}

impl SegmentStore {
    /// Stages, validates, and atomically activates one generation.
    pub fn activate(
        &mut self,
        generation: &IndexGeneration,
        fault: Option<SegmentFaultInjection>,
    ) -> Result<IndexMetadata, StoreError> {
        let version = self
            .active_manifest()
            .map_or(self.initial_version, |(manifest, _)| {
                manifest.physical_version
            });
        self.activate_with_version(generation, version, fault)
    }

    /// Returns the complete active logical generation.
    pub fn active_generation(&self) -> Result<IndexGeneration, StoreError> {
        if let Some(generation) = &self.active_cache {
            return Ok(generation.generation.clone());
        }
        let (manifest, _) = self.active_manifest()?;
        load_generation(&self.root, &manifest)
    }

    /// Reuses the immutable active generation after a full snapshot from a new
    /// editor session proves that the current graph is unchanged.
    ///
    /// Only the in-process session checkpoint and per-record source revisions
    /// are rebound. No manifest, generation, commit marker, or content-addressed
    /// segment is written; a later sidecar process validates the persisted
    /// generation against its connected editor session again.
    pub fn reuse_compatible_generation(
        &mut self,
        observed: &IndexGeneration,
    ) -> Result<bool, StoreError> {
        observed.validate()?;
        if observed.project_id != self.project_id {
            return Err(StoreError::ProjectMismatch);
        }
        if observed.creation_reason != "full_snapshot"
            || !observed.checkpoint.source_complete
            || observed.checkpoint.last_batch_id.is_some()
            || observed.checkpoint.last_batch_checksum.is_some()
        {
            return Err(StoreError::ValidationFailed(
                "cache reuse requires a complete full snapshot".to_owned(),
            ));
        }

        let active = self
            .active_cache
            .as_ref()
            .ok_or(StoreError::NotReady)?
            .generation
            .clone();
        if active.checkpoint.editor_session_id == observed.checkpoint.editor_session_id {
            return Err(StoreError::ValidationFailed(
                "cache reuse requires a new editor session".to_owned(),
            ));
        }
        let expected_validation_revision =
            active.index_revision.checked_add(1).ok_or_else(|| {
                StoreError::ValidationFailed("cache reuse index revision overflow".to_owned())
            })?;
        if observed.index_revision != expected_validation_revision {
            return Err(StoreError::ValidationFailed(
                "cache reuse requires the next index revision".to_owned(),
            ));
        }
        if !active.checkpoint.source_complete || !active.graph_is_compatible_with(observed) {
            return Ok(false);
        }

        let resource_revisions: BTreeMap<_, _> = observed
            .resources
            .iter()
            .map(|resource| (resource.entity_id.as_str(), resource.resource_revision))
            .collect();
        let dependency_revisions: BTreeMap<_, _> = observed
            .dependencies
            .iter()
            .map(|edge| (edge.edge_id.as_str(), edge.resource_revision))
            .collect();
        let mut rebound = active;
        for resource in &mut rebound.resources {
            resource.resource_revision = *resource_revisions
                .get(resource.entity_id.as_str())
                .ok_or_else(|| {
                    StoreError::ValidationFailed("compatible resource revision missing".to_owned())
                })?;
        }
        for edge in &mut rebound.dependencies {
            edge.resource_revision =
                *dependency_revisions
                    .get(edge.edge_id.as_str())
                    .ok_or_else(|| {
                        StoreError::ValidationFailed(
                            "compatible dependency revision missing".to_owned(),
                        )
                    })?;
        }
        rebound.checkpoint = observed.checkpoint.clone();
        rebound.checkpoint.index_revision = rebound.index_revision;
        rebound.validation_digest.clear();
        rebound.validation_digest = rebound.compute_validation_digest();
        rebound.validate()?;

        let metadata = self.active_manifest()?.0.metadata;
        if metadata.active_generation_id.as_deref() != Some(rebound.generation_id.as_str())
            || metadata.index_revision != rebound.index_revision
        {
            return Err(StoreError::CorruptStore(
                "active cache and manifest identity mismatch".to_owned(),
            ));
        }
        let cache = Arc::new(SegmentCache::new(rebound));
        let snapshot = IndexReadSnapshot {
            metadata,
            cache: cache.clone(),
        };
        self.active_cache = Some(cache);
        *self
            .reader_state
            .write()
            .map_err(|_| StoreError::StorageIo("segment reader state poisoned".to_owned()))? =
            Some(snapshot);
        Ok(true)
    }

    /// Runs a direct dependency query against the active generation.
    pub fn direct(&self, query: &ResourceQuery) -> Result<ResourceQueryResult, StoreError> {
        let generation = self.active_cache.as_ref().ok_or(StoreError::NotReady)?;
        generation.query(query, false)
    }

    /// Runs a reverse-owner query against the active generation.
    pub fn reverse(&self, query: &ResourceQuery) -> Result<ResourceQueryResult, StoreError> {
        let generation = self.active_cache.as_ref().ok_or(StoreError::NotReady)?;
        generation.query(query, true)
    }

    /// Migrates the active resource-only generation to logical 1.2 and
    /// `segment-v2`, preserving resource records and adding an empty scene domain.
    pub fn migrate_current(&mut self) -> Result<IndexMetadata, StoreError> {
        self.migrate_current_with_fault(None)
    }

    /// Testable migration entry point with an injected durability-boundary fault.
    pub fn migrate_current_with_fault(
        &mut self,
        fault: Option<SegmentFaultInjection>,
    ) -> Result<IndexMetadata, StoreError> {
        let (manifest, _) = self.active_manifest()?;
        if manifest.physical_version >= SEGMENT_PHYSICAL_VERSION {
            return Ok(manifest.metadata);
        }
        let mut generation = load_generation(&self.root, &manifest)?;
        generation.parent_generation_id = Some(generation.generation_id.clone());
        generation.index_revision = generation.index_revision.checked_add(1).ok_or_else(|| {
            StoreError::ValidationFailed("migration index revision overflow".to_owned())
        })?;
        generation.checkpoint.index_revision = generation.index_revision;
        generation.schema_version = crate::LOGICAL_SCHEMA_V1;
        generation.scene = crate::SceneDomainGeneration::default();
        generation.generation_id =
            format!("segment-v2-migration-{:020}", generation.index_revision);
        generation.creation_reason =
            format!("physical_segment_v{}_to_v2", manifest.physical_version);
        generation.canonicalize();
        generation.validation_digest.clear();
        generation.validation_digest = generation.compute_validation_digest();
        self.activate_with_version(&generation, SEGMENT_PHYSICAL_VERSION, fault)
    }

    /// Returns the active physical format version.
    pub fn physical_version(&self) -> Result<u32, StoreError> {
        let version = self
            .active_manifest()
            .map(|(manifest, _)| manifest.physical_version)?;
        if version <= SEGMENT_PHYSICAL_VERSION {
            Ok(version)
        } else {
            Err(StoreError::IncompatibleSchema)
        }
    }
}

/// Storage-neutral transaction adapter over the segment candidate.
pub struct SegmentTransaction<'a> {
    store: &'a mut SegmentStore,
    generation: Option<IndexGeneration>,
}

impl IndexWriteTransaction for SegmentTransaction<'_> {
    fn replace_generation(&mut self, generation: IndexGeneration) -> Result<(), StoreError> {
        generation.validate()?;
        self.generation = Some(generation);
        Ok(())
    }

    fn apply_incremental_batch(&mut self, batch: IncrementalBatch) -> Result<(), StoreError> {
        let current = self.generation.as_ref().ok_or_else(|| {
            StoreError::ValidationFailed("transaction generation missing".to_owned())
        })?;
        self.generation = Some(current.apply_incremental_batch(&batch)?);
        Ok(())
    }

    fn commit(mut self) -> Result<IndexMetadata, StoreError> {
        let generation = self.generation.take().ok_or_else(|| {
            StoreError::ValidationFailed("transaction generation missing".to_owned())
        })?;
        self.store.activate(&generation, None)
    }

    fn cancel(self) -> Result<(), StoreError> {
        Ok(())
    }
}

impl GenerationBuilder for SegmentStore {
    type Transaction<'a> = SegmentTransaction<'a>;

    fn begin_generation<'a>(
        &'a mut self,
        generation: &IndexGeneration,
    ) -> Result<Self::Transaction<'a>, StoreError> {
        generation.validate()?;
        Ok(SegmentTransaction {
            store: self,
            generation: Some(generation.clone()),
        })
    }
}

impl IndexRead for SegmentStore {
    fn metadata(&self) -> Result<IndexMetadata, StoreError> {
        self.active_manifest()
            .map(|(manifest, _)| manifest.metadata)
    }

    fn resource(&self, selector: &ResourceSelector) -> Result<ResourceEntity, StoreError> {
        self.active_cache
            .as_ref()
            .ok_or(StoreError::NotReady)?
            .query(
                &ResourceQuery {
                    selector: selector.clone(),
                    limit: 1,
                    offset: 0,
                },
                false,
            )
            .map(|result| result.resource)
    }

    fn direct_dependencies(
        &self,
        query: &ResourceQuery,
    ) -> Result<ResourceQueryResult, StoreError> {
        self.direct(query)
    }

    fn reverse_owners(&self, query: &ResourceQuery) -> Result<ResourceQueryResult, StoreError> {
        self.reverse(query)
    }
}

impl IndexRead for IndexReadSnapshot {
    fn metadata(&self) -> Result<IndexMetadata, StoreError> {
        Ok(self.metadata.clone())
    }

    fn resource(&self, selector: &ResourceSelector) -> Result<ResourceEntity, StoreError> {
        self.cache
            .query(
                &ResourceQuery {
                    selector: selector.clone(),
                    limit: 1,
                    offset: 0,
                },
                false,
            )
            .map(|result| result.resource)
    }

    fn direct_dependencies(
        &self,
        query: &ResourceQuery,
    ) -> Result<ResourceQueryResult, StoreError> {
        self.cache.query(query, false)
    }

    fn reverse_owners(&self, query: &ResourceQuery) -> Result<ResourceQueryResult, StoreError> {
        self.cache.query(query, true)
    }
}

impl MigrationRunner for SegmentStore {
    fn migrate(&mut self, target_physical_version: u32) -> Result<IndexMetadata, StoreError> {
        if target_physical_version != SEGMENT_PHYSICAL_VERSION {
            return Err(StoreError::IncompatibleSchema);
        }
        self.migrate_current()
    }
}

fn read_active_manifest(root: &Path) -> Result<(SegmentManifest, ActivationMarker), StoreError> {
    let commits = root.join("commits");
    let mut markers: Vec<_> = fs::read_dir(&commits)
        .map_err(io_error)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .collect();
    markers.sort_by_key(std::fs::DirEntry::file_name);
    let marker_path = markers
        .last()
        .map(std::fs::DirEntry::path)
        .ok_or(StoreError::NotReady)?;
    let marker: ActivationMarker = read_json(&marker_path)?;
    let manifest_path = existing_generation_artifact_path(
        &root.join("generations"),
        &marker.generation_id,
        ".json",
    );
    let bytes = fs::read(&manifest_path).map_err(io_error)?;
    if sha256(&bytes) != marker.manifest_digest {
        return Err(StoreError::CorruptStore(
            "active manifest digest mismatch".to_owned(),
        ));
    }
    let manifest: SegmentManifest = serde_json::from_slice(&bytes)
        .map_err(|error| StoreError::CorruptStore(error.to_string()))?;
    if manifest.generation_id != marker.generation_id
        || manifest.metadata.index_revision != marker.index_revision
    {
        return Err(StoreError::CorruptStore(
            "activation marker mismatch".to_owned(),
        ));
    }
    Ok((manifest, marker))
}

fn load_generation(root: &Path, manifest: &SegmentManifest) -> Result<IndexGeneration, StoreError> {
    if manifest.physical_version > SEGMENT_PHYSICAL_VERSION {
        return Err(StoreError::IncompatibleSchema);
    }
    if manifest.metadata.project_id != manifest.project_id
        || manifest.metadata.schema_version.major != crate::LOGICAL_SCHEMA_V1.major
        || manifest.metadata.schema_version.minor > crate::LOGICAL_SCHEMA_V1.minor
        || crate::LOGICAL_SCHEMA_V1.minor < manifest.metadata.reader_min_minor
        || manifest.metadata.reader_min_minor > manifest.metadata.reader_max_minor
        || (manifest.physical_version >= SCENE_SHARDS_PHYSICAL_VERSION
            && manifest.metadata.schema_version != crate::LOGICAL_SCHEMA_V1)
    {
        return Err(StoreError::IncompatibleSchema);
    }
    let resources = read_shards::<ResourceEntity>(root, &manifest.resources)?;
    let source_documents = read_shards::<SourceDocument>(root, &manifest.source_documents)?;
    let dependencies = read_shards::<DependencyEdge>(root, &manifest.direct_edges)?;
    let reverse = read_shards::<DependencyEdge>(root, &manifest.reverse_edges)?;
    let diagnostics = read_shards::<Diagnostic>(root, &manifest.diagnostics)?;
    let tombstones = read_shards::<Tombstone>(root, &manifest.tombstones)?;
    let direct_ids: BTreeSet<_> = dependencies.iter().map(|edge| &edge.edge_id).collect();
    let reverse_ids: BTreeSet<_> = reverse.iter().map(|edge| &edge.edge_id).collect();
    if direct_ids != reverse_ids {
        return Err(StoreError::CorruptStore(
            "direct/reverse segment parity mismatch".to_owned(),
        ));
    }
    if manifest.physical_version >= RESOURCE_LOOKUP_PHYSICAL_VERSION {
        let digest = manifest.resource_lookup.as_ref().ok_or_else(|| {
            StoreError::CorruptStore("resource lookup shards are missing".to_owned())
        })?;
        let lookup: Vec<LookupRecord> = read_shards(root, digest)?;
        let entity_lookups = lookup
            .iter()
            .filter(|record| record.kind == "entity_id")
            .count();
        if entity_lookups != resources.len() {
            return Err(StoreError::CorruptStore(
                "resource lookup parity mismatch".to_owned(),
            ));
        }
    }
    let generation_path = existing_generation_artifact_path(
        &root.join("generations"),
        &manifest.generation_id,
        ".generation.json",
    );
    let mut generation: IndexGeneration = read_json(&generation_path)?;
    generation.resources = resources;
    generation.source_documents = source_documents;
    generation.dependencies = dependencies;
    generation.diagnostics = diagnostics;
    generation.tombstones = tombstones;
    if manifest.physical_version >= SCENE_SHARDS_PHYSICAL_VERSION {
        generation.scene.scenes = read_shards::<SceneEntity>(root, &manifest.scenes)?;
        generation.scene.nodes = read_shards::<SceneNode>(root, &manifest.scene_nodes)?;
        generation.scene.properties =
            read_shards::<SceneProperty>(root, &manifest.scene_properties)?;
        generation.scene.relations = read_shards::<SceneRelation>(root, &manifest.scene_relations)?;
        generation.scene.connections =
            read_shards::<SceneConnection>(root, &manifest.scene_connections)?;
        generation.scene.groups =
            read_shards::<SceneGroupMembership>(root, &manifest.scene_groups)?;
        generation.scene.animations =
            read_shards::<SceneAnimationReference>(root, &manifest.scene_animations)?;
        let lookup_shards = manifest.scene_lookup.as_ref().ok_or_else(|| {
            StoreError::CorruptStore("scene lookup shards are missing".to_owned())
        })?;
        let lookup: Vec<SceneLookupRecord> = read_shards(root, lookup_shards)?;
        let scene_ids = lookup
            .iter()
            .filter(|record| record.kind == "scene_id")
            .count();
        let node_ids = lookup
            .iter()
            .filter(|record| record.kind == "node_id")
            .count();
        if scene_ids != generation.scene.scenes.len() || node_ids != generation.scene.nodes.len() {
            return Err(StoreError::CorruptStore(
                "scene lookup parity mismatch".to_owned(),
            ));
        }
    } else if !generation.scene.is_empty() {
        return Err(StoreError::CorruptStore(
            "segment-v1 generation contains scene-domain records".to_owned(),
        ));
    }
    generation.canonicalize();
    if generation.project_id != manifest.project_id
        || generation.generation_id != manifest.generation_id
        || generation.index_revision != manifest.metadata.index_revision
        || generation.schema_version != manifest.metadata.schema_version
    {
        return Err(StoreError::CorruptStore(
            "generation manifest binding mismatch".to_owned(),
        ));
    }
    generation.validate()?;
    Ok(generation)
}

fn next_metadata(previous: Option<&IndexMetadata>, generation: &IndexGeneration) -> IndexMetadata {
    let mut metadata = previous.cloned().unwrap_or(IndexMetadata {
        schema_version: generation.schema_version,
        reader_min_minor: 0,
        reader_max_minor: generation.schema_version.minor,
        project_id: generation.project_id.clone(),
        active_generation_id: None,
        index_revision: 0,
        build_state: BuildState::Empty,
        hash_algorithm: "sha256".to_owned(),
        full_rebuild_count: 0,
        incremental_commit_count: 0,
        quarantine_count: 0,
        failed_migration_count: 0,
    });
    metadata.schema_version = generation.schema_version;
    metadata.reader_min_minor = 0;
    metadata.reader_max_minor = generation.schema_version.minor;
    metadata.active_generation_id = Some(generation.generation_id.clone());
    metadata.index_revision = generation.index_revision;
    metadata.build_state = BuildState::Ready;
    if generation.creation_reason == "full_snapshot" {
        metadata.full_rebuild_count += 1;
    } else {
        metadata.incremental_commit_count += 1;
    }
    metadata
}

struct StagingProgress {
    threshold: usize,
    written: usize,
    triggered: bool,
    fault: Option<SegmentFaultInjection>,
}

impl StagingProgress {
    fn new(total: usize, fault: Option<SegmentFaultInjection>) -> Self {
        Self {
            threshold: total.div_ceil(2),
            written: 0,
            triggered: false,
            fault,
        }
    }

    fn advance(&mut self, records: usize) -> Result<(), StoreError> {
        self.written += records;
        if !self.triggered && self.written >= self.threshold {
            self.triggered = true;
            hit(self.fault, SegmentFaultPoint::Staging)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), StoreError> {
        if !self.triggered {
            self.advance(0)?;
        }
        Ok(())
    }
}

fn write_shards<T, F>(
    root: &Path,
    records: &[T],
    key: F,
    mut progress: Option<&mut StagingProgress>,
) -> Result<BTreeMap<u8, String>, StoreError>
where
    T: Serialize,
    F: Fn(&T) -> u8,
{
    let mut grouped: BTreeMap<u8, Vec<&T>> = BTreeMap::new();
    for record in records {
        grouped.entry(key(record)).or_default().push(record);
    }
    let mut written = BTreeMap::new();
    for (shard, records) in grouped {
        written.insert(shard, write_segment(root, &records)?);
        if let Some(progress) = progress.as_deref_mut() {
            progress.advance(records.len())?;
        }
    }
    Ok(written)
}

fn read_shards<T: DeserializeOwned>(
    root: &Path,
    shards: &BTreeMap<u8, String>,
) -> Result<Vec<T>, StoreError> {
    let mut records = Vec::new();
    for digest in shards.values() {
        records.extend(read_segment::<T>(root, digest)?);
    }
    Ok(records)
}

fn write_segment<T: Serialize>(root: &Path, records: &[T]) -> Result<String, StoreError> {
    let mut bytes = Vec::new();
    let mut offsets = Vec::with_capacity(records.len());
    for record in records {
        offsets.push(bytes.len() as u64);
        let encoded =
            serde_json::to_vec(record).map_err(|error| StoreError::StorageIo(error.to_string()))?;
        let length = u32::try_from(encoded.len())
            .map_err(|_| StoreError::ValidationFailed("segment record too large".to_owned()))?;
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(&encoded);
    }
    let digest = sha256(&bytes);
    let path = root.join("segments").join(format!("{digest}.seg"));
    let index_path = root.join("segments").join(format!("{digest}.idx"));
    if !path.exists() {
        write_new_synced(&path, &bytes)?;
        sync_parent(&path)?;
    }
    if !index_path.exists() {
        let index_bytes: Vec<_> = offsets.into_iter().flat_map(u64::to_be_bytes).collect();
        write_new_synced(&index_path, &index_bytes)?;
        sync_parent(&index_path)?;
    }
    Ok(digest)
}

fn read_segment<T: DeserializeOwned>(root: &Path, digest: &str) -> Result<Vec<T>, StoreError> {
    let path = root.join("segments").join(format!("{digest}.seg"));
    let bytes = fs::read(path).map_err(io_error)?;
    if sha256(&bytes) != digest {
        return Err(StoreError::CorruptStore(
            "segment checksum mismatch".to_owned(),
        ));
    }
    let index_bytes =
        fs::read(root.join("segments").join(format!("{digest}.idx"))).map_err(io_error)?;
    if index_bytes.len() % 8 != 0 {
        return Err(StoreError::CorruptStore(
            "segment offset index length is invalid".to_owned(),
        ));
    }
    let expected_offsets: Vec<_> = index_bytes
        .chunks_exact(8)
        .map(|chunk| u64::from_be_bytes(chunk.try_into().expect("eight bytes")))
        .collect();
    let mut cursor = bytes.as_slice();
    let mut records = Vec::new();
    let mut offset = 0_u64;
    while !cursor.is_empty() {
        if expected_offsets.get(records.len()) != Some(&offset) {
            return Err(StoreError::CorruptStore(
                "segment offset index mismatch".to_owned(),
            ));
        }
        if cursor.len() < 4 {
            return Err(StoreError::CorruptStore(
                "truncated segment length".to_owned(),
            ));
        }
        let length = u32::from_be_bytes(cursor[..4].try_into().expect("four bytes")) as usize;
        cursor = &cursor[4..];
        if cursor.len() < length {
            return Err(StoreError::CorruptStore(
                "truncated segment record".to_owned(),
            ));
        }
        records.push(
            serde_json::from_slice(&cursor[..length])
                .map_err(|error| StoreError::CorruptStore(error.to_string()))?,
        );
        offset += u64::try_from(4 + length)
            .map_err(|_| StoreError::CorruptStore("segment offset overflow".to_owned()))?;
        cursor = &cursor[length..];
    }
    if records.len() != expected_offsets.len() {
        return Err(StoreError::CorruptStore(
            "segment offset index count mismatch".to_owned(),
        ));
    }
    Ok(records)
}

fn write_atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let bytes = canonical_json(value)?;
    let temp = path.with_extension("tmp");
    write_new_synced(&temp, &bytes)?;
    fs::rename(&temp, path).map_err(io_error)?;
    sync_parent(path)
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::remove_file(path).map_err(io_error)?;
            options.open(path).map_err(io_error)?
        }
        Err(error) => return Err(io_error(error)),
    };
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, StoreError> {
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes).map_err(|error| StoreError::CorruptStore(error.to_string()))
}

fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(value).map_err(|error| StoreError::StorageIo(error.to_string()))
}

fn cleanup_staging(root: &Path) -> Result<(), StoreError> {
    for entry in fs::read_dir(root.join("staging")).map_err(io_error)? {
        let path = entry.map_err(io_error)?.path();
        if path.is_file() {
            fs::remove_file(path).map_err(io_error)?;
        } else if path.is_dir() {
            fs::remove_dir_all(path).map_err(io_error)?;
        }
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), StoreError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    match File::open(parent).and_then(|directory| directory.sync_all()) {
        Ok(()) => Ok(()),
        Err(error)
            if cfg!(windows)
                && matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidInput
                ) =>
        {
            Ok(())
        }
        Err(error) => Err(io_error(error)),
    }
}

fn hit(fault: Option<SegmentFaultInjection>, point: SegmentFaultPoint) -> Result<(), StoreError> {
    fault.map_or(Ok(()), |fault| fault.hit(point))
}

fn shard(value: &str) -> u8 {
    Sha256::digest(value.as_bytes())[0]
}

fn lookup_shard(record: &LookupRecord) -> u8 {
    let mut digest = Sha256::new();
    digest.update(record.kind.as_bytes());
    digest.update([0]);
    digest.update(record.value.as_bytes());
    digest.finalize()[0]
}

fn scene_lookup_shard(record: &SceneLookupRecord) -> u8 {
    let mut digest = Sha256::new();
    digest.update(record.kind.as_bytes());
    digest.update([0]);
    digest.update(record.value.as_bytes());
    digest.finalize()[0]
}

fn dependency_target(edge: &DependencyEdge) -> &str {
    edge.target_entity_id
        .as_deref()
        .or(edge.target_uid.as_deref())
        .or(edge.target_comparison_path.as_deref())
        .unwrap_or_default()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn generation_file_stem(generation_id: &str) -> String {
    format!("generation-{}", sha256(generation_id.as_bytes()))
}

fn generation_artifact_path(directory: &Path, generation_id: &str, suffix: &str) -> PathBuf {
    directory.join(format!("{}{suffix}", generation_file_stem(generation_id)))
}

fn existing_generation_artifact_path(
    directory: &Path,
    generation_id: &str,
    suffix: &str,
) -> PathBuf {
    let portable = generation_artifact_path(directory, generation_id, suffix);
    if portable.exists() {
        return portable;
    }
    let legacy_name_is_safe = generation_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_'));
    if legacy_name_is_safe {
        let legacy = directory.join(format!("{generation_id}{suffix}"));
        if legacy.exists() {
            return legacy;
        }
    }
    portable
}

fn io_error(error: std::io::Error) -> StoreError {
    StoreError::StorageIo(error.to_string())
}
