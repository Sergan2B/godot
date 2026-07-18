//! Deterministic canonical-oracle and synthetic graph inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use godot_codex_index_store::{
    DependencyEdge, DependencyResolution, Diagnostic, GenerationState, IdentityStrength,
    IncrementalBatch, IndexGeneration, IngestionCheckpoint, LOGICAL_SCHEMA_V1, RecordValidity,
    ResourceEntity, SourceDocument, StoreError,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Seed frozen by the D-05 plan.
pub const SYNTHETIC_SEED: &str = "D05-1";

/// Reserved synthetic targets and their exact incoming-edge counts.
pub const SYNTHETIC_FAN_IN_TARGETS: [(usize, usize); 5] =
    [(6, 0), (2, 1), (3, 10), (4, 100), (5, 1_000)];

/// Generates a deterministic resource graph with cycles and bounded high fan-in.
pub fn synthetic_generation(
    resource_count: usize,
    edge_count: usize,
    index_revision: u64,
) -> IndexGeneration {
    assert!(resource_count > 1);
    let resources: Vec<_> = (0..resource_count)
        .map(|index| synthetic_resource(index, index_revision))
        .collect();
    let source_documents = resources
        .iter()
        .map(|resource| SourceDocument {
            entity_id: resource.entity_id.clone(),
            comparison_path: resource.comparison_path.clone(),
            size_before: resource.byte_size,
            size_after: resource.byte_size,
            mtime_before_ns: resource.mtime_ns,
            mtime_after_ns: resource.mtime_ns,
            content_generation: resource.content_generation.clone(),
            ingest_state: "ready".to_owned(),
        })
        .collect();
    let dependencies = synthetic_edges(&resources, edge_count, index_revision);
    finish_generation(IndexGeneration {
        generation_id: format!("synthetic-{index_revision:020}"),
        parent_generation_id: index_revision
            .checked_sub(1)
            .filter(|revision| *revision > 0)
            .map(|revision| format!("synthetic-{revision:020}")),
        schema_version: LOGICAL_SCHEMA_V1,
        project_id: "project-d05-synthetic".to_owned(),
        index_revision,
        state: GenerationState::Active,
        creation_reason: if index_revision == 1 {
            "full_snapshot".to_owned()
        } else {
            "incremental".to_owned()
        },
        checkpoint: IngestionCheckpoint {
            editor_session_id: "session-d05".to_owned(),
            resource_revision: index_revision,
            project_revision: index_revision,
            index_revision,
            source_complete: true,
            snapshot_checksum: format!("sha256:synthetic-{index_revision}"),
            last_batch_id: (index_revision > 1)
                .then(|| format!("resource-batch:synthetic-{index_revision}")),
            last_batch_checksum: (index_revision > 1)
                .then(|| format!("sha256:synthetic-batch-{index_revision}")),
        },
        resources,
        source_documents,
        dependencies,
        diagnostics: Vec::new(),
        tombstones: Vec::new(),
        scene: Default::default(),
        validation_digest: String::new(),
    })
}

/// Produces the next UID-preserving rename generation.
pub fn renamed_generation(previous: &IndexGeneration, index_revision: u64) -> IndexGeneration {
    let mut next = previous.clone();
    next.parent_generation_id = Some(previous.generation_id.clone());
    next.generation_id = format!("rename-{index_revision:020}");
    next.index_revision = index_revision;
    next.creation_reason = "incremental_rename".to_owned();
    next.checkpoint.index_revision = index_revision;
    next.checkpoint.resource_revision = index_revision;
    next.checkpoint.project_revision = index_revision;
    let resource = &mut next.resources[0];
    let renamed = format!("res://generated/renamed/resource-{index_revision:06}.tres");
    resource.display_path = renamed.clone();
    resource.comparison_path = renamed.clone();
    resource.resource_revision = index_revision;
    if let Some(document) = next
        .source_documents
        .iter_mut()
        .find(|document| document.entity_id == resource.entity_id)
    {
        document.comparison_path = renamed;
    }
    for edge in &mut next.dependencies {
        if edge.target_entity_id.as_ref() == Some(&resource.entity_id) {
            edge.target_comparison_path = Some(resource.comparison_path.clone());
            edge.resource_revision = index_revision;
        }
    }
    next.validation_digest.clear();
    finish_generation(next)
}

/// Produces the storage-neutral delta equivalent of [`renamed_generation`].
pub fn renamed_batch(previous: &IndexGeneration, index_revision: u64) -> IncrementalBatch {
    let next = renamed_generation(previous, index_revision);
    IncrementalBatch {
        project_id: previous.project_id.clone(),
        base_generation_id: previous.generation_id.clone(),
        generation_id: next.generation_id.clone(),
        index_revision,
        creation_reason: next.creation_reason.clone(),
        checkpoint: next.checkpoint.clone(),
        upsert_resources: changed_records(&previous.resources, &next.resources, |record| {
            &record.entity_id
        }),
        remove_resource_entity_ids: removed_keys(&previous.resources, &next.resources, |record| {
            &record.entity_id
        }),
        upsert_source_documents: changed_records(
            &previous.source_documents,
            &next.source_documents,
            |record| &record.entity_id,
        ),
        remove_source_document_entity_ids: removed_keys(
            &previous.source_documents,
            &next.source_documents,
            |record| &record.entity_id,
        ),
        upsert_dependencies: changed_records(
            &previous.dependencies,
            &next.dependencies,
            |record| &record.edge_id,
        ),
        remove_dependency_edge_ids: removed_keys(
            &previous.dependencies,
            &next.dependencies,
            |record| &record.edge_id,
        ),
        upsert_diagnostics: changed_records(&previous.diagnostics, &next.diagnostics, |record| {
            &record.diagnostic_id
        }),
        remove_diagnostic_ids: removed_keys(&previous.diagnostics, &next.diagnostics, |record| {
            &record.diagnostic_id
        }),
        upsert_tombstones: changed_records(&previous.tombstones, &next.tombstones, |record| {
            &record.entity_id
        }),
        remove_tombstone_entity_ids: removed_keys(
            &previous.tombstones,
            &next.tombstones,
            |record| &record.entity_id,
        ),
    }
}

fn changed_records<T, F>(previous: &[T], next: &[T], key: F) -> Vec<T>
where
    T: Clone + PartialEq,
    F: Fn(&T) -> &String,
{
    let previous_by_key: BTreeMap<_, _> = previous
        .iter()
        .map(|record| (key(record).as_str(), record))
        .collect();
    next.iter()
        .filter(|candidate| {
            previous_by_key.get(key(candidate).as_str()).copied() != Some(*candidate)
        })
        .cloned()
        .collect()
}

fn removed_keys<T, F>(previous: &[T], next: &[T], key: F) -> Vec<String>
where
    F: Fn(&T) -> &String,
{
    let next_keys: BTreeSet<_> = next.iter().map(|record| key(record).as_str()).collect();
    previous
        .iter()
        .filter(|candidate| !next_keys.contains(key(candidate).as_str()))
        .map(|record| key(record).clone())
        .collect()
}

/// Loads all eight Stage 1 canonical oracle phases.
pub fn oracle_generations(path: &Path) -> Result<Vec<IndexGeneration>, StoreError> {
    let bytes = fs::read(path).map_err(io_error)?;
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|error| StoreError::CorruptStore(error.to_string()))?;
    let phases = document
        .get("phases")
        .and_then(Value::as_array)
        .ok_or_else(|| StoreError::CorruptStore("oracle phases missing".to_owned()))?;
    phases
        .iter()
        .enumerate()
        .map(|(index, phase)| oracle_phase(phase, (index + 1) as u64))
        .collect()
}

fn oracle_phase(phase: &Value, revision: u64) -> Result<IndexGeneration, StoreError> {
    let name = string(phase, "name")?;
    let resources_value = array(phase, "resources")?;
    let resources: Vec<_> = resources_value
        .iter()
        .map(|resource| {
            let uid = optional_string(resource, "uid");
            let path = string(resource, "path")?;
            Ok(ResourceEntity {
                entity_id: string(resource, "entity_id")?,
                identity_input: uid.clone().unwrap_or_else(|| {
                    format!(
                        "{}\0{}",
                        path,
                        optional_string(resource, "content_generation").unwrap_or_default()
                    )
                }),
                uid,
                display_path: path.clone(),
                comparison_path: path,
                identity_strength: match string(resource, "identity_strength")?.as_str() {
                    "resource_uid" => IdentityStrength::ResourceUid,
                    _ => IdentityStrength::PathContentGeneration,
                },
                resource_type: string(resource, "type")?,
                source_kind: "source".to_owned(),
                import_state: string(resource, "import_state")?,
                authority: "editor_file_system".to_owned(),
                content_generation: optional_string(resource, "content_generation"),
                mtime_ns: revision,
                byte_size: 1,
                validity: RecordValidity::Valid,
                resource_revision: revision,
            })
        })
        .collect::<Result<_, StoreError>>()?;
    let source_documents = resources
        .iter()
        .map(|resource| SourceDocument {
            entity_id: resource.entity_id.clone(),
            comparison_path: resource.comparison_path.clone(),
            size_before: resource.byte_size,
            size_after: resource.byte_size,
            mtime_before_ns: revision,
            mtime_after_ns: revision,
            content_generation: resource.content_generation.clone(),
            ingest_state: "ready".to_owned(),
        })
        .collect();
    let dependencies = array(phase, "dependencies")?
        .iter()
        .map(|edge| {
            let resolution = match string(edge, "resolution")?.as_str() {
                "resolved" => DependencyResolution::Resolved,
                "missing" => DependencyResolution::Missing,
                "stale_uid" => DependencyResolution::StaleUid,
                other => {
                    return Err(StoreError::CorruptStore(format!(
                        "unknown oracle resolution {other}"
                    )));
                }
            };
            Ok(DependencyEdge {
                edge_id: string(edge, "edge_id")?,
                source_entity_id: string(edge, "source_entity_id")?,
                target_uid: optional_string(edge, "target_uid"),
                target_comparison_path: optional_string(edge, "fallback_path"),
                target_display_path: optional_string(edge, "fallback_path"),
                target_entity_id: optional_string(edge, "resolved_target_entity_id"),
                resolved_target_path: optional_string(edge, "resolved_target_path"),
                relation: "references".to_owned(),
                declared_type: optional_string(edge, "declared_type"),
                authority: "godot_resource_loader".to_owned(),
                resolution,
                resource_revision: revision,
            })
        })
        .collect::<Result<_, StoreError>>()?;
    let diagnostics = array(phase, "diagnostics")?
        .iter()
        .enumerate()
        .map(|(index, diagnostic)| Diagnostic {
            diagnostic_id: format!("oracle:{name}:{index}"),
            code: optional_string(diagnostic, "code").unwrap_or_else(|| "unknown".to_owned()),
            subject: optional_string(diagnostic, "source")
                .or_else(|| optional_string(diagnostic, "target_reference"))
                .unwrap_or_else(|| "oracle".to_owned()),
            detail: optional_string(diagnostic, "target_reference"),
            first_index_revision: revision,
            last_index_revision: revision,
            active: true,
        })
        .collect();
    Ok(finish_generation(IndexGeneration {
        generation_id: format!("oracle-{revision:02}-{name}"),
        parent_generation_id: revision
            .checked_sub(1)
            .filter(|parent| *parent > 0)
            .map(|parent| format!("oracle-{parent:02}")),
        schema_version: LOGICAL_SCHEMA_V1,
        project_id: "project-oracle".to_owned(),
        index_revision: revision,
        state: GenerationState::Active,
        creation_reason: if revision == 1 || name == "journal_gap" {
            "full_snapshot".to_owned()
        } else {
            "oracle_transition".to_owned()
        },
        checkpoint: IngestionCheckpoint {
            editor_session_id: "session-oracle".to_owned(),
            resource_revision: revision,
            project_revision: revision,
            index_revision: revision,
            // The journal-gap oracle phase represents the required full-resnapshot
            // result after invalidation, never the incomplete gap observation itself.
            source_complete: true,
            snapshot_checksum: format!("sha256:oracle-{name}"),
            last_batch_id: (revision > 1).then(|| format!("resource-batch:oracle-{name}")),
            last_batch_checksum: (revision > 1).then(|| format!("sha256:oracle-{name}")),
        },
        resources,
        source_documents,
        dependencies,
        diagnostics,
        tombstones: Vec::new(),
        scene: Default::default(),
        validation_digest: String::new(),
    }))
}

fn synthetic_resource(index: usize, revision: u64) -> ResourceEntity {
    let entity_id = format!("godot:resource:uid:v1:synthetic-{index:08}");
    let uid = format!("uid://d05{index:08x}");
    let path = format!(
        "res://generated/{:03}/resource-{index:08}.tres",
        index % 256
    );
    ResourceEntity {
        entity_id,
        identity_input: uid.clone(),
        uid: Some(uid),
        display_path: path.clone(),
        comparison_path: path,
        identity_strength: IdentityStrength::ResourceUid,
        resource_type: "Resource".to_owned(),
        source_kind: "source".to_owned(),
        import_state: "ready".to_owned(),
        authority: "editor_file_system".to_owned(),
        content_generation: Some(format!("sha256:{:064x}", deterministic_u64(index as u64))),
        mtime_ns: revision,
        byte_size: 256 + (index % 4096) as u64,
        validity: RecordValidity::Valid,
        resource_revision: revision,
    }
}

fn synthetic_edges(
    resources: &[ResourceEntity],
    edge_count: usize,
    revision: u64,
) -> Vec<DependencyEdge> {
    let count = resources.len();
    let mut pairs = BTreeSet::new();
    let mut candidates = Vec::new();
    let reserved_targets: BTreeSet<_> = SYNTHETIC_FAN_IN_TARGETS
        .iter()
        .map(|(target, _)| target % count)
        .collect();

    let cycle_nodes: Vec<_> = (0..count)
        .filter(|node| !reserved_targets.contains(node))
        .take(3)
        .collect();
    if cycle_nodes.len() == 3 {
        candidates.extend([
            (cycle_nodes[0], cycle_nodes[1]),
            (cycle_nodes[1], cycle_nodes[2]),
            (cycle_nodes[2], cycle_nodes[0]),
        ]);
    }

    for (target, fan_in) in SYNTHETIC_FAN_IN_TARGETS {
        let target = target % count;
        for source in (0..count)
            .filter(|source| *source != target)
            .take(fan_in.min(count.saturating_sub(1)))
        {
            candidates.push((source, target));
        }
    }
    let mut cursor = 0_u64;
    while candidates.len() < edge_count.saturating_mul(2) {
        let source = (deterministic_u64(cursor) as usize) % count;
        let mut target = (deterministic_u64(cursor ^ 0x9e37_79b9_7f4a_7c15) as usize) % count;
        while reserved_targets.len() < count && reserved_targets.contains(&target) {
            target = (target + 1) % count;
        }
        candidates.push((source, target));
        cursor += 1;
    }
    let mut edges = Vec::with_capacity(edge_count);
    for (source, target) in candidates {
        if source == target || !pairs.insert((source, target)) {
            continue;
        }
        let source_resource = &resources[source];
        let target_resource = &resources[target];
        edges.push(DependencyEdge {
            edge_id: format!("godot:edge:references:v1:synthetic-{source:08}-{target:08}"),
            source_entity_id: source_resource.entity_id.clone(),
            target_uid: target_resource.uid.clone(),
            target_comparison_path: Some(target_resource.comparison_path.clone()),
            target_display_path: Some(target_resource.display_path.clone()),
            target_entity_id: Some(target_resource.entity_id.clone()),
            resolved_target_path: Some(target_resource.display_path.clone()),
            relation: "references".to_owned(),
            declared_type: None,
            authority: "godot_resource_loader".to_owned(),
            resolution: DependencyResolution::Resolved,
            resource_revision: revision,
        });
        if edges.len() == edge_count {
            break;
        }
    }
    assert_eq!(
        edges.len(),
        edge_count,
        "deterministic generator underfilled"
    );
    edges
}

/// Verifies the frozen fan-in coordinates and an explicit directed cycle.
pub fn validate_synthetic_shape(generation: &IndexGeneration) -> Result<(), StoreError> {
    let count = generation.resources.len();
    for (target, expected) in SYNTHETIC_FAN_IN_TARGETS {
        if target >= count || expected >= count {
            continue;
        }
        let entity_id = format!("godot:resource:uid:v1:synthetic-{target:08}");
        let actual = generation
            .dependencies
            .iter()
            .filter(|edge| edge.target_entity_id.as_deref() == Some(entity_id.as_str()))
            .count();
        if actual != expected {
            return Err(StoreError::ValidationFailed(format!(
                "synthetic fan-in target {target} has {actual} edges instead of {expected}"
            )));
        }
    }

    let cycle = [0_usize, 1, 7];
    let has_cycle = cycle.iter().enumerate().all(|(index, source)| {
        let target = cycle[(index + 1) % cycle.len()];
        let source_id = format!("godot:resource:uid:v1:synthetic-{source:08}");
        let target_id = format!("godot:resource:uid:v1:synthetic-{target:08}");
        generation.dependencies.iter().any(|edge| {
            edge.source_entity_id == source_id
                && edge.target_entity_id.as_deref() == Some(target_id.as_str())
        })
    });
    if !has_cycle {
        return Err(StoreError::ValidationFailed(
            "synthetic directed cycle is missing".to_owned(),
        ));
    }
    Ok(())
}

fn finish_generation(mut generation: IndexGeneration) -> IndexGeneration {
    generation.canonicalize();
    generation.validation_digest = generation.compute_validation_digest();
    generation
}

fn deterministic_u64(mut value: u64) -> u64 {
    value ^= 0xd05d_05d0_5d05_d05d;
    value = value.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value.wrapping_mul(0x94d0_49bb_1331_11eb) ^ (value >> 31)
}

fn string(value: &Value, key: &str) -> Result<String, StoreError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| StoreError::CorruptStore(format!("oracle field {key} missing")))
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, StoreError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| StoreError::CorruptStore(format!("oracle array {key} missing")))
}

fn io_error(error: std::io::Error) -> StoreError {
    StoreError::StorageIo(error.to_string())
}

/// SHA-256 of a file used to bind evidence to fixtures.
pub fn file_sha256(path: &Path) -> Result<String, StoreError> {
    let bytes = fs::read(path).map_err(io_error)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_profile_has_exact_reserved_fan_in_and_cycle() {
        let generation = synthetic_generation(2_000, 8_000, 1);
        validate_synthetic_shape(&generation).expect("frozen synthetic shape");
        assert_eq!(generation.resources.len(), 2_000);
        assert_eq!(generation.dependencies.len(), 8_000);
    }
}
