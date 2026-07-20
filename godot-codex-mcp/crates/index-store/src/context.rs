//! Deterministic token-bounded projections for model-facing MCP resources.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::{IndexGeneration, SemanticQueryIndex};

pub const PROJECT_SUMMARY_TOKEN_BUDGET: usize = 4_096;
pub const SCENE_SUMMARY_TOKEN_BUDGET: usize = 2_048;
pub const SUMMARY_BUDGET_METHOD: &str = "utf8_byte_upper_bound_v1";

/// Closed failures for a requested model-facing summary.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ContextSummaryError {
    #[error("scene_not_found")]
    SceneNotFound,
    #[error("summary_budget_too_small")]
    BudgetTooSmall,
    #[error("summary_serialization_failed")]
    Serialization,
}

/// Builds the fixed project summary resource within its conservative budget.
pub fn build_project_summary(
    generation: &IndexGeneration,
    index: &SemanticQueryIndex,
) -> Result<String, ContextSummaryError> {
    let revisions = index.revisions();
    let mut document = BTreeMap::from([
        ("kind".to_owned(), json!("godot_project_summary")),
        ("schema_version".to_owned(), json!(1)),
        ("project_id".to_owned(), json!(generation.project_id)),
        ("generation_id".to_owned(), json!(index.generation_id())),
        (
            "revisions".to_owned(),
            json!({
                "index_revision": revisions.index_revision,
                "resource_revision": revisions.resource_revision,
                "scene_graph_revision": revisions.scene_graph_revision,
                "script_graph_revision": revisions.script_graph_revision,
            }),
        ),
        (
            "coverage".to_owned(),
            json!({
                "resource": true,
                "scene": revisions.scene_graph_revision.is_some(),
                "script": revisions.script_graph_revision.is_some(),
            }),
        ),
        (
            "counts".to_owned(),
            json!({
                "resources": generation.resources.len(),
                "scenes": revisions.scene_graph_revision.map_or(0, |_| generation.scene.scenes.len()),
                "nodes": revisions.scene_graph_revision.map_or(0, |_| generation.scene.nodes.len()),
                "scripts": revisions.script_graph_revision.map_or(0, |_| generation.script.documents.len()),
                "symbols": revisions.script_graph_revision.map_or(0, |_| generation.script.symbols.len()),
                "semantic_facts": index.facts().len(),
                "conflicts": index.conflicts().len(),
            }),
        ),
        (
            "budget".to_owned(),
            json!({
                "method": SUMMARY_BUDGET_METHOD,
                "token_limit": PROJECT_SUMMARY_TOKEN_BUDGET,
            }),
        ),
    ]);

    let mut resources = generation
        .resources
        .iter()
        .map(|resource| {
            json!({
                "entity_id": resource.entity_id,
                "path": resource.comparison_path,
                "type": resource.resource_type,
            })
        })
        .collect::<Vec<_>>();
    resources.sort_by_key(canonical_value);
    let mut scenes = if revisions.scene_graph_revision.is_some() {
        generation
            .scene
            .scenes
            .iter()
            .map(|scene| {
                json!({
                    "entity_id": scene.scene_entity_id,
                    "path": scene.comparison_path,
                    "base_scene_entity_id": scene.base_scene_entity_id,
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    scenes.sort_by_key(canonical_value);
    let mut scripts = if revisions.script_graph_revision.is_some() {
        generation
            .script
            .documents
            .iter()
            .map(|script| {
                json!({
                    "entity_id": script.script_resource_id,
                    "path": script.path,
                    "language": script.language,
                    "completeness": script.completeness,
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    scripts.sort_by_key(canonical_value);
    let diagnostics = generation
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.active)
        .map(|diagnostic| {
            json!({
                "diagnostic_id": diagnostic.diagnostic_id,
                "code": diagnostic.code,
                "subject": diagnostic.subject,
            })
        })
        .collect::<Vec<_>>();
    let high_degree_relations = high_degree_relations(index);
    let evidence_ids = index
        .facts()
        .iter()
        .flat_map(|fact| fact.evidence.iter())
        .map(|evidence| json!(evidence.evidence_id))
        .collect::<Vec<_>>();

    bounded_document(
        &mut document,
        vec![
            ("important_scenes", scenes),
            ("important_resources", resources),
            ("important_scripts", scripts),
            ("diagnostics", diagnostics),
            ("high_degree_relations", high_degree_relations),
            ("evidence_ids", evidence_ids),
        ],
        PROJECT_SUMMARY_TOKEN_BUDGET,
    )
}

/// Builds one canonical scene summary resource within its conservative budget.
pub fn build_scene_summary(
    generation: &IndexGeneration,
    index: &SemanticQueryIndex,
    scene_entity_id: &str,
) -> Result<String, ContextSummaryError> {
    let scene = generation
        .scene
        .scenes
        .iter()
        .find(|scene| scene.scene_entity_id == scene_entity_id)
        .ok_or(ContextSummaryError::SceneNotFound)?;
    let revisions = index.revisions();
    if revisions.scene_graph_revision.is_none() {
        return Err(ContextSummaryError::SceneNotFound);
    }
    let scene_facts = index
        .facts()
        .iter()
        .filter(|fact| index.scene_for_entity(&fact.source_entity_id) == Some(scene_entity_id))
        .collect::<Vec<_>>();
    let mut document = BTreeMap::from([
        ("kind".to_owned(), json!("godot_scene_summary")),
        ("schema_version".to_owned(), json!(1)),
        ("project_id".to_owned(), json!(generation.project_id)),
        ("generation_id".to_owned(), json!(index.generation_id())),
        ("scene_entity_id".to_owned(), json!(scene.scene_entity_id)),
        ("path".to_owned(), json!(scene.comparison_path)),
        ("uid".to_owned(), json!(scene.uid)),
        (
            "base_scene_entity_id".to_owned(),
            json!(scene.base_scene_entity_id),
        ),
        (
            "revisions".to_owned(),
            json!({
                "index_revision": revisions.index_revision,
                "resource_revision": revisions.resource_revision,
                "scene_graph_revision": revisions.scene_graph_revision,
                "script_graph_revision": revisions.script_graph_revision,
            }),
        ),
        (
            "counts".to_owned(),
            json!({
                "nodes": generation.scene.nodes.iter().filter(|node| node.scene_entity_id == scene_entity_id).count(),
                "connections": generation.scene.connections.iter().filter(|item| item.scene_entity_id == scene_entity_id).count(),
                "groups": generation.scene.groups.iter().filter(|item| item.scene_entity_id == scene_entity_id).count(),
                "semantic_facts": scene_facts.len(),
            }),
        ),
        (
            "budget".to_owned(),
            json!({
                "method": SUMMARY_BUDGET_METHOD,
                "token_limit": SCENE_SUMMARY_TOKEN_BUDGET,
            }),
        ),
    ]);

    let mut nodes = generation
        .scene
        .nodes
        .iter()
        .filter(|node| node.scene_entity_id == scene_entity_id)
        .map(|node| {
            json!({
                "entity_id": node.node_entity_id,
                "node_path": node.node_path,
                "name": node.name,
                "type": node.godot_type,
            })
        })
        .collect::<Vec<_>>();
    nodes.sort_by_key(canonical_value);
    let mut instances = generation
        .scene
        .nodes
        .iter()
        .filter(|node| node.scene_entity_id == scene_entity_id)
        .filter_map(|node| {
            node.instance_scene_entity_id.as_ref().map(|target| {
                json!({
                    "node_entity_id": node.node_entity_id,
                    "node_path": node.node_path,
                    "scene_entity_id": target,
                })
            })
        })
        .collect::<Vec<_>>();
    instances.sort_by_key(canonical_value);
    let mut attached_scripts = generation
        .scene
        .nodes
        .iter()
        .filter(|node| node.scene_entity_id == scene_entity_id)
        .filter_map(|node| {
            node.attached_script_entity_id.as_ref().map(|script| {
                json!({
                    "node_entity_id": node.node_entity_id,
                    "node_path": node.node_path,
                    "script_entity_id": script,
                })
            })
        })
        .collect::<Vec<_>>();
    attached_scripts.sort_by_key(canonical_value);
    let signals = generation
        .scene
        .connections
        .iter()
        .filter(|connection| connection.scene_entity_id == scene_entity_id)
        .map(|connection| {
            json!({
                "emitter_node_entity_id": connection.emitter_node_entity_id,
                "signal": connection.signal,
                "receiver_node_entity_id": connection.receiver_node_entity_id,
                "method": connection.method,
            })
        })
        .collect::<Vec<_>>();
    let groups = generation
        .scene
        .groups
        .iter()
        .filter(|group| group.scene_entity_id == scene_entity_id)
        .map(|group| {
            json!({
                "member_node_entity_id": group.member_node_entity_id,
                "group": group.group,
            })
        })
        .collect::<Vec<_>>();
    let dependencies = scene_facts
        .iter()
        .map(|fact| {
            json!({
                "fact_id": fact.fact_id,
                "source_entity_id": fact.source_entity_id,
                "predicate": fact.predicate,
                "target_entity_id": fact.target_entity_id,
                "confidence": fact.confidence,
            })
        })
        .collect::<Vec<_>>();
    let diagnostics = generation
        .scene
        .relations
        .iter()
        .filter(|relation| {
            relation.relation == "diagnostic"
                && relation.scene_entity_id.as_deref() == Some(scene_entity_id)
        })
        .map(|relation| {
            json!({
                "diagnostic_id": relation.relation_id,
                "subject_entity_id": relation.source,
                "code": relation.attributes.get("code"),
            })
        })
        .collect::<Vec<_>>();
    let evidence_ids = scene_facts
        .iter()
        .flat_map(|fact| fact.evidence.iter())
        .map(|evidence| json!(evidence.evidence_id))
        .collect::<Vec<_>>();

    bounded_document(
        &mut document,
        vec![
            ("root_structure", nodes),
            ("instances", instances),
            ("attached_scripts", attached_scripts),
            ("signals", signals),
            ("groups", groups),
            ("dependencies", dependencies),
            ("diagnostics", diagnostics),
            ("evidence_ids", evidence_ids),
        ],
        SCENE_SUMMARY_TOKEN_BUDGET,
    )
}

fn high_degree_relations(index: &SemanticQueryIndex) -> Vec<Value> {
    let mut degrees = BTreeMap::<String, (usize, usize)>::new();
    for fact in index.facts() {
        degrees.entry(fact.source_entity_id.clone()).or_default().1 += 1;
        degrees.entry(fact.target_entity_id.clone()).or_default().0 += 1;
    }
    let mut values = degrees
        .into_iter()
        .map(|(entity_id, (incoming, outgoing))| {
            json!({
                "entity_id": entity_id,
                "incoming": incoming,
                "outgoing": outgoing,
            })
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| {
        degree(right)
            .cmp(&degree(left))
            .then_with(|| canonical_value(left).cmp(&canonical_value(right)))
    });
    values
}

fn degree(value: &Value) -> u64 {
    value["incoming"].as_u64().unwrap_or_default() + value["outgoing"].as_u64().unwrap_or_default()
}

fn bounded_document(
    document: &mut BTreeMap<String, Value>,
    categories: Vec<(&str, Vec<Value>)>,
    budget: usize,
) -> Result<String, ContextSummaryError> {
    let full_counts = categories
        .iter()
        .map(|(name, values)| ((*name).to_owned(), values.len()))
        .collect::<BTreeMap<_, _>>();
    let mut omitted = full_counts.clone();
    for (name, _) in &categories {
        document.insert((*name).to_owned(), json!([]));
    }
    document.insert("truncated".to_owned(), json!(true));
    set_omitted(document, &omitted);
    if encode(document)?.len() > budget {
        return Err(ContextSummaryError::BudgetTooSmall);
    }

    for (name, values) in categories {
        for value in values {
            document
                .get_mut(name)
                .and_then(Value::as_array_mut)
                .expect("summary category is an array")
                .push(value);
            *omitted.get_mut(name).expect("summary omission counter") -= 1;
            set_omitted(document, &omitted);
            if encode(document)?.len() > budget {
                document
                    .get_mut(name)
                    .and_then(Value::as_array_mut)
                    .expect("summary category is an array")
                    .pop();
                *omitted.get_mut(name).expect("summary omission counter") += 1;
                set_omitted(document, &omitted);
                break;
            }
        }
    }
    if omitted.values().all(|count| *count == 0) {
        document.insert("truncated".to_owned(), json!(false));
    }
    let mut encoded = encode(document)?;
    if encoded.len() > budget {
        document.insert("truncated".to_owned(), json!(true));
        let mut removed = false;
        for (name, _) in full_counts.iter().rev() {
            let values = document
                .get_mut(name)
                .and_then(Value::as_array_mut)
                .expect("summary category is an array");
            if values.pop().is_some() {
                *omitted.get_mut(name).expect("summary omission counter") += 1;
                removed = true;
                break;
            }
        }
        if !removed {
            return Err(ContextSummaryError::BudgetTooSmall);
        }
        set_omitted(document, &omitted);
        encoded = encode(document)?;
    }
    if encoded.len() > budget {
        return Err(ContextSummaryError::BudgetTooSmall);
    }
    Ok(String::from_utf8(encoded).expect("JSON is UTF-8"))
}

fn set_omitted(document: &mut BTreeMap<String, Value>, omitted: &BTreeMap<String, usize>) {
    document.insert("omitted_counts".to_owned(), json!(omitted));
    document.insert(
        "truncated".to_owned(),
        json!(omitted.values().any(|count| *count != 0)),
    );
}

fn encode(document: &BTreeMap<String, Value>) -> Result<Vec<u8>, ContextSummaryError> {
    serde_json::to_vec(document).map_err(|_| ContextSummaryError::Serialization)
}

fn canonical_value(value: &Value) -> String {
    canonicalize(value.clone()).to_string()
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        Value::Object(values) => {
            let values = values
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(Map::from_iter(values))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_document_is_deterministic_and_never_splits_items() {
        let mandatory = BTreeMap::from([
            ("kind".to_owned(), json!("test")),
            (
                "budget".to_owned(),
                json!({"method": SUMMARY_BUDGET_METHOD, "token_limit": 256}),
            ),
        ]);
        let items = (0..100)
            .map(|index| json!({"id": index, "value": "x".repeat(16)}))
            .collect::<Vec<_>>();
        let mut first = mandatory.clone();
        let first = bounded_document(&mut first, vec![("items", items.clone())], 256)
            .expect("bounded summary");
        let mut second = mandatory;
        let second =
            bounded_document(&mut second, vec![("items", items)], 256).expect("bounded summary");
        assert_eq!(first, second);
        assert!(first.len() <= 256);
        let value: Value = serde_json::from_str(&first).expect("summary JSON");
        assert_eq!(value["truncated"], true);
        assert!(value["omitted_counts"]["items"].as_u64().unwrap() > 0);
    }
}
