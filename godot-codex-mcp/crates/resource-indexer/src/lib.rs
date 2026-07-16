//! Normalization boundary between Bridge RPC resource facts and the persistent index.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

use base64::Engine as _;
use godot_codex_bridge_client::{
    DependencyResolution as BridgeDependencyResolution, ResourceDiagnostic as BridgeDiagnostic,
    ResourceDiagnosticCode, ResourceImportState, ResourceRef, ResourceSnapshot, ResourceSourceKind,
    ResourceValidity,
};
use godot_codex_index_store::{
    DependencyEdge, DependencyResolution, Diagnostic, IdentityStrength, IndexGeneration,
    IngestionCheckpoint, LOGICAL_SCHEMA_V1, RecordValidity, ResourceEntity, SourceDocument,
    StoreError,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

const UID_DOMAIN: &[u8] = b"godot-codex/resource-entity/uid/v1\0";
const PATH_CONTENT_DOMAIN: &[u8] = b"godot-codex/resource-entity/path-content/v1\0";
const EDGE_DOMAIN: &[u8] = b"godot-codex/resource-edge/references/v1\0";
const DIAGNOSTIC_DOMAIN: &[u8] = b"godot-codex/resource-diagnostic/v1\0";
const GENERATION_DOMAIN: &[u8] = b"godot-codex/index-generation-id/v1\0";
const MAX_RESOURCE_PATH_BYTES: usize = 1_024;
const HASH_BUFFER_BYTES: usize = 64 * 1_024;

/// Stable normalizer failure that never includes an absolute project path.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum IndexerError {
    #[error("invalid_path: {0}")]
    InvalidPath(&'static str),
    #[error("invalid_resource_uid")]
    InvalidUid,
    #[error("unsafe_resource_path")]
    UnsafeResourcePath,
    #[error("resource_observation_conflict: {0}")]
    ObservationConflict(&'static str),
    #[error("resource_hash_unavailable: {0}")]
    HashUnavailable(&'static str),
    #[error("serialization_failed")]
    Serialization,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Display and comparison forms frozen by INDEX-001.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedResourcePath {
    pub display: String,
    pub comparison: String,
}

/// Sidecar-owned normalizer bound to one physical canonical project root.
#[derive(Clone, Debug)]
pub struct ResourceNormalizer {
    project_root: PathBuf,
}

impl ResourceNormalizer {
    /// Creates a normalizer for a Godot project without retaining Bridge/Godot objects.
    pub fn new(project_root: impl AsRef<Path>) -> Result<Self, IndexerError> {
        let project_root = fs::canonicalize(project_root.as_ref())
            .map_err(|_| IndexerError::UnsafeResourcePath)?;
        if !project_root.is_dir() || !project_root.join("project.godot").is_file() {
            return Err(IndexerError::UnsafeResourcePath);
        }
        Ok(Self { project_root })
    }

    /// Converts one complete verified Bridge snapshot into an immutable logical generation.
    pub fn normalize_full_snapshot(
        &self,
        project_id: &str,
        index_revision: u64,
        snapshot: &ResourceSnapshot,
    ) -> Result<IndexGeneration, IndexerError> {
        if project_id.is_empty()
            || index_revision == 0
            || snapshot.accepted.resource_revision != snapshot.end.resource_revision
            || snapshot.accepted.revisions.editor_session_id
                != snapshot.end.revisions.editor_session_id
        {
            return Err(IndexerError::ObservationConflict("snapshot_context"));
        }

        let snapshot_checksum = normalized_input_checksum(snapshot)?;
        let generation_id = generation_id(
            project_id,
            &snapshot.end.revisions.editor_session_id,
            snapshot.end.resource_revision,
            index_revision,
            &snapshot_checksum,
        );
        let mut diagnostics = BTreeMap::new();
        let mut resources = Vec::with_capacity(snapshot.payload.resources.len());
        let mut source_documents = Vec::with_capacity(snapshot.payload.resources.len());
        let mut uid_lookup = BTreeMap::new();
        let mut path_lookup = BTreeMap::new();

        let mut observations = snapshot.payload.resources.clone();
        observations.sort_by(|left, right| left.path.cmp(&right.path));
        let paths: Vec<_> = observations
            .iter()
            .map(|observation| normalize_resource_path(&observation.path))
            .collect::<Result<_, _>>()?;
        for (observation, path) in observations.iter().zip(&paths) {
            validate_resource_ref(&observation.resource_ref, path)?;
        }
        let hashes = self.hash_resources(&paths);
        for ((observation, path), hash) in observations.iter().zip(paths).zip(hashes) {
            let (
                content_generation,
                size_before,
                size_after,
                mtime_before,
                mtime_after,
                hash_state,
            ) = match hash {
                Ok(hash) if hash.size_before == observation.byte_size => (
                    Some(hash.content_generation),
                    hash.size_before,
                    hash.size_after,
                    hash.mtime_before_ns,
                    hash.mtime_after_ns,
                    "ready".to_owned(),
                ),
                Ok(hash) => (
                    None,
                    hash.size_before,
                    hash.size_after,
                    hash.mtime_before_ns,
                    hash.mtime_after_ns,
                    "source_metadata_changed".to_owned(),
                ),
                Err(_) => (
                    None,
                    observation.byte_size,
                    observation.byte_size,
                    observation
                        .modified_time_unix_seconds
                        .saturating_mul(1_000_000_000),
                    observation
                        .modified_time_unix_seconds
                        .saturating_mul(1_000_000_000),
                    "hash_unavailable".to_owned(),
                ),
            };

            let (uid, identity_strength, identity_input, entity_id) =
                match &observation.resource_ref {
                    ResourceRef::Uid(reference) => {
                        validate_uid(&reference.uid)?;
                        (
                            Some(reference.uid.clone()),
                            IdentityStrength::ResourceUid,
                            reference.uid.clone(),
                            uid_entity_id(&reference.uid)?,
                        )
                    }
                    ResourceRef::Path(_) => {
                        let Some(content_generation) = content_generation.as_deref() else {
                            insert_diagnostic(
                                &mut diagnostics,
                                "hash_unavailable",
                                &path.display,
                                Some(&path.display),
                                index_revision,
                            );
                            continue;
                        };
                        (
                            None,
                            IdentityStrength::PathContentGeneration,
                            format!("{}\0{content_generation}", path.comparison),
                            fallback_entity_id(&path.display, content_generation)?,
                        )
                    }
                };

            let validity = match observation.validity {
                ResourceValidity::Valid if content_generation.is_some() => RecordValidity::Valid,
                ResourceValidity::Valid | ResourceValidity::Partial => RecordValidity::Partial,
                ResourceValidity::Invalid => RecordValidity::Invalid,
            };
            if content_generation.is_none() {
                insert_diagnostic(
                    &mut diagnostics,
                    "hash_unavailable",
                    &entity_id,
                    Some(&path.display),
                    index_revision,
                );
            }

            let entity = ResourceEntity {
                entity_id: entity_id.clone(),
                identity_input,
                uid: uid.clone(),
                display_path: path.display.clone(),
                comparison_path: path.comparison.clone(),
                identity_strength,
                resource_type: observation.godot_type.clone(),
                source_kind: source_kind_name(&observation.source_kind).to_owned(),
                import_state: import_state_name(&observation.import_state).to_owned(),
                authority: observation.authority.clone(),
                content_generation: content_generation.clone(),
                mtime_ns: mtime_after,
                byte_size: size_after,
                validity,
                resource_revision: observation.resource_revision,
            };
            if path_lookup
                .insert(path.comparison.clone(), entity_id.clone())
                .is_some()
            {
                return Err(IndexerError::ObservationConflict(
                    "path_normalization_collision",
                ));
            }
            if let Some(uid) = uid
                && uid_lookup.insert(uid, entity_id.clone()).is_some()
            {
                return Err(IndexerError::ObservationConflict("duplicate_resource_uid"));
            }
            source_documents.push(SourceDocument {
                entity_id: entity_id.clone(),
                comparison_path: path.comparison,
                size_before,
                size_after,
                mtime_before_ns: mtime_before,
                mtime_after_ns: mtime_after,
                content_generation,
                ingest_state: hash_state,
            });
            resources.push(entity);
        }

        let resources_by_id: BTreeMap<_, _> = resources
            .iter()
            .map(|resource| (resource.entity_id.clone(), resource))
            .collect();
        let mut dependencies = Vec::with_capacity(snapshot.payload.dependencies.len());
        let mut dependency_ids = BTreeSet::new();
        let mut observations = snapshot.payload.dependencies.clone();
        observations.sort_by(|left, right| {
            resource_ref_key(&left.source_ref)
                .cmp(&resource_ref_key(&right.source_ref))
                .then_with(|| left.target_uid.cmp(&right.target_uid))
                .then_with(|| left.fallback_path.cmp(&right.fallback_path))
        });
        for observation in &observations {
            let source_entity_id = resolve_ref(&observation.source_ref, &uid_lookup, &path_lookup)?;
            let fallback = normalize_resource_path(&observation.fallback_path)?;
            if let Some(uid) = observation.target_uid.as_deref() {
                validate_uid(uid)?;
            }
            let candidate = observation
                .target_uid
                .as_ref()
                .and_then(|uid| uid_lookup.get(uid))
                .or_else(|| path_lookup.get(&fallback.comparison));
            let (resolution, target_entity_id) = match observation.resolution {
                BridgeDependencyResolution::Resolved => match candidate {
                    Some(entity_id) => (DependencyResolution::Resolved, Some(entity_id.clone())),
                    None if observation.target_uid.is_some() => {
                        (DependencyResolution::StaleUid, None)
                    }
                    None => (DependencyResolution::Missing, None),
                },
                BridgeDependencyResolution::Missing => (DependencyResolution::Missing, None),
                BridgeDependencyResolution::StaleUid => (DependencyResolution::StaleUid, None),
            };
            let resolved_target_path = target_entity_id
                .as_ref()
                .and_then(|entity_id| resources_by_id.get(entity_id))
                .map(|resource| resource.display_path.clone());
            if let (Some(expected), Some(actual)) = (
                observation.resolved_path.as_deref(),
                resolved_target_path.as_deref(),
            ) {
                let expected = normalize_resource_path(expected)?;
                if expected.comparison != normalize_resource_path(actual)?.comparison {
                    return Err(IndexerError::ObservationConflict(
                        "resource_uid_path_mismatch",
                    ));
                }
            }
            let edge_id = dependency_edge_id(
                &source_entity_id,
                observation.target_uid.as_deref(),
                &fallback.display,
            )?;
            if !dependency_ids.insert(edge_id.clone()) {
                continue;
            }
            dependencies.push(DependencyEdge {
                edge_id,
                source_entity_id: source_entity_id.clone(),
                target_uid: observation.target_uid.clone(),
                target_comparison_path: Some(fallback.comparison.clone()),
                target_display_path: Some(fallback.display.clone()),
                target_entity_id,
                resolved_target_path,
                relation: "references".to_owned(),
                declared_type: observation.declared_type.clone(),
                authority: observation.authority.clone(),
                resolution,
                resource_revision: observation.resource_revision,
            });
            match resolution {
                DependencyResolution::Resolved => {}
                DependencyResolution::Missing => insert_diagnostic(
                    &mut diagnostics,
                    "missing_dependency",
                    &source_entity_id,
                    Some(&fallback.display),
                    index_revision,
                ),
                DependencyResolution::StaleUid => insert_diagnostic(
                    &mut diagnostics,
                    "stale_resource_uid",
                    &source_entity_id,
                    observation
                        .target_uid
                        .as_deref()
                        .or(Some(&fallback.display)),
                    index_revision,
                ),
            }
        }

        for diagnostic in &snapshot.payload.diagnostics {
            normalize_bridge_diagnostic(
                &mut diagnostics,
                diagnostic,
                &uid_lookup,
                &path_lookup,
                index_revision,
            )?;
        }

        let mut generation = IndexGeneration {
            generation_id,
            parent_generation_id: None,
            schema_version: LOGICAL_SCHEMA_V1,
            project_id: project_id.to_owned(),
            index_revision,
            state: godot_codex_index_store::GenerationState::Active,
            creation_reason: "full_snapshot".to_owned(),
            checkpoint: IngestionCheckpoint {
                editor_session_id: snapshot.end.revisions.editor_session_id.clone(),
                resource_revision: snapshot.end.resource_revision,
                project_revision: snapshot.end.revisions.project_revision,
                index_revision,
                source_complete: true,
                snapshot_checksum,
                last_batch_id: None,
                last_batch_checksum: None,
            },
            resources,
            source_documents,
            dependencies,
            diagnostics: diagnostics.into_values().collect(),
            tombstones: Vec::new(),
            validation_digest: String::new(),
        };
        generation.canonicalize();
        generation.validation_digest = generation.compute_validation_digest();
        generation.validate()?;
        Ok(generation)
    }

    fn hash_resource(
        &self,
        path: &NormalizedResourcePath,
    ) -> Result<HashObservation, IndexerError> {
        let relative = path
            .display
            .strip_prefix("res://")
            .ok_or(IndexerError::InvalidPath("invalid_path_scheme"))?;
        let candidate = relative
            .split('/')
            .fold(self.project_root.clone(), |path, segment| {
                path.join(segment)
            });
        let canonical = fs::canonicalize(&candidate)
            .map_err(|_| IndexerError::HashUnavailable("source_missing"))?;
        if !canonical.starts_with(&self.project_root) {
            return Err(IndexerError::UnsafeResourcePath);
        }
        let before = fs::metadata(&canonical)
            .map_err(|_| IndexerError::HashUnavailable("metadata_unavailable"))?;
        if !before.is_file() {
            return Err(IndexerError::HashUnavailable("source_not_file"));
        }
        let mut file = File::open(&canonical)
            .map_err(|_| IndexerError::HashUnavailable("source_open_failed"))?;
        let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
        let mut hasher = Sha256::new();
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|_| IndexerError::HashUnavailable("source_read_failed"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let after = fs::metadata(&canonical)
            .map_err(|_| IndexerError::HashUnavailable("metadata_unavailable"))?;
        let mtime_before_ns = metadata_mtime_ns(&before)?;
        let mtime_after_ns = metadata_mtime_ns(&after)?;
        if before.len() != after.len() || mtime_before_ns != mtime_after_ns {
            return Err(IndexerError::HashUnavailable("source_changed_during_hash"));
        }
        Ok(HashObservation {
            content_generation: format!("sha256:{:x}", hasher.finalize()),
            size_before: before.len(),
            size_after: after.len(),
            mtime_before_ns,
            mtime_after_ns,
        })
    }

    fn hash_resources(
        &self,
        paths: &[NormalizedResourcePath],
    ) -> Vec<Result<HashObservation, IndexerError>> {
        if paths.is_empty() {
            return Vec::new();
        }
        let available = std::thread::available_parallelism().map_or(1, usize::from);
        let workers = available.min(4).min(paths.len());
        let next = AtomicUsize::new(0);
        let results = Mutex::new(
            std::iter::repeat_with(|| None)
                .take(paths.len())
                .collect::<Vec<Option<Result<HashObservation, IndexerError>>>>(),
        );
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let next = &next;
                let results = &results;
                scope.spawn(move || {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(path) = paths.get(index) else {
                            break;
                        };
                        let result = self.hash_resource(path);
                        results.lock().expect("hash result lock")[index] = Some(result);
                    }
                });
            }
        });
        results
            .into_inner()
            .expect("hash result lock")
            .into_iter()
            .map(|result| result.expect("hash worker filled every slot"))
            .collect()
    }
}

struct HashObservation {
    content_generation: String,
    size_before: u64,
    size_after: u64,
    mtime_before_ns: u64,
    mtime_after_ns: u64,
}

/// Applies the frozen canonical `res://` path rules.
pub fn normalize_resource_path(value: &str) -> Result<NormalizedResourcePath, IndexerError> {
    if !value.starts_with("res://") {
        return Err(IndexerError::InvalidPath("invalid_path_scheme"));
    }
    if value.len() > MAX_RESOURCE_PATH_BYTES {
        return Err(IndexerError::InvalidPath("resource_path_too_long"));
    }
    if value.contains('\\') {
        return Err(IndexerError::InvalidPath("invalid_path_separator"));
    }
    if value.ends_with('/') || value.len() == "res://".len() {
        return Err(IndexerError::InvalidPath("invalid_resource_path"));
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(IndexerError::InvalidPath("invalid_path_character"));
    }
    let mut segments = Vec::new();
    for segment in value[6..].split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return Err(IndexerError::InvalidPath("invalid_path_traversal"));
        }
        if segment.contains([':', '?', '#']) {
            return Err(IndexerError::InvalidPath("invalid_path_character"));
        }
        segments.push(segment);
    }
    if segments.is_empty() {
        return Err(IndexerError::InvalidPath("invalid_resource_path"));
    }
    let display = format!("res://{}", segments.join("/"));
    let comparison = display.nfc().collect();
    Ok(NormalizedResourcePath {
        display,
        comparison,
    })
}

/// Computes the frozen UID-backed entity identity.
pub fn uid_entity_id(uid: &str) -> Result<String, IndexerError> {
    validate_uid(uid)?;
    Ok(format!(
        "godot:resource:uid:v1:{}",
        base64url_digest(&[UID_DOMAIN, uid.as_bytes()])
    ))
}

/// Computes the frozen UID-less path/content identity.
pub fn fallback_entity_id(path: &str, content_generation: &str) -> Result<String, IndexerError> {
    let path = normalize_resource_path(path)?;
    validate_content_generation(content_generation)?;
    Ok(format!(
        "godot:resource:path-content:v1:{}",
        base64url_digest(&[
            PATH_CONTENT_DOMAIN,
            path.comparison.as_bytes(),
            b"\0",
            content_generation.as_bytes(),
        ])
    ))
}

/// Computes the frozen direct dependency identity.
pub fn dependency_edge_id(
    source_entity_id: &str,
    target_uid: Option<&str>,
    fallback_path: &str,
) -> Result<String, IndexerError> {
    let fallback = normalize_resource_path(fallback_path)?;
    let (kind, target) = if let Some(uid) = target_uid {
        validate_uid(uid)?;
        ("uid", uid)
    } else {
        ("path", fallback.comparison.as_str())
    };
    Ok(format!(
        "godot:edge:references:v1:{}",
        base64url_digest(&[
            EDGE_DOMAIN,
            source_entity_id.as_bytes(),
            b"\0",
            kind.as_bytes(),
            b"\0",
            target.as_bytes(),
        ])
    ))
}

fn validate_uid(value: &str) -> Result<(), IndexerError> {
    if value
        .strip_prefix("uid://")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.len() <= 122)
        && value
            .bytes()
            .skip(6)
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(IndexerError::InvalidUid)
    }
}

fn validate_content_generation(value: &str) -> Result<(), IndexerError> {
    if value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(IndexerError::HashUnavailable("invalid_content_generation"))
    }
}

fn validate_resource_ref(
    reference: &ResourceRef,
    path: &NormalizedResourcePath,
) -> Result<(), IndexerError> {
    match reference {
        ResourceRef::Uid(reference) => validate_uid(&reference.uid),
        ResourceRef::Path(reference) => {
            let reference_path = normalize_resource_path(&reference.path)?;
            if reference.uid_missing && reference_path.comparison == path.comparison {
                Ok(())
            } else {
                Err(IndexerError::ObservationConflict("resource_ref_path"))
            }
        }
    }
}

fn resolve_ref(
    reference: &ResourceRef,
    uid_lookup: &BTreeMap<String, String>,
    path_lookup: &BTreeMap<String, String>,
) -> Result<String, IndexerError> {
    match reference {
        ResourceRef::Uid(reference) => uid_lookup
            .get(&reference.uid)
            .cloned()
            .ok_or(IndexerError::ObservationConflict("dependency_source_uid")),
        ResourceRef::Path(reference) => {
            let path = normalize_resource_path(&reference.path)?;
            path_lookup
                .get(&path.comparison)
                .cloned()
                .ok_or(IndexerError::ObservationConflict("dependency_source_path"))
        }
    }
}

fn normalize_bridge_diagnostic(
    diagnostics: &mut BTreeMap<String, Diagnostic>,
    diagnostic: &BridgeDiagnostic,
    uid_lookup: &BTreeMap<String, String>,
    path_lookup: &BTreeMap<String, String>,
    index_revision: u64,
) -> Result<(), IndexerError> {
    let subject = resolve_ref(&diagnostic.subject, uid_lookup, path_lookup)
        .unwrap_or_else(|_| resource_ref_key(&diagnostic.subject));
    insert_diagnostic(
        diagnostics,
        diagnostic_code_name(&diagnostic.code),
        &subject,
        diagnostic.target_reference.as_deref(),
        index_revision,
    );
    Ok(())
}

fn insert_diagnostic(
    diagnostics: &mut BTreeMap<String, Diagnostic>,
    code: &str,
    subject: &str,
    detail: Option<&str>,
    index_revision: u64,
) {
    let detail = detail.map(|value| value.chars().take(1_024).collect::<String>());
    let id = format!(
        "godot:diagnostic:v1:{}",
        base64url_digest(&[
            DIAGNOSTIC_DOMAIN,
            code.as_bytes(),
            b"\0",
            subject.as_bytes(),
            b"\0",
            detail.as_deref().unwrap_or_default().as_bytes(),
        ])
    );
    diagnostics.entry(id.clone()).or_insert(Diagnostic {
        diagnostic_id: id,
        code: code.to_owned(),
        subject: subject.to_owned(),
        detail,
        first_index_revision: index_revision,
        last_index_revision: index_revision,
        active: true,
    });
}

fn normalized_input_checksum(snapshot: &ResourceSnapshot) -> Result<String, IndexerError> {
    let mut records = Vec::new();
    append_serialized(&mut records, &snapshot.payload.resources)?;
    append_serialized(&mut records, &snapshot.payload.dependencies)?;
    append_serialized(&mut records, &snapshot.payload.diagnostics)?;
    records.sort();
    let mut hasher = Sha256::new();
    hasher.update(b"godot-codex/normalized-resource-input/v1\0");
    for record in records {
        hasher.update((record.len() as u64).to_be_bytes());
        hasher.update(record);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn append_serialized<T: Serialize>(
    output: &mut Vec<Vec<u8>>,
    values: &[T],
) -> Result<(), IndexerError> {
    for value in values {
        output.push(serde_json::to_vec(value).map_err(|_| IndexerError::Serialization)?);
    }
    Ok(())
}

fn generation_id(
    project_id: &str,
    editor_session_id: &str,
    resource_revision: u64,
    index_revision: u64,
    checksum: &str,
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        GENERATION_DOMAIN,
        project_id.as_bytes(),
        editor_session_id.as_bytes(),
        &resource_revision.to_be_bytes(),
        &index_revision.to_be_bytes(),
        checksum.as_bytes(),
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    format!("generation:sha256:{:x}", hasher.finalize())
}

fn base64url_digest(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

fn metadata_mtime_ns(metadata: &fs::Metadata) -> Result<u64, IndexerError> {
    let nanos = metadata
        .modified()
        .map_err(|_| IndexerError::HashUnavailable("mtime_unavailable"))?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| IndexerError::HashUnavailable("mtime_before_epoch"))?
        .as_nanos();
    u64::try_from(nanos).map_err(|_| IndexerError::HashUnavailable("mtime_overflow"))
}

fn resource_ref_key(reference: &ResourceRef) -> String {
    match reference {
        ResourceRef::Uid(reference) => reference.uid.clone(),
        ResourceRef::Path(reference) => reference.path.clone(),
    }
}

const fn source_kind_name(value: &ResourceSourceKind) -> &'static str {
    match value {
        ResourceSourceKind::Source => "source",
        ResourceSourceKind::ImportedSource => "imported_source",
    }
}

const fn import_state_name(value: &ResourceImportState) -> &'static str {
    match value {
        ResourceImportState::NotImported => "not_imported",
        ResourceImportState::Valid => "valid",
        ResourceImportState::Invalid => "invalid",
    }
}

const fn diagnostic_code_name(value: &ResourceDiagnosticCode) -> &'static str {
    match value {
        ResourceDiagnosticCode::MissingDependency => "missing_dependency",
        ResourceDiagnosticCode::StaleResourceUid => "stale_resource_uid",
        ResourceDiagnosticCode::ResourceUidPathMismatch => "resource_uid_path_mismatch",
        ResourceDiagnosticCode::DuplicateResourceUid => "duplicate_resource_uid",
        ResourceDiagnosticCode::InvalidImport => "invalid_import",
        ResourceDiagnosticCode::ResourceLimitExceeded => "resource_limit_exceeded",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use godot_codex_bridge_client::{
        DependencyObservation, DependencyResolution as BridgeResolution, ResourceObservation,
        ResourceRevisionVector, ResourceSnapshotAccepted, ResourceSnapshotBeginParams,
        ResourceSnapshotEndParams, ResourceSnapshotLimits, ResourceSnapshotPayload, UidResourceRef,
    };
    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn frozen_path_and_identity_vectors_match() {
        let vectors: Value = serde_json::from_str(include_str!(
            "../../../../tests/codex/fixtures/resource_graph_oracle/identity-vectors.json"
        ))
        .unwrap();
        for vector in vectors["path_vectors"].as_array().unwrap() {
            let input = vector["input"].as_str().unwrap();
            if vector["valid"].as_bool().unwrap() {
                let normalized = normalize_resource_path(input).unwrap();
                assert_eq!(normalized.display, vector["display_path"]);
                assert_eq!(normalized.comparison, vector["comparison_path"]);
            } else {
                assert!(normalize_resource_path(input).is_err(), "{input}");
            }
        }
        for vector in vectors["identity_vectors"].as_array().unwrap() {
            let expected = vector["expected_entity_id"].as_str().unwrap();
            let actual = if vector["kind"] == "resource_uid" {
                uid_entity_id(vector["uid"].as_str().unwrap()).unwrap()
            } else {
                fallback_entity_id(
                    vector["path"].as_str().unwrap(),
                    vector["content_generation"].as_str().unwrap(),
                )
                .unwrap()
            };
            assert_eq!(actual, expected);
        }
        assert_eq!(
            dependency_edge_id(
                "godot:resource:uid:v1:h0_yNQI_BLAsMWEtDgmVOSpxGQsGI9HI47ssVIwmrAo",
                Some("uid://b"),
                "res://resources/shared_leaf.tres",
            )
            .unwrap(),
            "godot:edge:references:v1:njL828OaCH6Dw0ZiBNk9QC-o4iVSKiM_WzTQf-FsDGs"
        );
    }

    #[test]
    fn full_snapshot_normalizes_hashes_and_reverse_ready_edges() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("project.godot"), "[application]\n").unwrap();
        fs::write(temp.path().join("a.tres"), "a").unwrap();
        fs::write(temp.path().join("b.tres"), "b").unwrap();
        let revision = ResourceRevisionVector {
            editor_session_id: "editor:0123456789abcdef0123456789abcdef".to_owned(),
            event_seq: 1,
            project_revision: 1,
            operation_seq: 1,
            resource_revision: 1,
            scene_revisions: BTreeMap::new(),
        };
        let resource = |uid: &str, path: &str| ResourceObservation {
            resource_ref: ResourceRef::Uid(UidResourceRef {
                uid: uid.to_owned(),
            }),
            path: path.to_owned(),
            godot_type: "Resource".to_owned(),
            source_kind: ResourceSourceKind::Source,
            import_state: ResourceImportState::NotImported,
            modified_time_unix_seconds: 1,
            byte_size: 1,
            validity: ResourceValidity::Valid,
            authority: "editor_file_system".to_owned(),
            resource_revision: 1,
        };
        let snapshot = ResourceSnapshot {
            accepted: ResourceSnapshotAccepted {
                snapshot_id: "snapshot:1123456789abcdef0123456789abcdef".to_owned(),
                domain: "resource_graph".to_owned(),
                resource_revision: 1,
                revisions: revision.clone(),
                limits_applied: ResourceSnapshotLimits {
                    resource_records: 250_000,
                    resource_dependencies: 2_000_000,
                    resource_dependencies_per_record: 4_096,
                    resource_path_bytes: 1_024,
                    snapshot_chunk_bytes: 256 * 1_024,
                    snapshot_window_bytes: 32 * 1_024 * 1_024,
                    snapshot_timeout_ms: 120_000,
                },
            },
            begin: ResourceSnapshotBeginParams {
                snapshot_id: "snapshot:1123456789abcdef0123456789abcdef".to_owned(),
                domain: "resource_graph".to_owned(),
                resource_revision: 1,
                revisions: revision.clone(),
            },
            payload: ResourceSnapshotPayload {
                resources: vec![
                    resource("uid://a", "res://a.tres"),
                    resource("uid://b", "res://b.tres"),
                ],
                dependencies: vec![DependencyObservation {
                    source_ref: ResourceRef::Uid(UidResourceRef {
                        uid: "uid://a".to_owned(),
                    }),
                    target_uid: Some("uid://b".to_owned()),
                    fallback_path: "res://b.tres".to_owned(),
                    resolved_path: Some("res://b.tres".to_owned()),
                    declared_type: Some("Resource".to_owned()),
                    resolution: BridgeResolution::Resolved,
                    authority: "resource_loader_dependencies".to_owned(),
                    resource_revision: 1,
                }],
                diagnostics: Vec::new(),
            },
            end: ResourceSnapshotEndParams {
                snapshot_id: "snapshot:1123456789abcdef0123456789abcdef".to_owned(),
                domain: "resource_graph".to_owned(),
                resource_revision: 1,
                chunk_count: 1,
                resource_count: 2,
                dependency_count: 1,
                diagnostic_count: 0,
                checksum: "fixture".to_owned(),
                revisions: revision,
            },
        };
        let generation = ResourceNormalizer::new(temp.path())
            .unwrap()
            .normalize_full_snapshot("project:test", 1, &snapshot)
            .unwrap();
        assert_eq!(generation.resources.len(), 2);
        assert_eq!(generation.dependencies.len(), 1);
        assert_eq!(
            generation.dependencies[0].resolution,
            DependencyResolution::Resolved
        );
        assert!(generation.resources.iter().all(|resource| {
            resource
                .content_generation
                .as_ref()
                .is_some_and(|hash| hash.starts_with("sha256:"))
        }));
        generation.validate().unwrap();

        let mut reordered = snapshot;
        reordered.payload.resources.reverse();
        let reordered = ResourceNormalizer::new(temp.path())
            .unwrap()
            .normalize_full_snapshot("project:test", 1, &reordered)
            .unwrap();
        assert_eq!(generation.generation_id, reordered.generation_id);
        assert_eq!(generation.validation_digest, reordered.validation_digest);
    }
}
