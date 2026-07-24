use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::sync::{Arc, LazyLock, Mutex};

use base64::Engine as _;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use godot_codex_product::{ConnectionHealth, FULL_BETA_TOOLS};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_BOUNDED_DEPTH: usize = 32;
const MAX_BOUNDED_STRING_CHARS: usize = 65_536;
const MAX_BOUNDED_COLLECTION_ITEMS: usize = 250_000;
const MAX_BOUNDED_OBJECT_KEY_CHARS: usize = 512;
const MAX_FIXED_DTO_DEPTH: usize = 8;
// A runtime snapshot is already bounded to a 32 MiB transfer window. No
// single paged MCP projection may exceed that upstream trust budget, while a
// 64 KiB validation-report page remains comfortably below it. The equivalent
// text block is produced directly by the checked writer, so an oversized
// structuredContent value is rejected before either wire copy is created.
const MAX_SERIALIZED_STRUCTURED_CONTENT_BYTES: usize = 32 * 1_024 * 1_024;
// Runtime capture is independently bounded to 512 KiB of decoded PNG bytes.
// Standard padded base64 needs at most four characters for every three bytes.
const MAX_CAPTURE_IMAGE_BYTES: usize = 512 * 1_024;
const MAX_CAPTURE_IMAGE_BASE64_CHARS: usize = MAX_CAPTURE_IMAGE_BYTES.div_ceil(3) * 4;

// This is the frozen vocabulary used by the fixed wire DTO projection. It is
// deliberately broader than any one DTO because the live snapshot and saved
// semantic profiles share the same finite recursive definitions. The four
// project-defined dictionary fields below are excluded from this vocabulary
// and receive the explicit dynamic-map profile instead.
const FIXED_DTO_FIELDS: &str = "
accepted action_name active active_generation_id active_script_id active_stack_id adapter_profile
adapter_statuses added_entities added_relations advertised_capabilities affected_closure
affected_closure_only affected_entities age_seconds animation animation_reference_id animation_references
animations applying_per_history approval_message_bytes approval_protocol_floor approval_timeout_ms
architecture artifact_sha256 attached_script_entity_id attached_script_resource_id attributes authorities
authority availability available base_generation_id base_scene_entity_id base_scene_id binds bridge
bridge_capabilities bridge_current_minor bridge_major bridge_min_minor bridge_minor bridge_profiles build_id
build_state byte_length byte_size cache cache_age_seconds cache_generation cache_revisions cache_schema
can_redo can_undo capabilities capability_profile change_set_id changed_domains changed_entities check
checkpoint checks checksum child_count chunk_count clock_skew_ms code column committed_entities
comparison_path compatibility compatibility_basis completed_at_ms completeness component components condition
confidence configuration conflict_id conflicts connection_id connections container_items content
content_bytes content_generation content_sha256 context coordinates created_at_ms creation_reason current
current_operation_seq current_scene_id current_scene_revision current_scene_dirty data_base64url declaration
declaration_range declaration_scope declared_type declaring_node_entity_id declaring_node_id
declaring_scene_entity_id declaring_scene_id defining_scene_id definition delete deleted_index_revision
dependencies
depth detail diagnostic diagnostic_code diagnostic_count diagnostic_id diagnostic_message_bytes diagnostics
diagnostics_bytes digest dirty dirty_effect dirty_open_scripts dirty_scene_count dirty_script_count disk_comparison
disk_value display_path document documentation_present documents domain domains edge_id edges editable editor
editor_node_id editor_session_id editor_value eligible emitter_node_entity_id emitter_node_id
enabled_capabilities end end_byte end_column
end_line entities entity_count entity_id error event_seq event_type evidence evidence_digest evidence_id
evidence_range exact expected_added_entities expected_changed_entities expected_operation_seq
expected_removed_entities expected_scene_revision expected_transaction_seq expires_at_ms fact_id fact_ids
failed_migration_count field first_index_revision fixed_resource_count flags frame frames freshness
full_rebuild_count function generation generation_id godot godot_artifact_sha256 godot_build_id
godot_source_commit godot_type group groups has_more hash_algorithm height histories history_id host_version id
ide_host_version idempotency_key identity_characters identity_input identity_scope identity_strength
import_state incremental_commit_count index index_converged index_revision index_schema ingest_state
insertion_index inspector_bytes_per_node inspector_object_id instance_chain instance_scene_entity_id
instance_scene_id instance_scene_paths internal
introduced introduced_errors introduced_warnings issued_at_ms journal_bytes journal_records
keep_global_transform key kind language last_batch_checksum last_batch_id last_contiguous_runtime_event_seq
last_index_revision last_operation_seq library limit limits_applied line live_converged live_coverage
live_node_id live_only_nodes live_overlay mac major
match match_mode matched matrix_id max_bytes may_be_stale_for_editor may_have_committed mcp_protocol
member_node_entity_id membership_id memory_only message message_digest method mime_type minor
mixer_node_entity_id modifiers
mtime_after_ns mtime_before_ns mtime_ns name negotiated_bridge_protocol negotiated_protocol new_parent_node_id
next_action next_cursor next_offset node node_entity_id node_id node_index node_occurrence_id node_path nodes
nonce object_bytes observed observed_version offline_cached offset omitted_count omitted_counts omitted_reason
open_script_ids operation operation_bytes operation_count operation_kind operation_seq operations origin
origin_scene_id os
outcome outgoing_relations overloaded overridden_property_id owned owner owner_node_entity_id owner_node_id
owner_path owner_symbol_id ownership_paths package package_manifest_verified package_version page page_count
parent_generation_id
parent_node_entity_id parent_node_id parent_runtime_object_id partial_reasons path paths policy pre_existing
page_bytes pages
preconditions predicate prepared_records prepared_ttl_ms preview preview_bytes preview_digest
preview_payload_json previous_state primary persistent_content profile profile_id project_context
project_context_truncated
project_id project_revision project_scope projected_selection_count projected_value_bytes properties
properties_coverage properties_truncated property property_id protocols qualification qualified_key
qualified_name quarantine_count
query range read_only reader_max_minor reader_min_minor reason reasons receipt receipt_hash receipt_ttl_ms
receiver_node_entity_id receiver_node_id recovery redacted reference_id references registry relation relation_id
relations relative_node_path remediation_id remove_dependency_edge_ids remove_diagnostic_ids
remove_resource_entity_ids remove_source_document_entity_ids remove_tombstone_entity_ids removed_entities
removed_relations repeat_count replacement replica_status report_digest report_id request_deadline_ms
requires_form_elicitation requires_full_snapshot resolution resolved resolved_target_path resource
resource_entity_id resource_revision
resource_template_count resource_type resources result retain_through_index_revision retryable revision_vector
retained_report_bytes
revisions risk role rollback runtime runtime_diagnostic_id runtime_event_seq runtime_launch runtime_node_path
runtime_object_id runtime_semantics_inferred runtime_session_id runtime_stack_id runtime_timeout_ms safe_message
save_effect
retained_for_comparison save_scope saved_state scene scene_attachments scene_entity_id
scene_graph_revision scene_id scene_path scene_unique_id
scene_revision scene_revisions scenes schema schema_version schemas scope screenshot_available script
script_graph_revision script_id script_path script_ref script_resource_id scripts selected selected_node_ids
selection_count selections selector semantic semantic_digest severity sha256 signal signature size_after
size_before snapshot_bytes snapshot_checksum snapshot_chunk_bytes snapshot_id snapshot_timeout_ms
snapshot_window_bytes negotiated_snapshot_chunk_bytes total_inspector_bytes source source_commit source_complete
source_documents source_entity_id
source_hashes_verified source_hint source_kind source_kinds source_resource_entity_id source_text_included stack
stack_frames stack_kind stacks stacks_bytes start start_byte start_column start_line started_at_ms state
static_cache status status_bytes string_characters structural_nodes subject subject_entity_id summary supported
supports_form_elicitation suppressed_disk_nodes surface surface_observed surfaces symbol symbol_id symbols target
target_comparison_path target_display_path target_entity_id target_kind target_node_entity_id target_path
target_uid terminal_reason timestamp_ms title tombstones tool_count total total_matches total_relations
track_index transaction_id transaction_journal transaction_journal_schema transaction_seq transactions
transition_kind tree_depth tree_nodes truncated type type_name type_state uid uid_missing unbinds
undo_eligibility unexpected_added_entities unexpected_added_relations unexpected_changed_entities
unexpected_removed_entities unexpected_removed_relations unique_scene_id updated_at_ms upsert_dependencies
upsert_diagnostics upsert_resources upsert_source_documents upsert_tombstones usage usages validated_checkpoint
validation_digest validation_policy validation_report validation_report_schema validity value value_type
variant_depth variant_type version viewport_kind visibility visible visible_in_tree warnings width coverage
nodes_truncated output_seq active_kind
";

const PROJECT_DEFINED_DICTIONARY_FIELDS: &[&str] =
    &["attributes", "binds", "scene_revisions", "value"];

pub(crate) const OFFLINE_STATIC_TOOLS: &[&str] = &[
    "godot_find_resource_owners",
    "godot_find_usages",
    "godot_get_connection_status",
    "godot_get_resource_dependencies",
    "godot_get_scene_graph",
    "godot_inspect_node",
    "godot_inspect_symbol",
    "godot_search_symbols",
];

const RUNTIME_TOOLS: &[&str] = &[
    "godot_capture_viewport",
    "godot_continue_project",
    "godot_get_runtime_tree",
    "godot_get_stack_trace",
    "godot_inspect_runtime_object",
    "godot_pause_project",
    "godot_run_current_scene",
    "godot_run_project",
    "godot_stop_project",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AvailabilityDomain {
    Editor,
    Runtime,
}

pub(crate) fn availability_domain(tool_name: &str) -> Option<AvailabilityDomain> {
    if OFFLINE_STATIC_TOOLS.contains(&tool_name) {
        None
    } else if RUNTIME_TOOLS.contains(&tool_name) {
        Some(AvailabilityDomain::Runtime)
    } else if FULL_BETA_TOOLS.contains(&tool_name) {
        Some(AvailabilityDomain::Editor)
    } else {
        None
    }
}

pub(crate) trait ToolAvailabilityGuard {
    fn unavailable_tool_result(&self, tool_name: &str) -> Option<CallToolResult>;
}

/// One successful top-level result shape. Nested fixed records use the closed
/// finite-depth DTO definitions below; only explicitly named project-defined
/// dictionaries use the dynamic-map profile.
#[derive(Clone)]
struct OutputShape {
    required: Vec<&'static str>,
    optional: Vec<&'static str>,
}

fn shape(required: &[&'static str], optional: &[&'static str]) -> OutputShape {
    OutputShape {
        required: required.to_vec(),
        optional: optional.to_vec(),
    }
}

const LIVE_ENVELOPE: &[&str] = &[
    "schema_version",
    "project_id",
    "editor_session_id",
    "snapshot_id",
    "revision_vector",
    "status",
    "freshness",
    "truncated",
    "evidence",
];

const TRANSACTION_STATUS: &[&str] = &[
    "transaction_id",
    "editor_session_id",
    "scene_id",
    "state",
    "transaction_seq",
    "operation_kind",
    "risk",
    "scope",
    "preview_digest",
    "current_scene_revision",
    "current_operation_seq",
    "outcome",
    "error",
    "undo_eligibility",
    "committed_entities",
    "updated_at_ms",
    "limits_applied",
    "truncated",
];

const CHANGE_SET_PREVIEW: &[&str] = &[
    "schema_version",
    "change_set_id",
    "state",
    "transaction_seq",
    "preview_digest",
    "request_digest",
    "preview",
    "risk",
    "scope",
    "operation_count",
    "created_at_ms",
    "expires_at_ms",
    "limits_applied",
];

const CHANGE_SET_STATUS: &[&str] = &[
    "schema_version",
    "change_set_id",
    "state",
    "transaction_seq",
    "preview_digest",
    "request_digest",
    "preview",
    "risk",
    "scope",
    "operation_count",
    "created_at_ms",
    "expires_at_ms",
    "limits_applied",
    "outcome",
];

fn output_shapes(tool_name: &str) -> Option<Vec<OutputShape>> {
    let shapes = match tool_name {
        "godot_get_connection_status" => vec![shape(
            &[
                "schema_version",
                "status",
                "project_scope",
                "package_version",
                "compatibility",
                "bridge",
                "static_cache",
                "recovery",
                "components",
                "diagnostic",
                "remediation_id",
                "next_action",
                "limits_applied",
                "evidence",
            ],
            &[],
        )],
        "godot_get_editor_state" => vec![shape(
            &[
                LIVE_ENVELOPE[0],
                LIVE_ENVELOPE[1],
                LIVE_ENVELOPE[2],
                LIVE_ENVELOPE[3],
                LIVE_ENVELOPE[4],
                "capabilities_used",
                LIVE_ENVELOPE[5],
                LIVE_ENVELOPE[6],
                LIVE_ENVELOPE[7],
                "partial_reasons",
                "diagnostics",
                "limits_applied",
                "entities",
                "facts",
                LIVE_ENVELOPE[8],
                "editor",
                "runtime",
            ],
            &[],
        )],
        "godot_get_current_scene" => vec![shape(
            &[
                LIVE_ENVELOPE[0],
                LIVE_ENVELOPE[1],
                LIVE_ENVELOPE[2],
                LIVE_ENVELOPE[3],
                LIVE_ENVELOPE[4],
                "capabilities_used",
                LIVE_ENVELOPE[5],
                LIVE_ENVELOPE[6],
                LIVE_ENVELOPE[7],
                "partial_reasons",
                "diagnostics",
                "limits_applied",
                "entities",
                "facts",
                LIVE_ENVELOPE[8],
                "scene",
                "nodes",
            ],
            &[],
        )],
        "godot_get_selected_nodes" => vec![shape(
            &[
                LIVE_ENVELOPE[0],
                LIVE_ENVELOPE[1],
                LIVE_ENVELOPE[2],
                LIVE_ENVELOPE[3],
                LIVE_ENVELOPE[4],
                "capabilities_used",
                LIVE_ENVELOPE[5],
                LIVE_ENVELOPE[6],
                LIVE_ENVELOPE[7],
                "partial_reasons",
                "diagnostics",
                "limits_applied",
                "entities",
                "facts",
                LIVE_ENVELOPE[8],
                "scene",
                "scene_dirty",
                "selected_nodes",
            ],
            &[],
        )],
        "godot_get_inspector_state" => vec![shape(
            &[
                LIVE_ENVELOPE[0],
                LIVE_ENVELOPE[1],
                LIVE_ENVELOPE[2],
                LIVE_ENVELOPE[3],
                LIVE_ENVELOPE[4],
                "capabilities_used",
                LIVE_ENVELOPE[5],
                LIVE_ENVELOPE[6],
                LIVE_ENVELOPE[7],
                "partial_reasons",
                "diagnostics",
                "limits_applied",
                "entities",
                "facts",
                LIVE_ENVELOPE[8],
                "inspector",
            ],
            &[],
        )],
        "godot_get_open_scenes" => vec![live_page_shape("scenes")],
        "godot_get_open_scripts" => vec![live_page_shape("scripts")],
        "godot_get_editor_history" => vec![live_page_shape("histories")],
        "godot_get_diagnostics" => vec![
            live_page_shape("diagnostics"),
            shape(
                &[
                    "schema_version",
                    "project_id",
                    "editor_session_id",
                    "runtime_session_id",
                    "runtime_event_seq",
                    "state",
                    "active_stack_id",
                    "snapshot_id",
                    "diagnostics",
                    "limit",
                    "offset",
                    "total",
                    "truncated",
                    "limits_applied",
                    "next_cursor",
                    "evidence",
                ],
                &[],
            ),
        ],
        "godot_get_viewport_state" => vec![shape(
            &[
                LIVE_ENVELOPE[0],
                LIVE_ENVELOPE[1],
                LIVE_ENVELOPE[2],
                LIVE_ENVELOPE[3],
                LIVE_ENVELOPE[4],
                LIVE_ENVELOPE[5],
                LIVE_ENVELOPE[6],
                LIVE_ENVELOPE[7],
                "entities",
                "viewport",
                LIVE_ENVELOPE[8],
            ],
            &[],
        )],
        "godot_run_project"
        | "godot_run_current_scene"
        | "godot_stop_project"
        | "godot_pause_project"
        | "godot_continue_project" => vec![shape(
            &[
                "schema_version",
                "project_id",
                "editor_session_id",
                "runtime_session_id",
                "runtime_event_seq",
                "state",
                "origin",
                "target",
            ],
            &["scene_path"],
        )],
        "godot_get_runtime_tree" => vec![shape(
            &[
                "schema_version",
                "project_id",
                "editor_session_id",
                "runtime_session_id",
                "runtime_event_seq",
                "state",
                "snapshot_id",
                "limit",
                "offset",
                "total",
                "nodes",
                "truncated",
                "limits_applied",
                "next_cursor",
                "evidence",
            ],
            &[],
        )],
        "godot_inspect_runtime_object" => vec![shape(
            &[
                "schema_version",
                "runtime_session_id",
                "runtime_event_seq",
                "state",
                "runtime_object_id",
                "properties",
                "limits_applied",
            ],
            &["godot_type"],
        )],
        "godot_get_stack_trace" => vec![shape(
            &[
                "schema_version",
                "runtime_session_id",
                "runtime_event_seq",
                "state",
                "stack",
            ],
            &[],
        )],
        "godot_capture_viewport" => vec![shape(
            &[
                "schema_version",
                "runtime_session_id",
                "runtime_event_seq",
                "state",
                "mime_type",
                "width",
                "height",
                "byte_length",
                "sha256",
            ],
            &[],
        )],
        "godot_get_resource_dependencies" => {
            vec![resource_page_shape("dependencies")]
        }
        "godot_find_resource_owners" => vec![resource_page_shape("owners")],
        "godot_get_scene_graph" => vec![shape(
            &[
                "project_id",
                "schema_version",
                "generation_id",
                "index_revision",
                "resource_revision",
                "scene_graph_revision",
                "freshness",
                "offline_cached",
                "status",
                "scene",
                "query",
                "nodes",
                "project_context",
                "project_context_truncated",
                "diagnostics",
                "partial_reasons",
                "truncated",
                "next_cursor",
                "live_overlay",
                "conflicts",
                "validated_checkpoint",
                "evidence",
            ],
            &[],
        )],
        "godot_inspect_node" => vec![shape(
            &[
                "project_id",
                "schema_version",
                "generation_id",
                "index_revision",
                "resource_revision",
                "scene_graph_revision",
                "freshness",
                "offline_cached",
                "status",
                "scene",
                "node",
                "query",
                "properties",
                "attached_script_resource_id",
                "resources",
                "groups",
                "connections",
                "animation_references",
                "diagnostics",
                "partial_reasons",
                "truncated",
                "next_cursor",
                "live_overlay",
                "conflicts",
                "validated_checkpoint",
                "evidence",
            ],
            &[],
        )],
        "godot_search_symbols" => vec![shape(
            &[
                "project_id",
                "schema_version",
                "generation_id",
                "index_revision",
                "resource_revision",
                "scene_graph_revision",
                "script_graph_revision",
                "freshness",
                "offline_cached",
                "status",
                "query",
                "symbols",
                "total_matches",
                "diagnostics",
                "partial_reasons",
                "truncated",
                "next_cursor",
                "may_be_stale_for_editor",
                "dirty_open_scripts",
                "validated_checkpoint",
                "evidence",
            ],
            &[],
        )],
        "godot_inspect_symbol" => vec![shape(
            &[
                "project_id",
                "schema_version",
                "generation_id",
                "index_revision",
                "resource_revision",
                "scene_graph_revision",
                "script_graph_revision",
                "freshness",
                "offline_cached",
                "status",
                "query",
                "document",
                "declaration",
                "owner",
                "outgoing_relations",
                "scene_attachments",
                "total_relations",
                "diagnostics",
                "partial_reasons",
                "truncated",
                "next_cursor",
                "may_be_stale_for_editor",
                "dirty_open_scripts",
                "validated_checkpoint",
                "evidence",
            ],
            &[],
        )],
        "godot_find_usages" => vec![shape(
            &[
                "project_id",
                "schema_version",
                "generation_id",
                "index_revision",
                "resource_revision",
                "scene_graph_revision",
                "script_graph_revision",
                "freshness",
                "offline_cached",
                "status",
                "target",
                "scope",
                "source_kinds",
                "confidence",
                "limit",
                "offset",
                "total_matches",
                "truncated",
                "usages",
                "conflicts",
                "diagnostics",
                "partial_reasons",
                "may_be_stale_for_editor",
                "dirty_open_scripts",
                "next_cursor",
                "validated_checkpoint",
                "evidence",
            ],
            &[],
        )],
        "godot_prepare_create_node"
        | "godot_prepare_delete_node"
        | "godot_prepare_reparent_node"
        | "godot_prepare_set_property"
        | "godot_prepare_attach_script"
        | "godot_prepare_detach_script"
        | "godot_prepare_connect_signal"
        | "godot_prepare_disconnect_signal" => vec![shape(
            &[
                "transaction_id",
                "editor_session_id",
                "scene_id",
                "state",
                "transaction_seq",
                "scene_revision",
                "operation_seq",
                "operation_kind",
                "risk",
                "scope",
                "affected_entities",
                "preview",
                "preview_digest",
                "created_at_ms",
                "expires_at_ms",
                "limits_applied",
            ],
            &[],
        )],
        "godot_prepare_change_set" => vec![shape(CHANGE_SET_PREVIEW, &["coordinates"])],
        "godot_get_validation_report" => vec![shape(
            &[
                "schema_version",
                "report_id",
                "report_digest",
                "page",
                "page_count",
                "content",
                "content_bytes",
                "limits_applied",
            ],
            &[],
        )],
        "godot_get_confirmation_policy" => vec![
            shape(
                &[
                    "mode",
                    "grant_active",
                    "expires_at_ms",
                    "allowed_scopes",
                    "max_grant_lifetime_ms",
                    "restrictions",
                ],
                &[],
            ),
            shape(
                &["mode", "grant_active", "reason", "max_grant_lifetime_ms"],
                &[],
            ),
        ],
        "godot_reset_confirmation_policy" => {
            vec![shape(&["mode", "revoked", "grant_active"], &[])]
        }
        "godot_apply_transaction" | "godot_get_transaction_status" | "godot_undo_transaction" => {
            vec![
                shape(TRANSACTION_STATUS, &[]),
                shape(
                    CHANGE_SET_STATUS,
                    &[
                        "coordinates",
                        "postimage_digest",
                        "validation_report_id",
                        "terminal_error",
                    ],
                ),
            ]
        }
        _ => return None,
    };
    Some(shapes)
}

fn live_page_shape(item_key: &'static str) -> OutputShape {
    let required = vec![
        "schema_version",
        "project_id",
        "editor_session_id",
        "snapshot_id",
        "revision_vector",
        "status",
        "freshness",
        "state",
        "limit",
        "offset",
        "total",
        "truncated",
        "next_cursor",
        "evidence",
        item_key,
    ];
    OutputShape {
        required,
        optional: Vec::new(),
    }
}

fn resource_page_shape(item_key: &'static str) -> OutputShape {
    let required = vec![
        "project_id",
        "schema_version",
        "generation_id",
        "index_revision",
        "validated_checkpoint",
        "query",
        "resource",
        "status",
        "freshness",
        "offline_cached",
        "diagnostics",
        "truncated",
        "next_cursor",
        "evidence",
        item_key,
    ];
    OutputShape {
        required,
        optional: Vec::new(),
    }
}

fn bounded_definitions() -> Map<String, Value> {
    let mut definitions = Map::new();
    definitions.insert(
        "BoundedString".to_owned(),
        json!({
            "type": "string",
            "maxLength": MAX_BOUNDED_STRING_CHARS
        }),
    );
    definitions.insert(
        "BoundedScalar".to_owned(),
        json!({
            "anyOf": [
                {"type": "null"},
                {"type": "boolean"},
                {
                    "type": "number",
                    "minimum": -9_007_199_254_740_991_i64,
                    "maximum": 9_007_199_254_740_991_u64
                },
                {"$ref": "#/$defs/BoundedString"}
            ]
        }),
    );
    definitions.insert(
        "BoundedValue0".to_owned(),
        json!({"$ref": "#/$defs/BoundedScalar"}),
    );
    for depth in 1..=MAX_BOUNDED_DEPTH {
        let previous = depth - 1;
        definitions.insert(
            format!("BoundedArray{depth}"),
            json!({
                "type": "array",
                "maxItems": MAX_BOUNDED_COLLECTION_ITEMS,
                "items": {"$ref": format!("#/$defs/BoundedValue{previous}")}
            }),
        );
        definitions.insert(
            format!("BoundedDynamicObject{depth}"),
            json!({
                "type": "object",
                "additionalProperties": false,
                "maxProperties": MAX_BOUNDED_COLLECTION_ITEMS,
                "propertyNames": {
                    "type": "string",
                    "maxLength": MAX_BOUNDED_OBJECT_KEY_CHARS
                },
                // Semantic facts and projected Godot dictionaries have
                // project-defined keys. This is the one explicit dynamic-map
                // escape hatch; it is finite-depth, key-bounded, and cannot
                // bypass the value bounds.
                "patternProperties": {
                    "^.{0,512}$": {
                        "$ref": format!("#/$defs/BoundedValue{previous}")
                    }
                },
                "x-godot-codex-dynamic-map": true
            }),
        );
        definitions.insert(
            format!("BoundedValue{depth}"),
            json!({
                "anyOf": [
                    {"$ref": "#/$defs/BoundedScalar"},
                    {"$ref": format!("#/$defs/BoundedArray{depth}")},
                    {"$ref": format!("#/$defs/BoundedDynamicObject{depth}")}
                ]
            }),
        );
    }
    definitions.insert(
        "BoundedValue".to_owned(),
        json!({"$ref": format!("#/$defs/BoundedValue{MAX_BOUNDED_DEPTH}")}),
    );

    let fixed_field_pattern = fixed_dto_field_pattern();
    definitions.insert(
        "FixedValue0".to_owned(),
        json!({"$ref": "#/$defs/BoundedScalar"}),
    );
    for depth in 1..=MAX_FIXED_DTO_DEPTH {
        let previous = depth - 1;
        definitions.insert(
            format!("FixedArray{depth}"),
            json!({
                "type": "array",
                "maxItems": MAX_BOUNDED_COLLECTION_ITEMS,
                "items": {"$ref": format!("#/$defs/FixedValue{previous}")}
            }),
        );
        let mut project_defined_properties = Map::new();
        for field in PROJECT_DEFINED_DICTIONARY_FIELDS {
            project_defined_properties
                .insert((*field).to_owned(), json!({"$ref": "#/$defs/BoundedValue"}));
        }
        let fixed_patterns = Map::from_iter([(
            fixed_field_pattern.clone(),
            json!({"$ref": format!("#/$defs/FixedValue{previous}")}),
        )]);
        definitions.insert(
            format!("FixedDto{depth}"),
            json!({
                "type": "object",
                "additionalProperties": false,
                "maxProperties": MAX_BOUNDED_COLLECTION_ITEMS,
                "properties": project_defined_properties,
                "patternProperties": fixed_patterns,
                "x-godot-codex-fixed-dto": true
            }),
        );
        definitions.insert(
            format!("FixedValue{depth}"),
            json!({
                "anyOf": [
                    {"$ref": "#/$defs/BoundedScalar"},
                    {"$ref": format!("#/$defs/FixedArray{depth}")},
                    {"$ref": format!("#/$defs/FixedDto{depth}")}
                ]
            }),
        );
    }
    definitions.insert(
        "FixedValue".to_owned(),
        json!({"$ref": format!("#/$defs/FixedValue{MAX_FIXED_DTO_DEPTH}")}),
    );
    definitions.extend(
        serde_json::from_value::<Map<String, Value>>(json!({
        "SafeCurrentCoordinates": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "runtime_session_id": {"type": "string", "maxLength": 128},
                "runtime_event_seq": {"type": "integer", "minimum": 0, "maximum": 9007199254740991_u64},
                "state": {"type": "string", "maxLength": 64},
                "editor_session_id": {"type": "string", "maxLength": 128},
                "snapshot_id": {"type": "string", "maxLength": 128},
                "event_seq": {"type": "integer", "minimum": 0, "maximum": 9007199254740991_u64},
                "current_scene_id": {
                    "anyOf": [
                        {"type": "string", "maxLength": 128},
                        {"type": "null"}
                    ]
                },
                "scene_revision": {
                    "anyOf": [
                        {"type": "integer", "minimum": 0, "maximum": 9007199254740991_u64},
                        {"type": "null"}
                    ]
                }
            }
        },
        "ToolError": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "code": {"type": "string", "minLength": 1, "maxLength": 64},
                "message": {"type": "string", "maxLength": 4096},
                "retryable": {"type": "boolean"},
                "transaction_id": {
                    "anyOf": [
                        {"type": "string", "maxLength": 128},
                        {"type": "null"}
                    ]
                },
                "replica_status": {"type": "string", "maxLength": 64},
                "current": {"$ref": "#/$defs/SafeCurrentCoordinates"},
                "status": {"type": "string", "maxLength": 64},
                "diagnostic_code": {"type": "string", "maxLength": 64},
                "compatibility": {"type": "string", "maxLength": 64},
                "remediation_id": {
                    "anyOf": [
                        {"type": "string", "maxLength": 64},
                        {"type": "null"}
                    ]
                }
            },
            "required": ["code", "message", "retryable"]
        }
    }))
        .expect("static output schema definitions are objects"),
    );
    definitions
}

fn fixed_dto_field_pattern() -> String {
    let fields = FIXED_DTO_FIELDS
        .split_ascii_whitespace()
        .filter(|field| !PROJECT_DEFINED_DICTIONARY_FIELDS.contains(field))
        .collect::<BTreeSet<_>>();
    assert!(!fields.is_empty(), "fixed DTO vocabulary must not be empty");
    format!("^(?:{})$", fields.into_iter().collect::<Vec<_>>().join("|"))
}

fn output_schema(tool_name: &str) -> Option<Arc<JsonObject>> {
    if tool_name == "godot_get_connection_status" {
        return Some(connection_output_schema());
    }
    let shapes = output_shapes(tool_name)?;
    assert_unique_shape_fields(tool_name, &shapes);
    let mut allowed = BTreeSet::from(["error"]);
    for shape in &shapes {
        allowed.extend(shape.required.iter().copied());
        allowed.extend(shape.optional.iter().copied());
    }
    let mut properties = Map::new();
    for field in allowed {
        properties.insert(field.to_owned(), field_schema(tool_name, field));
    }
    let mut variants = shapes
        .iter()
        .map(|shape| {
            let allowed = shape
                .required
                .iter()
                .chain(&shape.optional)
                .copied()
                .collect::<BTreeSet<_>>();
            json!({
                "required": &shape.required,
                "propertyNames": {"enum": allowed}
            })
        })
        .collect::<Vec<_>>();
    variants.push(json!({
        "required": ["error"],
        "minProperties": 1,
        "maxProperties": 1,
        "propertyNames": {"enum": ["error"]},
        "properties": {"error": {"$ref": "#/$defs/ToolError"}}
    }));
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": format!("{tool_name} result"),
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "oneOf": variants,
        "$defs": bounded_definitions(),
    });
    Some(Arc::new(
        schema
            .as_object()
            .expect("tool output schema is an object")
            .clone(),
    ))
}

fn assert_unique_shape_fields(tool_name: &str, shapes: &[OutputShape]) {
    for (index, shape) in shapes.iter().enumerate() {
        let required = shape.required.iter().copied().collect::<BTreeSet<_>>();
        let optional = shape.optional.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(
            required.len(),
            shape.required.len(),
            "{tool_name} success variant {index} has duplicate required fields"
        );
        assert_eq!(
            optional.len(),
            shape.optional.len(),
            "{tool_name} success variant {index} has duplicate optional fields"
        );
        assert!(
            required.is_disjoint(&optional),
            "{tool_name} success variant {index} repeats fields across required and optional"
        );
    }
}

fn connection_output_schema() -> Arc<JsonObject> {
    let typed = rmcp::handler::server::tool::schema_for_type::<ConnectionHealth>();
    let mut schema = typed.as_ref().clone();
    let required = schema
        .remove("required")
        .expect("closed ConnectionHealth schema has required fields");
    schema
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .expect("closed ConnectionHealth schema has properties")
        .insert(
            "error".to_owned(),
            field_schema("godot_get_connection_status", "error"),
        );
    let definitions = schema
        .entry("$defs")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("ConnectionHealth definitions are an object");
    for (name, definition) in bounded_definitions() {
        assert!(
            definitions.insert(name.clone(), definition).is_none(),
            "ConnectionHealth definition unexpectedly collides with {name}"
        );
    }
    schema.insert(
        "oneOf".to_owned(),
        json!([
            {
                "required": required,
                "propertyNames": {
                    "enum": schema
                        .get("properties")
                        .and_then(Value::as_object)
                        .expect("connection properties")
                        .keys()
                        .filter(|name| name.as_str() != "error")
                        .collect::<Vec<_>>()
                }
            },
            {
                "required": ["error"],
                "minProperties": 1,
                "maxProperties": 1,
                "propertyNames": {"enum": ["error"]},
                "properties": {"error": {"$ref": "#/$defs/ToolError"}}
            }
        ]),
    );
    let mut value = Value::Object(schema);
    bound_generated_schema(&mut value);
    Arc::new(
        value
            .as_object()
            .expect("bounded connection schema is an object")
            .clone(),
    )
}

fn bound_generated_schema(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let integer_type = object.get("type").is_some_and(|value| {
                value.as_str() == Some("integer")
                    || value
                        .as_array()
                        .is_some_and(|types| types.iter().any(|kind| kind == "integer"))
            });
            if integer_type
                && matches!(
                    object.get("format").and_then(Value::as_str),
                    Some("uint" | "uint8" | "uint16" | "uint32" | "uint64" | "usize")
                )
            {
                object.remove("format");
                object
                    .entry("minimum")
                    .or_insert_with(|| Value::from(0_u64));
                object
                    .entry("maximum")
                    .or_insert_with(|| Value::from(MAX_SAFE_INTEGER));
            }
            if object.get("type").and_then(Value::as_str) == Some("string") {
                object
                    .entry("maxLength")
                    .or_insert_with(|| Value::from(MAX_BOUNDED_STRING_CHARS));
            }
            if object.get("type").and_then(Value::as_str) == Some("array") {
                object
                    .entry("maxItems")
                    .or_insert_with(|| Value::from(MAX_BOUNDED_COLLECTION_ITEMS));
            }
            if object.get("type").and_then(Value::as_str) == Some("object") {
                object
                    .entry("maxProperties")
                    .or_insert_with(|| Value::from(MAX_BOUNDED_COLLECTION_ITEMS));
            }
            object.values_mut().for_each(bound_generated_schema);
        }
        Value::Array(values) => values.iter_mut().for_each(bound_generated_schema),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TopLevelFieldKind {
    Boolean,
    Integer,
    NullableInteger,
    String,
    NullableString,
    Object,
    NullableObject,
    ObjectArray,
    NullableObjectArray,
    StringArray,
    StringOrNullableObject,
    ErrorOrNull,
}

fn field_schema(tool_name: &str, field: &str) -> Value {
    match top_level_field_kind(tool_name, field) {
        TopLevelFieldKind::Boolean => json!({"type": "boolean"}),
        TopLevelFieldKind::Integer => {
            json!({"type": "integer", "minimum": 0, "maximum": MAX_SAFE_INTEGER})
        }
        TopLevelFieldKind::NullableInteger => json!({
            "anyOf": [
                {"type": "integer", "minimum": 0, "maximum": MAX_SAFE_INTEGER},
                {"type": "null"}
            ]
        }),
        TopLevelFieldKind::String => json!({"$ref": "#/$defs/BoundedString"}),
        TopLevelFieldKind::NullableString => json!({
            "anyOf": [
                {"$ref": "#/$defs/BoundedString"},
                {"type": "null"}
            ]
        }),
        TopLevelFieldKind::Object => {
            json!({"$ref": format!("#/$defs/FixedDto{MAX_FIXED_DTO_DEPTH}")})
        }
        TopLevelFieldKind::NullableObject => json!({
            "anyOf": [
                {"$ref": format!("#/$defs/FixedDto{MAX_FIXED_DTO_DEPTH}")},
                {"type": "null"}
            ]
        }),
        TopLevelFieldKind::ObjectArray => fixed_object_array_schema(),
        TopLevelFieldKind::NullableObjectArray => json!({
            "anyOf": [
                fixed_object_array_schema(),
                {"type": "null"}
            ]
        }),
        TopLevelFieldKind::StringArray => json!({
            "type": "array",
            "maxItems": MAX_BOUNDED_COLLECTION_ITEMS,
            "items": {"$ref": "#/$defs/BoundedString"}
        }),
        TopLevelFieldKind::StringOrNullableObject => json!({
            "anyOf": [
                {"$ref": "#/$defs/BoundedString"},
                {"$ref": format!("#/$defs/FixedDto{MAX_FIXED_DTO_DEPTH}")},
                {"type": "null"}
            ]
        }),
        TopLevelFieldKind::ErrorOrNull => json!({
            "anyOf": [
                {"$ref": "#/$defs/ToolError"},
                {"type": "null"}
            ]
        }),
    }
}

fn fixed_object_array_schema() -> Value {
    json!({
        "type": "array",
        "maxItems": MAX_BOUNDED_COLLECTION_ITEMS,
        "items": {"$ref": format!("#/$defs/FixedDto{}", MAX_FIXED_DTO_DEPTH - 1)}
    })
}

fn top_level_field_kind(tool_name: &str, field: &str) -> TopLevelFieldKind {
    use TopLevelFieldKind::{
        Boolean, ErrorOrNull, Integer, NullableInteger, NullableObject, NullableObjectArray,
        NullableString, Object, ObjectArray, String as StringField, StringArray,
        StringOrNullableObject,
    };

    match field {
        "schema_version" => {
            if OFFLINE_STATIC_TOOLS.contains(&tool_name)
                && tool_name != "godot_get_connection_status"
            {
                Object
            } else {
                StringField
            }
        }
        "state" => match tool_name {
            "godot_get_open_scenes" | "godot_get_open_scripts" | "godot_get_editor_history" => {
                NullableObject
            }
            "godot_get_diagnostics" => StringOrNullableObject,
            _ => StringField,
        },
        "evidence" => {
            if OFFLINE_STATIC_TOOLS.contains(&tool_name)
                && tool_name != "godot_get_connection_status"
            {
                Object
            } else {
                ObjectArray
            }
        }
        "scene" => match tool_name {
            "godot_get_current_scene" | "godot_get_selected_nodes" => NullableObject,
            _ => Object,
        },
        "target" => match tool_name {
            "godot_find_usages" => Object,
            _ => StringField,
        },
        "scope" => match tool_name {
            "godot_find_usages" => Object,
            _ => StringField,
        },
        "expires_at_ms" => match tool_name {
            "godot_get_confirmation_policy" => NullableInteger,
            _ => Integer,
        },
        "truncated"
        | "offline_cached"
        | "scene_dirty"
        | "may_be_stale_for_editor"
        | "project_context_truncated"
        | "grant_active"
        | "revoked" => Boolean,
        "runtime_event_seq"
        | "event_seq"
        | "limit"
        | "offset"
        | "total"
        | "total_matches"
        | "total_relations"
        | "index_revision"
        | "resource_revision"
        | "scene_revision"
        | "operation_seq"
        | "transaction_seq"
        | "current_scene_revision"
        | "current_operation_seq"
        | "updated_at_ms"
        | "created_at_ms"
        | "operation_count"
        | "width"
        | "height"
        | "byte_length"
        | "page"
        | "page_count"
        | "content_bytes"
        | "max_grant_lifetime_ms" => Integer,
        "scene_graph_revision" | "script_graph_revision" => NullableInteger,
        "next_cursor"
        | "scene_path"
        | "godot_type"
        | "active_stack_id"
        | "attached_script_resource_id"
        | "remediation_id"
        | "next_action"
        | "project_scope" => NullableString,
        "project_id"
        | "editor_session_id"
        | "snapshot_id"
        | "status"
        | "freshness"
        | "runtime_session_id"
        | "runtime_object_id"
        | "origin"
        | "mime_type"
        | "sha256"
        | "generation_id"
        | "report_id"
        | "report_digest"
        | "change_set_id"
        | "transaction_id"
        | "scene_id"
        | "preview_digest"
        | "request_digest"
        | "risk"
        | "operation_kind"
        | "mode"
        | "reason"
        | "package_version"
        | "diagnostic_code"
        | "content"
        | "postimage_digest"
        | "validation_report_id" => StringField,
        "outcome" => NullableString,
        "error" => ErrorOrNull,
        "capabilities_used" | "partial_reasons" | "source_kinds" | "confidence"
        | "allowed_scopes" => StringArray,
        "diagnostics"
        | "entities"
        | "facts"
        | "nodes"
        | "selected_nodes"
        | "scenes"
        | "scripts"
        | "histories"
        | "properties"
        | "dependencies"
        | "owners"
        | "project_context"
        | "conflicts"
        | "resources"
        | "groups"
        | "connections"
        | "animation_references"
        | "symbols"
        | "dirty_open_scripts"
        | "outgoing_relations"
        | "scene_attachments"
        | "usages"
        | "affected_entities" => ObjectArray,
        "committed_entities" => NullableObjectArray,
        "revision_vector"
        | "limits_applied"
        | "editor"
        | "runtime"
        | "resource"
        | "query"
        | "live_overlay"
        | "validated_checkpoint"
        | "node"
        | "document"
        | "declaration"
        | "preview"
        | "coordinates"
        | "undo_eligibility"
        | "restrictions"
        | "stack"
        | "terminal_error" => Object,
        "inspector" | "viewport" | "owner" => NullableObject,
        _ => panic!("unclassified top-level field {field:?} for canonical tool {tool_name:?}"),
    }
}

/// Installs the exact result schema and a single result normalizer on every
/// generated route. A valid routed call therefore cannot accidentally expose
/// unstructured output or a text/structuredContent mismatch.
pub(crate) fn install_output_contracts<S: Send + Sync + ToolAvailabilityGuard + 'static>(
    mut router: ToolRouter<S>,
) -> ToolRouter<S> {
    let registered = router
        .map
        .keys()
        .map(|name| name.as_ref())
        .collect::<BTreeSet<_>>();
    let expected = FULL_BETA_TOOLS.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(
        registered, expected,
        "generated MCP tool registry and output schema catalog diverged"
    );
    for route in router.map.values_mut() {
        let tool_name = route.attr.name.to_string();
        route.attr.title = Some(stable_tool_title(&tool_name));
        route.attr.output_schema = Some(
            output_schema(&tool_name)
                .expect("every registered MCP tool has one closed output schema"),
        );
        let call = Arc::clone(&route.call);
        route.call = Arc::new(move |context| {
            let call = Arc::clone(&call);
            let tool_name = tool_name.clone();
            Box::pin(async move {
                if let Some(result) = context.service.unavailable_tool_result(&tool_name) {
                    return Ok(normalize_tool_result(&tool_name, result));
                }
                call(context)
                    .await
                    .map(|result| normalize_tool_result(&tool_name, result))
            })
        });
    }
    router
}

fn stable_tool_title(tool_name: &str) -> String {
    let words = tool_name
        .strip_prefix("godot_")
        .expect("canonical Godot MCP tool name")
        .split('_')
        .map(|word| {
            let mut characters = word.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(characters).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("Godot {words}")
}

struct CheckedJsonWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl CheckedJsonWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(8 * 1_024)),
            limit,
        }
    }

    fn into_string(self) -> Result<String, ()> {
        String::from_utf8(self.bytes).map_err(|_| ())
    }
}

impl Write for CheckedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|next| next > self.limit)
        {
            return Err(io::Error::other(
                "serialized tool result exceeds its bounded wire budget",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn bounded_json_text(value: &Value, limit: usize) -> Result<String, ()> {
    let mut writer = CheckedJsonWriter::new(limit);
    serde_json::to_writer(&mut writer, value).map_err(|_| ())?;
    writer.into_string()
}

fn auxiliary_content_is_safe(tool_name: &str, structured: &Value, result: &CallToolResult) -> bool {
    let mut image = None;
    for content in &result.content {
        match content {
            ContentBlock::Text(_) => {}
            ContentBlock::Image(candidate) if image.is_none() => image = Some(candidate),
            _ => return false,
        }
    }
    let Some(image) = image else {
        return true;
    };
    if tool_name != "godot_capture_viewport"
        || result.is_error == Some(true)
        || image.mime_type != "image/png"
        || image.meta.is_some()
        || image.annotations.is_some()
        || image.data.len() > MAX_CAPTURE_IMAGE_BASE64_CHARS
        || structured.get("mime_type").and_then(Value::as_str) != Some("image/png")
    {
        return false;
    }
    let Some(expected_length) = structured
        .get("byte_length")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value <= MAX_CAPTURE_IMAGE_BYTES)
    else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(&image.data) else {
        return false;
    };
    decoded.len() == expected_length
        && structured.get("sha256").and_then(Value::as_str)
            == Some(format!("{:x}", Sha256::digest(&decoded)).as_str())
}

fn normalize_tool_result(tool_name: &str, mut result: CallToolResult) -> CallToolResult {
    let Some(structured) = result.structured_content.as_ref() else {
        return invalid_tool_result(
            "The tool returned no structuredContent for its advertised output schema.",
        );
    };
    if !structured.is_object() {
        return invalid_tool_result(
            "The tool returned non-object structuredContent for its advertised output schema.",
        );
    }
    let text = match bounded_json_text(structured, MAX_SERIALIZED_STRUCTURED_CONTENT_BYTES) {
        Ok(text) => text,
        Err(()) => {
            return invalid_tool_result(
                "The serialized tool result exceeded its bounded wire budget.",
            );
        }
    };
    if !matches_top_level_contract(tool_name, &result) {
        return invalid_tool_result(
            "The tool result did not match its recursively closed output contract.",
        );
    }
    if !auxiliary_content_is_safe(tool_name, structured, &result) {
        return invalid_tool_result(
            "The tool returned unexpected or oversized non-text result content.",
        );
    }
    // Keep the one independently bounded viewport image byte-for-byte, but
    // publish exactly one canonical JSON text block. Extra text blocks could
    // otherwise diverge from structuredContent or retain unvalidated data.
    result
        .content
        .retain(|content| !matches!(content, ContentBlock::Text(_)));
    result.content.insert(0, ContentBlock::text(text));
    result
}

pub(super) fn matches_top_level_contract(tool_name: &str, result: &CallToolResult) -> bool {
    let Some(object) = result
        .structured_content
        .as_ref()
        .and_then(Value::as_object)
    else {
        return false;
    };
    let has_error_envelope = object.len() == 1 && object.contains_key("error");
    if (result.is_error == Some(true)) != has_error_envelope {
        return false;
    }

    static VALIDATORS: LazyLock<Mutex<BTreeMap<String, jsonschema::Validator>>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));
    let Ok(mut validators) = VALIDATORS.lock() else {
        return false;
    };
    if !validators.contains_key(tool_name) {
        let Some(schema) = output_schema(tool_name) else {
            return false;
        };
        let Ok(validator) =
            jsonschema::draft202012::options().build(&Value::Object(schema.as_ref().clone()))
        else {
            return false;
        };
        validators.insert(tool_name.to_owned(), validator);
    }
    validators
        .get(tool_name)
        .is_some_and(|validator| validator.is_valid(&Value::Object(object.clone())))
}

fn invalid_tool_result(message: &str) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "error": {
            "code": "invalid_tool_result",
            "message": message,
            "retryable": true,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use godot_codex_product::{
        BridgeCondition, CacheCondition, CompatibilityEvaluation, ComponentCondition,
        ConfigurationCondition, ConnectionObservation, PackageCondition, RecoveryCondition,
    };

    fn assert_unique_required_arrays(value: &Value) {
        if let Some(required) = value.get("required").and_then(Value::as_array) {
            let strings = required
                .iter()
                .map(|field| field.as_str().expect("required entries are strings"))
                .collect::<BTreeSet<_>>();
            assert_eq!(
                strings.len(),
                required.len(),
                "duplicate required entry in {value}"
            );
        }
        match value {
            Value::Array(values) => values.iter().for_each(assert_unique_required_arrays),
            Value::Object(values) => values.values().for_each(assert_unique_required_arrays),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    fn assert_no_nonstandard_unsigned_formats(value: &Value) {
        if let Some(format) = value.get("format").and_then(Value::as_str) {
            assert!(
                !matches!(
                    format,
                    "uint" | "uint8" | "uint16" | "uint32" | "uint64" | "usize"
                ),
                "non-standard unsigned format remains in {value}"
            );
        }
        match value {
            Value::Array(values) => values
                .iter()
                .for_each(assert_no_nonstandard_unsigned_formats),
            Value::Object(values) => values
                .values()
                .for_each(assert_no_nonstandard_unsigned_formats),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    fn assert_closed_schema_objects(value: &Value) {
        if value.get("type").and_then(Value::as_str) == Some("object") {
            assert_eq!(
                value.get("additionalProperties").and_then(Value::as_bool),
                Some(false),
                "object schema is not closed: {value}"
            );
        }
        match value {
            Value::Array(values) => values.iter().for_each(assert_closed_schema_objects),
            Value::Object(values) => values.values().for_each(assert_closed_schema_objects),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    fn assert_dynamic_maps_are_explicit_and_bounded(value: &Value) {
        if value.get("patternProperties").is_some() {
            assert_eq!(
                value.get("additionalProperties").and_then(Value::as_bool),
                Some(false),
                "pattern-based object accepts unbounded additional properties: {value}"
            );
            if value
                .get("x-godot-codex-fixed-dto")
                .and_then(Value::as_bool)
                == Some(true)
            {
                assert!(
                    value.get("x-godot-codex-dynamic-map").is_none(),
                    "fixed DTO is also marked as a dynamic map: {value}"
                );
                let patterns = value["patternProperties"]
                    .as_object()
                    .expect("fixed DTO patternProperties is an object");
                assert_eq!(
                    patterns.len(),
                    1,
                    "fixed DTO has an ambiguous vocabulary: {value}"
                );
                let pattern = patterns.keys().next().expect("fixed DTO pattern");
                assert!(
                    pattern.starts_with("^(?:")
                        && pattern.ends_with(")$")
                        && !pattern.contains(".*"),
                    "fixed DTO vocabulary is not a finite field alternation: {value}"
                );
                return walk_schema_children(value, assert_dynamic_maps_are_explicit_and_bounded);
            }
            assert_eq!(
                value
                    .get("x-godot-codex-dynamic-map")
                    .and_then(Value::as_bool),
                Some(true),
                "patternProperties is neither a fixed DTO nor an explicitly marked dynamic map: {value}"
            );
            assert!(
                value
                    .pointer("/propertyNames/maxLength")
                    .and_then(Value::as_u64)
                    .is_some_and(|limit| limit > 0),
                "dynamic map has no bounded property names: {value}"
            );
            let patterns = value["patternProperties"]
                .as_object()
                .expect("patternProperties is an object");
            assert_eq!(
                patterns.len(),
                1,
                "dynamic map has an ambiguous pattern vocabulary: {value}"
            );
            assert!(
                patterns
                    .values()
                    .all(|schema| schema.get("$ref").and_then(Value::as_str).is_some()),
                "dynamic map values do not use a finite bounded definition: {value}"
            );
        }
        walk_schema_children(value, assert_dynamic_maps_are_explicit_and_bounded);
    }

    fn walk_schema_children(value: &Value, visitor: fn(&Value)) {
        match value {
            Value::Array(values) => values.iter().for_each(visitor),
            Value::Object(values) => values.values().for_each(visitor),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    fn minimal_field_value(tool_name: &str, field: &str) -> Value {
        match top_level_field_kind(tool_name, field) {
            TopLevelFieldKind::Boolean => json!(false),
            TopLevelFieldKind::Integer => json!(0),
            TopLevelFieldKind::NullableInteger
            | TopLevelFieldKind::NullableString
            | TopLevelFieldKind::NullableObject
            | TopLevelFieldKind::NullableObjectArray
            | TopLevelFieldKind::ErrorOrNull => Value::Null,
            TopLevelFieldKind::String => json!("fixture"),
            TopLevelFieldKind::Object => json!({}),
            TopLevelFieldKind::ObjectArray | TopLevelFieldKind::StringArray => json!([]),
            TopLevelFieldKind::StringOrNullableObject => Value::Null,
        }
    }

    fn minimal_success(tool_name: &str, shape_index: usize) -> Value {
        let shape = &output_shapes(tool_name).expect("known tool")[shape_index];
        Value::Object(
            shape
                .required
                .iter()
                .map(|field| ((*field).to_owned(), minimal_field_value(tool_name, field)))
                .collect(),
        )
    }

    fn assert_valid_normalized(tool_name: &str, value: Value) {
        let result = CallToolResult::structured(value.clone());
        if !matches_top_level_contract(tool_name, &result) {
            let schema = Value::Object(output_schema(tool_name).unwrap().as_ref().clone());
            let validator = jsonschema::draft202012::options().build(&schema).unwrap();
            let errors = validator
                .iter_errors(&value)
                .map(|error| error.to_string())
                .collect::<Vec<_>>();
            panic!("{tool_name} rejected a valid closed fixture: {value}; {errors:?}");
        }
        let normalized = normalize_tool_result(tool_name, result);
        assert_ne!(normalized.is_error, Some(true), "{tool_name}");
    }

    fn assert_invalid_normalized(tool_name: &str, value: Value) {
        let normalized = normalize_tool_result(tool_name, CallToolResult::structured(value));
        assert_invalid_tool_result(&normalized);
    }

    fn assert_invalid_tool_result(normalized: &CallToolResult) {
        assert_eq!(
            normalized
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/error/code")),
            Some(&json!("invalid_tool_result")),
            "an invalid result crossed the central output boundary"
        );
    }

    fn assert_unknown_object_field_rejected(
        tool_name: &str,
        shape_index: usize,
        field: &str,
        nested: Value,
    ) {
        let mut valid = minimal_success(tool_name, shape_index);
        valid[field] = nested;
        assert_valid_normalized(tool_name, valid.clone());
        valid[field]
            .as_object_mut()
            .expect("fixture field is an object")
            .insert("injected_unknown_key".to_owned(), json!(true));
        assert_invalid_normalized(tool_name, valid);
    }

    fn assert_unknown_array_item_field_rejected(
        tool_name: &str,
        shape_index: usize,
        field: &str,
        item: Value,
    ) {
        let mut valid = minimal_success(tool_name, shape_index);
        valid[field] = json!([item]);
        assert_valid_normalized(tool_name, valid.clone());
        valid[field][0]
            .as_object_mut()
            .expect("fixture item is an object")
            .insert("injected_unknown_key".to_owned(), json!(true));
        assert_invalid_normalized(tool_name, valid);
    }

    #[test]
    fn output_schema_catalog_is_exact_for_all_forty_one_tools() {
        assert_eq!(FULL_BETA_TOOLS.len(), 41);
        for name in FULL_BETA_TOOLS {
            let shapes = output_shapes(name).expect("canonical tool has output shapes");
            assert!(!shapes.is_empty(), "{name} has no successful output shape");
            let schema = Value::Object(
                output_schema(name)
                    .expect("canonical tool has output schema")
                    .as_ref()
                    .clone(),
            );
            assert_eq!(schema["type"], "object", "{name}");
            assert_eq!(schema["additionalProperties"], false, "{name}");
            assert!(schema["properties"]["error"].is_object(), "{name}");
            assert!(schema["oneOf"].is_array(), "{name}");
            assert!(schema.get("anyOf").is_none(), "{name}");
            assert_closed_schema_objects(&schema);
            assert_unique_required_arrays(&schema);
            assert_no_nonstandard_unsigned_formats(&schema);
            assert_dynamic_maps_are_explicit_and_bounded(&schema);
        }
        assert!(output_shapes("godot_unknown_tool").is_none());
        assert!(output_schema("godot_unknown_tool").is_none());
    }

    #[test]
    fn offline_tool_partition_is_exact_and_domain_complete() {
        let static_tools = OFFLINE_STATIC_TOOLS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(static_tools.len(), OFFLINE_STATIC_TOOLS.len());
        assert!(static_tools.is_subset(&FULL_BETA_TOOLS.iter().copied().collect()));
        for tool_name in FULL_BETA_TOOLS {
            match availability_domain(tool_name) {
                None => assert!(static_tools.contains(tool_name), "{tool_name}"),
                Some(AvailabilityDomain::Runtime) => {
                    assert!(RUNTIME_TOOLS.contains(tool_name), "{tool_name}")
                }
                Some(AvailabilityDomain::Editor) => {
                    assert!(!static_tools.contains(tool_name), "{tool_name}");
                    assert!(!RUNTIME_TOOLS.contains(tool_name), "{tool_name}");
                }
            }
        }
    }

    #[test]
    fn result_normalizer_enforces_equivalent_json_and_preserves_bounded_viewport_image() {
        let pixels = b"png";
        let structured = json!({
            "schema_version": "runtime/1.0",
            "runtime_session_id": "runtime:11111111111111111111111111111111",
            "runtime_event_seq": 1,
            "state": "running",
            "mime_type": "image/png",
            "width": 1,
            "height": 1,
            "byte_length": pixels.len(),
            "sha256": format!("{:x}", Sha256::digest(pixels)),
        });
        let mut result = CallToolResult::success(vec![
            ContentBlock::text("stale text"),
            ContentBlock::image(
                base64::engine::general_purpose::STANDARD.encode(pixels),
                "image/png",
            ),
            ContentBlock::text("second stale text"),
        ]);
        result.structured_content = Some(structured.clone());
        let normalized = normalize_tool_result("godot_capture_viewport", result);
        assert_eq!(normalized.structured_content, Some(structured.clone()));
        let ContentBlock::Text(text) = &normalized.content[0] else {
            panic!("first result block must be equivalent JSON text");
        };
        assert_eq!(
            serde_json::from_str::<Value>(&text.text).unwrap(),
            structured
        );
        assert_eq!(normalized.content.len(), 2);
        assert!(matches!(normalized.content[1], ContentBlock::Image(_)));
    }

    #[test]
    fn result_normalizer_rejects_unexpected_or_oversized_non_text_content() {
        let policy = json!({
            "mode": "always_ask",
            "revoked": false,
            "grant_active": false,
        });
        let mut unexpected =
            CallToolResult::success(vec![ContentBlock::image("cG5n", "image/png")]);
        unexpected.structured_content = Some(policy);
        let normalized = normalize_tool_result("godot_reset_confirmation_policy", unexpected);
        assert_invalid_tool_result(&normalized);

        let capture = json!({
            "schema_version": "runtime/1.0",
            "runtime_session_id": "runtime:11111111111111111111111111111111",
            "runtime_event_seq": 1,
            "state": "running",
            "mime_type": "image/png",
            "width": 1,
            "height": 1,
            "byte_length": MAX_CAPTURE_IMAGE_BYTES + 1,
            "sha256": "fixture",
        });
        let mut oversized = CallToolResult::success(vec![ContentBlock::image(
            "A".repeat(MAX_CAPTURE_IMAGE_BASE64_CHARS + 1),
            "image/png",
        )]);
        oversized.structured_content = Some(capture);
        let normalized = normalize_tool_result("godot_capture_viewport", oversized);
        assert_invalid_tool_result(&normalized);
    }

    #[test]
    fn top_level_collection_and_object_types_fail_closed() {
        for (tool_name, field, wrong_value) in [
            ("godot_get_runtime_tree", "nodes", json!("not-an-array")),
            ("godot_get_editor_state", "revision_vector", Value::Null),
            ("godot_get_validation_report", "limits_applied", json!([])),
            ("godot_search_symbols", "symbols", json!({})),
        ] {
            let mut result = minimal_success(tool_name, 0);
            result[field] = wrong_value;
            assert_invalid_normalized(tool_name, result);
        }
    }

    #[test]
    fn serialized_structured_content_is_bounded_before_wire_text_is_created() {
        let item = "x".repeat(MAX_BOUNDED_STRING_CHARS);
        let count = MAX_SERIALIZED_STRUCTURED_CONTENT_BYTES.div_ceil(MAX_BOUNDED_STRING_CHARS) + 1;
        let mut result = minimal_success("godot_get_editor_state", 0);
        result["capabilities_used"] =
            Value::Array((0..count).map(|_| Value::String(item.clone())).collect());
        let normalized =
            normalize_tool_result("godot_get_editor_state", CallToolResult::structured(result));
        assert_invalid_tool_result(&normalized);
        assert_eq!(
            normalized
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/error/message"))
                .and_then(Value::as_str),
            Some("The serialized tool result exceeded its bounded wire budget.")
        );
    }

    #[test]
    fn result_normalizer_fails_closed_for_missing_or_non_object_structured_content() {
        for result in [
            CallToolResult::success(vec![ContentBlock::text("unstructured")]),
            CallToolResult::structured(json!(["not", "an", "object"])),
        ] {
            let normalized = normalize_tool_result("godot_get_connection_status", result);
            assert_eq!(normalized.is_error, Some(true));
            assert_eq!(
                normalized
                    .structured_content
                    .as_ref()
                    .and_then(|value| value.pointer("/error/code")),
                Some(&json!("invalid_tool_result"))
            );
            let ContentBlock::Text(text) = &normalized.content[0] else {
                panic!("fail-closed result must contain JSON text");
            };
            assert_eq!(
                serde_json::from_str::<Value>(&text.text).unwrap(),
                normalized.structured_content.unwrap()
            );
        }
    }

    #[test]
    fn result_normalizer_rejects_wrong_types_unknown_fields_and_malformed_errors() {
        for result in [
            CallToolResult::structured(json!({
                "mode": "always_ask",
                "revoked": "false",
                "grant_active": false,
            })),
            CallToolResult::structured(json!({
                "mode": "always_ask",
                "revoked": false,
                "grant_active": false,
                "unexpected": true,
            })),
            CallToolResult::structured_error(json!({
                "error": {
                    "code": 7,
                    "message": "wrong type",
                    "retryable": true,
                }
            })),
            CallToolResult::structured_error(json!({
                "error": {
                    "code": "bad_error",
                    "message": "mixed error envelope",
                    "retryable": false,
                },
                "mode": "always_ask",
            })),
        ] {
            let normalized = normalize_tool_result("godot_reset_confirmation_policy", result);
            assert_eq!(normalized.is_error, Some(true));
            assert_eq!(
                normalized
                    .structured_content
                    .as_ref()
                    .and_then(|value| value.pointer("/error/code")),
                Some(&json!("invalid_tool_result"))
            );
        }
    }

    #[test]
    fn bounded_values_have_a_real_depth_limit() {
        let mut nested = json!("leaf");
        for _ in 0..=MAX_BOUNDED_DEPTH {
            nested = json!([nested]);
        }
        let result = CallToolResult::structured(json!({
            "mode": "always_ask",
            "grant_active": false,
            "expires_at_ms": null,
            "allowed_scopes": [],
            "max_grant_lifetime_ms": 1,
            "restrictions": nested,
        }));
        let normalized = normalize_tool_result("godot_get_confirmation_policy", result);
        assert_eq!(
            normalized
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/error/code")),
            Some(&json!("invalid_tool_result"))
        );
    }

    #[test]
    fn connection_diagnostic_and_remediation_dtos_reject_unknown_nested_fields() {
        let health = ConnectionHealth::reduce(ConnectionObservation {
            project_scope: Some("project:fixture".to_owned()),
            package_version: "0.1.0".to_owned(),
            package: PackageCondition::Ready,
            configuration: ConfigurationCondition::Ready,
            compatibility: CompatibilityEvaluation::not_observed(),
            bridge: BridgeCondition::Ready,
            negotiated_bridge_protocol: Some("1.8".to_owned()),
            bridge_capabilities: Vec::new(),
            cache: CacheCondition::Unavailable,
            cache_schema: None,
            cache_generation: None,
            cache_revisions: None,
            source_hashes_verified: false,
            cache_age_seconds: None,
            recovery: RecoveryCondition::None,
            overloaded: false,
            editor: ComponentCondition::Ready,
            runtime: ComponentCondition::Ready,
            transactions: ComponentCondition::Ready,
        });
        let mut valid = serde_json::to_value(health).expect("serialize health fixture");
        assert_valid_normalized("godot_get_connection_status", valid.clone());
        valid["diagnostic"]
            .as_object_mut()
            .expect("diagnostic DTO")
            .insert("injected_unknown_key".to_owned(), json!("leak"));
        assert_invalid_normalized("godot_get_connection_status", valid);
    }

    #[test]
    fn revision_evidence_editor_and_runtime_dtos_reject_unknown_nested_fields() {
        assert_unknown_object_field_rejected(
            "godot_get_editor_state",
            0,
            "revision_vector",
            json!({"event_seq": 1}),
        );
        assert_unknown_array_item_field_rejected(
            "godot_get_editor_state",
            0,
            "evidence",
            json!({"source": "live_editor_snapshot", "freshness": "current"}),
        );
        assert_unknown_object_field_rejected(
            "godot_get_editor_state",
            0,
            "editor",
            json!({"kind": "editor_state", "entity_id": "editor:fixture"}),
        );
        assert_unknown_array_item_field_rejected(
            "godot_get_runtime_tree",
            0,
            "nodes",
            json!({"runtime_object_id": "runtime-object:fixture", "name": "Player"}),
        );
        assert_unknown_array_item_field_rejected(
            "godot_inspect_runtime_object",
            0,
            "properties",
            json!({"name": "position", "value": {"project_axis": [1, 2]}}),
        );
        assert_unknown_object_field_rejected(
            "godot_get_stack_trace",
            0,
            "stack",
            json!({"runtime_stack_id": "runtime-stack:fixture", "frames": []}),
        );

        // The screenshot metadata DTO is intentionally flat. Its equivalent
        // adversarial case is therefore an unknown field on that closed DTO.
        let mut capture = minimal_success("godot_capture_viewport", 0);
        assert_valid_normalized("godot_capture_viewport", capture.clone());
        capture["injected_unknown_key"] = json!(true);
        assert_invalid_normalized("godot_capture_viewport", capture);
    }

    #[test]
    fn saved_semantic_and_transaction_dtos_reject_unknown_nested_fields() {
        assert_unknown_object_field_rejected(
            "godot_get_resource_dependencies",
            0,
            "resource",
            json!({"entity_id": "resource:fixture", "display_path": "res://fixture.tres"}),
        );
        assert_unknown_object_field_rejected(
            "godot_get_scene_graph",
            0,
            "scene",
            json!({"scene_entity_id": "scene:fixture", "comparison_path": "main.tscn"}),
        );
        assert_unknown_array_item_field_rejected(
            "godot_search_symbols",
            0,
            "symbols",
            json!({"symbol_id": "symbol:fixture", "qualified_name": "Player.attack"}),
        );
        assert_unknown_array_item_field_rejected(
            "godot_find_usages",
            0,
            "usages",
            json!({"usage": "resource_reference", "source": "res://main.tscn"}),
        );
        assert_unknown_object_field_rejected(
            "godot_prepare_create_node",
            0,
            "preview",
            json!({"operation_kind": "create_node", "summary": "Create Player"}),
        );
        assert_unknown_object_field_rejected(
            "godot_prepare_change_set",
            0,
            "preview",
            json!({"operation_count": 1, "summary": "Atomic scene update"}),
        );
        assert_unknown_object_field_rejected(
            "godot_get_transaction_status",
            0,
            "undo_eligibility",
            json!({"eligible": true, "reason": "eligible"}),
        );
        assert_unknown_object_field_rejected(
            "godot_get_validation_report",
            0,
            "limits_applied",
            json!({"content_bytes": 8, "truncated": false}),
        );
        assert_unknown_object_field_rejected(
            "godot_apply_transaction",
            1,
            "preview",
            json!({"operation_count": 1, "summary": "Committed scene update"}),
        );
        assert_unknown_object_field_rejected(
            "godot_undo_transaction",
            0,
            "undo_eligibility",
            json!({"eligible": false, "reason": "not_newest_action"}),
        );
    }

    #[test]
    fn project_defined_semantic_dictionaries_remain_explicit_and_bounded() {
        let mut valid = minimal_success("godot_inspect_runtime_object", 0);
        valid["properties"] = json!([{
            "name": "metadata",
            "value": {
                "project_custom_key": {
                    "nested_project_key": [1, true, "bounded"]
                }
            }
        }]);
        assert_valid_normalized("godot_inspect_runtime_object", valid.clone());

        valid["properties"][0]
            .as_object_mut()
            .expect("fixed runtime property DTO")
            .insert("injected_unknown_key".to_owned(), json!(true));
        assert_invalid_normalized("godot_inspect_runtime_object", valid);
    }
}
