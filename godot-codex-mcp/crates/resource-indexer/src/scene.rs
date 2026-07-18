use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use godot_codex_bridge_client::{
    AnimationTrackResolution as BridgeAnimationResolution, ProjectContextObservation, ResourceRef,
    SceneDiagnostic, SceneObservation, SceneSnapshot,
};
use godot_codex_index_store::{
    IndexGeneration, ResourceEntity, SceneAnimationReference, SceneAnimationResolution,
    SceneConnection, SceneDomainGeneration, SceneEntity, SceneGroupMembership, SceneIdentityScope,
    SceneNode, SceneProperty, ScenePropertyOrigin, SceneRelation,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{IndexerError, normalize_resource_path};

const SCENE_UID_DOMAIN: &str = "godot-codex/scene-entity/uid/v1";
const SCENE_PATH_DOMAIN: &str = "godot-codex/scene-entity/path-content/v1";
const NODE_UNIQUE_DOMAIN: &str = "godot-codex/node-definition/scene-unique-id/v1";
const NODE_PATH_DOMAIN: &str = "godot-codex/node-definition/path-content/v1";
const NODE_OCCURRENCE_DOMAIN: &str = "godot-codex/node-occurrence/instance-chain/v1";
const SUBRESOURCE_UNIQUE_DOMAIN: &str = "godot-codex/subresource/scene-unique-id/v1";
const SUBRESOURCE_FALLBACK_DOMAIN: &str = "godot-codex/subresource/content-revision/v1";
const PROPERTY_DOMAIN: &str = "godot-codex/scene-property/v1";
const RELATION_DOMAIN: &str = "godot-codex/scene-relation/v1";
const CONNECTION_DOMAIN: &str = "godot-codex/scene-connection/v1";
const GROUP_DOMAIN: &str = "godot-codex/scene-group/v1";
const ANIMATION_DOMAIN: &str = "godot-codex/scene-animation-reference/v1";
const MAX_COMPOSITION_DEPTH: usize = 64;

/// Stateless conversion from verified Bridge scene observations to the
/// storage-neutral logical scene domain.
#[derive(Clone, Copy, Debug, Default)]
pub struct SceneNormalizer;

#[derive(Clone)]
struct RawScene {
    observation: SceneObservation,
    entity: SceneEntity,
    base_scene_id: Option<String>,
    local_definitions: BTreeMap<String, String>,
    effective_definitions: BTreeMap<String, String>,
    effective_instances: BTreeMap<String, InstanceDeclaration>,
}

#[derive(Clone)]
struct InstanceDeclaration {
    root_definition_id: String,
    target_scene_id: String,
    editable: bool,
    declaration_scene_id: String,
}

#[derive(Clone)]
struct Occurrence {
    occurrence_id: String,
    root_scene_id: String,
    source_scene_id: String,
    definition_id: String,
    effective_path: String,
    instance_chain: Vec<String>,
    editable: bool,
    internal: bool,
    resource_revision: u64,
    scene_graph_revision: u64,
}

impl SceneNormalizer {
    /// Normalizes and composes one complete verified scene snapshot against
    /// exactly one current resource generation.
    pub fn normalize_full_snapshot(
        &self,
        resource_generation: &IndexGeneration,
        snapshot: &SceneSnapshot,
    ) -> Result<SceneDomainGeneration, IndexerError> {
        if snapshot.accepted.resource_revision != snapshot.end.resource_revision
            || snapshot.accepted.scene_graph_revision != snapshot.end.scene_graph_revision
            || snapshot.accepted.revisions.editor_session_id
                != snapshot.end.revisions.editor_session_id
            || snapshot.end.resource_revision != resource_generation.checkpoint.resource_revision
        {
            return Err(IndexerError::Scene("snapshot_context"));
        }
        self.normalize_observations(
            resource_generation,
            &snapshot.end.revisions.editor_session_id,
            snapshot.end.resource_revision,
            snapshot.end.scene_graph_revision,
            &snapshot.end.checksum,
            &snapshot.payload.scenes,
            &snapshot.payload.project_context,
            &snapshot.payload.diagnostics,
        )
    }

    /// Normalizes a complete in-memory catalog. The coordinator uses this after
    /// applying an exact ordered delta to its detached observation cache.
    #[allow(clippy::too_many_arguments)]
    pub fn normalize_observations(
        &self,
        resource_generation: &IndexGeneration,
        editor_session_id: &str,
        resource_revision: u64,
        scene_graph_revision: u64,
        snapshot_checksum: &str,
        observations: &[SceneObservation],
        project_context: &[ProjectContextObservation],
        diagnostics: &[SceneDiagnostic],
    ) -> Result<SceneDomainGeneration, IndexerError> {
        if editor_session_id.is_empty()
            || resource_revision == 0
            || scene_graph_revision == 0
            || snapshot_checksum.is_empty()
            || resource_revision != resource_generation.checkpoint.resource_revision
        {
            return Err(IndexerError::Scene("catalog_context"));
        }

        let resources_by_path: BTreeMap<_, _> = resource_generation
            .resources
            .iter()
            .map(|resource| (resource.comparison_path.clone(), resource))
            .collect();
        let resources_by_uid: BTreeMap<_, _> = resource_generation
            .resources
            .iter()
            .filter_map(|resource| resource.uid.as_ref().map(|uid| (uid.clone(), resource)))
            .collect();

        let mut ordered_observations = observations.to_vec();
        ordered_observations.sort_by(|left, right| left.path.cmp(&right.path));
        let mut reference_to_scene = BTreeMap::new();
        let mut scenes = BTreeMap::new();
        for observation in ordered_observations {
            if !observation.source_complete
                || observation.resource_revision > resource_revision
                || observation.scene_graph_revision > scene_graph_revision
            {
                return Err(IndexerError::Scene("scene_checkpoint"));
            }
            let path = normalize_resource_path(&observation.path)?;
            let resource = resources_by_path.get(&path.comparison).copied();
            let (scene_entity_id, uid, identity_scope) = scene_identity(
                &observation.scene_ref,
                &path.comparison,
                &observation.content_generation,
                resource,
                &resources_by_uid,
            )?;
            if scenes.contains_key(&scene_entity_id) {
                return Err(IndexerError::Scene("duplicate_scene_entity"));
            }
            reference_to_scene.insert(format!("path:{}", path.comparison), scene_entity_id.clone());
            if let Some(uid) = &uid
                && reference_to_scene
                    .insert(format!("uid:{uid}"), scene_entity_id.clone())
                    .is_some()
            {
                return Err(IndexerError::Scene("duplicate_scene_uid"));
            }
            let entity = SceneEntity {
                scene_entity_id: scene_entity_id.clone(),
                source_resource_entity_id: resource.map(|resource| resource.entity_id.clone()),
                uid,
                comparison_path: path.comparison,
                content_generation: observation.content_generation.clone(),
                identity_scope,
                base_scene_entity_id: None,
                authority: observation.authority.clone(),
                resource_revision: observation.resource_revision,
                scene_graph_revision: observation.scene_graph_revision,
            };
            scenes.insert(
                scene_entity_id,
                RawScene {
                    observation,
                    entity,
                    base_scene_id: None,
                    local_definitions: BTreeMap::new(),
                    effective_definitions: BTreeMap::new(),
                    effective_instances: BTreeMap::new(),
                },
            );
        }

        for scene in scenes.values_mut() {
            scene.base_scene_id = scene
                .observation
                .base_scene_ref
                .as_ref()
                .map(|reference| resolve_scene_reference(reference, &reference_to_scene))
                .transpose()?;
            scene.entity.base_scene_entity_id = scene.base_scene_id.clone();
        }
        let order = inheritance_order(&scenes)?;

        let mut nodes = Vec::new();
        for scene_id in &order {
            normalize_definitions(
                scene_id,
                &mut scenes,
                &reference_to_scene,
                &resources_by_path,
                &resources_by_uid,
                &mut nodes,
            )?;
        }

        let mut relations = Vec::new();
        append_base_and_instance_relations(&scenes, &mut relations)?;
        append_subresources_and_resource_relations(
            &scenes,
            &resources_by_path,
            &resources_by_uid,
            &mut relations,
        )?;
        append_project_context(project_context, resource_revision, &mut relations)?;
        append_diagnostics(diagnostics, resource_revision, &scenes, &mut relations)?;

        let mut properties_by_scene = BTreeMap::new();
        let mut properties = Vec::new();
        for scene_id in &order {
            let effective = compose_definition_properties(
                scene_id,
                &scenes,
                properties_by_scene.get(
                    scenes
                        .get(scene_id)
                        .and_then(|scene| scene.base_scene_id.as_ref())
                        .unwrap_or(&String::new()),
                ),
            )?;
            properties.extend(effective.values().cloned());
            properties_by_scene.insert(scene_id.clone(), effective);
        }

        let mut occurrences = Vec::new();
        let mut occurrence_lookup = BTreeMap::new();
        for scene_id in &order {
            let mut stack = Vec::new();
            expand_scene_occurrences(
                scene_id,
                scene_id,
                ".",
                &[],
                true,
                false,
                false,
                &scenes,
                &mut stack,
                &mut occurrences,
                &mut occurrence_lookup,
            )?;
        }
        append_occurrence_relations(&occurrences, &mut relations)?;
        append_occurrence_properties(
            &scenes,
            &occurrences,
            &occurrence_lookup,
            &properties_by_scene,
            &mut properties,
        )?;

        let groups = compose_groups(&order, &scenes, &occurrences)?;
        let connections = compose_connections(&order, &scenes, &occurrences)?;
        let animations = compose_animations(&scenes, &occurrence_lookup)?;
        for animation in animations
            .iter()
            .filter(|animation| animation.resolution == SceneAnimationResolution::Broken)
        {
            relations.push(relation(
                Some(&animation.scene_entity_id),
                "diagnostic",
                &animation.animation_reference_id,
                None,
                Some(&animation.scene_entity_id),
                BTreeMap::from([("code".to_owned(), json!("broken_animation_node_path"))]),
                "scene_composer",
                animation.resource_revision,
                animation.scene_graph_revision,
            )?);
        }

        let mut domain = SceneDomainGeneration {
            editor_session_id: editor_session_id.to_owned(),
            resource_revision,
            scene_graph_revision,
            source_complete: true,
            snapshot_checksum: snapshot_checksum.to_owned(),
            scenes: scenes.into_values().map(|scene| scene.entity).collect(),
            nodes,
            properties,
            relations,
            connections,
            groups,
            animations,
            validation_digest: String::new(),
        };
        domain.canonicalize();
        domain.validation_digest = domain.compute_validation_digest();
        domain.validate()?;
        Ok(domain)
    }
}

fn scene_identity(
    reference: &ResourceRef,
    comparison_path: &str,
    content_generation: &str,
    resource: Option<&ResourceEntity>,
    resources_by_uid: &BTreeMap<String, &ResourceEntity>,
) -> Result<(String, Option<String>, SceneIdentityScope), IndexerError> {
    let uid = resource
        .and_then(|resource| resource.uid.clone())
        .or_else(|| match reference {
            ResourceRef::Uid(reference) if resources_by_uid.contains_key(&reference.uid) => {
                Some(reference.uid.clone())
            }
            _ => None,
        });
    if let Some(uid) = uid {
        Ok((
            prefixed_identity("godot:scene:uid:v1:", SCENE_UID_DOMAIN, &[&uid]),
            Some(uid),
            SceneIdentityScope::Persistent,
        ))
    } else {
        Ok((
            prefixed_identity(
                "godot:scene:path-content:v1:",
                SCENE_PATH_DOMAIN,
                &[comparison_path, content_generation],
            ),
            None,
            SceneIdentityScope::ContentRevision,
        ))
    }
}

fn resolve_scene_reference(
    reference: &ResourceRef,
    lookup: &BTreeMap<String, String>,
) -> Result<String, IndexerError> {
    let key = match reference {
        ResourceRef::Uid(reference) => format!("uid:{}", reference.uid),
        ResourceRef::Path(reference) => {
            let path = normalize_resource_path(&reference.path)?;
            format!("path:{}", path.comparison)
        }
    };
    lookup
        .get(&key)
        .cloned()
        .ok_or(IndexerError::Scene("scene_reference_missing"))
}

fn inheritance_order(scenes: &BTreeMap<String, RawScene>) -> Result<Vec<String>, IndexerError> {
    let mut pending: BTreeSet<_> = scenes.keys().cloned().collect();
    let mut complete = BTreeSet::new();
    let mut order = Vec::with_capacity(scenes.len());
    while !pending.is_empty() {
        let ready: Vec<_> = pending
            .iter()
            .filter(|scene_id| {
                scenes[*scene_id]
                    .base_scene_id
                    .as_ref()
                    .is_none_or(|base| complete.contains(base))
            })
            .cloned()
            .collect();
        if ready.is_empty() {
            return Err(IndexerError::Scene("scene_composition_cycle"));
        }
        for scene_id in ready {
            pending.remove(&scene_id);
            complete.insert(scene_id.clone());
            order.push(scene_id);
        }
    }
    Ok(order)
}

#[allow(clippy::too_many_arguments)]
fn normalize_definitions(
    scene_id: &str,
    scenes: &mut BTreeMap<String, RawScene>,
    reference_to_scene: &BTreeMap<String, String>,
    resources_by_path: &BTreeMap<String, &ResourceEntity>,
    resources_by_uid: &BTreeMap<String, &ResourceEntity>,
    output: &mut Vec<SceneNode>,
) -> Result<(), IndexerError> {
    let (base_definitions, base_instances) = if let Some(base) = scenes
        .get(scene_id)
        .and_then(|scene| scene.base_scene_id.as_ref())
    {
        let base = scenes
            .get(base)
            .ok_or(IndexerError::Scene("base_scene_missing"))?;
        (
            base.effective_definitions.clone(),
            base.effective_instances.clone(),
        )
    } else {
        (BTreeMap::new(), BTreeMap::new())
    };
    let mut effective = base_definitions;
    let mut effective_instances = base_instances;
    let scene = scenes
        .get(scene_id)
        .cloned()
        .ok_or(IndexerError::Scene("scene_missing"))?;
    let mut unique_ids = BTreeSet::new();
    let instance_paths: Vec<_> = scene
        .observation
        .nodes
        .iter()
        .filter(|node| node.instance.is_some())
        .map(|node| node.node_path.as_str())
        .collect();
    let mut local = BTreeMap::new();
    for node in &scene.observation.nodes {
        let under_instance = instance_paths.iter().any(|prefix| {
            node.node_path != **prefix
                && node
                    .node_path
                    .strip_prefix(*prefix)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        });
        let node_id = if let Some(unique_id) = node.unique_scene_id {
            if unique_id == 0 || !unique_ids.insert(unique_id) {
                return Err(IndexerError::Scene("duplicate_scene_node_id"));
            }
            Some(prefixed_identity(
                "godot:node:scene-id:v1:",
                NODE_UNIQUE_DOMAIN,
                &[scene_id, &unique_id.to_string()],
            ))
        } else if effective.contains_key(&node.node_path) || under_instance {
            None
        } else {
            Some(prefixed_identity(
                "godot:node:path-content:v1:",
                NODE_PATH_DOMAIN,
                &[scene_id, &node.node_path, &scene.entity.content_generation],
            ))
        };
        if let Some(node_id) = node_id {
            if effective
                .insert(node.node_path.clone(), node_id.clone())
                .is_some()
            {
                return Err(IndexerError::Scene("duplicate_scene_node_path"));
            }
            local.insert(node.node_path.clone(), node_id);
        }
    }

    for node in &scene.observation.nodes {
        let Some(node_id) = local.get(&node.node_path).cloned() else {
            continue;
        };
        let instance_scene_entity_id = node
            .instance
            .as_ref()
            .map(|reference| resolve_scene_reference(reference, reference_to_scene))
            .transpose()?;
        if let Some(target_scene_id) = &instance_scene_entity_id {
            effective_instances.insert(
                node.node_path.clone(),
                InstanceDeclaration {
                    root_definition_id: node_id.clone(),
                    target_scene_id: target_scene_id.clone(),
                    editable: scene
                        .observation
                        .editable_instances
                        .contains(&node.node_path),
                    declaration_scene_id: scene_id.to_owned(),
                },
            );
        }
        output.push(SceneNode {
            node_entity_id: node_id,
            scene_entity_id: scene_id.to_owned(),
            node_path: node.node_path.clone(),
            identity_scope: if node.unique_scene_id.is_some() {
                SceneIdentityScope::Persistent
            } else {
                SceneIdentityScope::ContentRevision
            },
            unique_scene_id: node.unique_scene_id,
            parent_node_entity_id: node
                .parent_path
                .as_ref()
                .and_then(|path| effective.get(path))
                .cloned(),
            owner_node_entity_id: node
                .owner_path
                .as_ref()
                .and_then(|path| effective.get(path))
                .cloned(),
            name: node.name.clone(),
            godot_type: node.godot_type.clone(),
            node_index: node.node_index,
            owned: node.owned,
            internal: node.internal,
            attached_script_entity_id: node.attached_script.as_ref().and_then(|reference| {
                resolve_resource_reference(reference, resources_by_path, resources_by_uid)
                    .map(|resource| resource.entity_id.clone())
            }),
            instance_scene_entity_id,
            authority: scene.observation.authority.clone(),
            resource_revision: scene.observation.resource_revision,
            scene_graph_revision: scene.observation.scene_graph_revision,
        });
    }
    let scene = scenes
        .get_mut(scene_id)
        .ok_or(IndexerError::Scene("scene_missing"))?;
    scene.local_definitions = local;
    scene.effective_definitions = effective;
    scene.effective_instances = effective_instances;
    Ok(())
}

fn append_base_and_instance_relations(
    scenes: &BTreeMap<String, RawScene>,
    output: &mut Vec<SceneRelation>,
) -> Result<(), IndexerError> {
    for scene in scenes.values() {
        if let Some(base) = &scene.base_scene_id {
            output.push(relation(
                Some(&scene.entity.scene_entity_id),
                "base_scene",
                &scene.entity.scene_entity_id,
                Some(base),
                Some(&scene.entity.scene_entity_id),
                BTreeMap::new(),
                "scene_composer",
                scene.entity.resource_revision,
                scene.entity.scene_graph_revision,
            )?);
        }
        for (path, instance) in &scene.effective_instances {
            if instance.declaration_scene_id != scene.entity.scene_entity_id {
                continue;
            }
            output.push(relation(
                Some(&scene.entity.scene_entity_id),
                "instance",
                &instance.root_definition_id,
                Some(&instance.target_scene_id),
                Some(&instance.declaration_scene_id),
                BTreeMap::from([
                    ("node_path".to_owned(), json!(path)),
                    ("editable".to_owned(), json!(instance.editable)),
                ]),
                "godot_scene_state",
                scene.entity.resource_revision,
                scene.entity.scene_graph_revision,
            )?);
        }
    }
    Ok(())
}

fn append_subresources_and_resource_relations(
    scenes: &BTreeMap<String, RawScene>,
    resources_by_path: &BTreeMap<String, &ResourceEntity>,
    resources_by_uid: &BTreeMap<String, &ResourceEntity>,
    output: &mut Vec<SceneRelation>,
) -> Result<(), IndexerError> {
    for scene in scenes.values() {
        let mut unique_ids = BTreeSet::new();
        let mut subresource_ids = BTreeMap::new();
        let mut ordered = scene.observation.subresources.clone();
        ordered.sort_by(|left, right| {
            left.ownership_paths
                .cmp(&right.ownership_paths)
                .then_with(|| left.godot_type.cmp(&right.godot_type))
        });
        for (ordinal, subresource) in ordered.iter().enumerate() {
            let (entity_id, scope) = if let Some(unique_id) = subresource
                .scene_unique_id
                .as_ref()
                .filter(|unique_id| !unique_id.is_empty())
            {
                if !unique_ids.insert(unique_id.clone()) {
                    return Err(IndexerError::Scene("duplicate_subresource_id"));
                }
                (
                    prefixed_identity(
                        "godot:subresource:scene-id:v1:",
                        SUBRESOURCE_UNIQUE_DOMAIN,
                        &[&scene.entity.scene_entity_id, unique_id],
                    ),
                    SceneIdentityScope::Persistent,
                )
            } else {
                let mut parts = vec![
                    scene.entity.scene_entity_id.as_str(),
                    scene.entity.content_generation.as_str(),
                ];
                let mut paths = subresource.ownership_paths.clone();
                paths.sort();
                parts.extend(paths.iter().map(String::as_str));
                parts.push(&subresource.godot_type);
                let ordinal = ordinal.to_string();
                parts.push(&ordinal);
                (
                    prefixed_identity(
                        "godot:subresource:content-revision:v1:",
                        SUBRESOURCE_FALLBACK_DOMAIN,
                        &parts,
                    ),
                    SceneIdentityScope::ContentRevision,
                )
            };
            if let Some(unique_id) = &subresource.scene_unique_id {
                subresource_ids.insert(unique_id.clone(), entity_id.clone());
            }
            output.push(relation(
                Some(&scene.entity.scene_entity_id),
                "subresource",
                &entity_id,
                Some(&scene.entity.scene_entity_id),
                Some(&scene.entity.scene_entity_id),
                BTreeMap::from([
                    ("resource_type".to_owned(), json!(subresource.godot_type)),
                    ("identity_scope".to_owned(), enum_value(scope)?),
                    (
                        "scene_unique_id".to_owned(),
                        json!(subresource.scene_unique_id),
                    ),
                    (
                        "ownership_paths".to_owned(),
                        json!(subresource.ownership_paths),
                    ),
                ]),
                &scene.observation.authority,
                scene.entity.resource_revision,
                scene.entity.scene_graph_revision,
            )?);
        }
        for node in &scene.observation.nodes {
            let owner = scene
                .effective_definitions
                .get(&node.node_path)
                .cloned()
                .unwrap_or_else(|| node.node_path.clone());
            if let Some(script) = &node.attached_script
                && let Some(resource) =
                    resolve_resource_reference(script, resources_by_path, resources_by_uid)
            {
                output.push(relation(
                    Some(&scene.entity.scene_entity_id),
                    "attached_script",
                    &owner,
                    Some(&resource.entity_id),
                    Some(&scene.entity.scene_entity_id),
                    BTreeMap::new(),
                    &scene.observation.authority,
                    scene.entity.resource_revision,
                    scene.entity.scene_graph_revision,
                )?);
            }
            for property in &node.properties {
                let mut references = Vec::new();
                collect_projected_resources(&property.value.value, &mut references);
                for reference in references {
                    let target = reference
                        .get("scene_unique_id")
                        .and_then(Value::as_str)
                        .and_then(|unique_id| subresource_ids.get(unique_id))
                        .cloned()
                        .or_else(|| {
                            reference
                                .get("path")
                                .and_then(Value::as_str)
                                .and_then(|path| normalize_resource_path(path).ok())
                                .and_then(|path| resources_by_path.get(&path.comparison))
                                .map(|resource| resource.entity_id.clone())
                        });
                    if let Some(target) = target {
                        output.push(relation(
                            Some(&scene.entity.scene_entity_id),
                            "resource_reference",
                            &owner,
                            Some(&target),
                            Some(&scene.entity.scene_entity_id),
                            BTreeMap::from([("property".to_owned(), json!(property.name))]),
                            &scene.observation.authority,
                            scene.entity.resource_revision,
                            scene.entity.scene_graph_revision,
                        )?);
                    }
                }
            }
        }
    }
    Ok(())
}

fn append_project_context(
    context: &[ProjectContextObservation],
    resource_revision: u64,
    output: &mut Vec<SceneRelation>,
) -> Result<(), IndexerError> {
    for fact in context {
        output.push(relation(
            None,
            "project_context",
            &fact.key,
            None,
            None,
            BTreeMap::from([
                ("value_type".to_owned(), json!(fact.value.variant_type)),
                ("value".to_owned(), fact.value.value.clone()),
                ("truncated".to_owned(), json!(fact.value.truncated)),
            ]),
            &fact.authority,
            resource_revision,
            fact.scene_graph_revision,
        )?);
    }
    Ok(())
}

fn append_diagnostics(
    diagnostics: &[SceneDiagnostic],
    resource_revision: u64,
    scenes: &BTreeMap<String, RawScene>,
    output: &mut Vec<SceneRelation>,
) -> Result<(), IndexerError> {
    for diagnostic in diagnostics {
        let code = enum_value(&diagnostic.code)?;
        let code_name = code.as_str().unwrap_or_default();
        if code_name == "weak_scene_identity"
            && scenes.values().any(|scene| {
                scene.entity.comparison_path == diagnostic.subject
                    && scene.entity.identity_scope == SceneIdentityScope::Persistent
            })
        {
            continue;
        }
        if code_name == "weak_node_identity"
            && diagnostic
                .subject
                .split_once('#')
                .is_some_and(|(path, node_path)| {
                    scenes.values().any(|scene| {
                        scene.entity.comparison_path == path
                            && (scene
                                .effective_definitions
                                .get(node_path)
                                .is_some_and(|node_id| {
                                    node_id.starts_with("godot:node:scene-id:v1:")
                                })
                                || under_instance_path(node_path, &scene.effective_instances))
                    })
                })
        {
            continue;
        }
        output.push(relation(
            None,
            "diagnostic",
            &diagnostic.subject,
            None,
            None,
            BTreeMap::from([
                ("code".to_owned(), code),
                ("detail".to_owned(), json!(diagnostic.detail)),
            ]),
            "godot_scene_state",
            resource_revision,
            diagnostic.scene_graph_revision,
        )?);
    }
    Ok(())
}

fn compose_definition_properties(
    scene_id: &str,
    scenes: &BTreeMap<String, RawScene>,
    base: Option<&BTreeMap<(String, String), SceneProperty>>,
) -> Result<BTreeMap<(String, String), SceneProperty>, IndexerError> {
    let scene = &scenes[scene_id];
    let mut effective = BTreeMap::new();
    if let Some(base) = base {
        for ((_, name), property) in base {
            let mut inherited = property.clone();
            inherited.scene_entity_id = scene_id.to_owned();
            inherited.origin = ScenePropertyOrigin::Inherited;
            inherited.overridden_property_id = None;
            inherited.property_id = property_id(&inherited)?;
            effective.insert(
                (inherited.subject_entity_id.clone(), name.clone()),
                inherited,
            );
        }
    }
    for node in &scene.observation.nodes {
        let Some(subject) = scene.effective_definitions.get(&node.node_path) else {
            continue;
        };
        if under_instance_path(&node.node_path, &scene.effective_instances) {
            continue;
        }
        for property in &node.properties {
            let key = (subject.clone(), property.name.clone());
            let previous = effective
                .get(&key)
                .map(|property| property.property_id.clone());
            let mut fact = SceneProperty {
                property_id: String::new(),
                scene_entity_id: scene_id.to_owned(),
                subject_entity_id: subject.clone(),
                name: property.name.clone(),
                value_type: property.value.variant_type.clone(),
                value: property.value.value.clone(),
                truncated: property.value.truncated,
                declaring_scene_entity_id: scene_id.to_owned(),
                declaring_node_entity_id: subject.clone(),
                origin: ScenePropertyOrigin::Local,
                overridden_property_id: previous,
                authority: scene.observation.authority.clone(),
                resource_revision: scene.observation.resource_revision,
                scene_graph_revision: scene.observation.scene_graph_revision,
            };
            fact.property_id = property_id(&fact)?;
            effective.insert(key, fact);
        }
    }
    Ok(effective)
}

#[allow(clippy::too_many_arguments)]
fn expand_scene_occurrences(
    root_scene_id: &str,
    source_scene_id: &str,
    prefix: &str,
    chain: &[String],
    editable: bool,
    parent_internal: bool,
    hide_descendants: bool,
    scenes: &BTreeMap<String, RawScene>,
    stack: &mut Vec<String>,
    output: &mut Vec<Occurrence>,
    lookup: &mut BTreeMap<(String, String), Occurrence>,
) -> Result<(), IndexerError> {
    if stack.len() >= MAX_COMPOSITION_DEPTH || stack.iter().any(|scene| scene == source_scene_id) {
        return Err(IndexerError::Scene("scene_composition_cycle"));
    }
    stack.push(source_scene_id.to_owned());
    let scene = &scenes[source_scene_id];
    for (node_path, definition_id) in &scene.effective_definitions {
        let effective_path = join_node_path(prefix, node_path);
        if let Some(instance) = scene.effective_instances.get(node_path) {
            let instance_internal = parent_internal || (hide_descendants && node_path != ".");
            let mut nested_chain = chain.to_vec();
            nested_chain.push(instance.root_definition_id.clone());
            expand_scene_occurrences(
                root_scene_id,
                &instance.target_scene_id,
                &effective_path,
                &nested_chain,
                editable && instance.editable,
                instance_internal,
                !instance.editable,
                scenes,
                stack,
                output,
                lookup,
            )?;
            continue;
        }
        let internal = parent_internal || (hide_descendants && node_path != ".");
        let occurrence_id = node_occurrence_id(root_scene_id, chain, definition_id);
        let occurrence = Occurrence {
            occurrence_id,
            root_scene_id: root_scene_id.to_owned(),
            source_scene_id: source_scene_id.to_owned(),
            definition_id: definition_id.clone(),
            effective_path: effective_path.clone(),
            instance_chain: chain.to_vec(),
            editable,
            internal,
            resource_revision: scenes[root_scene_id].entity.resource_revision,
            scene_graph_revision: scenes[root_scene_id].entity.scene_graph_revision,
        };
        if lookup
            .insert(
                (root_scene_id.to_owned(), effective_path),
                occurrence.clone(),
            )
            .is_some()
        {
            return Err(IndexerError::Scene("duplicate_node_occurrence_path"));
        }
        output.push(occurrence);
    }
    stack.pop();
    Ok(())
}

fn append_occurrence_relations(
    occurrences: &[Occurrence],
    output: &mut Vec<SceneRelation>,
) -> Result<(), IndexerError> {
    for occurrence in occurrences {
        output.push(relation_with_id(
            &occurrence.occurrence_id,
            Some(&occurrence.root_scene_id),
            "occurrence",
            &occurrence.occurrence_id,
            Some(&occurrence.definition_id),
            Some(&occurrence.source_scene_id),
            BTreeMap::from([
                (
                    "effective_path".to_owned(),
                    json!(occurrence.effective_path),
                ),
                (
                    "instance_chain".to_owned(),
                    json!(occurrence.instance_chain),
                ),
                ("editable".to_owned(), json!(occurrence.editable)),
                ("internal".to_owned(), json!(occurrence.internal)),
            ]),
            "scene_composer",
            occurrence.resource_revision,
            occurrence.scene_graph_revision,
        ));
    }
    Ok(())
}

fn append_occurrence_properties(
    scenes: &BTreeMap<String, RawScene>,
    occurrences: &[Occurrence],
    occurrence_lookup: &BTreeMap<(String, String), Occurrence>,
    properties_by_scene: &BTreeMap<String, BTreeMap<(String, String), SceneProperty>>,
    output: &mut Vec<SceneProperty>,
) -> Result<(), IndexerError> {
    let mut occurrence_properties = BTreeMap::new();
    for occurrence in occurrences
        .iter()
        .filter(|occurrence| !occurrence.instance_chain.is_empty())
    {
        if let Some(properties) = properties_by_scene.get(&occurrence.source_scene_id) {
            for ((subject, name), property) in properties {
                if subject != &occurrence.definition_id {
                    continue;
                }
                let mut inherited = property.clone();
                inherited.scene_entity_id = occurrence.root_scene_id.clone();
                inherited.subject_entity_id = occurrence.occurrence_id.clone();
                inherited.origin = ScenePropertyOrigin::Inherited;
                inherited.overridden_property_id = Some(property.property_id.clone());
                inherited.property_id = property_id(&inherited)?;
                occurrence_properties.insert(
                    (
                        occurrence.root_scene_id.clone(),
                        occurrence.occurrence_id.clone(),
                        name.clone(),
                    ),
                    inherited,
                );
            }
        }
    }
    for (scene_id, scene) in scenes {
        for node in &scene.observation.nodes {
            if !under_instance_path(&node.node_path, &scene.effective_instances) {
                continue;
            }
            let Some(occurrence) =
                occurrence_lookup.get(&(scene_id.clone(), node.node_path.clone()))
            else {
                return Err(IndexerError::Scene("broken_override_target"));
            };
            let declaring_node = nearest_instance(&node.node_path, &scene.effective_instances)
                .map(|instance| instance.root_definition_id.clone())
                .ok_or(IndexerError::Scene("broken_override_target"))?;
            for property in &node.properties {
                let key = (
                    scene_id.clone(),
                    occurrence.occurrence_id.clone(),
                    property.name.clone(),
                );
                let previous = occurrence_properties
                    .get(&key)
                    .map(|property| property.property_id.clone());
                let mut fact = SceneProperty {
                    property_id: String::new(),
                    scene_entity_id: scene_id.clone(),
                    subject_entity_id: occurrence.occurrence_id.clone(),
                    name: property.name.clone(),
                    value_type: property.value.variant_type.clone(),
                    value: property.value.value.clone(),
                    truncated: property.value.truncated,
                    declaring_scene_entity_id: scene_id.clone(),
                    declaring_node_entity_id: declaring_node.clone(),
                    origin: ScenePropertyOrigin::InstanceOverride,
                    overridden_property_id: previous,
                    authority: scene.observation.authority.clone(),
                    resource_revision: scene.observation.resource_revision,
                    scene_graph_revision: scene.observation.scene_graph_revision,
                };
                fact.property_id = property_id(&fact)?;
                occurrence_properties.insert(key, fact);
            }
        }
    }
    output.extend(occurrence_properties.into_values());
    Ok(())
}

fn compose_groups(
    order: &[String],
    scenes: &BTreeMap<String, RawScene>,
    occurrences: &[Occurrence],
) -> Result<Vec<SceneGroupMembership>, IndexerError> {
    let mut per_scene: BTreeMap<String, BTreeMap<(String, String), SceneGroupMembership>> =
        BTreeMap::new();
    let mut output = Vec::new();
    for scene_id in order {
        let scene = &scenes[scene_id];
        let mut effective = scene
            .base_scene_id
            .as_ref()
            .and_then(|base| per_scene.get(base))
            .cloned()
            .unwrap_or_default();
        for group in effective.values_mut() {
            group.scene_entity_id = scene_id.clone();
            group.membership_id = group_id(group);
        }
        for node in &scene.observation.nodes {
            let Some(member) = scene.effective_definitions.get(&node.node_path) else {
                continue;
            };
            if under_instance_path(&node.node_path, &scene.effective_instances) {
                continue;
            }
            for name in &node.groups {
                let mut group = SceneGroupMembership {
                    membership_id: String::new(),
                    scene_entity_id: scene_id.clone(),
                    member_node_entity_id: member.clone(),
                    group: name.clone(),
                    declaration_scope: scene_id.clone(),
                    authority: scene.observation.authority.clone(),
                    resource_revision: scene.observation.resource_revision,
                    scene_graph_revision: scene.observation.scene_graph_revision,
                };
                group.membership_id = group_id(&group);
                effective.insert((member.clone(), name.clone()), group);
            }
        }
        output.extend(effective.values().cloned());
        per_scene.insert(scene_id.clone(), effective);
    }
    for occurrence in occurrences
        .iter()
        .filter(|occurrence| !occurrence.instance_chain.is_empty())
    {
        if let Some(groups) = per_scene.get(&occurrence.source_scene_id) {
            for ((member, _), group) in groups {
                if member != &occurrence.definition_id {
                    continue;
                }
                let mut composed = group.clone();
                composed.scene_entity_id = occurrence.root_scene_id.clone();
                composed.member_node_entity_id = occurrence.occurrence_id.clone();
                composed.membership_id = group_id(&composed);
                output.push(composed);
            }
        }
    }
    Ok(output)
}

fn compose_connections(
    order: &[String],
    scenes: &BTreeMap<String, RawScene>,
    occurrences: &[Occurrence],
) -> Result<Vec<SceneConnection>, IndexerError> {
    let mut per_scene: BTreeMap<String, Vec<SceneConnection>> = BTreeMap::new();
    let mut output = Vec::new();
    for scene_id in order {
        let scene = &scenes[scene_id];
        let mut effective = scene
            .base_scene_id
            .as_ref()
            .and_then(|base| per_scene.get(base))
            .cloned()
            .unwrap_or_default();
        for connection in &mut effective {
            connection.scene_entity_id = scene_id.clone();
            connection.connection_id = connection_id(connection)?;
        }
        for observed in &scene.observation.connections {
            let emitter = scene
                .effective_definitions
                .get(&observed.emitter)
                .ok_or(IndexerError::Scene("connection_emitter_missing"))?;
            let receiver = scene.effective_definitions.get(&observed.receiver).cloned();
            let mut connection = SceneConnection {
                connection_id: String::new(),
                scene_entity_id: scene_id.clone(),
                emitter_node_entity_id: emitter.clone(),
                signal: observed.signal.clone(),
                receiver_node_entity_id: receiver,
                method: observed.method.clone(),
                flags: observed.flags,
                unbinds: observed.unbinds,
                binds: observed
                    .binds
                    .iter()
                    .map(|bind| bind.value.clone())
                    .collect(),
                declaration_scope: scene_id.clone(),
                authority: scene.observation.authority.clone(),
                resource_revision: scene.observation.resource_revision,
                scene_graph_revision: scene.observation.scene_graph_revision,
            };
            connection.connection_id = connection_id(&connection)?;
            effective.push(connection);
        }
        output.extend(effective.clone());
        per_scene.insert(scene_id.clone(), effective);
    }
    for occurrence in occurrences
        .iter()
        .filter(|occurrence| !occurrence.instance_chain.is_empty())
    {
        if let Some(connections) = per_scene.get(&occurrence.source_scene_id) {
            for connection in connections {
                if connection.emitter_node_entity_id != occurrence.definition_id {
                    continue;
                }
                let mut composed = connection.clone();
                composed.scene_entity_id = occurrence.root_scene_id.clone();
                composed.emitter_node_entity_id = occurrence.occurrence_id.clone();
                composed.receiver_node_entity_id = connection
                    .receiver_node_entity_id
                    .as_ref()
                    .and_then(|receiver| {
                        occurrences.iter().find(|candidate| {
                            candidate.root_scene_id == occurrence.root_scene_id
                                && candidate.instance_chain == occurrence.instance_chain
                                && candidate.definition_id == *receiver
                        })
                    })
                    .map(|receiver| receiver.occurrence_id.clone());
                composed.connection_id = connection_id(&composed)?;
                output.push(composed);
            }
        }
    }
    Ok(output)
}

fn compose_animations(
    scenes: &BTreeMap<String, RawScene>,
    occurrence_lookup: &BTreeMap<(String, String), Occurrence>,
) -> Result<Vec<SceneAnimationReference>, IndexerError> {
    let mut output = Vec::new();
    for (scene_id, scene) in scenes {
        for track in &scene.observation.animation_tracks {
            let mixer = occurrence_lookup
                .get(&(scene_id.clone(), track.mixer.clone()))
                .map(|occurrence| occurrence.occurrence_id.clone())
                .or_else(|| scene.effective_definitions.get(&track.mixer).cloned())
                .ok_or(IndexerError::Scene("animation_mixer_missing"))?;
            let node_path = track.node_path.split(':').next().unwrap_or_default();
            let target = occurrence_lookup
                .get(&(scene_id.clone(), node_path.to_owned()))
                .map(|occurrence| occurrence.occurrence_id.clone())
                .or_else(|| scene.effective_definitions.get(node_path).cloned());
            let resolution = if target.is_some() {
                SceneAnimationResolution::Resolved
            } else if !node_path.is_empty() {
                SceneAnimationResolution::Broken
            } else {
                match track.resolution {
                    BridgeAnimationResolution::Resolved | BridgeAnimationResolution::Broken => {
                        SceneAnimationResolution::Broken
                    }
                    BridgeAnimationResolution::Unresolved => SceneAnimationResolution::Unresolved,
                }
            };
            let mut record = SceneAnimationReference {
                animation_reference_id: String::new(),
                scene_entity_id: scene_id.clone(),
                mixer_node_entity_id: mixer,
                library: track.library.clone(),
                animation: track.animation.clone(),
                track_index: track.track_index,
                node_path: track.node_path.clone(),
                target_node_entity_id: target,
                resolution,
                authority: scene.observation.authority.clone(),
                resource_revision: scene.observation.resource_revision,
                scene_graph_revision: scene.observation.scene_graph_revision,
            };
            record.animation_reference_id = semantic_id(
                "animation",
                ANIMATION_DOMAIN,
                &[
                    scene_id,
                    &record.mixer_node_entity_id,
                    &record.library,
                    &record.animation,
                    &record.track_index.to_string(),
                ],
            );
            output.push(record);
        }
    }
    Ok(output)
}

fn under_instance_path(path: &str, instances: &BTreeMap<String, InstanceDeclaration>) -> bool {
    instances.keys().any(|prefix| {
        path != prefix
            && path
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

fn nearest_instance<'a>(
    path: &str,
    instances: &'a BTreeMap<String, InstanceDeclaration>,
) -> Option<&'a InstanceDeclaration> {
    instances
        .iter()
        .filter(|(prefix, _)| {
            path.strip_prefix(prefix.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
        })
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, instance)| instance)
}

fn join_node_path(prefix: &str, path: &str) -> String {
    match (prefix, path) {
        (".", ".") => ".".to_owned(),
        (_, ".") => prefix.to_owned(),
        (".", _) => path.to_owned(),
        _ => format!("{prefix}/{path}"),
    }
}

fn resolve_resource_reference<'a>(
    reference: &ResourceRef,
    resources_by_path: &'a BTreeMap<String, &ResourceEntity>,
    resources_by_uid: &'a BTreeMap<String, &ResourceEntity>,
) -> Option<&'a ResourceEntity> {
    match reference {
        ResourceRef::Uid(reference) => resources_by_uid.get(&reference.uid).copied(),
        ResourceRef::Path(reference) => normalize_resource_path(&reference.path)
            .ok()
            .and_then(|path| resources_by_path.get(&path.comparison).copied()),
    }
}

fn collect_projected_resources<'a>(
    value: &'a Value,
    output: &mut Vec<&'a serde_json::Map<String, Value>>,
) {
    match value {
        Value::Object(object) => {
            if object.get("godot_type").is_some() && object.get("path").is_some() {
                output.push(object);
            }
            for value in object.values() {
                collect_projected_resources(value, output);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_projected_resources(value, output);
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn relation(
    scene_entity_id: Option<&str>,
    kind: &str,
    source: &str,
    target: Option<&str>,
    declaration_scope: Option<&str>,
    attributes: BTreeMap<String, Value>,
    authority: &str,
    resource_revision: u64,
    scene_graph_revision: u64,
) -> Result<SceneRelation, IndexerError> {
    let attributes_json =
        serde_json::to_string(&attributes).map_err(|_| IndexerError::Serialization)?;
    let id = semantic_id(
        "relation",
        RELATION_DOMAIN,
        &[
            scene_entity_id.unwrap_or_default(),
            kind,
            source,
            target.unwrap_or_default(),
            declaration_scope.unwrap_or_default(),
            &attributes_json,
        ],
    );
    Ok(relation_with_id(
        &id,
        scene_entity_id,
        kind,
        source,
        target,
        declaration_scope,
        attributes,
        authority,
        resource_revision,
        scene_graph_revision,
    ))
}

#[allow(clippy::too_many_arguments)]
fn relation_with_id(
    relation_id: &str,
    scene_entity_id: Option<&str>,
    kind: &str,
    source: &str,
    target: Option<&str>,
    declaration_scope: Option<&str>,
    attributes: BTreeMap<String, Value>,
    authority: &str,
    resource_revision: u64,
    scene_graph_revision: u64,
) -> SceneRelation {
    SceneRelation {
        relation_id: relation_id.to_owned(),
        scene_entity_id: scene_entity_id.map(str::to_owned),
        relation: kind.to_owned(),
        source: source.to_owned(),
        target: target.map(str::to_owned),
        declaration_scope: declaration_scope.map(str::to_owned),
        attributes,
        authority: authority.to_owned(),
        resource_revision,
        scene_graph_revision,
    }
}

fn property_id(property: &SceneProperty) -> Result<String, IndexerError> {
    let value = serde_json::to_string(&property.value).map_err(|_| IndexerError::Serialization)?;
    Ok(semantic_id(
        "property",
        PROPERTY_DOMAIN,
        &[
            &property.scene_entity_id,
            &property.subject_entity_id,
            &property.name,
            &property.declaring_scene_entity_id,
            &value,
        ],
    ))
}

fn group_id(group: &SceneGroupMembership) -> String {
    semantic_id(
        "group",
        GROUP_DOMAIN,
        &[
            &group.scene_entity_id,
            &group.member_node_entity_id,
            &group.group,
            &group.declaration_scope,
        ],
    )
}

fn connection_id(connection: &SceneConnection) -> Result<String, IndexerError> {
    let binds =
        serde_json::to_string(&connection.binds).map_err(|_| IndexerError::Serialization)?;
    Ok(semantic_id(
        "connection",
        CONNECTION_DOMAIN,
        &[
            &connection.scene_entity_id,
            &connection.emitter_node_entity_id,
            &connection.signal,
            connection
                .receiver_node_entity_id
                .as_deref()
                .unwrap_or_default(),
            &connection.method,
            &binds,
        ],
    ))
}

fn node_occurrence_id(root_scene_id: &str, chain: &[String], definition_id: &str) -> String {
    let mut parts = Vec::with_capacity(chain.len() + 2);
    parts.push(root_scene_id);
    parts.extend(chain.iter().map(String::as_str));
    parts.push(definition_id);
    prefixed_identity("godot:node-occurrence:v1:", NODE_OCCURRENCE_DOMAIN, &parts)
}

fn semantic_id(prefix: &str, domain: &str, parts: &[&str]) -> String {
    format!("{prefix}:{}", identity_digest(domain, parts))
}

fn prefixed_identity(prefix: &str, domain: &str, parts: &[&str]) -> String {
    format!("{prefix}{}", identity_digest(domain, parts))
}

fn identity_digest(domain: &str, parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            hasher.update([0]);
        }
        hasher.update(part.as_bytes());
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
}

fn enum_value<T: Serialize>(value: T) -> Result<Value, IndexerError> {
    serde_json::to_value(value).map_err(|_| IndexerError::Serialization)
}

#[cfg(test)]
mod tests {
    use super::*;
    use godot_codex_index_store::{
        GenerationState, IdentityStrength, IngestionCheckpoint, LOGICAL_SCHEMA_V1, RecordValidity,
    };

    #[test]
    fn d06_identity_algorithms_match_frozen_vectors() {
        let base_scene =
            prefixed_identity("godot:scene:uid:v1:", SCENE_UID_DOMAIN, &["uid://s4base"]);
        assert_eq!(
            base_scene,
            "godot:scene:uid:v1:rQv-XVepZaIDKeQgWGddBRHDcjumUfCuUs_P0MtSnuo"
        );
        let player = prefixed_identity(
            "godot:node:scene-id:v1:",
            NODE_UNIQUE_DOMAIN,
            &[&base_scene, "1002"],
        );
        assert_eq!(
            player,
            "godot:node:scene-id:v1:wJGT0C9v3tbpP4ApyGQmzFxFzroRh1DSrdIBfPUmvoY"
        );
        let main_scene =
            prefixed_identity("godot:scene:uid:v1:", SCENE_UID_DOMAIN, &["uid://s4main"]);
        let instance = prefixed_identity(
            "godot:node:scene-id:v1:",
            NODE_UNIQUE_DOMAIN,
            &[&main_scene, "2001"],
        );
        assert_eq!(
            node_occurrence_id(&main_scene, &[instance], &player),
            "godot:node-occurrence:v1:C5-xhT14pMXTeFhop47Qatmarkb4jferldZ9qeZ4kbs"
        );
        assert_eq!(
            prefixed_identity(
                "godot:subresource:scene-id:v1:",
                SUBRESOURCE_UNIQUE_DOMAIN,
                &[&base_scene, "Gradient_a1b2c3"],
            ),
            "godot:subresource:scene-id:v1:z7fkkBOJ8N70PRrgPznosP7_j3sdWnGsjIbNNF5ZoiQ"
        );
    }

    #[test]
    fn composes_inheritance_instances_overrides_and_order_deterministically() {
        let resource = |entity_id: &str, uid: &str, path: &str| ResourceEntity {
            entity_id: entity_id.to_owned(),
            identity_input: uid.to_owned(),
            uid: Some(uid.to_owned()),
            display_path: path.to_owned(),
            comparison_path: path.to_owned(),
            identity_strength: IdentityStrength::ResourceUid,
            resource_type: "PackedScene".to_owned(),
            source_kind: "source".to_owned(),
            import_state: "ready".to_owned(),
            authority: "editor_file_system".to_owned(),
            content_generation: Some(format!("sha256:{entity_id}")),
            mtime_ns: 1,
            byte_size: 1,
            validity: RecordValidity::Valid,
            resource_revision: 1,
        };
        let resources = vec![
            resource("resource-base", "uid://s4base", "res://base.tscn"),
            resource("resource-derived", "uid://s4derived", "res://derived.tscn"),
            resource("resource-main", "uid://s4main", "res://main.tscn"),
        ];
        let resource_generation = IndexGeneration {
            generation_id: "resource-generation".to_owned(),
            parent_generation_id: None,
            schema_version: LOGICAL_SCHEMA_V1,
            project_id: "project:test".to_owned(),
            index_revision: 1,
            state: GenerationState::Active,
            creation_reason: "full_snapshot".to_owned(),
            checkpoint: IngestionCheckpoint {
                editor_session_id: "editor:test".to_owned(),
                resource_revision: 1,
                project_revision: 1,
                index_revision: 1,
                source_complete: true,
                snapshot_checksum: "sha256:resource".to_owned(),
                last_batch_id: None,
                last_batch_checksum: None,
            },
            resources,
            source_documents: Vec::new(),
            dependencies: Vec::new(),
            diagnostics: Vec::new(),
            tombstones: Vec::new(),
            scene: SceneDomainGeneration::default(),
            validation_digest: String::new(),
        };
        let node = |path: &str, name: &str, unique_id: Option<u32>, properties: Value| {
            json!({
                "node_path": path,
                "parent_path": if path == "." { Value::Null } else { json!(path.rsplit_once('/').map_or(".", |(parent, _)| parent)) },
                "owner_path": if path == "." { Value::Null } else { json!(".") },
                "name": name,
                "godot_type": if unique_id.is_some() { "Node2D" } else { "" },
                "node_index": -1,
                "unique_scene_id": unique_id,
                "identity_scope": if unique_id.is_some() { "persistent" } else { "content_revision" },
                "owned": unique_id.is_some(),
                "internal": unique_id.is_none(),
                "instance": Value::Null,
                "instance_placeholder": Value::Null,
                "groups": [],
                "properties": properties,
                "attached_script": Value::Null
            })
        };
        let property = |name: &str, value: i64| {
            json!({
                "name": name,
                "value": {"type": "int", "value": value, "truncated": false},
                "deferred_node_path": false
            })
        };
        let mut base_root = node(".", "Base", Some(1001), json!([]));
        base_root["groups"] = json!(["actors"]);
        let base_player = node(
            "Player",
            "Player",
            Some(1002),
            json!([property("speed", 10), property("target", 7)]),
        );
        let derived_root = node(".", "Derived", None, json!([]));
        let derived_player = node("Player", "Player", None, json!([property("speed", 20)]));
        let main_root = node(".", "Main", Some(5001), json!([]));
        let mut actor = node("Actor", "Actor", Some(5002), json!([]));
        actor["godot_type"] = json!("");
        actor["instance"] = json!({"uid": "uid://s4derived"});
        let actor_player = node(
            "Actor/Player",
            "Player",
            None,
            json!([property("speed", 30)]),
        );
        let scene = |uid: &str, path: &str, base: Value, nodes: Vec<Value>, editable: Vec<&str>| {
            json!({
                "scene_ref": {"uid": uid},
                "path": path,
                "content_generation": format!("sha256:{uid}"),
                "identity_scope": "persistent",
                "base_scene_ref": base,
                "nodes": nodes,
                "connections": [],
                "editable_instances": editable,
                "subresources": [],
                "animation_tracks": [],
                "authority": "packed_scene_state",
                "resource_revision": 1,
                "scene_graph_revision": 1,
                "source_complete": true
            })
        };
        let observations: Vec<SceneObservation> = serde_json::from_value(json!([
            scene(
                "uid://s4main",
                "res://main.tscn",
                Value::Null,
                vec![main_root, actor, actor_player],
                vec!["Actor"]
            ),
            scene(
                "uid://s4base",
                "res://base.tscn",
                Value::Null,
                vec![base_root, base_player],
                vec![]
            ),
            scene(
                "uid://s4derived",
                "res://derived.tscn",
                json!({"uid": "uid://s4base"}),
                vec![derived_root, derived_player],
                vec![]
            )
        ]))
        .expect("observations");
        let diagnostics: Vec<SceneDiagnostic> = serde_json::from_value(json!([
            {"code":"weak_scene_identity","subject":"res://main.tscn","detail":null,"scene_graph_revision":1},
            {"code":"weak_node_identity","subject":"res://main.tscn#Actor/Player","detail":null,"scene_graph_revision":1}
        ]))
        .expect("diagnostics");
        let normalizer = SceneNormalizer;
        let domain = normalizer
            .normalize_observations(
                &resource_generation,
                "editor:test",
                1,
                1,
                "sha256:scene",
                &observations,
                &[],
                &diagnostics,
            )
            .expect("normalized scene domain");
        let main_scene =
            prefixed_identity("godot:scene:uid:v1:", SCENE_UID_DOMAIN, &["uid://s4main"]);
        let base_scene =
            prefixed_identity("godot:scene:uid:v1:", SCENE_UID_DOMAIN, &["uid://s4base"]);
        let base_player = prefixed_identity(
            "godot:node:scene-id:v1:",
            NODE_UNIQUE_DOMAIN,
            &[&base_scene, "1002"],
        );
        let actor = prefixed_identity(
            "godot:node:scene-id:v1:",
            NODE_UNIQUE_DOMAIN,
            &[&main_scene, "5002"],
        );
        let player_occurrence = node_occurrence_id(&main_scene, &[actor], &base_player);
        assert!(domain.scenes.iter().all(|scene| {
            scene.identity_scope == SceneIdentityScope::Persistent && scene.uid.is_some()
        }));
        assert!(domain.relations.iter().any(|relation| {
            relation.relation == "occurrence"
                && relation.source == player_occurrence
                && relation.attributes["effective_path"] == "Actor/Player"
        }));
        let override_fact = domain
            .properties
            .iter()
            .find(|property| {
                property.scene_entity_id == main_scene
                    && property.subject_entity_id == player_occurrence
                    && property.name == "speed"
            })
            .expect("instance override");
        assert_eq!(override_fact.origin, ScenePropertyOrigin::InstanceOverride);
        assert_eq!(override_fact.value, json!(30));
        assert!(override_fact.overridden_property_id.is_some());
        assert!(!domain.relations.iter().any(|relation| {
            relation.relation == "diagnostic"
                && matches!(
                    relation.attributes.get("code").and_then(Value::as_str),
                    Some("weak_scene_identity" | "weak_node_identity")
                )
        }));

        let mut reordered = observations;
        reordered.reverse();
        let reordered_domain = normalizer
            .normalize_observations(
                &resource_generation,
                "editor:test",
                1,
                1,
                "sha256:scene",
                &reordered,
                &[],
                &diagnostics,
            )
            .expect("reordered domain");
        assert_eq!(domain.validation_digest, reordered_domain.validation_digest);
    }
}
