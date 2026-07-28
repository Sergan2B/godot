use serde::{Deserialize, Serialize};

use crate::compatibility::{
    CompatibilityEvaluation, CompatibilityStatus, ProductCompatibilityBasis,
};
use crate::diagnostics::{Diagnostic, DiagnosticCode, RemediationId};
use crate::{CONNECTION_STATUS_MAX_BYTES, CONNECTION_STATUS_SCHEMA};

const MAX_CAPABILITIES: usize = 64;
const MAX_CACHE_AGE_SECONDS: u64 = 31_536_000;

/// Package layer observed by setup, doctor, or server initialization.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PackageCondition {
    Ready,
    Missing,
    NotExecutable,
    ArchitectureMismatch,
    Invalid,
}

/// Project/configuration layer observed without reading private host trust
/// internals.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationCondition {
    Ready,
    ProjectInvalid,
    Missing,
    Invalid,
    NotEffective,
    TrustOrRestartRequired,
}

/// Immutable startup facts shared by doctor and the MCP server. Filesystem
/// inspection belongs to operations; this product-core value contains no
/// paths, process identifiers, endpoints, or secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProductStartupObservation {
    pub package_version: String,
    pub package: PackageCondition,
    pub configuration: ConfigurationCondition,
    pub compatibility_basis: Option<ProductCompatibilityBasis>,
}

impl ProductStartupObservation {
    /// Fail-closed startup state for constructors without a verified package
    /// and exact project configuration.
    #[must_use]
    pub fn unverified(package_version: impl Into<String>) -> Self {
        Self {
            package_version: package_version.into(),
            package: PackageCondition::Invalid,
            configuration: ConfigurationCondition::Missing,
            compatibility_basis: None,
        }
    }
}

/// Authenticated Bridge lifecycle condition. Severe failures are distinct and
/// never collapsed into generic offline state.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BridgeCondition {
    Starting,
    Discovering,
    DiscoveryStale,
    Unreachable,
    VersionIncompatible,
    AuthenticationFailed,
    ProjectBindingMismatch,
    Syncing,
    Ready,
    Disconnecting,
    Offline,
}

/// Static-cache authority condition.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CacheCondition {
    Unknown,
    Unavailable,
    /// Current only while an authoritative editor is connected. This state is
    /// never eligible for offline reads.
    OnlineCurrent,
    VerifiedCurrent,
    Stale,
    Incompatible,
    Corrupt,
    Rebuilding,
}

/// Private transaction-journal recovery projection.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryCondition {
    None,
    InDoubt,
}

/// Stable user/model-facing connection states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    Ready,
    Connecting,
    Syncing,
    ProjectSessionBusy,
    OfflineCached,
    OfflineEmpty,
    Incompatible,
    AuthFailed,
    Misconfigured,
    Overloaded,
}

/// Bounded availability of a product component. Detailed failure diagnosis
/// remains owned by the top-level canonical diagnostic registry.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComponentCondition {
    Ready,
    Degraded,
    Syncing,
    Offline,
    Stale,
    Disconnected,
    Unavailable,
}

/// Inputs to the pure health reducer. Strings are treated as untrusted
/// observations and are projected through strict bounded coordinate filters.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionObservation {
    pub project_scope: Option<String>,
    pub package_version: String,
    pub package: PackageCondition,
    pub configuration: ConfigurationCondition,
    pub compatibility: CompatibilityEvaluation,
    pub bridge: BridgeCondition,
    pub negotiated_bridge_protocol: Option<String>,
    pub bridge_capabilities: Vec<String>,
    pub cache: CacheCondition,
    pub cache_schema: Option<String>,
    pub cache_generation: Option<String>,
    pub cache_revisions: Option<CacheRevisions>,
    pub source_hashes_verified: bool,
    pub cache_age_seconds: Option<u64>,
    pub recovery: RecoveryCondition,
    pub overloaded: bool,
    pub project_session_busy: bool,
    pub editor: ComponentCondition,
    pub runtime: ComponentCondition,
    pub transactions: ComponentCondition,
}

/// Closed, bounded connection-status DTO. It carries no endpoint, token, PID,
/// absolute root, editor/runtime session identifier, or live payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionHealth {
    pub schema_version: String,
    pub status: ConnectionStatus,
    pub project_scope: Option<String>,
    pub package_version: String,
    pub compatibility: CompatibilityEvaluation,
    pub bridge: BridgeProjection,
    pub static_cache: StaticCacheProjection,
    pub recovery: RecoveryCondition,
    pub components: ConnectionComponents,
    pub diagnostic: Diagnostic,
    pub remediation_id: Option<RemediationId>,
    pub next_action: Option<String>,
    pub limits_applied: ConnectionLimits,
    pub evidence: Vec<ConnectionEvidence>,
}

/// Safe editor/runtime/transaction availability projection.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionComponents {
    pub editor: ComponentCondition,
    pub runtime: ComponentCondition,
    pub transactions: ComponentCondition,
}

/// Fixed serialization policy for the connection projection.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionLimits {
    pub max_bytes: usize,
    pub truncated: bool,
    pub omitted_counts: ConnectionOmittedCounts,
}

/// Closed empty omission map: this fixed-size status projection never drops a
/// variable record collection.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionOmittedCounts {}

/// Safe authority marker for status facts.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionEvidence {
    pub source: String,
    pub freshness: String,
}

/// Safe Bridge projection used inside [`ConnectionHealth`].
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BridgeProjection {
    pub condition: BridgeCondition,
    pub negotiated_protocol: Option<String>,
    pub capabilities: Vec<String>,
}

/// Safe static-cache projection used inside [`ConnectionHealth`].
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StaticCacheProjection {
    pub condition: CacheCondition,
    pub schema: Option<String>,
    pub generation: Option<String>,
    pub revisions: Option<CacheRevisions>,
    pub source_hashes_verified: bool,
    pub age_seconds: Option<u64>,
}

/// Relational semantic-index coordinates. They contain no editor-session or
/// filesystem identity and are emitted only for a current generation.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CacheRevisions {
    pub index_revision: u64,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub script_graph_revision: u64,
}

impl ConnectionHealth {
    /// Reduces one complete observation with deterministic failure precedence.
    ///
    /// Package/configuration failures precede compatibility; protocol,
    /// authentication, and project binding precede offline/cache fallback.
    #[must_use]
    pub fn reduce(observation: ConnectionObservation) -> Self {
        let (status, code) = classify(&observation);
        let diagnostic = Diagnostic::new(code, None, None);
        let remediation_id = diagnostic.remediation_id;
        let cache = if observation.project_session_busy {
            CacheCondition::Unavailable
        } else {
            observation.cache
        };
        Self {
            schema_version: CONNECTION_STATUS_SCHEMA.to_owned(),
            status,
            project_scope: observation.project_scope.as_deref().map(safe_project_scope),
            package_version: safe_coordinate(&observation.package_version),
            compatibility: observation.compatibility,
            bridge: BridgeProjection {
                condition: observation.bridge,
                negotiated_protocol: observation
                    .negotiated_bridge_protocol
                    .as_deref()
                    .map(safe_coordinate),
                capabilities: bounded_capabilities(observation.bridge_capabilities),
            },
            static_cache: StaticCacheProjection {
                condition: cache,
                schema: (!matches!(cache, CacheCondition::Unknown | CacheCondition::Unavailable))
                    .then(|| observation.cache_schema.as_deref().map(safe_coordinate))
                    .flatten(),
                generation: matches!(
                    cache,
                    CacheCondition::OnlineCurrent | CacheCondition::VerifiedCurrent
                )
                .then(|| observation.cache_generation.as_deref().map(safe_coordinate))
                .flatten(),
                revisions: matches!(
                    cache,
                    CacheCondition::OnlineCurrent | CacheCondition::VerifiedCurrent
                )
                .then_some(observation.cache_revisions)
                .flatten(),
                source_hashes_verified: observation.source_hashes_verified
                    && cache == CacheCondition::VerifiedCurrent,
                age_seconds: (!matches!(
                    cache,
                    CacheCondition::Unknown | CacheCondition::Unavailable
                ))
                .then(|| {
                    observation
                        .cache_age_seconds
                        .map(|age| age.min(MAX_CACHE_AGE_SECONDS))
                })
                .flatten(),
            },
            recovery: observation.recovery,
            components: ConnectionComponents {
                editor: observation.editor,
                runtime: if observation.project_session_busy {
                    ComponentCondition::Unavailable
                } else {
                    observation.runtime
                },
                transactions: if observation.project_session_busy {
                    ComponentCondition::Unavailable
                } else {
                    observation.transactions
                },
            },
            diagnostic,
            remediation_id,
            next_action: remediation_id.map(remediation_action).map(str::to_owned),
            limits_applied: ConnectionLimits {
                max_bytes: CONNECTION_STATUS_MAX_BYTES,
                truncated: false,
                omitted_counts: ConnectionOmittedCounts::default(),
            },
            evidence: vec![ConnectionEvidence {
                source: "sidecar_connection_state".to_owned(),
                freshness: "current".to_owned(),
            }],
        }
    }
}

fn classify(observation: &ConnectionObservation) -> (ConnectionStatus, DiagnosticCode) {
    let package_code = package_diagnostic(observation.package);
    if let Some(code) = package_code {
        return (ConnectionStatus::Misconfigured, code);
    }

    let configuration_code = configuration_diagnostic(observation.configuration);
    if let Some(code) = configuration_code {
        return (ConnectionStatus::Misconfigured, code);
    }

    if observation.compatibility.status == CompatibilityStatus::Incompatible {
        return (
            ConnectionStatus::Incompatible,
            observation
                .compatibility
                .diagnostic_code
                .unwrap_or(DiagnosticCode::BridgeVersionIncompatible),
        );
    }

    match observation.bridge {
        BridgeCondition::VersionIncompatible => {
            return (
                ConnectionStatus::Incompatible,
                DiagnosticCode::BridgeVersionIncompatible,
            );
        }
        BridgeCondition::AuthenticationFailed => {
            return (
                ConnectionStatus::AuthFailed,
                DiagnosticCode::BridgeAuthenticationFailed,
            );
        }
        BridgeCondition::ProjectBindingMismatch => {
            return (
                ConnectionStatus::AuthFailed,
                DiagnosticCode::ProjectBindingMismatch,
            );
        }
        _ => {}
    }

    // A missing product/Bridge compatibility observation is terminal only if
    // the lifecycle would otherwise claim a live ready connection. During
    // connect/auth/offline states, those more specific observations retain
    // precedence.
    if observation.compatibility.status == CompatibilityStatus::NotTested
        && observation.bridge == BridgeCondition::Ready
    {
        return (
            ConnectionStatus::Incompatible,
            observation
                .compatibility
                .diagnostic_code
                .unwrap_or(DiagnosticCode::BridgeVersionIncompatible),
        );
    }

    if observation.cache == CacheCondition::Incompatible {
        return (
            ConnectionStatus::Incompatible,
            DiagnosticCode::StaticCacheIncompatible,
        );
    }
    if observation.cache == CacheCondition::Corrupt {
        return (
            ConnectionStatus::Incompatible,
            DiagnosticCode::StaticCacheCorrupt,
        );
    }
    if observation.project_session_busy {
        return (
            ConnectionStatus::ProjectSessionBusy,
            DiagnosticCode::ProjectSessionBusy,
        );
    }
    if observation.overloaded {
        return (
            ConnectionStatus::Overloaded,
            DiagnosticCode::SidecarOverloaded,
        );
    }

    match observation.bridge {
        BridgeCondition::Starting | BridgeCondition::Discovering => (
            ConnectionStatus::Connecting,
            DiagnosticCode::BridgeConnecting,
        ),
        BridgeCondition::DiscoveryStale => (
            ConnectionStatus::Syncing,
            DiagnosticCode::BridgeDiscoveryStale,
        ),
        BridgeCondition::Syncing => (ConnectionStatus::Syncing, DiagnosticCode::BridgeSyncing),
        BridgeCondition::Ready => match observation.cache {
            CacheCondition::OnlineCurrent | CacheCondition::VerifiedCurrent => {
                if observation.recovery == RecoveryCondition::InDoubt {
                    (
                        ConnectionStatus::Ready,
                        DiagnosticCode::TransactionRecoveryRequired,
                    )
                } else if observation.compatibility.status == CompatibilityStatus::CompatibleReduced
                {
                    (
                        ConnectionStatus::Ready,
                        observation
                            .compatibility
                            .diagnostic_code
                            .unwrap_or(DiagnosticCode::CapabilityUnavailable),
                    )
                } else if observation.compatibility.status == CompatibilityStatus::NotTested {
                    (
                        ConnectionStatus::Ready,
                        observation
                            .compatibility
                            .diagnostic_code
                            .unwrap_or(DiagnosticCode::ProjectUntrustedOrRestartRequired),
                    )
                } else {
                    (ConnectionStatus::Ready, DiagnosticCode::Ready)
                }
            }
            CacheCondition::Rebuilding => (
                ConnectionStatus::Syncing,
                DiagnosticCode::StaticCacheRebuilding,
            ),
            CacheCondition::Stale => (ConnectionStatus::Syncing, DiagnosticCode::StaticCacheStale),
            CacheCondition::Unknown | CacheCondition::Unavailable => (
                ConnectionStatus::Syncing,
                DiagnosticCode::StaticCacheUnavailable,
            ),
            CacheCondition::Incompatible | CacheCondition::Corrupt => {
                unreachable!("severe cache states handled above")
            }
        },
        BridgeCondition::Unreachable
        | BridgeCondition::Disconnecting
        | BridgeCondition::Offline => {
            offline_status(observation.cache, observation.source_hashes_verified)
        }
        BridgeCondition::VersionIncompatible
        | BridgeCondition::AuthenticationFailed
        | BridgeCondition::ProjectBindingMismatch => {
            unreachable!("severe Bridge states handled above")
        }
    }
}

/// Canonical package fault mapping shared by doctor and connection status.
#[must_use]
pub fn package_diagnostic(condition: PackageCondition) -> Option<DiagnosticCode> {
    match condition {
        PackageCondition::Ready => None,
        PackageCondition::Missing => Some(DiagnosticCode::BinaryMissing),
        PackageCondition::NotExecutable => Some(DiagnosticCode::BinaryNotExecutable),
        PackageCondition::ArchitectureMismatch => Some(DiagnosticCode::BinaryArchMismatch),
        PackageCondition::Invalid => Some(DiagnosticCode::PackageInvalid),
    }
}

/// Canonical project/configuration fault mapping shared by doctor and
/// connection status.
#[must_use]
pub fn configuration_diagnostic(condition: ConfigurationCondition) -> Option<DiagnosticCode> {
    match condition {
        ConfigurationCondition::Ready => None,
        ConfigurationCondition::ProjectInvalid => Some(DiagnosticCode::ProjectInvalid),
        ConfigurationCondition::Missing => Some(DiagnosticCode::ProjectConfigMissing),
        ConfigurationCondition::Invalid => Some(DiagnosticCode::ProjectConfigInvalid),
        ConfigurationCondition::NotEffective => Some(DiagnosticCode::ProjectConfigNotEffective),
        ConfigurationCondition::TrustOrRestartRequired => {
            Some(DiagnosticCode::ProjectUntrustedOrRestartRequired)
        }
    }
}

fn offline_status(
    cache: CacheCondition,
    source_hashes_verified: bool,
) -> (ConnectionStatus, DiagnosticCode) {
    match cache {
        CacheCondition::VerifiedCurrent if source_hashes_verified => (
            ConnectionStatus::OfflineCached,
            DiagnosticCode::EditorOffline,
        ),
        CacheCondition::OnlineCurrent | CacheCondition::VerifiedCurrent => (
            ConnectionStatus::OfflineEmpty,
            DiagnosticCode::StaticCacheUnavailable,
        ),
        CacheCondition::Stale => (
            ConnectionStatus::OfflineEmpty,
            DiagnosticCode::StaticCacheStale,
        ),
        CacheCondition::Rebuilding => (
            ConnectionStatus::OfflineEmpty,
            DiagnosticCode::StaticCacheRebuilding,
        ),
        CacheCondition::Unknown | CacheCondition::Unavailable => (
            ConnectionStatus::OfflineEmpty,
            DiagnosticCode::StaticCacheUnavailable,
        ),
        CacheCondition::Incompatible | CacheCondition::Corrupt => {
            unreachable!("severe cache states handled before offline fallback")
        }
    }
}

fn bounded_capabilities(values: Vec<String>) -> Vec<String> {
    let mut projected = values
        .into_iter()
        .map(|value| safe_coordinate(&value))
        .filter(|value| value != "[redacted]")
        .collect::<Vec<_>>();
    projected.sort();
    projected.dedup();
    projected.truncate(MAX_CAPABILITIES);
    projected
}

fn safe_project_scope(value: &str) -> String {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        value.to_owned()
    } else {
        "[redacted]".to_owned()
    }
}

fn safe_coordinate(value: &str) -> String {
    if value.is_empty()
        || value.len() > 96
        || value.starts_with('/')
        || looks_like_windows_absolute_path(value)
        || contains_sensitive_marker(value)
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+' | b':' | b'/')
        })
    {
        "[redacted]".to_owned()
    } else {
        value.to_owned()
    }
}

fn looks_like_windows_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn contains_sensitive_marker(value: &str) -> bool {
    let lowercase = value.to_ascii_lowercase();
    [
        "token", "secret", "socket", "endpoint", "hmac", "approval", "password",
    ]
    .iter()
    .any(|marker| lowercase.contains(marker))
}

fn remediation_action(remediation: RemediationId) -> &'static str {
    match remediation {
        RemediationId::StartMatchingEditor => "Start the matching Godot editor project.",
        RemediationId::OpenExactProject => "Open the exact configured Godot project.",
        RemediationId::RestartCodexSurface => "Restart the current Codex surface.",
        RemediationId::WaitForProjectSession => {
            "Close the other Codex task for this project, or wait for it to finish."
        }
        RemediationId::WaitForFullSync => "Wait for the project sync to complete.",
        RemediationId::RunGodotCodexDoctor => "Run godot-codex doctor for this project.",
        RemediationId::RepairProjectConfig => "Repair the project-scoped Codex configuration.",
        RemediationId::FixPrivatePermissions => "Repair project-private Bridge permissions.",
        RemediationId::UpgradeGodotBridge => "Install the matching Godot Bridge build.",
        RemediationId::UpgradeGodotCodex => "Install the matching Godot Codex package.",
        RemediationId::RebuildStaticCache => "Rebuild the project static semantic cache.",
        RemediationId::ResolveTransactionRecovery => {
            "Resolve the outstanding transaction recovery state."
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::compatibility::{CompatibilityEvaluation, CompatibilityReason, CompatibilityStatus};

    use super::*;

    fn evaluation(status: CompatibilityStatus) -> CompatibilityEvaluation {
        CompatibilityEvaluation {
            status,
            surface_observed: false,
            capability_profile: Some("bridge-rpc-1.8".to_owned()),
            enabled_capabilities: BTreeSet::new(),
            reasons: if status == CompatibilityStatus::Supported {
                Vec::new()
            } else {
                vec![CompatibilityReason::BridgeCapabilitiesReduced]
            },
            diagnostic_code: (status != CompatibilityStatus::Supported)
                .then_some(DiagnosticCode::CapabilityUnavailable),
        }
    }

    fn observation() -> ConnectionObservation {
        ConnectionObservation {
            project_scope: Some("a".repeat(64)),
            package_version: "0.1.0".to_owned(),
            package: PackageCondition::Ready,
            configuration: ConfigurationCondition::Ready,
            compatibility: evaluation(CompatibilityStatus::Supported),
            bridge: BridgeCondition::Ready,
            negotiated_bridge_protocol: Some("1.8".to_owned()),
            bridge_capabilities: vec!["runtime.debugger".to_owned(), "bridge.lifecycle".to_owned()],
            cache: CacheCondition::VerifiedCurrent,
            cache_schema: Some("1.3".to_owned()),
            cache_generation: Some("generation:abc123".to_owned()),
            cache_revisions: Some(CacheRevisions {
                index_revision: 9,
                resource_revision: 7,
                scene_graph_revision: 5,
                script_graph_revision: 3,
            }),
            source_hashes_verified: true,
            cache_age_seconds: Some(7),
            recovery: RecoveryCondition::None,
            overloaded: false,
            project_session_busy: false,
            editor: ComponentCondition::Ready,
            runtime: ComponentCondition::Ready,
            transactions: ComponentCondition::Ready,
        }
    }

    #[test]
    fn healthy_observation_reduces_to_bounded_ready_projection() {
        let health = ConnectionHealth::reduce(observation());
        assert_eq!(health.status, ConnectionStatus::Ready);
        assert_eq!(health.diagnostic.code, DiagnosticCode::Ready);
        assert_eq!(health.schema_version, CONNECTION_STATUS_SCHEMA);
        assert_eq!(
            health.bridge.capabilities,
            ["bridge.lifecycle", "runtime.debugger"]
        );
        assert!(health.static_cache.source_hashes_verified);
        assert_eq!(
            health.static_cache.revisions,
            Some(CacheRevisions {
                index_revision: 9,
                resource_revision: 7,
                scene_graph_revision: 5,
                script_graph_revision: 3,
            })
        );
        assert_eq!(health.components.editor, ComponentCondition::Ready);
        assert_eq!(health.components.runtime, ComponentCondition::Ready);
        assert_eq!(health.components.transactions, ComponentCondition::Ready);
        assert_eq!(health.limits_applied.max_bytes, CONNECTION_STATUS_MAX_BYTES);
        assert!(!health.limits_applied.truncated);
        assert_eq!(health.evidence.len(), 1);
    }

    #[test]
    fn project_session_contention_is_explicit_and_fail_closed() {
        let mut value = observation();
        value.project_session_busy = true;
        let health = ConnectionHealth::reduce(value);
        assert_eq!(health.schema_version, "godot-connection-status/1.1");
        assert_eq!(health.status, ConnectionStatus::ProjectSessionBusy);
        assert_eq!(health.diagnostic.code, DiagnosticCode::ProjectSessionBusy);
        assert_eq!(
            health.remediation_id,
            Some(RemediationId::WaitForProjectSession)
        );
        assert_eq!(
            health.next_action.as_deref(),
            Some("Close the other Codex task for this project, or wait for it to finish.")
        );
        assert_eq!(health.bridge.condition, BridgeCondition::Ready);
        assert_eq!(health.components.editor, ComponentCondition::Ready);
        assert_eq!(health.static_cache.condition, CacheCondition::Unavailable);
        assert!(health.static_cache.schema.is_none());
        assert!(health.static_cache.generation.is_none());
        assert!(health.static_cache.revisions.is_none());
        assert!(!health.static_cache.source_hashes_verified);
        assert!(health.static_cache.age_seconds.is_none());
        assert_eq!(health.components.runtime, ComponentCondition::Unavailable);
        assert_eq!(
            health.components.transactions,
            ComponentCondition::Unavailable
        );
    }

    #[test]
    fn severe_bridge_failures_precede_offline_cache_fallback() {
        for (bridge, status, code, remediation, retryable) in [
            (
                BridgeCondition::AuthenticationFailed,
                ConnectionStatus::AuthFailed,
                DiagnosticCode::BridgeAuthenticationFailed,
                RemediationId::StartMatchingEditor,
                true,
            ),
            (
                BridgeCondition::ProjectBindingMismatch,
                ConnectionStatus::AuthFailed,
                DiagnosticCode::ProjectBindingMismatch,
                RemediationId::OpenExactProject,
                false,
            ),
            (
                BridgeCondition::VersionIncompatible,
                ConnectionStatus::Incompatible,
                DiagnosticCode::BridgeVersionIncompatible,
                RemediationId::UpgradeGodotBridge,
                false,
            ),
        ] {
            let mut value = observation();
            value.bridge = bridge;
            value.cache = CacheCondition::VerifiedCurrent;
            let health = ConnectionHealth::reduce(value);
            assert_eq!(health.status, status);
            assert_eq!(health.diagnostic.code, code);
            assert_eq!(health.diagnostic.status, crate::DiagnosticSeverity::Error);
            assert_eq!(health.remediation_id, Some(remediation));
            assert_eq!(health.diagnostic.retryable, retryable);
        }

        let mut value = observation();
        value.bridge = BridgeCondition::Offline;
        let health = ConnectionHealth::reduce(value);
        assert_eq!(health.status, ConnectionStatus::OfflineCached);
        assert_eq!(health.diagnostic.code, DiagnosticCode::EditorOffline);
        assert_eq!(health.diagnostic.status, crate::DiagnosticSeverity::Warning);
        assert_eq!(
            health.remediation_id,
            Some(RemediationId::StartMatchingEditor)
        );
    }

    #[test]
    fn verified_cache_is_the_only_offline_cached_authority() {
        let mut value = observation();
        value.bridge = BridgeCondition::Offline;
        let health = ConnectionHealth::reduce(value);
        assert_eq!(health.status, ConnectionStatus::OfflineCached);

        let mut value = observation();
        value.bridge = BridgeCondition::Offline;
        value.cache = CacheCondition::Stale;
        value.source_hashes_verified = true;
        let health = ConnectionHealth::reduce(value);
        assert_eq!(health.status, ConnectionStatus::OfflineEmpty);
        assert_eq!(health.diagnostic.code, DiagnosticCode::StaticCacheStale);
        assert!(!health.static_cache.source_hashes_verified);
        assert!(health.static_cache.revisions.is_none());

        let mut value = observation();
        value.bridge = BridgeCondition::Offline;
        value.cache = CacheCondition::OnlineCurrent;
        value.source_hashes_verified = false;
        let health = ConnectionHealth::reduce(value);
        assert_eq!(health.status, ConnectionStatus::OfflineEmpty);
        assert_eq!(
            health.diagnostic.code,
            DiagnosticCode::StaticCacheUnavailable
        );
    }

    #[test]
    fn reducer_precedence_is_package_then_config_then_compatibility() {
        let mut value = observation();
        value.package = PackageCondition::Missing;
        value.configuration = ConfigurationCondition::Invalid;
        value.compatibility = evaluation(CompatibilityStatus::Incompatible);
        value.bridge = BridgeCondition::AuthenticationFailed;
        let health = ConnectionHealth::reduce(value);
        assert_eq!(health.status, ConnectionStatus::Misconfigured);
        assert_eq!(health.diagnostic.code, DiagnosticCode::BinaryMissing);
    }

    #[test]
    fn output_redacts_untrusted_strings_and_applies_limits() {
        let mut value = observation();
        value.project_scope = Some("/Users/private/project".to_owned());
        value.package_version = "token secret".to_owned();
        value.negotiated_bridge_protocol = Some("/tmp/private.sock".to_owned());
        value.cache_generation = Some("x".repeat(97));
        value.cache_age_seconds = Some(u64::MAX);
        value.bridge_capabilities = (0..100)
            .map(|index| format!("capability.{index}"))
            .collect();
        let health = ConnectionHealth::reduce(value);
        assert_eq!(health.project_scope.as_deref(), Some("[redacted]"));
        assert_eq!(health.package_version, "[redacted]");
        assert_eq!(
            health.bridge.negotiated_protocol.as_deref(),
            Some("[redacted]")
        );
        assert_eq!(
            health.static_cache.generation.as_deref(),
            Some("[redacted]")
        );
        assert_eq!(health.static_cache.age_seconds, Some(MAX_CACHE_AGE_SECONDS));
        assert_eq!(health.bridge.capabilities.len(), MAX_CAPABILITIES);
        assert!(serde_json::to_vec(&health).unwrap().len() <= crate::CONNECTION_STATUS_MAX_BYTES);
    }

    #[test]
    fn every_remediation_has_bounded_safe_action_text() {
        let actions = crate::all_diagnostic_specs()
            .iter()
            .filter_map(|spec| spec.remediation_id)
            .map(remediation_action)
            .collect::<BTreeSet<_>>();
        assert!(!actions.is_empty());
        assert!(actions.iter().all(|action| action.len() <= 80));
        assert!(actions.iter().all(|action| !action.contains('/')));
    }
}
