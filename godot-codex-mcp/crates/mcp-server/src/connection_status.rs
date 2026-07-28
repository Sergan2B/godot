use std::collections::BTreeSet;

use godot_codex_product::{
    BridgeCondition, CacheCondition, CacheRevisions, CompatibilityEvaluation, ComponentCondition,
    ConnectionHealth, ConnectionObservation, ProductStartupObservation, ProtocolVersion,
    RecoveryCondition, embedded_compatibility_matrix,
};
use godot_codex_semantic_model::NegotiatedBridgeMetadata;
use serde::Deserialize;

pub(crate) const CONNECTION_STATUS_URI: &str = "godot://connection/status";

#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConnectionStatusInput {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IndexCoordinates {
    pub project_id: String,
    pub generation_id: String,
    pub index_revision: u64,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub script_graph_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum IndexObservation {
    Current(IndexCoordinates),
    ProjectSessionBusy,
    Degraded,
    Syncing,
    Offline,
    Stale,
    Disconnected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReplicaObservation {
    Ready { project_id: String },
    BridgeReady { project_id: String },
    Connecting,
    Syncing,
    Stale,
    TransportDisconnected,
    ProtocolIncompatible,
    AuthenticationFailed,
    ProjectBindingMismatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServerConnectionObservation {
    pub product: ProductStartupObservation,
    pub replica: ReplicaObservation,
    pub negotiated_bridge: Option<NegotiatedBridgeMetadata>,
    pub resource_index: IndexObservation,
    pub scene_index: IndexObservation,
    pub script_index: IndexObservation,
    /// True only after an offline generation's saved-source manifest has been
    /// independently verified. An online current index does not set this.
    pub static_cache_verified: bool,
    pub runtime: ComponentCondition,
    pub transactions_available: bool,
    pub project_session_busy: bool,
    pub transaction_recovery_degraded: bool,
    pub cache_age_seconds: Option<u64>,
}

#[derive(Clone, Debug)]
struct IndexProjection {
    condition: CacheCondition,
    generation_id: Option<String>,
    revisions: Option<CacheRevisions>,
    project_id: Option<String>,
    project_binding_mismatch: bool,
    project_session_busy: bool,
}

pub(crate) fn connection_health(observation: ServerConnectionObservation) -> ConnectionHealth {
    let index = index_projection(&observation);
    let project_session_busy = observation.project_session_busy || index.project_session_busy;
    let mut bridge = bridge_condition(&observation.replica);
    if !matches!(
        bridge,
        BridgeCondition::VersionIncompatible
            | BridgeCondition::AuthenticationFailed
            | BridgeCondition::ProjectBindingMismatch
    ) && (index.project_binding_mismatch
        || matches!(
            &observation.replica,
            ReplicaObservation::Ready { project_id }
            | ReplicaObservation::BridgeReady { project_id }
                if index.project_id.as_deref().is_some_and(|indexed| indexed != project_id)
        ))
    {
        bridge = BridgeCondition::ProjectBindingMismatch;
    }

    let editor = editor_condition(&observation.replica);
    let transactions = if project_session_busy {
        ComponentCondition::Unavailable
    } else if observation.transaction_recovery_degraded {
        ComponentCondition::Degraded
    } else if !observation.transactions_available {
        ComponentCondition::Unavailable
    } else {
        editor
    };
    let cache = if project_session_busy {
        CacheCondition::Unavailable
    } else if observation.static_cache_verified && index.condition == CacheCondition::OnlineCurrent
    {
        CacheCondition::VerifiedCurrent
    } else if bridge != BridgeCondition::Ready && index.condition == CacheCondition::OnlineCurrent {
        // Online freshness cannot survive loss of its editor authority.
        CacheCondition::Stale
    } else {
        index.condition
    };
    let project_id = match &observation.replica {
        ReplicaObservation::Ready { project_id }
        | ReplicaObservation::BridgeReady { project_id } => Some(project_id.as_str()),
        ReplicaObservation::Connecting
        | ReplicaObservation::Syncing
        | ReplicaObservation::Stale
        | ReplicaObservation::TransportDisconnected
        | ReplicaObservation::ProtocolIncompatible
        | ReplicaObservation::AuthenticationFailed
        | ReplicaObservation::ProjectBindingMismatch => index.project_id.as_deref(),
    };

    ConnectionHealth::reduce(ConnectionObservation {
        project_scope: project_id.map(project_scope),
        package_version: observation.product.package_version.clone(),
        package: observation.product.package,
        configuration: observation.product.configuration,
        compatibility: compatibility_evaluation(
            &observation.product,
            observation.negotiated_bridge.as_ref(),
        ),
        bridge,
        negotiated_bridge_protocol: observation
            .negotiated_bridge
            .as_ref()
            .map(|metadata| metadata.protocol_version.clone()),
        bridge_capabilities: observation
            .negotiated_bridge
            .as_ref()
            .map(|metadata| metadata.capabilities.iter().cloned().collect())
            .unwrap_or_default(),
        cache,
        cache_schema: (!matches!(cache, CacheCondition::Unknown | CacheCondition::Unavailable))
            .then(|| {
                format!(
                    "{}.{}",
                    godot_codex_index_store::LOGICAL_SCHEMA_V1.major,
                    godot_codex_index_store::LOGICAL_SCHEMA_V1.minor
                )
            }),
        cache_generation: matches!(
            cache,
            CacheCondition::OnlineCurrent | CacheCondition::VerifiedCurrent
        )
        .then_some(index.generation_id)
        .flatten(),
        cache_revisions: matches!(
            cache,
            CacheCondition::OnlineCurrent | CacheCondition::VerifiedCurrent
        )
        .then_some(index.revisions)
        .flatten(),
        source_hashes_verified: observation.static_cache_verified
            && cache == CacheCondition::VerifiedCurrent,
        cache_age_seconds: observation.cache_age_seconds,
        recovery: if observation.transaction_recovery_degraded {
            RecoveryCondition::InDoubt
        } else {
            RecoveryCondition::None
        },
        overloaded: false,
        project_session_busy,
        editor,
        runtime: if project_session_busy {
            ComponentCondition::Unavailable
        } else {
            observation.runtime
        },
        transactions,
    })
}

fn compatibility_evaluation(
    product: &ProductStartupObservation,
    negotiated: Option<&NegotiatedBridgeMetadata>,
) -> CompatibilityEvaluation {
    let (Some(basis), Some(negotiated)) = (product.compatibility_basis.as_ref(), negotiated) else {
        return CompatibilityEvaluation::not_observed();
    };
    let bridge = ProtocolVersion::parse(&negotiated.protocol_version)
        .unwrap_or(ProtocolVersion { major: 0, minor: 0 });
    let observation = basis.with_bridge(bridge, negotiated.capabilities.clone());
    embedded_compatibility_matrix()
        .and_then(|matrix| matrix.evaluate_product(&observation))
        .unwrap_or_else(|_| CompatibilityEvaluation::not_observed())
}

fn bridge_condition(observation: &ReplicaObservation) -> BridgeCondition {
    match observation {
        ReplicaObservation::Ready { .. } | ReplicaObservation::BridgeReady { .. } => {
            BridgeCondition::Ready
        }
        ReplicaObservation::Connecting => BridgeCondition::Starting,
        ReplicaObservation::Syncing => BridgeCondition::Syncing,
        ReplicaObservation::Stale => BridgeCondition::DiscoveryStale,
        ReplicaObservation::TransportDisconnected => BridgeCondition::Offline,
        ReplicaObservation::ProtocolIncompatible => BridgeCondition::VersionIncompatible,
        ReplicaObservation::AuthenticationFailed => BridgeCondition::AuthenticationFailed,
        ReplicaObservation::ProjectBindingMismatch => BridgeCondition::ProjectBindingMismatch,
    }
}

fn editor_condition(observation: &ReplicaObservation) -> ComponentCondition {
    match observation {
        ReplicaObservation::Ready { .. } | ReplicaObservation::BridgeReady { .. } => {
            ComponentCondition::Ready
        }
        ReplicaObservation::Connecting | ReplicaObservation::Syncing => ComponentCondition::Syncing,
        ReplicaObservation::Stale => ComponentCondition::Stale,
        ReplicaObservation::TransportDisconnected => ComponentCondition::Disconnected,
        ReplicaObservation::ProtocolIncompatible
        | ReplicaObservation::AuthenticationFailed
        | ReplicaObservation::ProjectBindingMismatch => ComponentCondition::Degraded,
    }
}

fn index_projection(observation: &ServerConnectionObservation) -> IndexProjection {
    let observations = [
        &observation.resource_index,
        &observation.scene_index,
        &observation.script_index,
    ];
    let coordinates = observations
        .iter()
        .filter_map(|status| match status {
            IndexObservation::Current(coordinates) => Some(coordinates),
            IndexObservation::ProjectSessionBusy
            | IndexObservation::Degraded
            | IndexObservation::Syncing
            | IndexObservation::Offline
            | IndexObservation::Stale
            | IndexObservation::Disconnected => None,
        })
        .collect::<Vec<_>>();
    let all_current = coordinates.len() == observations.len();
    let project_binding_mismatch = all_current
        && coordinates.first().is_some_and(|expected| {
            coordinates
                .iter()
                .skip(1)
                .any(|candidate| candidate.project_id != expected.project_id)
        });
    let coordinate_skew = all_current
        && !project_binding_mismatch
        && (coordinates.first().is_some_and(|expected| {
            coordinates.iter().skip(1).any(|candidate| {
                candidate.generation_id != expected.generation_id
                    || candidate.index_revision != expected.index_revision
                    || candidate.resource_revision != expected.resource_revision
            })
        }) || distinct_nonzero_revision_count(
            coordinates
                .iter()
                .map(|coordinates| coordinates.scene_graph_revision),
        ) > 1
            || distinct_nonzero_revision_count(
                coordinates
                    .iter()
                    .map(|coordinates| coordinates.script_graph_revision),
            ) > 1);
    if all_current && !project_binding_mismatch && !coordinate_skew {
        let current = coordinates[0];
        return IndexProjection {
            condition: CacheCondition::OnlineCurrent,
            generation_id: Some(current.generation_id.clone()),
            revisions: Some(CacheRevisions {
                index_revision: current.index_revision,
                resource_revision: coordinates
                    .iter()
                    .map(|coordinates| coordinates.resource_revision)
                    .max()
                    .unwrap_or_default(),
                scene_graph_revision: coordinates
                    .iter()
                    .map(|coordinates| coordinates.scene_graph_revision)
                    .max()
                    .unwrap_or_default(),
                script_graph_revision: coordinates
                    .iter()
                    .map(|coordinates| coordinates.script_graph_revision)
                    .max()
                    .unwrap_or_default(),
            }),
            project_id: Some(current.project_id.clone()),
            project_binding_mismatch: false,
            project_session_busy: false,
        };
    }
    let condition = if project_binding_mismatch
        || observations
            .iter()
            .any(|status| matches!(status, IndexObservation::Degraded))
    {
        CacheCondition::Unavailable
    } else if coordinate_skew {
        CacheCondition::Rebuilding
    } else if observations.iter().any(|status| {
        matches!(
            status,
            IndexObservation::Stale | IndexObservation::Disconnected
        )
    }) {
        CacheCondition::Stale
    } else if observations
        .iter()
        .any(|status| matches!(status, IndexObservation::Syncing))
    {
        CacheCondition::Rebuilding
    } else {
        CacheCondition::Unavailable
    };
    IndexProjection {
        condition,
        generation_id: None,
        revisions: None,
        project_id: None,
        project_binding_mismatch,
        project_session_busy: observations
            .iter()
            .any(|status| matches!(status, IndexObservation::ProjectSessionBusy)),
    }
}

fn distinct_nonzero_revision_count(revisions: impl Iterator<Item = u64>) -> usize {
    revisions
        .filter(|revision| *revision != 0)
        .collect::<BTreeSet<_>>()
        .len()
}

fn project_scope(project_id: &str) -> String {
    project_id
        .strip_prefix("project:sha256:")
        .unwrap_or(project_id)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use godot_codex_product::{
        CONNECTION_STATUS_MAX_BYTES, ConfigurationCondition, ConnectionStatus, DiagnosticCode,
        PackageCondition, ProductCompatibilityBasis, ProductStartupObservation,
        embedded_compatibility_matrix,
    };

    use super::*;

    fn coordinates(project_id: &str) -> IndexCoordinates {
        IndexCoordinates {
            project_id: project_id.to_owned(),
            generation_id: format!("generation:sha256:{}", "2".repeat(64)),
            index_revision: 9,
            resource_revision: 7,
            scene_graph_revision: 5,
            script_graph_revision: 3,
        }
    }

    fn ready_product() -> ProductStartupObservation {
        let matrix = embedded_compatibility_matrix().unwrap();
        ProductStartupObservation {
            package_version: matrix.package.version.clone(),
            package: PackageCondition::Ready,
            configuration: ConfigurationCondition::Ready,
            compatibility_basis: Some(ProductCompatibilityBasis {
                package_version: matrix.package.version.clone(),
                package_manifest_verified: true,
                target: matrix.package.target,
                godot_source_commit: matrix.godot.source_commit.clone(),
                godot_build_id: matrix.godot.build_id.clone(),
                godot_artifact_sha256: matrix.godot.artifact_sha256.clone(),
                mcp_protocol: matrix.protocols.mcp_protocol.clone(),
                index_schema: matrix.schemas.index.clone(),
                transaction_journal_schema: matrix.schemas.transaction_journal.clone(),
                validation_report_schema: matrix.schemas.validation_report.clone(),
            }),
        }
    }

    fn negotiated_bridge() -> NegotiatedBridgeMetadata {
        let matrix = embedded_compatibility_matrix().unwrap();
        let profile = matrix
            .bridge_profiles
            .iter()
            .find(|profile| profile.bridge_minor == matrix.protocols.bridge_current_minor)
            .unwrap();
        NegotiatedBridgeMetadata::new(
            format!(
                "{}.{}",
                matrix.protocols.bridge_major, matrix.protocols.bridge_current_minor
            ),
            profile.capabilities.iter().cloned(),
        )
        .unwrap()
    }

    fn ready_observation() -> ServerConnectionObservation {
        let project_id = format!("project:sha256:{}", "1".repeat(64));
        ServerConnectionObservation {
            product: ready_product(),
            replica: ReplicaObservation::Ready {
                project_id: project_id.clone(),
            },
            negotiated_bridge: Some(negotiated_bridge()),
            resource_index: IndexObservation::Current(coordinates(&project_id)),
            scene_index: IndexObservation::Current(coordinates(&project_id)),
            script_index: IndexObservation::Current(coordinates(&project_id)),
            static_cache_verified: false,
            runtime: ComponentCondition::Ready,
            transactions_available: true,
            project_session_busy: false,
            transaction_recovery_degraded: false,
            cache_age_seconds: Some(7),
        }
    }

    #[test]
    fn ready_health_uses_the_shared_bounded_projection() {
        let health = connection_health(ready_observation());
        assert_eq!(health.status, ConnectionStatus::Ready);
        assert_eq!(health.diagnostic.code, DiagnosticCode::Ready);
        assert_eq!(health.bridge.condition, BridgeCondition::Ready);
        assert_eq!(health.bridge.negotiated_protocol.as_deref(), Some("1.8"));
        assert!(
            health
                .bridge
                .capabilities
                .contains(&"bridge.lifecycle".to_owned())
        );
        assert_eq!(health.static_cache.condition, CacheCondition::OnlineCurrent);
        assert_eq!(health.static_cache.age_seconds, Some(7));
        assert_eq!(health.components.editor, ComponentCondition::Ready);
        assert_eq!(health.components.runtime, ComponentCondition::Ready);
        assert_eq!(health.components.transactions, ComponentCondition::Ready);
        assert!(!health.static_cache.source_hashes_verified);
        assert!(health.static_cache.revisions.is_some());
        assert_eq!(
            health.compatibility.status,
            godot_codex_product::CompatibilityStatus::Supported
        );
        assert!(!health.compatibility.surface_observed);

        let text = serde_json::to_string(&health).unwrap();
        assert!(text.len() <= CONNECTION_STATUS_MAX_BYTES);
        for forbidden in [
            "session_token",
            "endpoint",
            "native_handle",
            "approval_receipt",
            "/Users/",
            "res://",
        ] {
            assert!(
                !text.contains(forbidden),
                "leaked forbidden field: {forbidden}"
            );
        }
    }

    #[test]
    fn degraded_and_stale_states_never_claim_ready() {
        let mut observation = ready_observation();
        observation.script_index = IndexObservation::Degraded;
        let degraded = connection_health(observation.clone());
        assert_eq!(degraded.status, ConnectionStatus::Syncing);
        assert_eq!(degraded.static_cache.condition, CacheCondition::Unavailable);
        assert_eq!(
            degraded.diagnostic.code,
            DiagnosticCode::StaticCacheUnavailable
        );

        observation = ready_observation();
        observation.replica = ReplicaObservation::Stale;
        let stale = connection_health(observation);
        assert_eq!(stale.status, ConnectionStatus::Syncing);
        assert_eq!(stale.diagnostic.code, DiagnosticCode::BridgeDiscoveryStale);
        assert_eq!(stale.components.editor, ComponentCondition::Stale);
    }

    #[test]
    fn same_project_lease_contention_is_not_reported_as_syncing() {
        let mut observation = ready_observation();
        observation.resource_index = IndexObservation::ProjectSessionBusy;
        observation.scene_index = IndexObservation::ProjectSessionBusy;
        observation.script_index = IndexObservation::ProjectSessionBusy;
        observation.transactions_available = false;
        observation.project_session_busy = true;
        let health = connection_health(observation);
        assert_eq!(health.status, ConnectionStatus::ProjectSessionBusy);
        assert_eq!(health.diagnostic.code, DiagnosticCode::ProjectSessionBusy);
        assert_eq!(health.bridge.condition, BridgeCondition::Ready);
        assert_eq!(health.components.editor, ComponentCondition::Ready);
        assert_eq!(health.static_cache.condition, CacheCondition::Unavailable);
        assert_eq!(health.components.runtime, ComponentCondition::Unavailable);
        assert_eq!(
            health.components.transactions,
            ComponentCondition::Unavailable
        );
        assert_ne!(
            health.remediation_id,
            Some(godot_codex_product::RemediationId::WaitForFullSync)
        );
    }

    #[test]
    fn offline_cache_requires_independent_source_verification() {
        let mut observation = ready_observation();
        observation.replica = ReplicaObservation::TransportDisconnected;
        let empty = connection_health(observation.clone());
        assert_eq!(empty.status, ConnectionStatus::OfflineEmpty);
        assert_eq!(empty.static_cache.condition, CacheCondition::Stale);
        assert!(!empty.static_cache.source_hashes_verified);

        observation.static_cache_verified = true;
        let cached = connection_health(observation);
        assert_eq!(cached.status, ConnectionStatus::OfflineCached);
        assert_eq!(
            cached.static_cache.condition,
            CacheCondition::VerifiedCurrent
        );
        assert!(cached.static_cache.source_hashes_verified);
    }

    #[test]
    fn connecting_auth_and_incompatible_states_remain_distinct() {
        let mut observation = ready_observation();
        observation.replica = ReplicaObservation::Connecting;
        let connecting = connection_health(observation.clone());
        assert_eq!(connecting.status, ConnectionStatus::Connecting);
        assert_eq!(connecting.diagnostic.code, DiagnosticCode::BridgeConnecting);

        observation.replica = ReplicaObservation::AuthenticationFailed;
        let auth = connection_health(observation.clone());
        assert_eq!(auth.status, ConnectionStatus::AuthFailed);
        assert_eq!(
            auth.diagnostic.code,
            DiagnosticCode::BridgeAuthenticationFailed
        );

        observation.replica = ReplicaObservation::ProtocolIncompatible;
        let incompatible = connection_health(observation);
        assert_eq!(incompatible.status, ConnectionStatus::Incompatible);
        assert_eq!(
            incompatible.diagnostic.code,
            DiagnosticCode::BridgeVersionIncompatible
        );
    }

    #[test]
    fn package_and_configuration_faults_use_canonical_diagnostics() {
        let mut observation = ready_observation();
        observation.product.package = PackageCondition::Missing;
        let package = connection_health(observation.clone());
        assert_eq!(package.status, ConnectionStatus::Misconfigured);
        assert_eq!(package.diagnostic.code, DiagnosticCode::BinaryMissing);

        observation.product.package = PackageCondition::Ready;
        observation.product.configuration = ConfigurationCondition::Invalid;
        let configuration = connection_health(observation);
        assert_eq!(configuration.status, ConnectionStatus::Misconfigured);
        assert_eq!(
            configuration.diagnostic.code,
            DiagnosticCode::ProjectConfigInvalid
        );
    }

    #[test]
    fn project_binding_mismatch_fails_closed() {
        let mut observation = ready_observation();
        observation.replica = ReplicaObservation::ProjectBindingMismatch;
        observation.static_cache_verified = true;
        let health = connection_health(observation);
        assert_eq!(health.status, ConnectionStatus::AuthFailed);
        assert_eq!(
            health.diagnostic.code,
            DiagnosticCode::ProjectBindingMismatch
        );
        assert_eq!(
            health.static_cache.condition,
            CacheCondition::VerifiedCurrent
        );
        assert!(health.static_cache.source_hashes_verified);
    }

    #[test]
    fn inconsistent_index_binding_still_fails_closed() {
        let mut observation = ready_observation();
        observation.scene_index =
            IndexObservation::Current(coordinates(&format!("project:sha256:{}", "f".repeat(64))));
        let health = connection_health(observation);
        assert_eq!(health.status, ConnectionStatus::AuthFailed);
        assert_eq!(
            health.diagnostic.code,
            DiagnosticCode::ProjectBindingMismatch
        );
        assert!(health.static_cache.revisions.is_none());
    }

    #[test]
    fn transient_index_coordinate_skew_is_rebuilding_not_project_mismatch() {
        let mut observation = ready_observation();
        let mut rebuilding = coordinates(&format!("project:sha256:{}", "1".repeat(64)));
        rebuilding.generation_id = format!("generation:sha256:{}", "3".repeat(64));
        rebuilding.index_revision += 1;
        observation.scene_index = IndexObservation::Current(rebuilding);

        let health = connection_health(observation);
        assert_eq!(health.status, ConnectionStatus::Syncing);
        assert_eq!(
            health.diagnostic.code,
            DiagnosticCode::StaticCacheRebuilding
        );
        assert_eq!(health.bridge.condition, BridgeCondition::Ready);
        assert_eq!(health.components.editor, ComponentCondition::Ready);
        assert_eq!(health.static_cache.condition, CacheCondition::Rebuilding);
        assert!(health.static_cache.revisions.is_none());
    }

    #[test]
    fn typed_live_failure_precedes_unrelated_index_inconsistency() {
        let mut observation = ready_observation();
        observation.replica = ReplicaObservation::AuthenticationFailed;
        observation.scene_index =
            IndexObservation::Current(coordinates(&format!("project:sha256:{}", "f".repeat(64))));
        let health = connection_health(observation);
        assert_eq!(health.status, ConnectionStatus::AuthFailed);
        assert_eq!(
            health.diagnostic.code,
            DiagnosticCode::BridgeAuthenticationFailed
        );
        assert_eq!(
            health.bridge.condition,
            BridgeCondition::AuthenticationFailed
        );
    }

    #[test]
    fn severe_live_failures_are_never_downgraded_by_verified_offline_cache() {
        for (replica, status, code) in [
            (
                ReplicaObservation::AuthenticationFailed,
                ConnectionStatus::AuthFailed,
                DiagnosticCode::BridgeAuthenticationFailed,
            ),
            (
                ReplicaObservation::ProjectBindingMismatch,
                ConnectionStatus::AuthFailed,
                DiagnosticCode::ProjectBindingMismatch,
            ),
            (
                ReplicaObservation::ProtocolIncompatible,
                ConnectionStatus::Incompatible,
                DiagnosticCode::BridgeVersionIncompatible,
            ),
        ] {
            let mut observation = ready_observation();
            observation.replica = replica;
            observation.static_cache_verified = true;
            let health = connection_health(observation);
            assert_eq!(health.status, status);
            assert_eq!(health.diagnostic.code, code);
            assert_eq!(
                health.static_cache.condition,
                CacheCondition::VerifiedCurrent
            );
        }
    }

    #[test]
    fn unsafe_project_identity_is_redacted_by_product_core() {
        let mut observation = ready_observation();
        let unsafe_project = "/Users/private/project?token=secret".to_owned();
        observation.replica = ReplicaObservation::Ready {
            project_id: unsafe_project.clone(),
        };
        observation.resource_index = IndexObservation::Current(coordinates(&unsafe_project));
        observation.scene_index = IndexObservation::Current(coordinates(&unsafe_project));
        observation.script_index = IndexObservation::Current(coordinates(&unsafe_project));
        let health = connection_health(observation);
        let text = serde_json::to_string(&health).unwrap();
        assert_eq!(health.project_scope.as_deref(), Some("[redacted]"));
        assert!(!text.contains("/Users/"));
        assert!(!text.contains("secret"));
    }
}
