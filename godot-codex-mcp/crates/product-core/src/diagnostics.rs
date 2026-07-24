use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Stable public component names shared by doctor, MCP, setup, and stderr.
#[derive(
    Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Component {
    Package,
    Sidecar,
    Project,
    ProjectConfig,
    HostSurface,
    BridgeDiscovery,
    Bridge,
    StaticCache,
    TransactionRecovery,
}

/// Stable diagnostic severity. It does not encode process exit status.
#[derive(
    Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

/// Closed remediation identifiers rendered identically across product surfaces.
#[derive(
    Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RemediationId {
    StartMatchingEditor,
    OpenExactProject,
    RestartCodexSurface,
    WaitForFullSync,
    RunGodotCodexDoctor,
    RepairProjectConfig,
    FixPrivatePermissions,
    UpgradeGodotBridge,
    UpgradeGodotCodex,
    RebuildStaticCache,
    ResolveTransactionRecovery,
}

/// Frozen public diagnostic codes for Sprint 11.
#[derive(
    Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    PackageInvalid,
    BinaryMissing,
    BinaryNotExecutable,
    BinaryArchMismatch,
    ProjectInvalid,
    ProjectConfigMissing,
    ProjectConfigInvalid,
    ProjectConfigNotEffective,
    ProjectUntrustedOrRestartRequired,
    BridgeDiscoveryMissing,
    BridgeDiscoveryStale,
    PermissionsInvalid,
    BridgeUnreachable,
    BridgeConnecting,
    BridgeSyncing,
    BridgeVersionIncompatible,
    BridgeAuthenticationFailed,
    ProjectBindingMismatch,
    EditorOffline,
    StaticCacheUnavailable,
    StaticCacheStale,
    StaticCacheIncompatible,
    StaticCacheCorrupt,
    StaticCacheRebuilding,
    SurfaceUnsupported,
    SurfaceRestartRequired,
    HostInteractionUnsupported,
    CapabilityUnavailable,
    SidecarOverloaded,
    TransactionRecoveryRequired,
    Ready,
}

impl DiagnosticCode {
    /// Stable wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackageInvalid => "package_invalid",
            Self::BinaryMissing => "binary_missing",
            Self::BinaryNotExecutable => "binary_not_executable",
            Self::BinaryArchMismatch => "binary_arch_mismatch",
            Self::ProjectInvalid => "project_invalid",
            Self::ProjectConfigMissing => "project_config_missing",
            Self::ProjectConfigInvalid => "project_config_invalid",
            Self::ProjectConfigNotEffective => "project_config_not_effective",
            Self::ProjectUntrustedOrRestartRequired => "project_untrusted_or_restart_required",
            Self::BridgeDiscoveryMissing => "bridge_discovery_missing",
            Self::BridgeDiscoveryStale => "bridge_discovery_stale",
            Self::PermissionsInvalid => "permissions_invalid",
            Self::BridgeUnreachable => "bridge_unreachable",
            Self::BridgeConnecting => "bridge_connecting",
            Self::BridgeSyncing => "bridge_syncing",
            Self::BridgeVersionIncompatible => "bridge_version_incompatible",
            Self::BridgeAuthenticationFailed => "bridge_authentication_failed",
            Self::ProjectBindingMismatch => "project_binding_mismatch",
            Self::EditorOffline => "editor_offline",
            Self::StaticCacheUnavailable => "static_cache_unavailable",
            Self::StaticCacheStale => "static_cache_stale",
            Self::StaticCacheIncompatible => "static_cache_incompatible",
            Self::StaticCacheCorrupt => "static_cache_corrupt",
            Self::StaticCacheRebuilding => "static_cache_rebuilding",
            Self::SurfaceUnsupported => "surface_unsupported",
            Self::SurfaceRestartRequired => "surface_restart_required",
            Self::HostInteractionUnsupported => "host_interaction_unsupported",
            Self::CapabilityUnavailable => "capability_unavailable",
            Self::SidecarOverloaded => "sidecar_overloaded",
            Self::TransactionRecoveryRequired => "transaction_recovery_required",
            Self::Ready => "ready",
        }
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for DiagnosticCode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        ALL_DIAGNOSTIC_SPECS
            .iter()
            .find(|spec| spec.code.as_str() == value)
            .map(|spec| spec.code)
            .ok_or(())
    }
}

/// Normative immutable metadata for one public diagnostic code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagnosticSpec {
    pub code: DiagnosticCode,
    pub severity: DiagnosticSeverity,
    pub component: Component,
    pub remediation_id: Option<RemediationId>,
    pub retryable: bool,
    pub summary: &'static str,
}

/// Bounded serialized diagnostic. Version observations accept only short safe
/// coordinate characters; arbitrary parser/process/path text is never stored.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub component: Component,
    pub status: DiagnosticSeverity,
    pub code: DiagnosticCode,
    pub summary: String,
    pub observed_version: Option<String>,
    pub supported: Option<String>,
    pub remediation_id: Option<RemediationId>,
    pub retryable: bool,
}

impl Diagnostic {
    /// Creates a safe diagnostic from registry-owned prose.
    #[must_use]
    pub fn new(
        code: DiagnosticCode,
        observed_version: Option<&str>,
        supported: Option<&str>,
    ) -> Self {
        let spec = diagnostic_spec(code);
        Self {
            component: spec.component,
            status: spec.severity,
            code,
            summary: spec.summary.to_owned(),
            observed_version: observed_version.map(safe_coordinate),
            supported: supported.map(safe_coordinate),
            remediation_id: spec.remediation_id,
            retryable: spec.retryable,
        }
    }
}

const ALL_DIAGNOSTIC_SPECS: &[DiagnosticSpec] = &[
    DiagnosticSpec {
        code: DiagnosticCode::PackageInvalid,
        severity: DiagnosticSeverity::Error,
        component: Component::Package,
        remediation_id: Some(RemediationId::UpgradeGodotCodex),
        retryable: false,
        summary: "The installed Godot Codex package did not pass integrity checks.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::BinaryMissing,
        severity: DiagnosticSeverity::Error,
        component: Component::Package,
        remediation_id: Some(RemediationId::UpgradeGodotCodex),
        retryable: false,
        summary: "A required Godot Codex executable is missing.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::BinaryNotExecutable,
        severity: DiagnosticSeverity::Error,
        component: Component::Package,
        remediation_id: Some(RemediationId::UpgradeGodotCodex),
        retryable: false,
        summary: "A required Godot Codex binary is not executable.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::BinaryArchMismatch,
        severity: DiagnosticSeverity::Error,
        component: Component::Package,
        remediation_id: Some(RemediationId::UpgradeGodotCodex),
        retryable: false,
        summary: "The installed Godot Codex binary targets a different architecture.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::ProjectInvalid,
        severity: DiagnosticSeverity::Error,
        component: Component::Project,
        remediation_id: Some(RemediationId::OpenExactProject),
        retryable: false,
        summary: "The selected directory is not a canonical Godot project.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::ProjectConfigMissing,
        severity: DiagnosticSeverity::Error,
        component: Component::ProjectConfig,
        remediation_id: Some(RemediationId::RepairProjectConfig),
        retryable: false,
        summary: "The project-scoped Codex MCP configuration is missing.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::ProjectConfigInvalid,
        severity: DiagnosticSeverity::Error,
        component: Component::ProjectConfig,
        remediation_id: Some(RemediationId::RepairProjectConfig),
        retryable: false,
        summary: "The project-scoped Codex MCP configuration is invalid.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::ProjectConfigNotEffective,
        severity: DiagnosticSeverity::Error,
        component: Component::ProjectConfig,
        remediation_id: Some(RemediationId::RestartCodexSurface),
        retryable: true,
        summary: "The expected project-scoped Codex configuration is not effective.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::ProjectUntrustedOrRestartRequired,
        severity: DiagnosticSeverity::Warning,
        component: Component::HostSurface,
        remediation_id: Some(RemediationId::RestartCodexSurface),
        retryable: true,
        summary: "The Codex surface may require project trust or a restart.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::BridgeDiscoveryMissing,
        severity: DiagnosticSeverity::Warning,
        component: Component::BridgeDiscovery,
        remediation_id: Some(RemediationId::StartMatchingEditor),
        retryable: true,
        summary: "No project-scoped Godot Bridge discovery record is available.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::BridgeDiscoveryStale,
        severity: DiagnosticSeverity::Warning,
        component: Component::BridgeDiscovery,
        remediation_id: Some(RemediationId::StartMatchingEditor),
        retryable: true,
        summary: "The Godot Bridge discovery record is stale.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::PermissionsInvalid,
        severity: DiagnosticSeverity::Error,
        component: Component::BridgeDiscovery,
        remediation_id: Some(RemediationId::FixPrivatePermissions),
        retryable: false,
        summary: "Project-private Bridge state has unsafe permissions.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::BridgeUnreachable,
        severity: DiagnosticSeverity::Warning,
        component: Component::Bridge,
        remediation_id: Some(RemediationId::StartMatchingEditor),
        retryable: true,
        summary: "The project-scoped Godot Bridge is not reachable.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::BridgeConnecting,
        severity: DiagnosticSeverity::Info,
        component: Component::Bridge,
        remediation_id: Some(RemediationId::WaitForFullSync),
        retryable: true,
        summary: "The sidecar is connecting to the project-scoped Godot Bridge.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::BridgeSyncing,
        severity: DiagnosticSeverity::Info,
        component: Component::Bridge,
        remediation_id: Some(RemediationId::WaitForFullSync),
        retryable: true,
        summary: "The project-scoped Godot Bridge is synchronizing.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::BridgeVersionIncompatible,
        severity: DiagnosticSeverity::Error,
        component: Component::Bridge,
        remediation_id: Some(RemediationId::UpgradeGodotBridge),
        retryable: false,
        summary: "The Godot Bridge protocol version is incompatible.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::BridgeAuthenticationFailed,
        severity: DiagnosticSeverity::Error,
        component: Component::Bridge,
        remediation_id: Some(RemediationId::StartMatchingEditor),
        retryable: true,
        summary: "Authentication with the project-scoped Godot Bridge failed.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::ProjectBindingMismatch,
        severity: DiagnosticSeverity::Error,
        component: Component::Bridge,
        remediation_id: Some(RemediationId::OpenExactProject),
        retryable: false,
        summary: "The Godot Bridge is bound to a different project.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::EditorOffline,
        severity: DiagnosticSeverity::Warning,
        component: Component::Bridge,
        remediation_id: Some(RemediationId::StartMatchingEditor),
        retryable: true,
        summary: "The exact Godot editor project is offline.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::StaticCacheUnavailable,
        severity: DiagnosticSeverity::Warning,
        component: Component::StaticCache,
        remediation_id: Some(RemediationId::StartMatchingEditor),
        retryable: true,
        summary: "No verified static semantic cache is available.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::StaticCacheStale,
        severity: DiagnosticSeverity::Warning,
        component: Component::StaticCache,
        remediation_id: Some(RemediationId::RebuildStaticCache),
        retryable: true,
        summary: "The retained static semantic cache is stale.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::StaticCacheIncompatible,
        severity: DiagnosticSeverity::Error,
        component: Component::StaticCache,
        remediation_id: Some(RemediationId::RebuildStaticCache),
        retryable: false,
        summary: "The retained static semantic cache uses an incompatible schema.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::StaticCacheCorrupt,
        severity: DiagnosticSeverity::Error,
        component: Component::StaticCache,
        remediation_id: Some(RemediationId::RebuildStaticCache),
        retryable: false,
        summary: "The retained static semantic cache failed integrity checks.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::StaticCacheRebuilding,
        severity: DiagnosticSeverity::Info,
        component: Component::StaticCache,
        remediation_id: Some(RemediationId::WaitForFullSync),
        retryable: true,
        summary: "The static semantic cache is rebuilding.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::SurfaceUnsupported,
        severity: DiagnosticSeverity::Error,
        component: Component::HostSurface,
        remediation_id: Some(RemediationId::UpgradeGodotCodex),
        retryable: false,
        summary: "This Codex host surface is not supported by the beta package.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::SurfaceRestartRequired,
        severity: DiagnosticSeverity::Warning,
        component: Component::HostSurface,
        remediation_id: Some(RemediationId::RestartCodexSurface),
        retryable: true,
        summary: "The Codex host surface must restart to load the project configuration.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::HostInteractionUnsupported,
        severity: DiagnosticSeverity::Warning,
        component: Component::HostSurface,
        remediation_id: Some(RemediationId::UpgradeGodotCodex),
        retryable: false,
        summary: "The Codex host does not provide the required form interaction.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::CapabilityUnavailable,
        severity: DiagnosticSeverity::Warning,
        component: Component::Bridge,
        remediation_id: Some(RemediationId::UpgradeGodotBridge),
        retryable: false,
        summary: "A required Godot Bridge capability is unavailable.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::SidecarOverloaded,
        severity: DiagnosticSeverity::Warning,
        component: Component::Sidecar,
        remediation_id: Some(RemediationId::RunGodotCodexDoctor),
        retryable: true,
        summary: "The project-scoped sidecar is temporarily overloaded.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::TransactionRecoveryRequired,
        severity: DiagnosticSeverity::Error,
        component: Component::TransactionRecovery,
        remediation_id: Some(RemediationId::ResolveTransactionRecovery),
        retryable: false,
        summary: "A transaction requires explicit recovery before another write.",
    },
    DiagnosticSpec {
        code: DiagnosticCode::Ready,
        severity: DiagnosticSeverity::Info,
        component: Component::Sidecar,
        remediation_id: None,
        retryable: false,
        summary: "Godot Codex is ready for the selected project.",
    },
];

/// Returns every frozen code exactly once in deterministic registry order.
#[must_use]
pub const fn all_diagnostic_specs() -> &'static [DiagnosticSpec] {
    ALL_DIAGNOSTIC_SPECS
}

/// Looks up immutable metadata for a public code.
#[must_use]
pub fn diagnostic_spec(code: DiagnosticCode) -> &'static DiagnosticSpec {
    ALL_DIAGNOSTIC_SPECS
        .iter()
        .find(|spec| spec.code == code)
        .expect("every DiagnosticCode has one static registry entry")
}

fn safe_coordinate(value: &str) -> String {
    const MAX_COORDINATE_BYTES: usize = 96;
    if value.is_empty()
        || value.len() > MAX_COORDINATE_BYTES
        || value.starts_with('/')
        || looks_like_windows_absolute_path(value)
        || contains_sensitive_marker(value)
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+' | b':' | b'/')
        })
    {
        return "[redacted]".to_owned();
    }
    value.to_owned()
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn diagnostic_registry_is_total_unique_and_bounded() {
        let codes = all_diagnostic_specs()
            .iter()
            .map(|spec| spec.code)
            .collect::<BTreeSet<_>>();
        assert_eq!(codes.len(), all_diagnostic_specs().len());
        for spec in all_diagnostic_specs() {
            assert_eq!(diagnostic_spec(spec.code), spec);
            assert_eq!(DiagnosticCode::from_str(spec.code.as_str()), Ok(spec.code));
            assert!(spec.summary.len() <= 160);
            assert!(!spec.summary.contains('/'));
            assert!(!spec.summary.contains("token"));
        }
    }

    #[test]
    fn diagnostic_serialization_uses_frozen_public_spelling() {
        let diagnostic = Diagnostic::new(
            DiagnosticCode::BridgeDiscoveryStale,
            Some("bridge-rpc/1.8"),
            Some("bridge-rpc/1.0-1.8"),
        );
        let value = serde_json::to_value(diagnostic).unwrap();
        assert_eq!(value["code"], "bridge_discovery_stale");
        assert_eq!(value["component"], "bridge_discovery");
        assert_eq!(value["remediation_id"], "start_matching_editor");
        assert_eq!(value["observed_version"], "bridge-rpc/1.8");
    }

    #[test]
    fn arbitrary_paths_tokens_and_long_values_are_redacted() {
        for unsafe_value in [
            "/Users/private/project",
            "token=secret",
            "token:secret",
            "C:/Users/private/project",
            &"a".repeat(97),
            "socket path",
        ] {
            let diagnostic =
                Diagnostic::new(DiagnosticCode::BridgeUnreachable, Some(unsafe_value), None);
            assert_eq!(diagnostic.observed_version.as_deref(), Some("[redacted]"));
        }
    }
}
