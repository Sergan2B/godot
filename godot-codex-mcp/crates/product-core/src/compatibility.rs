use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::diagnostics::DiagnosticCode;
use crate::registry::canonical_registry_profile;
use crate::{COMPATIBILITY_MATRIX_JSON, COMPATIBILITY_MATRIX_SCHEMA};

const MAX_BRIDGE_PROFILES: usize = 32;
const MAX_CAPABILITIES_PER_PROFILE: usize = 128;
const MAX_SURFACE_RULES: usize = 32;
const MAX_SURFACE_BUNDLE_SEQUENCE: u64 = 9_007_199_254_740_991;

/// Operating-system coordinate supported by the compatibility contract.
#[derive(
    Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OperatingSystem {
    Macos,
    Linux,
    Windows,
}

/// CPU architecture coordinate supported by the compatibility contract.
#[derive(
    Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    Arm64,
    X86_64,
}

/// Exact platform coordinate.
#[derive(
    Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct TargetCoordinate {
    pub os: OperatingSystem,
    pub architecture: Architecture,
}

/// Package/sidecar semantic coordinate. Artifact hashes live in the detached
/// package manifest to avoid a matrix/binary self-reference.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageCoordinate {
    pub version: String,
    pub target: TargetCoordinate,
}

/// Exact separately distributed Godot Bridge artifact.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GodotCoordinate {
    pub source_commit: String,
    pub build_id: String,
    pub artifact_sha256: String,
    pub target: TargetCoordinate,
}

/// Bridge RPC parsed major/minor coordinate.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    /// Parses a bounded Bridge coordinate emitted by discovery or an
    /// authenticated session. Both `1.8` and `bridge-rpc/1.8` are accepted;
    /// prefixes, patch versions, and unbounded numeric components are not.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.strip_prefix("bridge-rpc/").unwrap_or(value);
        let (major, minor) = value.split_once('.')?;
        if major.is_empty()
            || minor.is_empty()
            || major.len() > 5
            || minor.len() > 5
            || !major.bytes().all(|byte| byte.is_ascii_digit())
            || !minor.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Some(Self {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }
}

/// Protocol compatibility boundary.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolContract {
    pub bridge_major: u16,
    pub bridge_min_minor: u16,
    pub bridge_current_minor: u16,
    pub mcp_protocol: String,
    pub approval_protocol_floor: String,
    pub requires_form_elicitation: bool,
}

/// Persistent private/public schema coordinates relevant to compatibility.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaContract {
    pub index: String,
    pub transaction_journal: String,
    pub validation_report: String,
}

/// Hash-bound MCP registry identity.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryBinding {
    pub profile_id: String,
    pub digest: String,
    pub tool_count: usize,
    pub fixed_resource_count: usize,
    pub resource_template_count: usize,
}

/// Product capability set selected for one exact Bridge minor.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeCapabilityProfile {
    pub profile_id: String,
    pub bridge_minor: u16,
    pub capabilities: BTreeSet<String>,
    pub qualification: CompatibilityStatus,
}

/// Qualified Codex host surfaces.
#[derive(
    Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    App,
    Cli,
    Ide,
    Cursor,
}

/// Evidence state for one exact host coordinate. `Candidate` records an
/// observed coordinate without claiming that its full acceptance workflow
/// has passed. Evaluation maps candidates to fail-closed `NotTested`.
#[derive(
    Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceQualification {
    Candidate,
    Supported,
    CompatibleReduced,
    Incompatible,
    NotTested,
}

/// One exact surface/version/host/target rule. `ide_host_version` is present
/// for IDE integrations and absent for standalone App/CLI.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceRule {
    pub surface: SurfaceKind,
    pub host_version: String,
    pub ide_host_version: Option<String>,
    pub target: TargetCoordinate,
    pub qualification: SurfaceQualification,
}

/// Independently distributed host-surface compatibility snapshot.
///
/// A bundle can replace only App/CLI/IDE rules. Package, Godot, protocol,
/// schema, registry, Bridge, and Cursor coordinates remain pinned by the
/// embedded matrix. The detached host-coordinate profile carries exact
/// artifact hashes and is bound here by digest.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceCompatibilityBundle {
    pub schema_version: String,
    pub bundle_id: String,
    pub sequence: u64,
    pub package_version: String,
    pub baseline_matrix_sha256: String,
    pub host_coordinate_profile_sha256: String,
    pub target: TargetCoordinate,
    pub surfaces: Vec<SurfaceRule>,
}

/// Closed machine-readable compatibility matrix.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityMatrix {
    pub schema_version: String,
    pub matrix_id: String,
    pub package: PackageCoordinate,
    pub godot: GodotCoordinate,
    pub protocols: ProtocolContract,
    pub schemas: SchemaContract,
    pub registry: RegistryBinding,
    pub bridge_profiles: Vec<BridgeCapabilityProfile>,
    pub surfaces: Vec<SurfaceRule>,
}

/// Complete model-free compatibility observation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityObservation {
    pub package_version: String,
    pub package_manifest_verified: bool,
    pub target: TargetCoordinate,
    pub godot_source_commit: String,
    pub godot_build_id: String,
    pub godot_artifact_sha256: String,
    pub bridge: ProtocolVersion,
    pub advertised_capabilities: BTreeSet<String>,
    pub mcp_protocol: String,
    pub supports_form_elicitation: bool,
    pub index_schema: String,
    pub transaction_journal_schema: String,
    pub validation_report_schema: String,
    pub surface: SurfaceKind,
    pub host_version: String,
    pub ide_host_version: Option<String>,
}

/// Host-independent product coordinates known before a Bridge session is
/// negotiated. Package/setup and doctor construct this from verified local
/// artifacts; the sidecar never reconstructs these coordinates from a
/// discovery record.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductCompatibilityBasis {
    pub package_version: String,
    pub package_manifest_verified: bool,
    pub target: TargetCoordinate,
    pub godot_source_commit: String,
    pub godot_build_id: String,
    pub godot_artifact_sha256: String,
    pub mcp_protocol: String,
    pub index_schema: String,
    pub transaction_journal_schema: String,
    pub validation_report_schema: String,
}

impl ProductCompatibilityBasis {
    /// Binds the immutable product coordinates to one authenticated Bridge
    /// negotiation. Capabilities remain exact and ordered by `BTreeSet`.
    #[must_use]
    pub fn with_bridge(
        &self,
        bridge: ProtocolVersion,
        advertised_capabilities: BTreeSet<String>,
    ) -> ProductCompatibilityObservation {
        ProductCompatibilityObservation {
            package_version: self.package_version.clone(),
            package_manifest_verified: self.package_manifest_verified,
            target: self.target,
            godot_source_commit: self.godot_source_commit.clone(),
            godot_build_id: self.godot_build_id.clone(),
            godot_artifact_sha256: self.godot_artifact_sha256.clone(),
            bridge,
            advertised_capabilities,
            mcp_protocol: self.mcp_protocol.clone(),
            index_schema: self.index_schema.clone(),
            transaction_journal_schema: self.transaction_journal_schema.clone(),
            validation_report_schema: self.validation_report_schema.clone(),
        }
    }
}

/// Host-independent compatibility observation shared by doctor and the MCP
/// connection-status adapter.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductCompatibilityObservation {
    pub package_version: String,
    pub package_manifest_verified: bool,
    pub target: TargetCoordinate,
    pub godot_source_commit: String,
    pub godot_build_id: String,
    pub godot_artifact_sha256: String,
    pub bridge: ProtocolVersion,
    pub advertised_capabilities: BTreeSet<String>,
    pub mcp_protocol: String,
    pub index_schema: String,
    pub transaction_journal_schema: String,
    pub validation_report_schema: String,
}

impl From<&CompatibilityObservation> for ProductCompatibilityObservation {
    fn from(observation: &CompatibilityObservation) -> Self {
        Self {
            package_version: observation.package_version.clone(),
            package_manifest_verified: observation.package_manifest_verified,
            target: observation.target,
            godot_source_commit: observation.godot_source_commit.clone(),
            godot_build_id: observation.godot_build_id.clone(),
            godot_artifact_sha256: observation.godot_artifact_sha256.clone(),
            bridge: observation.bridge,
            advertised_capabilities: observation.advertised_capabilities.clone(),
            mcp_protocol: observation.mcp_protocol.clone(),
            index_schema: observation.index_schema.clone(),
            transaction_journal_schema: observation.transaction_journal_schema.clone(),
            validation_report_schema: observation.validation_report_schema.clone(),
        }
    }
}

/// Deterministic compatibility outcome.
#[derive(
    Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityStatus {
    Supported,
    CompatibleReduced,
    Incompatible,
    NotTested,
}

/// Closed evidence-safe reason vocabulary.
#[derive(
    Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityReason {
    PackageManifestUnverified,
    PackageVersionMismatch,
    PlatformMismatch,
    ArchitectureMismatch,
    GodotSourceMismatch,
    GodotBuildMismatch,
    GodotArtifactMismatch,
    BridgeMajorMismatch,
    BridgeMinorUnsupported,
    BridgeMinorReduced,
    BridgeCapabilitiesReduced,
    McpProtocolMismatch,
    HostInteractionUnsupported,
    IndexSchemaMismatch,
    TransactionJournalSchemaMismatch,
    ValidationReportSchemaMismatch,
    SurfaceVersionUnsupported,
    SurfaceCandidate,
    SurfaceNotTested,
}

/// Bounded compatibility result shared by doctor and connection status.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityEvaluation {
    pub status: CompatibilityStatus,
    /// `true` only when one exact App/CLI/IDE host coordinate participated in
    /// this evaluation. Product/Bridge readiness does not imply that claim.
    pub surface_observed: bool,
    pub capability_profile: Option<String>,
    pub enabled_capabilities: BTreeSet<String>,
    pub reasons: Vec<CompatibilityReason>,
    pub diagnostic_code: Option<DiagnosticCode>,
}

impl CompatibilityEvaluation {
    /// Fail-closed value used before an authenticated Bridge negotiation is
    /// available. It deliberately carries no capability profile or capability
    /// claims.
    #[must_use]
    pub fn not_observed() -> Self {
        Self {
            status: CompatibilityStatus::NotTested,
            surface_observed: false,
            capability_profile: None,
            enabled_capabilities: BTreeSet::new(),
            reasons: vec![CompatibilityReason::SurfaceNotTested],
            diagnostic_code: Some(DiagnosticCode::ProjectUntrustedOrRestartRequired),
        }
    }
}

/// Structural matrix validation error. Messages contain only static field
/// names and never echo untrusted matrix values.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MatrixError {
    #[error("compatibility matrix schema_version is unsupported")]
    SchemaVersion,
    #[error("compatibility matrix contains an invalid identifier")]
    InvalidIdentifier,
    #[error("compatibility matrix contains an invalid SHA-256")]
    InvalidSha256,
    #[error("compatibility matrix Bridge range is invalid")]
    BridgeRange,
    #[error("compatibility matrix package and Godot targets differ")]
    TargetMismatch,
    #[error("compatibility matrix Bridge profiles are incomplete or overlapping")]
    BridgeProfiles,
    #[error("compatibility matrix Bridge capabilities are not additive")]
    NonAdditiveCapabilities,
    #[error("compatibility matrix surface rules are missing, duplicate, or overlapping")]
    SurfaceRules,
    #[error("compatibility matrix registry binding does not match the canonical registry")]
    RegistryBinding,
    #[error("compatibility matrix digest does not match")]
    DigestMismatch,
    #[error("compatibility matrix JSON is invalid")]
    Json,
}

/// Structural or baseline-binding failure for a surface compatibility bundle.
/// Messages contain only static field names and never echo untrusted values.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SurfaceBundleError {
    #[error("surface compatibility bundle schema_version is unsupported")]
    SchemaVersion,
    #[error("surface compatibility bundle contains an invalid identifier")]
    InvalidIdentifier,
    #[error("surface compatibility bundle contains an invalid SHA-256")]
    InvalidSha256,
    #[error("surface compatibility bundle sequence is invalid")]
    InvalidSequence,
    #[error("surface compatibility bundle does not match the embedded product")]
    ProductBinding,
    #[error("surface compatibility bundle rules are missing, duplicate, or invalid")]
    SurfaceRules,
    #[error("surface compatibility bundle produces an invalid matrix")]
    Matrix,
    #[error("surface compatibility bundle JSON is invalid")]
    Json,
}

impl SurfaceCompatibilityBundle {
    /// Parses and validates a bundle against one immutable baseline matrix.
    pub fn parse(bytes: &[u8], baseline: &CompatibilityMatrix) -> Result<Self, SurfaceBundleError> {
        let bundle = serde_json::from_slice::<Self>(bytes).map_err(|_| SurfaceBundleError::Json)?;
        bundle.validate_against(baseline)?;
        Ok(bundle)
    }

    /// Validates that the bundle is a complete, single-valued App/CLI/IDE
    /// snapshot for exactly the supplied embedded product.
    pub fn validate_against(
        &self,
        baseline: &CompatibilityMatrix,
    ) -> Result<(), SurfaceBundleError> {
        baseline
            .validate()
            .map_err(|_| SurfaceBundleError::Matrix)?;
        if self.schema_version != crate::SURFACE_COMPATIBILITY_BUNDLE_SCHEMA {
            return Err(SurfaceBundleError::SchemaVersion);
        }
        if !safe_identifier(&self.bundle_id) || !safe_identifier(&self.package_version) {
            return Err(SurfaceBundleError::InvalidIdentifier);
        }
        if self.sequence == 0 || self.sequence > MAX_SURFACE_BUNDLE_SEQUENCE {
            return Err(SurfaceBundleError::InvalidSequence);
        }
        if !prefixed_lowercase_sha256(&self.baseline_matrix_sha256)
            || !prefixed_lowercase_sha256(&self.host_coordinate_profile_sha256)
        {
            return Err(SurfaceBundleError::InvalidSha256);
        }
        let baseline_digest = baseline
            .canonical_digest()
            .map_err(|_| SurfaceBundleError::Matrix)?;
        if self.package_version != baseline.package.version
            || self.target != baseline.package.target
            || self.baseline_matrix_sha256 != format!("sha256:{baseline_digest}")
        {
            return Err(SurfaceBundleError::ProductBinding);
        }
        if self.surfaces.len() < 3 || self.surfaces.len() > MAX_SURFACE_RULES {
            return Err(SurfaceBundleError::SurfaceRules);
        }
        let mut keys = BTreeSet::new();
        let mut required = BTreeSet::new();
        for rule in &self.surfaces {
            if !matches!(
                rule.surface,
                SurfaceKind::App | SurfaceKind::Cli | SurfaceKind::Ide
            ) || rule.target != self.target
                || !safe_identifier(&rule.host_version)
                || rule
                    .ide_host_version
                    .as_deref()
                    .is_some_and(|value| !safe_identifier(value))
                || (rule.surface == SurfaceKind::Ide) != rule.ide_host_version.is_some()
                || matches!(
                    rule.qualification,
                    SurfaceQualification::Incompatible | SurfaceQualification::NotTested
                )
                || !keys.insert((
                    rule.surface,
                    rule.host_version.as_str(),
                    rule.ide_host_version.as_deref(),
                ))
            {
                return Err(SurfaceBundleError::SurfaceRules);
            }
            required.insert(rule.surface);
        }
        if required != BTreeSet::from([SurfaceKind::App, SurfaceKind::Cli, SurfaceKind::Ide]) {
            return Err(SurfaceBundleError::SurfaceRules);
        }
        Ok(())
    }

    /// Applies the validated surface snapshot while retaining every
    /// host-independent baseline field and all baseline Cursor rules.
    pub fn apply_to(
        &self,
        baseline: &CompatibilityMatrix,
    ) -> Result<CompatibilityMatrix, SurfaceBundleError> {
        self.validate_against(baseline)?;
        let mut matrix = baseline.clone();
        matrix.surfaces.retain(|rule| {
            !matches!(
                rule.surface,
                SurfaceKind::App | SurfaceKind::Cli | SurfaceKind::Ide
            )
        });
        matrix.surfaces.extend(self.surfaces.iter().cloned());
        matrix.validate().map_err(|_| SurfaceBundleError::Matrix)?;
        Ok(matrix)
    }

    /// Stable lowercase SHA-256 of the canonical typed JSON representation.
    pub fn canonical_digest(&self) -> Result<String, SurfaceBundleError> {
        let bytes = serde_json::to_vec(self).map_err(|_| SurfaceBundleError::Json)?;
        Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
    }
}

impl CompatibilityMatrix {
    /// Validates all structural constraints that make evaluation single-valued.
    pub fn validate(&self) -> Result<(), MatrixError> {
        if self.schema_version != COMPATIBILITY_MATRIX_SCHEMA {
            return Err(MatrixError::SchemaVersion);
        }
        if !safe_identifier(&self.matrix_id)
            || !safe_identifier(&self.package.version)
            || !safe_identifier(&self.godot.source_commit)
            || !safe_identifier(&self.godot.build_id)
            || !safe_identifier(&self.protocols.mcp_protocol)
            || !safe_identifier(&self.protocols.approval_protocol_floor)
            || !safe_identifier(&self.schemas.index)
            || !safe_identifier(&self.schemas.transaction_journal)
            || !safe_identifier(&self.schemas.validation_report)
        {
            return Err(MatrixError::InvalidIdentifier);
        }
        if !lowercase_sha256(&self.godot.artifact_sha256) {
            return Err(MatrixError::InvalidSha256);
        }
        if self.protocols.bridge_major == 0
            || self.protocols.bridge_min_minor > self.protocols.bridge_current_minor
        {
            return Err(MatrixError::BridgeRange);
        }
        if self.godot.target != self.package.target {
            return Err(MatrixError::TargetMismatch);
        }
        self.validate_registry()?;
        self.validate_bridge_profiles()?;
        self.validate_surface_rules()?;
        Ok(())
    }

    /// Evaluates package, target, Godot, schema, MCP, and authenticated Bridge
    /// coordinates without making a Codex host-surface claim.
    pub fn evaluate_product(
        &self,
        observation: &ProductCompatibilityObservation,
    ) -> Result<CompatibilityEvaluation, MatrixError> {
        self.validate()?;

        if !observation.package_manifest_verified {
            return Ok(incompatible(
                CompatibilityReason::PackageManifestUnverified,
                DiagnosticCode::PackageInvalid,
            ));
        }
        if observation.target.os != self.package.target.os {
            return Ok(incompatible(
                CompatibilityReason::PlatformMismatch,
                DiagnosticCode::BinaryArchMismatch,
            ));
        }
        if observation.target.architecture != self.package.target.architecture {
            return Ok(incompatible(
                CompatibilityReason::ArchitectureMismatch,
                DiagnosticCode::BinaryArchMismatch,
            ));
        }
        if observation.package_version != self.package.version {
            return Ok(incompatible(
                CompatibilityReason::PackageVersionMismatch,
                DiagnosticCode::PackageInvalid,
            ));
        }
        if observation.godot_source_commit != self.godot.source_commit {
            return Ok(incompatible(
                CompatibilityReason::GodotSourceMismatch,
                DiagnosticCode::BridgeVersionIncompatible,
            ));
        }
        if observation.godot_build_id != self.godot.build_id {
            return Ok(incompatible(
                CompatibilityReason::GodotBuildMismatch,
                DiagnosticCode::BridgeVersionIncompatible,
            ));
        }
        if observation.godot_artifact_sha256 != self.godot.artifact_sha256 {
            return Ok(incompatible(
                CompatibilityReason::GodotArtifactMismatch,
                DiagnosticCode::BridgeVersionIncompatible,
            ));
        }
        if observation.index_schema != self.schemas.index {
            return Ok(incompatible(
                CompatibilityReason::IndexSchemaMismatch,
                DiagnosticCode::StaticCacheIncompatible,
            ));
        }
        if observation.transaction_journal_schema != self.schemas.transaction_journal {
            return Ok(incompatible(
                CompatibilityReason::TransactionJournalSchemaMismatch,
                DiagnosticCode::PackageInvalid,
            ));
        }
        if observation.validation_report_schema != self.schemas.validation_report {
            return Ok(incompatible(
                CompatibilityReason::ValidationReportSchemaMismatch,
                DiagnosticCode::PackageInvalid,
            ));
        }
        if observation.bridge.major != self.protocols.bridge_major {
            return Ok(incompatible(
                CompatibilityReason::BridgeMajorMismatch,
                DiagnosticCode::BridgeVersionIncompatible,
            ));
        }
        if observation.bridge.minor < self.protocols.bridge_min_minor
            || observation.bridge.minor > self.protocols.bridge_current_minor
        {
            return Ok(incompatible(
                CompatibilityReason::BridgeMinorUnsupported,
                DiagnosticCode::BridgeVersionIncompatible,
            ));
        }
        if observation.mcp_protocol != self.protocols.mcp_protocol {
            return Ok(incompatible(
                CompatibilityReason::McpProtocolMismatch,
                DiagnosticCode::SurfaceUnsupported,
            ));
        }

        let profile = self
            .bridge_profiles
            .iter()
            .find(|profile| profile.bridge_minor == observation.bridge.minor)
            .expect("validated profile coverage");
        let mut status = profile.qualification;
        let mut reasons = Vec::new();
        if status == CompatibilityStatus::CompatibleReduced {
            reasons.push(CompatibilityReason::BridgeMinorReduced);
        }
        let enabled_capabilities = profile
            .capabilities
            .intersection(&observation.advertised_capabilities)
            .cloned()
            .collect::<BTreeSet<_>>();
        if enabled_capabilities.len() != profile.capabilities.len() {
            status = CompatibilityStatus::CompatibleReduced;
            reasons.push(CompatibilityReason::BridgeCapabilitiesReduced);
        }
        reasons.sort();
        reasons.dedup();
        Ok(CompatibilityEvaluation {
            status,
            surface_observed: false,
            capability_profile: Some(profile.profile_id.clone()),
            enabled_capabilities,
            reasons,
            diagnostic_code: (status == CompatibilityStatus::CompatibleReduced)
                .then_some(DiagnosticCode::CapabilityUnavailable),
        })
    }

    /// Returns the exact matrix qualification for one host coordinate. This is
    /// the single version-rule lookup used by both full compatibility
    /// evaluation and doctor host probes.
    pub fn surface_qualification(
        &self,
        surface: SurfaceKind,
        host_version: &str,
        ide_host_version: Option<&str>,
        target: TargetCoordinate,
    ) -> Result<Option<SurfaceQualification>, MatrixError> {
        self.validate()?;
        Ok(self
            .surfaces
            .iter()
            .find(|rule| {
                rule.surface == surface
                    && rule.host_version == host_version
                    && rule.ide_host_version.as_deref() == ide_host_version
                    && rule.target == target
            })
            .map(|rule| rule.qualification))
    }

    /// Evaluates one complete product and host observation in a fixed
    /// precedence order.
    pub fn evaluate(
        &self,
        observation: &CompatibilityObservation,
    ) -> Result<CompatibilityEvaluation, MatrixError> {
        let mut evaluation =
            self.evaluate_product(&ProductCompatibilityObservation::from(observation))?;
        evaluation.surface_observed = true;
        if evaluation.status == CompatibilityStatus::Incompatible {
            return Ok(evaluation);
        }

        let Some(surface_qualification) = self.surface_qualification(
            observation.surface,
            &observation.host_version,
            observation.ide_host_version.as_deref(),
            observation.target,
        )?
        else {
            return Ok(CompatibilityEvaluation {
                status: CompatibilityStatus::NotTested,
                surface_observed: true,
                capability_profile: evaluation.capability_profile,
                enabled_capabilities: BTreeSet::new(),
                reasons: vec![CompatibilityReason::SurfaceVersionUnsupported],
                diagnostic_code: Some(DiagnosticCode::SurfaceUnsupported),
            });
        };
        match surface_qualification {
            SurfaceQualification::Supported => {}
            SurfaceQualification::CompatibleReduced => {
                evaluation.status = CompatibilityStatus::CompatibleReduced;
                evaluation
                    .diagnostic_code
                    .get_or_insert(DiagnosticCode::CapabilityUnavailable);
            }
            SurfaceQualification::Incompatible => {
                return Ok(CompatibilityEvaluation {
                    status: CompatibilityStatus::Incompatible,
                    surface_observed: true,
                    capability_profile: evaluation.capability_profile,
                    enabled_capabilities: BTreeSet::new(),
                    reasons: vec![CompatibilityReason::SurfaceVersionUnsupported],
                    diagnostic_code: Some(DiagnosticCode::SurfaceUnsupported),
                });
            }
            SurfaceQualification::Candidate => {
                return Ok(CompatibilityEvaluation {
                    status: CompatibilityStatus::NotTested,
                    surface_observed: true,
                    capability_profile: evaluation.capability_profile,
                    enabled_capabilities: BTreeSet::new(),
                    reasons: vec![CompatibilityReason::SurfaceCandidate],
                    diagnostic_code: Some(DiagnosticCode::SurfaceUnsupported),
                });
            }
            SurfaceQualification::NotTested => {
                return Ok(CompatibilityEvaluation {
                    status: CompatibilityStatus::NotTested,
                    surface_observed: true,
                    capability_profile: evaluation.capability_profile,
                    enabled_capabilities: BTreeSet::new(),
                    reasons: vec![CompatibilityReason::SurfaceNotTested],
                    diagnostic_code: Some(DiagnosticCode::SurfaceUnsupported),
                });
            }
        }

        if self.protocols.requires_form_elicitation && !observation.supports_form_elicitation {
            evaluation.status = CompatibilityStatus::CompatibleReduced;
            evaluation
                .reasons
                .push(CompatibilityReason::HostInteractionUnsupported);
            evaluation
                .enabled_capabilities
                .remove("transaction.scene_v1");
            evaluation
                .enabled_capabilities
                .remove("transaction.change_set_v1");
            evaluation
                .enabled_capabilities
                .remove("validation.automatic_v1");
            evaluation.diagnostic_code = Some(DiagnosticCode::HostInteractionUnsupported);
        }

        evaluation.reasons.sort();
        evaluation.reasons.dedup();
        Ok(evaluation)
    }

    /// Stable lowercase SHA-256 of the canonical typed JSON representation.
    pub fn canonical_digest(&self) -> Result<String, MatrixError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| MatrixError::Json)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    /// Verifies a detached manifest's matrix binding without accepting a
    /// prefix, uppercase spelling, or malformed digest.
    pub fn verify_canonical_digest(&self, expected: &str) -> Result<(), MatrixError> {
        if !lowercase_sha256(expected) || self.canonical_digest()? != expected {
            return Err(MatrixError::DigestMismatch);
        }
        Ok(())
    }

    fn validate_registry(&self) -> Result<(), MatrixError> {
        let registry = canonical_registry_profile();
        if self.registry.profile_id != registry.profile_id
            || self.registry.digest != registry.digest
            || self.registry.tool_count != registry.tool_count
            || self.registry.fixed_resource_count != registry.fixed_resource_count
            || self.registry.resource_template_count != registry.resource_template_count
        {
            return Err(MatrixError::RegistryBinding);
        }
        Ok(())
    }

    fn validate_bridge_profiles(&self) -> Result<(), MatrixError> {
        let expected_count =
            usize::from(self.protocols.bridge_current_minor - self.protocols.bridge_min_minor + 1);
        if self.bridge_profiles.len() != expected_count
            || self.bridge_profiles.len() > MAX_BRIDGE_PROFILES
        {
            return Err(MatrixError::BridgeProfiles);
        }
        let mut by_minor = BTreeMap::new();
        let mut profile_ids = BTreeSet::new();
        for profile in &self.bridge_profiles {
            if !safe_identifier(&profile.profile_id)
                || profile.capabilities.len() > MAX_CAPABILITIES_PER_PROFILE
                || !profile
                    .capabilities
                    .iter()
                    .all(|value| safe_identifier(value))
                || by_minor.insert(profile.bridge_minor, profile).is_some()
                || !profile_ids.insert(&profile.profile_id)
            {
                return Err(MatrixError::BridgeProfiles);
            }
        }
        let mut previous = BTreeSet::new();
        for minor in self.protocols.bridge_min_minor..=self.protocols.bridge_current_minor {
            let profile = by_minor.get(&minor).ok_or(MatrixError::BridgeProfiles)?;
            let expected_qualification = if minor == self.protocols.bridge_current_minor {
                CompatibilityStatus::Supported
            } else {
                CompatibilityStatus::CompatibleReduced
            };
            if profile.qualification != expected_qualification {
                return Err(MatrixError::BridgeProfiles);
            }
            if !previous.is_subset(&profile.capabilities) {
                return Err(MatrixError::NonAdditiveCapabilities);
            }
            previous.clone_from(&profile.capabilities);
        }
        Ok(())
    }

    fn validate_surface_rules(&self) -> Result<(), MatrixError> {
        if self.surfaces.is_empty() || self.surfaces.len() > MAX_SURFACE_RULES {
            return Err(MatrixError::SurfaceRules);
        }
        let mut keys = BTreeSet::new();
        let mut required = BTreeSet::new();
        let mut has_cursor_not_tested = false;
        for rule in &self.surfaces {
            if !safe_identifier(&rule.host_version)
                || rule
                    .ide_host_version
                    .as_deref()
                    .is_some_and(|value| !safe_identifier(value))
                || !keys.insert((
                    rule.surface,
                    rule.host_version.as_str(),
                    rule.ide_host_version.as_deref(),
                    rule.target,
                ))
            {
                return Err(MatrixError::SurfaceRules);
            }
            if rule.qualification == SurfaceQualification::Supported
                && rule.target != self.package.target
            {
                return Err(MatrixError::SurfaceRules);
            }
            if matches!(
                rule.surface,
                SurfaceKind::App | SurfaceKind::Cli | SurfaceKind::Ide
            ) {
                if rule.target != self.package.target
                    || matches!(
                        rule.qualification,
                        SurfaceQualification::Incompatible | SurfaceQualification::NotTested
                    )
                {
                    return Err(MatrixError::SurfaceRules);
                }
                required.insert(rule.surface);
            }
            has_cursor_not_tested |= rule.surface == SurfaceKind::Cursor
                && rule.qualification == SurfaceQualification::NotTested;
        }
        if !required.contains(&SurfaceKind::App)
            || !required.contains(&SurfaceKind::Cli)
            || !required.contains(&SurfaceKind::Ide)
            || !has_cursor_not_tested
        {
            return Err(MatrixError::SurfaceRules);
        }
        Ok(())
    }
}

/// Parses and validates the immutable matrix embedded in this crate.
pub fn embedded_compatibility_matrix() -> Result<CompatibilityMatrix, MatrixError> {
    let matrix: CompatibilityMatrix =
        serde_json::from_str(COMPATIBILITY_MATRIX_JSON).map_err(|_| MatrixError::Json)?;
    matrix.validate()?;
    Ok(matrix)
}

fn incompatible(
    reason: CompatibilityReason,
    diagnostic_code: DiagnosticCode,
) -> CompatibilityEvaluation {
    CompatibilityEvaluation {
        status: CompatibilityStatus::Incompatible,
        surface_observed: false,
        capability_profile: None,
        enabled_capabilities: BTreeSet::new(),
        reasons: vec![reason],
        diagnostic_code: Some(diagnostic_code),
    }
}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+' | b':' | b'/')
        })
}

fn lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn prefixed_lowercase_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(lowercase_sha256)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{
        COMPATIBILITY_MATRIX_JSON_SCHEMA, SURFACE_COMPATIBILITY_BUNDLE_JSON_SCHEMA,
        SURFACE_COMPATIBILITY_BUNDLE_SCHEMA,
    };

    use super::*;

    fn current_observation(
        matrix: &CompatibilityMatrix,
        surface: SurfaceKind,
    ) -> CompatibilityObservation {
        let rule = matrix
            .surfaces
            .iter()
            .find(|rule| rule.surface == surface)
            .unwrap();
        let profile = matrix
            .bridge_profiles
            .iter()
            .find(|profile| profile.bridge_minor == matrix.protocols.bridge_current_minor)
            .unwrap();
        CompatibilityObservation {
            package_version: matrix.package.version.clone(),
            package_manifest_verified: true,
            target: matrix.package.target,
            godot_source_commit: matrix.godot.source_commit.clone(),
            godot_build_id: matrix.godot.build_id.clone(),
            godot_artifact_sha256: matrix.godot.artifact_sha256.clone(),
            bridge: ProtocolVersion {
                major: matrix.protocols.bridge_major,
                minor: matrix.protocols.bridge_current_minor,
            },
            advertised_capabilities: profile.capabilities.clone(),
            mcp_protocol: matrix.protocols.mcp_protocol.clone(),
            supports_form_elicitation: true,
            index_schema: matrix.schemas.index.clone(),
            transaction_journal_schema: matrix.schemas.transaction_journal.clone(),
            validation_report_schema: matrix.schemas.validation_report.clone(),
            surface,
            host_version: rule.host_version.clone(),
            ide_host_version: rule.ide_host_version.clone(),
        }
    }

    fn surface_bundle(matrix: &CompatibilityMatrix, sequence: u64) -> SurfaceCompatibilityBundle {
        let mut surfaces = matrix
            .surfaces
            .iter()
            .filter(|rule| rule.surface != SurfaceKind::Cursor)
            .cloned()
            .collect::<Vec<_>>();
        for rule in &mut surfaces {
            rule.qualification = SurfaceQualification::Supported;
        }
        SurfaceCompatibilityBundle {
            schema_version: SURFACE_COMPATIBILITY_BUNDLE_SCHEMA.to_owned(),
            bundle_id: format!("qualified-hosts-{sequence}"),
            sequence,
            package_version: matrix.package.version.clone(),
            baseline_matrix_sha256: format!("sha256:{}", matrix.canonical_digest().unwrap()),
            host_coordinate_profile_sha256: format!("sha256:{}", "a".repeat(64)),
            target: matrix.package.target,
            surfaces,
        }
    }

    #[test]
    fn embedded_matrix_is_valid_and_digest_is_deterministic() {
        let matrix = embedded_compatibility_matrix().unwrap();
        let digest = matrix.canonical_digest().unwrap();
        assert_eq!(digest.len(), 64);
        assert_eq!(digest, matrix.canonical_digest().unwrap());
        assert_eq!(matrix.verify_canonical_digest(&digest), Ok(()));
        assert_eq!(
            matrix.verify_canonical_digest(&"0".repeat(64)),
            Err(MatrixError::DigestMismatch)
        );
        assert_eq!(
            matrix.verify_canonical_digest("not-a-digest"),
            Err(MatrixError::DigestMismatch)
        );
        assert_eq!(matrix.registry.tool_count, 41);
        assert_eq!(matrix.registry.fixed_resource_count, 4);
        assert_eq!(matrix.registry.resource_template_count, 1);
    }

    #[test]
    fn committed_schema_is_closed_and_embedded_json_rejects_unknown_fields() {
        let schema: serde_json::Value =
            serde_json::from_str(COMPATIBILITY_MATRIX_JSON_SCHEMA).unwrap();
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["schema_version"]["const"],
            COMPATIBILITY_MATRIX_SCHEMA
        );
        assert_eq!(
            schema["$defs"]["surface_rule"]["additionalProperties"],
            false
        );
        assert_eq!(
            schema["$defs"]["surface_rule"]["properties"]["qualification"]["$ref"],
            "#/$defs/surface_qualification"
        );
        assert!(
            schema["$defs"]["surface_qualification"]["enum"]
                .as_array()
                .is_some_and(|values| values.contains(&json!("candidate")))
        );

        let mut matrix_value: serde_json::Value =
            serde_json::from_str(COMPATIBILITY_MATRIX_JSON).unwrap();
        matrix_value["unexpected"] = json!(true);
        assert!(serde_json::from_value::<CompatibilityMatrix>(matrix_value).is_err());
    }

    #[test]
    fn surface_bundle_schema_is_closed_and_matches_the_rust_contract() {
        let schema: serde_json::Value =
            serde_json::from_str(SURFACE_COMPATIBILITY_BUNDLE_JSON_SCHEMA).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["schema_version"]["const"],
            SURFACE_COMPATIBILITY_BUNDLE_SCHEMA
        );
        assert_eq!(schema["$defs"]["surface"]["additionalProperties"], false);

        let matrix = embedded_compatibility_matrix().unwrap();
        let bundle = surface_bundle(&matrix, 1);
        let generated = schemars::schema_for!(SurfaceCompatibilityBundle);
        let generated_value = serde_json::to_value(generated).unwrap();
        assert_eq!(
            generated_value["properties"]["schema_version"]["type"],
            "string"
        );
        let mut bundle_value = serde_json::to_value(bundle).unwrap();
        bundle_value["unexpected"] = json!(true);
        assert!(serde_json::from_value::<SurfaceCompatibilityBundle>(bundle_value).is_err());
    }

    #[test]
    fn surface_bundle_promotes_only_host_surfaces() {
        let matrix = embedded_compatibility_matrix().unwrap();
        let bundle = surface_bundle(&matrix, 1);
        bundle.validate_against(&matrix).unwrap();
        let effective = bundle.apply_to(&matrix).unwrap();
        assert_eq!(effective.package, matrix.package);
        assert_eq!(effective.godot, matrix.godot);
        assert_eq!(effective.protocols, matrix.protocols);
        assert_eq!(effective.schemas, matrix.schemas);
        assert_eq!(effective.registry, matrix.registry);
        assert_eq!(effective.bridge_profiles, matrix.bridge_profiles);
        for surface in [SurfaceKind::App, SurfaceKind::Cli, SurfaceKind::Ide] {
            assert_eq!(
                effective
                    .surfaces
                    .iter()
                    .find(|rule| rule.surface == surface)
                    .unwrap()
                    .qualification,
                SurfaceQualification::Supported
            );
        }
        assert_eq!(
            effective
                .surfaces
                .iter()
                .find(|rule| rule.surface == SurfaceKind::Cursor),
            matrix
                .surfaces
                .iter()
                .find(|rule| rule.surface == SurfaceKind::Cursor)
        );
    }

    #[test]
    fn surface_bundle_rejects_wrong_baseline_and_incomplete_or_unsafe_rules() {
        let matrix = embedded_compatibility_matrix().unwrap();
        let mut bundle = surface_bundle(&matrix, 1);
        bundle.baseline_matrix_sha256 = format!("sha256:{}", "b".repeat(64));
        assert_eq!(
            bundle.validate_against(&matrix),
            Err(SurfaceBundleError::ProductBinding)
        );

        let mut bundle = surface_bundle(&matrix, 1);
        bundle
            .surfaces
            .retain(|rule| rule.surface != SurfaceKind::Ide);
        assert_eq!(
            bundle.validate_against(&matrix),
            Err(SurfaceBundleError::SurfaceRules)
        );

        let mut bundle = surface_bundle(&matrix, 1);
        bundle.surfaces[0].surface = SurfaceKind::Cursor;
        assert_eq!(
            bundle.validate_against(&matrix),
            Err(SurfaceBundleError::SurfaceRules)
        );

        let mut bundle = surface_bundle(&matrix, 1);
        bundle.sequence = 0;
        assert_eq!(
            bundle.validate_against(&matrix),
            Err(SurfaceBundleError::InvalidSequence)
        );
    }

    #[test]
    fn canonical_app_cli_and_ide_rows_are_candidates_and_fail_closed() {
        let matrix = embedded_compatibility_matrix().unwrap();
        for surface in [SurfaceKind::App, SurfaceKind::Cli, SurfaceKind::Ide] {
            let rule = matrix
                .surfaces
                .iter()
                .find(|rule| rule.surface == surface)
                .unwrap();
            assert_eq!(rule.qualification, SurfaceQualification::Candidate);
            let result = matrix
                .evaluate(&current_observation(&matrix, surface))
                .unwrap();
            assert_eq!(result.status, CompatibilityStatus::NotTested);
            assert!(result.surface_observed);
            assert_eq!(result.reasons, [CompatibilityReason::SurfaceCandidate]);
            assert!(result.enabled_capabilities.is_empty());
            assert_eq!(
                result.diagnostic_code,
                Some(DiagnosticCode::SurfaceUnsupported)
            );
        }
    }

    #[test]
    fn canonical_surface_coordinates_are_exact_and_unqualified() {
        let matrix = embedded_compatibility_matrix().unwrap();
        let expected = [
            (
                SurfaceKind::App,
                "26.730.61639",
                None,
                SurfaceQualification::Candidate,
            ),
            (
                SurfaceKind::Cli,
                "0.147.0-alpha.1.2",
                None,
                SurfaceQualification::Candidate,
            ),
            (
                SurfaceKind::Ide,
                "26.721.41059",
                Some("1.131.0"),
                SurfaceQualification::Candidate,
            ),
            (
                SurfaceKind::Cursor,
                "26.721.30844",
                Some("3.8.23"),
                SurfaceQualification::NotTested,
            ),
        ];
        assert_eq!(matrix.surfaces.len(), expected.len());
        for (surface, host_version, ide_host_version, qualification) in expected {
            let rule = matrix
                .surfaces
                .iter()
                .find(|rule| rule.surface == surface)
                .unwrap();
            assert_eq!(rule.host_version, host_version);
            assert_eq!(rule.ide_host_version.as_deref(), ide_host_version);
            assert_eq!(rule.target, matrix.package.target);
            assert_eq!(rule.qualification, qualification);
        }
    }

    fn qualify_surface(matrix: &mut CompatibilityMatrix, surface: SurfaceKind) {
        matrix
            .surfaces
            .iter_mut()
            .find(|rule| rule.surface == surface)
            .unwrap()
            .qualification = SurfaceQualification::Supported;
    }

    #[test]
    fn explicitly_qualified_surface_can_evaluate_as_supported() {
        let mut matrix = embedded_compatibility_matrix().unwrap();
        qualify_surface(&mut matrix, SurfaceKind::Cli);
        let result = matrix
            .evaluate(&current_observation(&matrix, SurfaceKind::Cli))
            .unwrap();
        assert_eq!(result.status, CompatibilityStatus::Supported);
        assert!(result.surface_observed);
        assert!(result.reasons.is_empty());
        assert_eq!(result.diagnostic_code, None);
        assert!(!result.enabled_capabilities.is_empty());
    }

    #[test]
    fn product_evaluation_is_ready_without_claiming_a_host_surface() {
        let matrix = embedded_compatibility_matrix().unwrap();
        let observation = current_observation(&matrix, SurfaceKind::Cli);
        let result = matrix
            .evaluate_product(&ProductCompatibilityObservation::from(&observation))
            .unwrap();
        assert_eq!(result.status, CompatibilityStatus::Supported);
        assert!(!result.surface_observed);
        assert!(result.reasons.is_empty());
        assert_eq!(result.diagnostic_code, None);
        assert!(!result.enabled_capabilities.is_empty());
    }

    #[test]
    fn every_lower_bridge_minor_is_explicitly_reduced() {
        let mut matrix = embedded_compatibility_matrix().unwrap();
        qualify_surface(&mut matrix, SurfaceKind::Cli);
        for minor in matrix.protocols.bridge_min_minor..matrix.protocols.bridge_current_minor {
            let profile = matrix
                .bridge_profiles
                .iter()
                .find(|profile| profile.bridge_minor == minor)
                .unwrap();
            let mut observation = current_observation(&matrix, SurfaceKind::Cli);
            observation.bridge.minor = minor;
            observation.advertised_capabilities = profile.capabilities.clone();
            let result = matrix.evaluate(&observation).unwrap();
            assert_eq!(result.status, CompatibilityStatus::CompatibleReduced);
            assert!(
                result
                    .reasons
                    .contains(&CompatibilityReason::BridgeMinorReduced)
            );
            assert_eq!(result.enabled_capabilities, profile.capabilities);
        }
    }

    #[test]
    fn future_minor_and_wrong_major_fail_closed() {
        let mut matrix = embedded_compatibility_matrix().unwrap();
        qualify_surface(&mut matrix, SurfaceKind::Cli);
        let mut observation = current_observation(&matrix, SurfaceKind::Cli);
        observation.bridge.minor += 1;
        let result = matrix.evaluate(&observation).unwrap();
        assert_eq!(result.status, CompatibilityStatus::Incompatible);
        assert_eq!(
            result.reasons,
            [CompatibilityReason::BridgeMinorUnsupported]
        );

        let mut observation = current_observation(&matrix, SurfaceKind::Cli);
        observation.bridge.major += 1;
        let result = matrix.evaluate(&observation).unwrap();
        assert_eq!(result.status, CompatibilityStatus::Incompatible);
        assert_eq!(result.reasons, [CompatibilityReason::BridgeMajorMismatch]);
    }

    #[test]
    fn missing_capability_or_form_support_only_enables_proven_subset() {
        let mut matrix = embedded_compatibility_matrix().unwrap();
        qualify_surface(&mut matrix, SurfaceKind::Cli);
        let mut observation = current_observation(&matrix, SurfaceKind::Cli);
        observation
            .advertised_capabilities
            .remove("runtime.viewport_capture");
        let result = matrix.evaluate(&observation).unwrap();
        assert_eq!(result.status, CompatibilityStatus::CompatibleReduced);
        assert!(
            result
                .reasons
                .contains(&CompatibilityReason::BridgeCapabilitiesReduced)
        );
        assert!(
            !result
                .enabled_capabilities
                .contains("runtime.viewport_capture")
        );

        let mut observation = current_observation(&matrix, SurfaceKind::Cli);
        observation.supports_form_elicitation = false;
        let result = matrix.evaluate(&observation).unwrap();
        assert_eq!(result.status, CompatibilityStatus::CompatibleReduced);
        assert_eq!(
            result.diagnostic_code,
            Some(DiagnosticCode::HostInteractionUnsupported)
        );
        assert!(!result.enabled_capabilities.contains("transaction.scene_v1"));
        assert!(
            !result
                .enabled_capabilities
                .contains("transaction.change_set_v1")
        );
    }

    #[test]
    fn cursor_is_not_a_substitute_for_the_required_ide() {
        let matrix = embedded_compatibility_matrix().unwrap();
        let cursor = matrix
            .surfaces
            .iter()
            .find(|rule| rule.surface == SurfaceKind::Cursor)
            .unwrap();
        let mut observation = current_observation(&matrix, SurfaceKind::Ide);
        observation.surface = SurfaceKind::Cursor;
        observation.host_version.clone_from(&cursor.host_version);
        observation
            .ide_host_version
            .clone_from(&cursor.ide_host_version);
        let result = matrix.evaluate(&observation).unwrap();
        assert_eq!(result.status, CompatibilityStatus::NotTested);
        assert_eq!(result.reasons, [CompatibilityReason::SurfaceNotTested]);
        assert!(result.enabled_capabilities.is_empty());
    }

    #[test]
    fn exact_versions_prevent_prefix_or_future_host_guessing() {
        let mut matrix = embedded_compatibility_matrix().unwrap();
        qualify_surface(&mut matrix, SurfaceKind::Cli);
        let mut observation = current_observation(&matrix, SurfaceKind::Cli);
        observation.host_version.push_str(".1");
        let result = matrix.evaluate(&observation).unwrap();
        assert_eq!(result.status, CompatibilityStatus::NotTested);
        assert_eq!(
            result.reasons,
            [CompatibilityReason::SurfaceVersionUnsupported]
        );
        assert!(result.enabled_capabilities.is_empty());
    }

    #[test]
    fn package_godot_schema_and_target_mismatches_have_stable_precedence() {
        let mut matrix = embedded_compatibility_matrix().unwrap();
        qualify_surface(&mut matrix, SurfaceKind::Cli);
        let mut observation = current_observation(&matrix, SurfaceKind::Cli);
        observation.package_manifest_verified = false;
        observation.index_schema = "9.9".to_owned();
        let result = matrix.evaluate(&observation).unwrap();
        assert_eq!(
            result.reasons,
            [CompatibilityReason::PackageManifestUnverified]
        );

        let mut observation = current_observation(&matrix, SurfaceKind::Cli);
        observation.godot_artifact_sha256 = "0".repeat(64);
        let result = matrix.evaluate(&observation).unwrap();
        assert_eq!(result.reasons, [CompatibilityReason::GodotArtifactMismatch]);

        let mut observation = current_observation(&matrix, SurfaceKind::Cli);
        observation.index_schema = "1.2".to_owned();
        let result = matrix.evaluate(&observation).unwrap();
        assert_eq!(result.reasons, [CompatibilityReason::IndexSchemaMismatch]);

        let mut observation = current_observation(&matrix, SurfaceKind::Cli);
        observation.target.architecture = Architecture::X86_64;
        let result = matrix.evaluate(&observation).unwrap();
        assert_eq!(result.reasons, [CompatibilityReason::ArchitectureMismatch]);

        let mut observation = current_observation(&matrix, SurfaceKind::Cli);
        observation.package_version = "0.0.9".to_owned();
        let result = matrix.evaluate(&observation).unwrap();
        assert_eq!(
            result.reasons,
            [CompatibilityReason::PackageVersionMismatch]
        );
    }

    #[test]
    fn malformed_duplicate_and_non_additive_rules_are_rejected() {
        let matrix = embedded_compatibility_matrix().unwrap();

        let mut duplicate = matrix.clone();
        duplicate.surfaces.push(duplicate.surfaces[0].clone());
        assert_eq!(duplicate.validate(), Err(MatrixError::SurfaceRules));

        let mut gap = matrix.clone();
        gap.bridge_profiles.remove(1);
        assert_eq!(gap.validate(), Err(MatrixError::BridgeProfiles));

        let mut non_additive = matrix.clone();
        non_additive.bridge_profiles[2].capabilities.clear();
        assert_eq!(
            non_additive.validate(),
            Err(MatrixError::NonAdditiveCapabilities)
        );

        let mut wrong_registry = matrix;
        wrong_registry.registry.tool_count = 40;
        assert_eq!(wrong_registry.validate(), Err(MatrixError::RegistryBinding));
    }
}
