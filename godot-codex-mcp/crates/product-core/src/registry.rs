use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Canonical machine-readable registry consumed by product and acceptance.
pub const REGISTRY_PROFILE_JSON: &str = include_str!("../../../product/registry-profile.v1.json");

/// Exact External Codex Beta tool registry, sorted by public MCP tool name.
pub const FULL_BETA_TOOLS: &[&str] = &[
    "godot_apply_transaction",
    "godot_capture_viewport",
    "godot_continue_project",
    "godot_find_resource_owners",
    "godot_find_usages",
    "godot_get_confirmation_policy",
    "godot_get_connection_status",
    "godot_get_current_scene",
    "godot_get_diagnostics",
    "godot_get_editor_history",
    "godot_get_editor_state",
    "godot_get_inspector_state",
    "godot_get_open_scenes",
    "godot_get_open_scripts",
    "godot_get_resource_dependencies",
    "godot_get_runtime_tree",
    "godot_get_scene_graph",
    "godot_get_selected_nodes",
    "godot_get_stack_trace",
    "godot_get_transaction_status",
    "godot_get_validation_report",
    "godot_get_viewport_state",
    "godot_inspect_node",
    "godot_inspect_runtime_object",
    "godot_inspect_symbol",
    "godot_pause_project",
    "godot_prepare_attach_script",
    "godot_prepare_change_set",
    "godot_prepare_connect_signal",
    "godot_prepare_create_node",
    "godot_prepare_delete_node",
    "godot_prepare_detach_script",
    "godot_prepare_disconnect_signal",
    "godot_prepare_reparent_node",
    "godot_prepare_set_property",
    "godot_reset_confirmation_policy",
    "godot_run_current_scene",
    "godot_run_project",
    "godot_search_symbols",
    "godot_stop_project",
    "godot_undo_transaction",
];

/// Closed read-only setup profile. Operations that alter editor/runtime state,
/// allocate prepared mutation state, or narrow confirmation policy are absent.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "godot_capture_viewport",
    "godot_find_resource_owners",
    "godot_find_usages",
    "godot_get_confirmation_policy",
    "godot_get_connection_status",
    "godot_get_current_scene",
    "godot_get_diagnostics",
    "godot_get_editor_history",
    "godot_get_editor_state",
    "godot_get_inspector_state",
    "godot_get_open_scenes",
    "godot_get_open_scripts",
    "godot_get_resource_dependencies",
    "godot_get_runtime_tree",
    "godot_get_scene_graph",
    "godot_get_selected_nodes",
    "godot_get_stack_trace",
    "godot_get_transaction_status",
    "godot_get_validation_report",
    "godot_get_viewport_state",
    "godot_inspect_node",
    "godot_inspect_runtime_object",
    "godot_inspect_symbol",
    "godot_search_symbols",
];

/// Exact fixed-resource registry, sorted by URI.
pub const FIXED_RESOURCE_URIS: &[&str] = &[
    "godot://connection/status",
    "godot://editor/summary",
    "godot://project/summary",
    "godot://runtime/summary",
];

/// Exact resource-template registry, sorted by URI template.
pub const RESOURCE_TEMPLATE_URIS: &[&str] = &["godot://scene/{scene_id}/summary"];

/// Serializable metadata used by setup, doctor, MCP tests, and packaging.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryProfile {
    pub schema_version: String,
    pub profile_id: String,
    pub tool_count: usize,
    pub fixed_resource_count: usize,
    pub resource_template_count: usize,
    pub tools: Vec<String>,
    pub read_only_tools: Vec<String>,
    pub fixed_resources: Vec<String>,
    pub resource_templates: Vec<String>,
    pub digest: String,
}

#[derive(Serialize)]
struct RegistryDigestPayload<'a> {
    profile_id: &'a str,
    tools: &'a [&'a str],
    read_only_tools: &'a [&'a str],
    fixed_resources: &'a [&'a str],
    resource_templates: &'a [&'a str],
}

/// Returns a stable SHA-256 over the exact public names and profile membership.
#[must_use]
pub fn registry_profile_digest() -> String {
    let payload = RegistryDigestPayload {
        profile_id: "external-codex-beta-v1",
        tools: FULL_BETA_TOOLS,
        read_only_tools: READ_ONLY_TOOLS,
        fixed_resources: FIXED_RESOURCE_URIS,
        resource_templates: RESOURCE_TEMPLATE_URIS,
    };
    let bytes = serde_json::to_vec(&payload).expect("static registry payload is serializable");
    format!("{:x}", Sha256::digest(bytes))
}

/// Builds the canonical registry profile. Returned collections are owned so
/// the value can cross crate/process boundaries without lifetime coupling.
#[must_use]
pub fn canonical_registry_profile() -> RegistryProfile {
    serde_json::from_str(REGISTRY_PROFILE_JSON)
        .expect("embedded registry profile is validated by product-core tests")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn external_beta_registry_is_exact_unique_and_sorted() {
        let profile = canonical_registry_profile();
        assert_eq!(FULL_BETA_TOOLS.len(), 41);
        assert_eq!(FIXED_RESOURCE_URIS.len(), 4);
        assert_eq!(RESOURCE_TEMPLATE_URIS.len(), 1);
        assert!(FULL_BETA_TOOLS.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(FIXED_RESOURCE_URIS.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            FULL_BETA_TOOLS
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len(),
            FULL_BETA_TOOLS.len()
        );
        assert!(FULL_BETA_TOOLS.contains(&"godot_get_connection_status"));
        assert!(FIXED_RESOURCE_URIS.contains(&"godot://connection/status"));
        assert_eq!(
            profile.tools,
            FULL_BETA_TOOLS
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            profile.read_only_tools,
            READ_ONLY_TOOLS
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            profile.fixed_resources,
            FIXED_RESOURCE_URIS
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            profile.resource_templates,
            RESOURCE_TEMPLATE_URIS
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn read_only_profile_is_an_exact_subset() {
        let full = FULL_BETA_TOOLS.iter().copied().collect::<BTreeSet<_>>();
        let read_only = READ_ONLY_TOOLS.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(read_only.len(), READ_ONLY_TOOLS.len());
        assert!(read_only.is_subset(&full));
        assert!(!read_only.contains("godot_apply_transaction"));
        assert!(!read_only.contains("godot_run_project"));
        assert!(!read_only.contains("godot_prepare_change_set"));
        assert!(read_only.contains("godot_get_connection_status"));
    }

    #[test]
    fn profile_digest_is_stable_and_lowercase_sha256() {
        let first = canonical_registry_profile();
        let second = canonical_registry_profile();
        assert_eq!(first, second);
        assert_eq!(first.digest, registry_profile_digest());
        assert_eq!(first.schema_version, "godot-codex-registry-profile/1.0");
        assert_eq!(first.profile_id, "external-codex-beta-v1");
        assert_eq!(first.tool_count, first.tools.len());
        assert_eq!(first.fixed_resource_count, first.fixed_resources.len());
        assert_eq!(
            first.resource_template_count,
            first.resource_templates.len()
        );
        assert_eq!(first.digest.len(), 64);
        assert!(
            first
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
    }

    #[test]
    fn committed_registry_artifact_matches_the_product_projection() {
        let committed: RegistryProfile =
            serde_json::from_str(crate::REGISTRY_PROFILE_JSON).unwrap();
        assert_eq!(committed, canonical_registry_profile());
        assert_eq!(committed.digest, registry_profile_digest());
    }
}
