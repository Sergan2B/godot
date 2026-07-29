use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};

use godot_codex_index_store::{IndexGeneration, LOGICAL_SCHEMA_V1, RecordValidity, SchemaVersion};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

const AUTHORITY_SCHEMA: &str = "offline-authority-v1";
const AUTHORITY_FILE: &str = "offline-authority-v1.json";
const MAX_SOURCE_FILES: usize = 20_000;
const MAX_SCAN_ENTRIES: usize = 100_000;
const MAX_SOURCE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SOURCE_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SOURCE_PATH_BYTES: usize = 1_024;
const MAX_SCAN_DEPTH: usize = 64;
const HASH_BUFFER_BYTES: usize = 64 * 1024;
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;

/// One saved, project-relative source file covered by offline authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInventoryEntry {
    pub path: String,
    pub byte_size: u64,
    pub sha256: String,
}

/// Durable proof that one complete semantic generation exactly matches disk.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfflineAuthorityManifest {
    pub schema: String,
    pub project_id: String,
    pub generation_id: String,
    pub index_schema: SchemaVersion,
    pub index_revision: u64,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub script_graph_revision: u64,
    pub source_file_count: usize,
    pub source_total_bytes: u64,
    pub source_inventory_digest: String,
    pub sources: Vec<SourceInventoryEntry>,
}

/// Successfully verified store coordinates safe to expose without an editor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineAuthorityVerification {
    pub manifest: OfflineAuthorityManifest,
}

/// Safe failures. Paths and source contents never cross this boundary.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum OfflineAuthorityError {
    #[error("offline_authority_missing")]
    Missing,
    #[error("offline_authority_corrupt")]
    Corrupt,
    #[error("offline_authority_incompatible")]
    Incompatible,
    #[error("offline_authority_project_mismatch")]
    ProjectMismatch,
    #[error("offline_authority_generation_mismatch")]
    GenerationMismatch,
    #[error("offline_authority_source_mismatch")]
    SourceMismatch,
    #[error("offline_authority_unsafe_symlink")]
    UnsafeSymlink,
    #[error("offline_authority_scan_limit")]
    ScanLimit,
    #[error("offline_authority_unsafe_path")]
    UnsafePath,
    #[error("offline_authority_io")]
    Io,
}

/// Recomputes the complete bounded saved-source inventory.
pub fn capture_source_inventory(
    project_root: &Path,
) -> Result<Vec<SourceInventoryEntry>, OfflineAuthorityError> {
    let canonical_root =
        fs::canonicalize(project_root).map_err(|_| OfflineAuthorityError::UnsafePath)?;
    if !canonical_root.is_dir() || !canonical_root.join("project.godot").is_file() {
        return Err(OfflineAuthorityError::UnsafePath);
    }
    let mut pending = vec![(canonical_root.clone(), 0_usize)];
    let mut sources = Vec::new();
    let mut total_bytes = 0_u64;
    let mut scanned_entries = 0_usize;
    while let Some((directory, depth)) = pending.pop() {
        if depth > MAX_SCAN_DEPTH {
            return Err(OfflineAuthorityError::ScanLimit);
        }
        let entries = fs::read_dir(&directory).map_err(|_| OfflineAuthorityError::Io)?;
        for entry in entries {
            scanned_entries = scanned_entries
                .checked_add(1)
                .ok_or(OfflineAuthorityError::ScanLimit)?;
            if scanned_entries > MAX_SCAN_ENTRIES {
                return Err(OfflineAuthorityError::ScanLimit);
            }
            let entry = entry.map_err(|_| OfflineAuthorityError::Io)?;
            let path = entry.path();
            let relative = path
                .strip_prefix(&canonical_root)
                .map_err(|_| OfflineAuthorityError::UnsafePath)?;
            let metadata = fs::symlink_metadata(&path).map_err(|_| OfflineAuthorityError::Io)?;
            if metadata.file_type().is_symlink() {
                return Err(OfflineAuthorityError::UnsafeSymlink);
            }
            if metadata.is_dir() {
                if should_skip_directory(relative) {
                    continue;
                }
                pending.push((path, depth + 1));
                continue;
            }
            if !metadata.is_file() || !is_saved_godot_source(relative) {
                continue;
            }
            if sources.len() >= MAX_SOURCE_FILES
                || metadata.len() > MAX_SOURCE_FILE_BYTES
                || total_bytes.saturating_add(metadata.len()) > MAX_SOURCE_TOTAL_BYTES
            {
                return Err(OfflineAuthorityError::ScanLimit);
            }
            let normalized_path = normalized_relative_path(relative)?;
            let canonical_file = fs::canonicalize(&path).map_err(|_| OfflineAuthorityError::Io)?;
            if !canonical_file.starts_with(&canonical_root) {
                return Err(OfflineAuthorityError::UnsafePath);
            }
            let (byte_size, sha256) = hash_bounded_file(&canonical_file, metadata.len())?;
            total_bytes = total_bytes
                .checked_add(byte_size)
                .ok_or(OfflineAuthorityError::ScanLimit)?;
            sources.push(SourceInventoryEntry {
                path: normalized_path,
                byte_size,
                sha256,
            });
        }
    }
    sources.sort_by(|left, right| left.path.cmp(&right.path));
    if sources.windows(2).any(|pair| pair[0].path == pair[1].path) {
        return Err(OfflineAuthorityError::UnsafePath);
    }
    if !sources.iter().any(|source| source.path == "project.godot") {
        return Err(OfflineAuthorityError::SourceMismatch);
    }
    Ok(sources)
}

/// Atomically publishes authority only for a complete current three-domain generation.
pub fn persist_offline_authority(
    project_root: &Path,
    generation: &IndexGeneration,
) -> Result<OfflineAuthorityManifest, OfflineAuthorityError> {
    if generation.schema_version != LOGICAL_SCHEMA_V1
        || !generation.checkpoint.source_complete
        || generation.scene.is_empty()
        || !generation.scene.source_complete
        || generation.script.is_empty()
        || !generation.script.source_complete
        || generation.scene.resource_revision != generation.checkpoint.resource_revision
        || generation.script.resource_revision != generation.checkpoint.resource_revision
        || generation.script.scene_graph_revision != generation.scene.scene_graph_revision
    {
        return Err(OfflineAuthorityError::Incompatible);
    }
    let sources = capture_source_inventory(project_root)?;
    validate_generation_sources(generation, &sources)?;
    let source_total_bytes = sources.iter().try_fold(0_u64, |total, source| {
        total
            .checked_add(source.byte_size)
            .ok_or(OfflineAuthorityError::ScanLimit)
    })?;
    let manifest = OfflineAuthorityManifest {
        schema: AUTHORITY_SCHEMA.to_owned(),
        project_id: generation.project_id.clone(),
        generation_id: generation.generation_id.clone(),
        index_schema: generation.schema_version,
        index_revision: generation.index_revision,
        resource_revision: generation.checkpoint.resource_revision,
        scene_graph_revision: generation.scene.scene_graph_revision,
        script_graph_revision: generation.script.script_graph_revision,
        source_file_count: sources.len(),
        source_total_bytes,
        source_inventory_digest: inventory_digest(&sources),
        sources,
    };
    write_manifest_atomic(project_root, &manifest)?;
    Ok(manifest)
}

/// Loads and independently validates an offline authority against store and disk.
pub fn load_verified_offline_authority(
    project_root: &Path,
    expected_project_id: &str,
    generation: &IndexGeneration,
) -> Result<OfflineAuthorityVerification, OfflineAuthorityError> {
    let path = verified_authority_directory(project_root, false)?.join(AUTHORITY_FILE);
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            OfflineAuthorityError::Missing
        } else {
            OfflineAuthorityError::Io
        }
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return Err(OfflineAuthorityError::Corrupt);
    }
    let bytes = fs::read(&path).map_err(|_| OfflineAuthorityError::Io)?;
    let manifest: OfflineAuthorityManifest =
        serde_json::from_slice(&bytes).map_err(|_| OfflineAuthorityError::Corrupt)?;
    validate_manifest_coordinates(&manifest, expected_project_id, generation)?;
    let sources = capture_source_inventory(project_root)?;
    validate_generation_sources(generation, &sources)?;
    let source_total_bytes = sources.iter().try_fold(0_u64, |total, source| {
        total
            .checked_add(source.byte_size)
            .ok_or(OfflineAuthorityError::ScanLimit)
    })?;
    if manifest.source_file_count != sources.len()
        || manifest.source_total_bytes != source_total_bytes
        || manifest.source_inventory_digest != inventory_digest(&sources)
        || manifest.sources != sources
    {
        return Err(OfflineAuthorityError::SourceMismatch);
    }
    Ok(OfflineAuthorityVerification { manifest })
}

fn validate_manifest_coordinates(
    manifest: &OfflineAuthorityManifest,
    expected_project_id: &str,
    generation: &IndexGeneration,
) -> Result<(), OfflineAuthorityError> {
    if manifest.schema != AUTHORITY_SCHEMA
        || manifest.index_schema != LOGICAL_SCHEMA_V1
        || generation.schema_version != LOGICAL_SCHEMA_V1
    {
        return Err(OfflineAuthorityError::Incompatible);
    }
    if manifest.project_id != expected_project_id || generation.project_id != expected_project_id {
        return Err(OfflineAuthorityError::ProjectMismatch);
    }
    if manifest.generation_id != generation.generation_id
        || manifest.index_revision != generation.index_revision
        || manifest.resource_revision != generation.checkpoint.resource_revision
        || manifest.scene_graph_revision != generation.scene.scene_graph_revision
        || manifest.script_graph_revision != generation.script.script_graph_revision
        || !generation.checkpoint.source_complete
        || generation.scene.is_empty()
        || !generation.scene.source_complete
        || generation.script.is_empty()
        || !generation.script.source_complete
        || generation.scene.resource_revision != generation.checkpoint.resource_revision
        || generation.script.resource_revision != generation.checkpoint.resource_revision
        || generation.script.scene_graph_revision != generation.scene.scene_graph_revision
    {
        return Err(OfflineAuthorityError::GenerationMismatch);
    }
    Ok(())
}

fn validate_generation_sources(
    generation: &IndexGeneration,
    sources: &[SourceInventoryEntry],
) -> Result<(), OfflineAuthorityError> {
    let inventory: BTreeMap<_, _> = sources
        .iter()
        .map(|source| (source.path.as_str(), source.sha256.as_str()))
        .collect();
    let mut covered = BTreeSet::new();
    for resource in &generation.resources {
        if matches!(
            resource.validity,
            RecordValidity::Invalid | RecordValidity::Deleted
        ) {
            continue;
        }
        covered.insert(require_inventory_hash(
            &inventory,
            &resource.display_path,
            resource.content_generation.as_deref(),
        )?);
    }
    for document in &generation.source_documents {
        covered.insert(require_inventory_hash(
            &inventory,
            &document.comparison_path,
            document.content_generation.as_deref(),
        )?);
    }
    for scene in &generation.scene.scenes {
        covered.insert(require_inventory_hash(
            &inventory,
            &scene.comparison_path,
            Some(&scene.content_generation),
        )?);
    }
    for document in &generation.script.documents {
        covered.insert(require_inventory_hash(
            &inventory,
            &document.path,
            Some(&document.content_sha256),
        )?);
    }
    if sources
        .iter()
        .any(|source| !source_has_generation_coverage(source, &covered))
    {
        return Err(OfflineAuthorityError::SourceMismatch);
    }
    Ok(())
}

fn source_has_generation_coverage(
    source: &SourceInventoryEntry,
    covered: &BTreeSet<String>,
) -> bool {
    source.path == "project.godot"
        || covered.contains(&source.path)
        || source
            .path
            .strip_suffix(".uid")
            .is_some_and(|owner| covered.contains(owner))
}

fn require_inventory_hash(
    inventory: &BTreeMap<&str, &str>,
    resource_path: &str,
    expected_hash: Option<&str>,
) -> Result<String, OfflineAuthorityError> {
    let relative = resource_path
        .strip_prefix("res://")
        .ok_or(OfflineAuthorityError::SourceMismatch)?;
    let normalized = normalized_relative_path(Path::new(relative))?;
    let actual = inventory
        .get(normalized.as_str())
        .ok_or(OfflineAuthorityError::SourceMismatch)?;
    let expected_hash = expected_hash.ok_or(OfflineAuthorityError::SourceMismatch)?;
    if expected_hash.len() != 71
        || !expected_hash.starts_with("sha256:")
        || !expected_hash[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || *actual != expected_hash
    {
        return Err(OfflineAuthorityError::SourceMismatch);
    }
    Ok(normalized)
}

fn should_skip_directory(relative: &Path) -> bool {
    relative.components().next().is_some_and(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| name.starts_with('.') || name == "target")
    })
}

fn is_saved_godot_source(relative: &Path) -> bool {
    if relative == Path::new("project.godot") {
        return true;
    }
    if relative
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("uid"))
    {
        return is_primary_saved_godot_source(&relative.with_extension(""));
    }
    is_primary_saved_godot_source(relative)
}

fn is_primary_saved_godot_source(relative: &Path) -> bool {
    relative
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "tscn"
                    | "scn"
                    | "tres"
                    | "res"
                    | "md"
                    | "gd"
                    | "gdc"
                    | "gdshader"
                    | "shader"
                    | "cs"
                    | "csproj"
                    | "sln"
                    | "cfg"
                    | "ini"
                    | "json"
                    | "csv"
                    | "xml"
                    | "po"
                    | "mo"
                    | "translation"
                    | "theme"
                    | "atlas"
                    | "png"
                    | "jpg"
                    | "jpeg"
                    | "webp"
                    | "svg"
                    | "bmp"
                    | "tga"
                    | "exr"
                    | "hdr"
                    | "dds"
                    | "ktx"
                    | "wav"
                    | "ogg"
                    | "mp3"
                    | "flac"
                    | "ttf"
                    | "otf"
                    | "woff"
                    | "woff2"
                    | "obj"
                    | "fbx"
                    | "gltf"
                    | "glb"
                    | "dae"
                    | "blend"
            )
        })
}

fn normalized_relative_path(relative: &Path) -> Result<String, OfflineAuthorityError> {
    let mut segments = Vec::new();
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(OfflineAuthorityError::UnsafePath);
        };
        let segment = segment.to_str().ok_or(OfflineAuthorityError::UnsafePath)?;
        if segment.is_empty() || segment == "." || segment == ".." || segment.contains(['/', '\\'])
        {
            return Err(OfflineAuthorityError::UnsafePath);
        }
        segments.push(segment.nfc().collect::<String>());
    }
    let path = segments.join("/");
    if path.is_empty() || path.len() > MAX_SOURCE_PATH_BYTES {
        return Err(OfflineAuthorityError::ScanLimit);
    }
    Ok(path)
}

fn hash_bounded_file(
    path: &Path,
    expected_size: u64,
) -> Result<(u64, String), OfflineAuthorityError> {
    if expected_size > MAX_SOURCE_FILE_BYTES {
        return Err(OfflineAuthorityError::ScanLimit);
    }
    let file = File::open(path).map_err(|_| OfflineAuthorityError::Io)?;
    let opened_metadata = file.metadata().map_err(|_| OfflineAuthorityError::Io)?;
    if !opened_metadata.is_file() || opened_metadata.len() != expected_size {
        return Err(OfflineAuthorityError::SourceMismatch);
    }
    let mut reader = BufReader::with_capacity(HASH_BUFFER_BYTES, file);
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    let mut bytes = 0_u64;
    let mut hasher = Sha256::new();
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| OfflineAuthorityError::Io)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or(OfflineAuthorityError::ScanLimit)?;
        if bytes > MAX_SOURCE_FILE_BYTES || bytes > expected_size {
            return Err(OfflineAuthorityError::SourceMismatch);
        }
        hasher.update(&buffer[..read]);
    }
    if bytes != expected_size {
        return Err(OfflineAuthorityError::SourceMismatch);
    }
    let closed_metadata = reader
        .get_ref()
        .metadata()
        .map_err(|_| OfflineAuthorityError::Io)?;
    if !closed_metadata.is_file()
        || closed_metadata.len() != opened_metadata.len()
        || closed_metadata.modified().ok() != opened_metadata.modified().ok()
    {
        return Err(OfflineAuthorityError::SourceMismatch);
    }
    Ok((bytes, format!("sha256:{:x}", hasher.finalize())))
}

fn inventory_digest(sources: &[SourceInventoryEntry]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"godot-codex/offline-source-inventory/v1\0");
    for source in sources {
        hasher.update((source.path.len() as u64).to_be_bytes());
        hasher.update(source.path.as_bytes());
        hasher.update(source.byte_size.to_be_bytes());
        hasher.update((source.sha256.len() as u64).to_be_bytes());
        hasher.update(source.sha256.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
fn authority_path(project_root: &Path) -> PathBuf {
    project_root
        .join(".godot")
        .join("codex")
        .join(AUTHORITY_FILE)
}

fn write_manifest_atomic(
    project_root: &Path,
    manifest: &OfflineAuthorityManifest,
) -> Result<(), OfflineAuthorityError> {
    let directory = verified_authority_directory(project_root, true)?;
    let bytes = serde_json::to_vec(manifest).map_err(|_| OfflineAuthorityError::Corrupt)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(OfflineAuthorityError::ScanLimit);
    }
    let temporary = directory.join(format!(
        ".{AUTHORITY_FILE}.{}.{}.tmp",
        std::process::id(),
        unique_suffix()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| OfflineAuthorityError::Io)?;
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|_| OfflineAuthorityError::Io)?;
        file.sync_all().map_err(|_| OfflineAuthorityError::Io)?;
        fs::rename(&temporary, directory.join(AUTHORITY_FILE))
            .map_err(|_| OfflineAuthorityError::Io)?;
        File::open(&directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| OfflineAuthorityError::Io)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn verified_authority_directory(
    project_root: &Path,
    create: bool,
) -> Result<PathBuf, OfflineAuthorityError> {
    let canonical_root =
        fs::canonicalize(project_root).map_err(|_| OfflineAuthorityError::UnsafePath)?;
    let godot = canonical_root.join(".godot");
    ensure_real_directory(&godot, create)?;
    let codex = godot.join("codex");
    ensure_real_directory(&codex, create)?;
    let canonical_directory = fs::canonicalize(&codex).map_err(|_| OfflineAuthorityError::Io)?;
    if !canonical_directory.starts_with(&canonical_root) {
        return Err(OfflineAuthorityError::UnsafePath);
    }
    Ok(canonical_directory)
}

fn ensure_real_directory(path: &Path, create: bool) -> Result<(), OfflineAuthorityError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(OfflineAuthorityError::UnsafeSymlink)
        }
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(OfflineAuthorityError::UnsafePath),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            fs::create_dir(path).map_err(|_| OfflineAuthorityError::Io)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(OfflineAuthorityError::Missing)
        }
        Err(_) => Err(OfflineAuthorityError::Io),
    }
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use godot_codex_index_store::{
        GenerationState, IndexGeneration, IngestionCheckpoint, SceneDomainGeneration,
        ScriptDomainGeneration, SourceDocument,
    };
    use tempfile::TempDir;

    use super::*;

    fn project() -> TempDir {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("project.godot"), b"[application]\\n").unwrap();
        fs::create_dir_all(project.path().join("scenes")).unwrap();
        fs::write(project.path().join("scenes/main.tscn"), b"[gd_scene]\\n").unwrap();
        fs::write(project.path().join("player.gd"), b"extends Node\\n").unwrap();
        project
    }

    fn generation(project_id: &str) -> IndexGeneration {
        let source_document = |entity_id: &str, path: &str, bytes: &[u8]| SourceDocument {
            entity_id: entity_id.to_owned(),
            comparison_path: path.to_owned(),
            size_before: bytes.len() as u64,
            size_after: bytes.len() as u64,
            mtime_before_ns: 1,
            mtime_after_ns: 1,
            content_generation: Some(format!("sha256:{:x}", Sha256::digest(bytes))),
            ingest_state: "ready".to_owned(),
        };
        let mut generation = IndexGeneration {
            generation_id: format!("generation:sha256:{}", "2".repeat(64)),
            parent_generation_id: None,
            schema_version: LOGICAL_SCHEMA_V1,
            project_id: project_id.to_owned(),
            index_revision: 7,
            state: GenerationState::Active,
            creation_reason: "full_snapshot".to_owned(),
            checkpoint: IngestionCheckpoint {
                editor_session_id: "historical-session".to_owned(),
                resource_revision: 5,
                project_revision: 6,
                index_revision: 7,
                source_complete: true,
                snapshot_checksum: format!("sha256:{}", "3".repeat(64)),
                last_batch_id: None,
                last_batch_checksum: None,
            },
            resources: Vec::new(),
            source_documents: vec![
                source_document(
                    "godot:resource:v1:scene",
                    "res://scenes/main.tscn",
                    b"[gd_scene]\\n",
                ),
                source_document(
                    "godot:resource:v1:script",
                    "res://player.gd",
                    b"extends Node\\n",
                ),
            ],
            dependencies: Vec::new(),
            diagnostics: Vec::new(),
            tombstones: Vec::new(),
            scene: SceneDomainGeneration {
                editor_session_id: "historical-session".to_owned(),
                resource_revision: 5,
                scene_graph_revision: 4,
                source_complete: true,
                snapshot_checksum: format!("sha256:{}", "4".repeat(64)),
                validation_digest: String::new(),
                ..SceneDomainGeneration::default()
            },
            script: ScriptDomainGeneration {
                editor_session_id: "historical-session".to_owned(),
                resource_revision: 5,
                scene_graph_revision: 4,
                script_graph_revision: 3,
                source_complete: true,
                snapshot_checksum: format!("sha256:{}", "5".repeat(64)),
                validation_digest: String::new(),
                ..ScriptDomainGeneration::default()
            },
            validation_digest: String::new(),
        };
        generation.scene.validation_digest = generation.scene.compute_validation_digest();
        generation.script.validation_digest = generation.script.compute_validation_digest();
        generation.validation_digest = generation.compute_validation_digest();
        generation
    }

    #[test]
    fn inventory_is_sorted_bounded_and_ignores_generated_state() {
        let project = project();
        fs::create_dir_all(project.path().join(".godot/codex")).unwrap();
        fs::write(project.path().join(".godot/codex/private.gd"), b"secret").unwrap();
        fs::write(project.path().join("player.gd.uid"), b"uid://player\n").unwrap();
        fs::write(project.path().join("notes.txt"), b"not Godot source").unwrap();
        let sources = capture_source_inventory(project.path()).unwrap();
        assert_eq!(
            sources
                .iter()
                .map(|source| source.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "player.gd",
                "player.gd.uid",
                "project.godot",
                "scenes/main.tscn"
            ]
        );
        assert!(
            sources
                .iter()
                .all(|source| source.sha256.starts_with("sha256:"))
        );
    }

    #[test]
    fn manifest_binds_godot_uid_sidecars_to_their_source() {
        let project = project();
        let sidecar = project.path().join("player.gd.uid");
        fs::write(&sidecar, b"uid://player\n").unwrap();
        let project_id = godot_codex_bridge_client::project_id_for_path(project.path()).unwrap();
        let generation = generation(&project_id);
        persist_offline_authority(project.path(), &generation).unwrap();
        load_verified_offline_authority(project.path(), &project_id, &generation).unwrap();

        fs::write(&sidecar, b"uid://changed\n").unwrap();
        assert_eq!(
            load_verified_offline_authority(project.path(), &project_id, &generation).unwrap_err(),
            OfflineAuthorityError::SourceMismatch
        );
    }

    #[test]
    fn manifest_covers_editor_indexed_markdown_but_skips_hidden_guidance() {
        let project = project();
        let guidance = b"# Godot guidance\n";
        fs::write(project.path().join("AGENTS.md"), guidance).unwrap();
        fs::create_dir_all(project.path().join(".agents/skills/godot-editor")).unwrap();
        fs::write(
            project.path().join(".agents/skills/godot-editor/SKILL.md"),
            b"# Hidden host guidance\n",
        )
        .unwrap();
        let project_id = godot_codex_bridge_client::project_id_for_path(project.path()).unwrap();
        let mut generation = generation(&project_id);
        generation.source_documents.push(SourceDocument {
            entity_id: "godot:resource:v1:agents-guidance".to_owned(),
            comparison_path: "res://AGENTS.md".to_owned(),
            size_before: guidance.len() as u64,
            size_after: guidance.len() as u64,
            mtime_before_ns: 1,
            mtime_after_ns: 1,
            content_generation: Some(format!("sha256:{:x}", Sha256::digest(guidance))),
            ingest_state: "ready".to_owned(),
        });
        generation.validation_digest.clear();
        generation.validation_digest = generation.compute_validation_digest();

        let manifest = persist_offline_authority(project.path(), &generation).unwrap();
        assert!(
            manifest
                .sources
                .iter()
                .any(|source| source.path == "AGENTS.md")
        );
        assert!(
            manifest
                .sources
                .iter()
                .all(|source| source.path != ".agents/skills/godot-editor/SKILL.md")
        );
        load_verified_offline_authority(project.path(), &project_id, &generation).unwrap();

        fs::write(project.path().join("AGENTS.md"), b"# Changed guidance\n").unwrap();
        assert_eq!(
            load_verified_offline_authority(project.path(), &project_id, &generation).unwrap_err(),
            OfflineAuthorityError::SourceMismatch
        );
    }

    #[test]
    fn manifest_detects_changed_added_deleted_and_renamed_sources() {
        for mutation in ["changed", "added", "deleted", "renamed"] {
            let project = project();
            let project_id =
                godot_codex_bridge_client::project_id_for_path(project.path()).unwrap();
            let generation = generation(&project_id);
            persist_offline_authority(project.path(), &generation).unwrap();
            match mutation {
                "changed" => {
                    fs::write(project.path().join("player.gd"), b"extends Node2D\\n").unwrap()
                }
                "added" => fs::write(project.path().join("new.gd"), b"extends Node\\n").unwrap(),
                "deleted" => fs::remove_file(project.path().join("player.gd")).unwrap(),
                "renamed" => fs::rename(
                    project.path().join("player.gd"),
                    project.path().join("renamed.gd"),
                )
                .unwrap(),
                _ => unreachable!(),
            }
            assert_eq!(
                load_verified_offline_authority(project.path(), &project_id, &generation)
                    .unwrap_err(),
                OfflineAuthorityError::SourceMismatch,
                "{mutation}"
            );
        }
    }

    #[test]
    fn manifest_rejects_wrong_project_generation_schema_and_corruption() {
        let project = project();
        let project_id = godot_codex_bridge_client::project_id_for_path(project.path()).unwrap();
        let generation = generation(&project_id);
        persist_offline_authority(project.path(), &generation).unwrap();
        assert_eq!(
            load_verified_offline_authority(project.path(), "project:wrong", &generation)
                .unwrap_err(),
            OfflineAuthorityError::ProjectMismatch
        );
        let mut other_generation = generation.clone();
        other_generation.generation_id = format!("generation:sha256:{}", "9".repeat(64));
        assert_eq!(
            load_verified_offline_authority(project.path(), &project_id, &other_generation)
                .unwrap_err(),
            OfflineAuthorityError::GenerationMismatch
        );
        fs::write(authority_path(project.path()), b"{").unwrap();
        assert_eq!(
            load_verified_offline_authority(project.path(), &project_id, &generation).unwrap_err(),
            OfflineAuthorityError::Corrupt
        );
    }

    #[test]
    fn manifest_requires_complete_source_coverage_and_exact_hashes() {
        let project = project();
        let project_id = godot_codex_bridge_client::project_id_for_path(project.path()).unwrap();
        let mut uncovered = generation(&project_id);
        uncovered.source_documents.pop();
        assert_eq!(
            persist_offline_authority(project.path(), &uncovered).unwrap_err(),
            OfflineAuthorityError::SourceMismatch
        );

        let mut unhashed = generation(&project_id);
        unhashed.source_documents[0].content_generation = None;
        assert_eq!(
            persist_offline_authority(project.path(), &unhashed).unwrap_err(),
            OfflineAuthorityError::SourceMismatch
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_rejects_symlinks_in_the_source_tree() {
        use std::os::unix::fs::symlink;

        let project = project();
        symlink(
            project.path().join("player.gd"),
            project.path().join("linked.gd"),
        )
        .unwrap();
        assert_eq!(
            capture_source_inventory(project.path()).unwrap_err(),
            OfflineAuthorityError::UnsafeSymlink
        );
    }

    #[cfg(unix)]
    #[test]
    fn manifest_rejects_a_symlinked_authority_directory() {
        use std::os::unix::fs::symlink;

        let project = project();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(project.path().join(".godot")).unwrap();
        symlink(outside.path(), project.path().join(".godot/codex")).unwrap();
        let project_id = godot_codex_bridge_client::project_id_for_path(project.path()).unwrap();
        assert_eq!(
            persist_offline_authority(project.path(), &generation(&project_id)).unwrap_err(),
            OfflineAuthorityError::UnsafeSymlink
        );
    }

    #[test]
    fn scan_rejects_oversized_sources() {
        let project = project();
        let file = File::create(project.path().join("huge.gd")).unwrap();
        file.set_len(MAX_SOURCE_FILE_BYTES + 1).unwrap();
        assert_eq!(
            capture_source_inventory(project.path()).unwrap_err(),
            OfflineAuthorityError::ScanLimit
        );
    }
}
