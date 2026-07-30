use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component as PathComponent, Path, PathBuf};

use godot_codex_bridge_client::{BridgeError, BridgeFailureClass, project_id_for_path};
use godot_codex_index_store::{SegmentStore, StoreError};
use godot_codex_product::{
    COMPATIBILITY_MATRIX_JSON, Component, ConfigurationCondition, Diagnostic, DiagnosticCode,
    HOST_COORDINATE_PROFILE_JSON, PackageCondition, ProductCompatibilityBasis,
    ProductStartupObservation, READ_ONLY_TOOLS, REGISTRY_PROFILE_JSON, SERVER_INSTRUCTIONS_TEXT,
    SurfaceKind, TargetCoordinate, canonical_registry_profile, configuration_diagnostic,
    embedded_compatibility_matrix, package_diagnostic,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use toml_edit::DocumentMut;

use crate::PRODUCT_VERSION;
use crate::compatibility_bundle::load_effective_compatibility_matrix;
use crate::launcher::{LauncherResolution, resolve_launcher};

const REPORT_SCHEMA: &str = "godot-codex-doctor-report/1.0";
#[cfg(test)]
const REPORT_SCHEMA_JSON: &str =
    include_str!("../../../schemas/godot_codex/doctor-report.schema.json");
const PACKAGE_MANIFEST_SCHEMA: &str = "godot-codex-package/1.0";
const PACKAGE_MANIFEST_NAME: &str = "package-manifest.json";
const PACKAGE_CHECKSUMS_NAME: &str = "checksums.sha256";
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MAX_DISCOVERY_BYTES: u64 = 16 * 1024;
const MAX_MANIFEST_BYTES: u64 = 512 * 1024;
const MAX_CHECKSUMS_BYTES: u64 = 128 * 1024;
const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_REPORT_BYTES: usize = 64 * 1024;
const MAX_COORDINATE_BYTES: usize = 128;
const GODOT_EXPECTED_INSTALL_PATH: &str = "~/Applications/Godot Codex.app/Contents/MacOS/Godot";

/// Codex surface selected for deterministic compatibility checks.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceSelection {
    Auto,
    App,
    Cli,
    Ide,
}

impl SurfaceSelection {
    pub(crate) fn explicit(self) -> Option<SurfaceKind> {
        match self {
            Self::Auto => None,
            Self::App => Some(SurfaceKind::App),
            Self::Cli => Some(SurfaceKind::Cli),
            Self::Ide => Some(SurfaceKind::Ide),
        }
    }
}

/// Bounded observation supplied by a host-specific, read-only probe adapter.
/// The diagnostics engine never shells out or opens a transport itself.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostProbeObservation {
    pub surface: SurfaceKind,
    pub host_version: String,
    pub ide_host_version: Option<String>,
    /// `None` means the host-private effective layer cannot be observed by a
    /// bounded local command. It must never be projected as a pass.
    pub effective_project_config: Option<bool>,
    /// `None` means restart state is private to the selected host process.
    pub restart_required: Option<bool>,
    /// Static local capability observation. This is not a substitute for the
    /// live accept/decline/cancel/timeout qualification trace.
    pub supports_form_elicitation: Option<bool>,
}

/// Exact observation of the separately distributed Godot prerequisite.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GodotProbeObservation {
    pub architecture: String,
    pub source_commit: String,
    pub build_id: String,
    pub artifact_sha256: String,
}

/// Bounded result of one model-free MCP stdio contract probe.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpProbeObservation {
    pub initialized: bool,
    pub protocol_version: String,
    pub project_id: String,
    pub status_tool_available: bool,
    pub registry_digest: String,
    pub tool_count: usize,
    pub fixed_resource_count: usize,
    pub resource_template_count: usize,
    pub instructions_present: bool,
    /// Whether the selected real host advertised standard form elicitation to
    /// this server. A direct doctor stdio client cannot prove that, so its
    /// honest value is `None`.
    pub supports_form_elicitation: Option<bool>,
}

/// Closed outcome of the production Bridge client's authenticated,
/// read-only connect/initialize handshake.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeProbeStatus {
    Ready,
    Unreachable,
    TimedOut,
    AuthenticationFailed,
    ProjectBindingMismatch,
    ProtocolVersionMismatch,
}

impl BridgeProbeStatus {
    /// Converts the shared Bridge classifier without inspecting error text.
    #[must_use]
    pub const fn from_failure_class(failure: BridgeFailureClass) -> Self {
        match failure {
            BridgeFailureClass::TransportUnavailable => Self::Unreachable,
            BridgeFailureClass::TimedOut => Self::TimedOut,
            BridgeFailureClass::AuthenticationFailed => Self::AuthenticationFailed,
            BridgeFailureClass::ProjectBindingMismatch => Self::ProjectBindingMismatch,
            BridgeFailureClass::ProtocolVersionIncompatible => Self::ProtocolVersionMismatch,
        }
    }

    /// Public diagnostic selected by the production doctor gate.
    #[must_use]
    pub const fn diagnostic_code(self) -> Option<DiagnosticCode> {
        match self {
            Self::Ready => None,
            Self::Unreachable | Self::TimedOut => Some(DiagnosticCode::BridgeUnreachable),
            Self::AuthenticationFailed => Some(DiagnosticCode::BridgeAuthenticationFailed),
            Self::ProjectBindingMismatch => Some(DiagnosticCode::ProjectBindingMismatch),
            Self::ProtocolVersionMismatch => Some(DiagnosticCode::BridgeVersionIncompatible),
        }
    }

    const fn failure_policy(self) -> Option<(DiagnosticCode, u8, bool)> {
        let code = match self.diagnostic_code() {
            Some(code) => code,
            None => return None,
        };
        let (class, retryable_transport) = match self {
            Self::Unreachable | Self::TimedOut => (4, true),
            Self::ProtocolVersionMismatch => (5, false),
            Self::AuthenticationFailed | Self::ProjectBindingMismatch => (6, false),
            Self::Ready => return None,
        };
        Some((code, class, retryable_transport))
    }
}

/// Safe Bridge handshake observation. Endpoint, token, PID, nonces, proofs,
/// native identifiers, and raw protocol errors are never retained.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeProbeObservation {
    pub status: BridgeProbeStatus,
    pub negotiated_protocol: Option<String>,
}

impl BridgeProbeObservation {
    pub(crate) fn ready(protocol: String) -> Self {
        Self {
            status: BridgeProbeStatus::Ready,
            negotiated_protocol: Some(protocol),
        }
    }

    pub(crate) fn timed_out() -> Self {
        Self {
            status: BridgeProbeStatus::TimedOut,
            negotiated_protocol: None,
        }
    }

    pub fn failed(error: &BridgeError) -> Self {
        let status = BridgeProbeStatus::from_failure_class(error.failure_class());
        Self {
            status,
            negotiated_protocol: None,
        }
    }
}

/// Optional precomputed observations injected by a surface adapter. Keeping
/// these values separate preserves a deterministic core after the bounded
/// adapters have completed.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorProbeObservations {
    pub host: Option<HostProbeObservation>,
    pub godot: Option<GodotProbeObservation>,
    pub mcp: Option<McpProbeObservation>,
    pub bridge: Option<BridgeProbeObservation>,
}

/// Inputs for one network-free doctor run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoctorOptions {
    pub project_root: PathBuf,
    pub surface: SurfaceSelection,
    pub require_editor: bool,
    pub show_paths: bool,
    /// Overrides the directory containing the detached package manifest and
    /// both binaries. This is primarily useful to package validators.
    pub package_directory: Option<PathBuf>,
}

impl DoctorOptions {
    #[must_use]
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            surface: SurfaceSelection::Auto,
            require_editor: false,
            show_paths: false,
            package_directory: None,
        }
    }
}

/// Closed status for a single deterministic check.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCheckStatus {
    Pass,
    Warn,
    Fail,
}

/// One safe, bounded diagnostic check.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorCheck {
    pub check_id: String,
    pub component: Component,
    pub status: DoctorCheckStatus,
    pub code: DiagnosticCode,
    pub summary: String,
    pub observed_version: Option<String>,
    pub supported: Option<String>,
    pub remediation_id: Option<godot_codex_product::RemediationId>,
    pub retryable: bool,
}

impl DoctorCheck {
    fn from_code(
        check_id: &'static str,
        status: DoctorCheckStatus,
        code: DiagnosticCode,
        observed_version: Option<&str>,
        supported: Option<&str>,
    ) -> Self {
        let diagnostic = Diagnostic::new(code, observed_version, supported);
        Self {
            check_id: check_id.to_owned(),
            component: diagnostic.component,
            status,
            code,
            summary: diagnostic.summary,
            observed_version: diagnostic.observed_version,
            supported: diagnostic.supported,
            remediation_id: diagnostic.remediation_id,
            retryable: diagnostic.retryable,
        }
    }
}

/// Overall safe doctor status.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    Ready,
    Warning,
    Error,
}

/// Closed machine-readable doctor report.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorReport {
    pub schema_version: String,
    pub status: DoctorStatus,
    pub exit_code: u8,
    pub editor_required: bool,
    pub requested_surface: SurfaceSelection,
    pub surface: Option<SurfaceKind>,
    pub project_root: Option<String>,
    pub package_version: String,
    pub compatibility_matrix_digest: String,
    pub registry_digest: String,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageManifest {
    schema_version: String,
    package_version: String,
    source_commit: String,
    target: ManifestTarget,
    build_provenance: BuildProvenance,
    godot_prerequisite: GodotPrerequisite,
    compatibility_matrix_sha256: String,
    registry_sha256: String,
    third_party_licenses_sha256: String,
    contents: Vec<PackageContent>,
    checksums_path: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct BuildProvenance {
    cargo_lock_sha256: String,
    cargo_version: String,
    fresh_target: bool,
    rust_toolchain_sha256: String,
    rustc_commit: String,
    rustc_release: String,
    target_triple: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GodotPrerequisite {
    architecture: String,
    commit: String,
    expected_install_path: String,
    sha256: String,
    verification: GodotVerification,
    version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GodotVerification {
    sha256: Vec<String>,
    version: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestTarget {
    architecture: String,
    os: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageContent {
    mode: String,
    path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryRecord {
    discovery_schema: u32,
    created_at: String,
    transport: String,
    endpoint: String,
    token_file: String,
    project_id: String,
    editor_session_id: String,
    pid: i64,
    protocol_versions: Vec<String>,
}

/// Runs all checks without connecting to an endpoint, mutating a file, or
/// consulting the network or a model.
#[must_use]
pub fn run_doctor(options: &DoctorOptions) -> DoctorReport {
    run_doctor_inner(options, None)
}

/// Runs the deterministic diagnostic engine with observations from an
/// explicitly selected, bounded, read-only adapter.
#[must_use]
pub fn run_doctor_with_probes(
    options: &DoctorOptions,
    probes: &DoctorProbeObservations,
) -> DoctorReport {
    run_doctor_inner(options, Some(probes))
}

fn run_doctor_inner(
    options: &DoctorOptions,
    injected_probes: Option<&DoctorProbeObservations>,
) -> DoctorReport {
    let baseline = embedded_compatibility_matrix()
        .expect("the compiled compatibility matrix passed product-core tests");
    let installed_data_root = options
        .package_directory
        .is_none()
        .then(|| resolve_launcher(None).ok())
        .flatten()
        .and_then(|launcher| launcher.installed_data_root().map(Path::to_path_buf));
    let (matrix, bundle_invalid) = installed_data_root.as_deref().map_or_else(
        || (baseline.clone(), false),
        |data_root| match load_effective_compatibility_matrix(data_root) {
            Ok(effective) => (effective.matrix, false),
            Err(_) => (baseline.clone(), true),
        },
    );
    let registry = canonical_registry_profile();
    let matrix_digest = matrix
        .canonical_digest()
        .expect("the compiled compatibility matrix is canonical");
    let mut checks = Vec::with_capacity(10);
    let mut exit_code = 0_u8;

    if bundle_invalid {
        exit_code = exit_code.max(3);
        checks.push(DoctorCheck::from_code(
            "compatibility.bundle",
            DoctorCheckStatus::Fail,
            DiagnosticCode::PackageInvalid,
            None,
            None,
        ));
    } else {
        checks.push(pass("compatibility.bundle", Component::Package));
    }

    let startup = inspect_product_startup(
        &options.project_root,
        options.package_directory.as_deref(),
        &matrix,
    );
    let package = &startup.package;
    if let Some(code) = package_diagnostic(startup.observation.package) {
        exit_code = exit_code.max(3);
        checks.push(DoctorCheck::from_code(
            "package.integrity",
            DoctorCheckStatus::Fail,
            code,
            None,
            Some(PRODUCT_VERSION),
        ));
    } else {
        checks.push(pass("package.integrity", Component::Package));
    }

    let Some(canonical_root) = startup.canonical_root.clone() else {
        exit_code = exit_code.max(2);
        checks.push(DoctorCheck::from_code(
            "project.root",
            DoctorCheckStatus::Fail,
            DiagnosticCode::ProjectInvalid,
            None,
            None,
        ));
        return finish_report(
            options,
            &matrix_digest,
            &registry.digest,
            checks,
            exit_code,
            None,
            options.surface.explicit(),
        );
    };
    checks.push(pass("project.root", Component::Project));

    if let Some(code) = configuration_diagnostic(startup.observation.configuration) {
        exit_code = exit_code.max(2);
        checks.push(DoctorCheck::from_code(
            "project.config",
            DoctorCheckStatus::Fail,
            code,
            None,
            None,
        ));
    } else {
        checks.push(pass("project.config", Component::ProjectConfig));
    }

    let local_probes;
    let probes = if let Some(probes) = injected_probes {
        probes
    } else {
        let sidecar = package
            .package_root
            .as_deref()
            .and(startup.launcher.as_ref())
            .map(|launcher| launcher.command().to_path_buf())
            .unwrap_or_default();
        let programs = crate::local_probe::LocalProbePrograms::for_system(sidecar);
        let godot_spec = package
            .godot_prerequisite
            .as_ref()
            .and_then(godot_probe_spec);
        local_probes = crate::local_probe::collect_local_observations(
            &canonical_root,
            options.surface,
            &matrix.protocols.mcp_protocol,
            &registry,
            godot_spec.as_ref(),
            &programs,
            (
                startup
                    .launcher
                    .as_ref()
                    .map(LauncherResolution::command)
                    .unwrap_or_else(|| Path::new("")),
                startup
                    .launcher
                    .as_ref()
                    .and_then(LauncherResolution::installed_data_root),
            ),
        );
        &local_probes
    };

    let surface = check_host_probe(
        options,
        probes.host.as_ref(),
        &matrix,
        &mut checks,
        &mut exit_code,
    );

    check_godot_probe(
        probes.godot.as_ref(),
        package.godot_prerequisite.as_ref(),
        &matrix,
        &mut checks,
        &mut exit_code,
    );

    let discovery = check_discovery(&canonical_root, &matrix);
    match &discovery {
        DiscoveryOutcome::Online { protocol } => {
            checks.push(DoctorCheck::from_code(
                "discovery.binding",
                DoctorCheckStatus::Pass,
                DiagnosticCode::Ready,
                Some(protocol),
                Some("bridge-rpc/1.0-1.8"),
            ));
        }
        DiscoveryOutcome::Offline(code) => {
            let is_required = options.require_editor;
            if is_required {
                exit_code = exit_code.max(4);
            } else {
                exit_code = exit_code.max(1);
            }
            checks.push(DoctorCheck::from_code(
                "discovery.binding",
                if is_required {
                    DoctorCheckStatus::Fail
                } else {
                    DoctorCheckStatus::Warn
                },
                *code,
                None,
                Some("bridge-rpc/1.0-1.8"),
            ));
        }
        DiscoveryOutcome::Failed { code, class } => {
            exit_code = exit_code.max(*class);
            checks.push(DoctorCheck::from_code(
                "discovery.binding",
                DoctorCheckStatus::Fail,
                *code,
                None,
                Some("bridge-rpc/1.0-1.8"),
            ));
        }
    }
    if matches!(discovery, DiscoveryOutcome::Online { .. }) {
        check_bridge_probe(
            options,
            probes.bridge.as_ref(),
            &matrix,
            &mut checks,
            &mut exit_code,
        );
    }

    let project_id = project_id_for_path(&canonical_root).ok();
    let generation =
        project_id.as_deref().map(|project_id| {
            match SegmentStore::writer_lease_busy(&canonical_root) {
                Ok(true) => Err(StoreError::StoreBusy),
                Ok(false) => SegmentStore::read_generation(&canonical_root, project_id),
                Err(error) => Err(error),
            }
        });
    match generation {
        Some(Ok(generation))
            if format!(
                "{}.{}",
                generation.schema_version.major, generation.schema_version.minor
            ) == matrix.schemas.index =>
        {
            let observed_schema = format!(
                "{}.{}",
                generation.schema_version.major, generation.schema_version.minor
            );
            checks.push(DoctorCheck::from_code(
                "cache.integrity",
                DoctorCheckStatus::Pass,
                DiagnosticCode::Ready,
                Some(&observed_schema),
                Some(&matrix.schemas.index),
            ));
        }
        Some(Ok(_)) | Some(Err(StoreError::IncompatibleSchema)) => {
            exit_code = exit_code.max(7);
            checks.push(DoctorCheck::from_code(
                "cache.integrity",
                DoctorCheckStatus::Fail,
                DiagnosticCode::StaticCacheIncompatible,
                None,
                Some(&matrix.schemas.index),
            ));
        }
        Some(Err(StoreError::CorruptStore(_)))
        | Some(Err(StoreError::ValidationFailed(_)))
        | Some(Err(StoreError::ProjectMismatch)) => {
            exit_code = exit_code.max(7);
            checks.push(DoctorCheck::from_code(
                "cache.integrity",
                DoctorCheckStatus::Fail,
                DiagnosticCode::StaticCacheCorrupt,
                None,
                Some(&matrix.schemas.index),
            ));
        }
        Some(Err(StoreError::StoreBusy)) => {
            exit_code = exit_code.max(1);
            checks.push(DoctorCheck::from_code(
                "cache.integrity",
                DoctorCheckStatus::Warn,
                DiagnosticCode::ProjectSessionBusy,
                None,
                Some(&matrix.schemas.index),
            ));
        }
        Some(Err(StoreError::Cancelled)) => {
            exit_code = exit_code.max(1);
            checks.push(DoctorCheck::from_code(
                "cache.integrity",
                DoctorCheckStatus::Warn,
                DiagnosticCode::StaticCacheRebuilding,
                None,
                Some(&matrix.schemas.index),
            ));
        }
        Some(Err(StoreError::NotReady | StoreError::StorageIo(_)))
        | Some(Err(
            StoreError::ResourceNotFound
            | StoreError::ScriptSymbolNotFound
            | StoreError::QueryOffsetOutOfRange,
        ))
        | None => {
            exit_code = exit_code.max(1);
            checks.push(DoctorCheck::from_code(
                "cache.integrity",
                DoctorCheckStatus::Warn,
                DiagnosticCode::StaticCacheUnavailable,
                None,
                Some(&matrix.schemas.index),
            ));
        }
    }

    check_mcp_probe(
        probes.mcp.as_ref(),
        project_id.as_deref(),
        &matrix,
        &registry.digest,
        &mut checks,
        &mut exit_code,
    );

    if exit_code == 0 {
        checks.push(pass("overall.ready", Component::Sidecar));
    }
    finish_report(
        options,
        &matrix_digest,
        &registry.digest,
        checks,
        exit_code,
        Some(&canonical_root),
        surface,
    )
}

fn godot_probe_spec(
    prerequisite: &GodotPrerequisite,
) -> Option<crate::local_probe::GodotProbeSpec> {
    let suffix = prerequisite.expected_install_path.strip_prefix("~/")?;
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if !home.is_absolute() || home.as_os_str().as_encoded_bytes().len() > 4096 || suffix.is_empty()
    {
        return None;
    }
    Some(crate::local_probe::GodotProbeSpec {
        path: home.join(suffix),
        architecture: prerequisite.architecture.clone(),
        source_commit: prerequisite.commit.clone(),
        build_id: prerequisite.version.clone(),
        artifact_sha256: prerequisite.sha256.clone(),
    })
}

struct PackageCheck {
    code: DiagnosticCode,
    godot_prerequisite: Option<GodotPrerequisite>,
    package_root: Option<PathBuf>,
}

struct StartupInspection {
    observation: ProductStartupObservation,
    package: PackageCheck,
    canonical_root: Option<PathBuf>,
    launcher: Option<LauncherResolution>,
}

/// Observes package integrity and exact project configuration using the same
/// bounded, read-only adapters as doctor. The returned value contains no
/// filesystem paths and is safe to retain in the MCP server.
#[must_use]
pub fn observe_product_startup(
    project_root: &Path,
    package_directory: Option<&Path>,
) -> ProductStartupObservation {
    let matrix = embedded_compatibility_matrix()
        .expect("the compiled compatibility matrix passed product-core tests");
    inspect_product_startup(project_root, package_directory, &matrix).observation
}

fn inspect_product_startup(
    project_root: &Path,
    package_directory: Option<&Path>,
    matrix: &godot_codex_product::CompatibilityMatrix,
) -> StartupInspection {
    let launcher = resolve_launcher(package_directory).ok();
    let package_directory = launcher
        .as_ref()
        .map(|launcher| launcher.package_root().to_path_buf())
        .or_else(|| package_directory.map(Path::to_path_buf))
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf))
        })
        .unwrap_or_default();
    let mut package = check_package(&package_directory, matrix);
    if launcher.is_none() && package.code == DiagnosticCode::Ready {
        package.code = DiagnosticCode::PackageInvalid;
        package.godot_prerequisite = None;
        package.package_root = None;
    }
    let package_condition = package_condition(package.code);
    let canonical_root = canonical_project_root(project_root);
    let configuration =
        canonical_root
            .as_deref()
            .map_or(ConfigurationCondition::ProjectInvalid, |root| {
                let Some(launcher) = launcher.as_ref() else {
                    return ConfigurationCondition::Invalid;
                };
                match check_config(root, launcher.command(), launcher.installed_data_root()) {
                    Ok(()) => ConfigurationCondition::Ready,
                    Err(DiagnosticCode::ProjectConfigMissing) => ConfigurationCondition::Missing,
                    Err(DiagnosticCode::ProjectConfigNotEffective) => {
                        ConfigurationCondition::NotEffective
                    }
                    Err(DiagnosticCode::ProjectUntrustedOrRestartRequired) => {
                        ConfigurationCondition::TrustOrRestartRequired
                    }
                    Err(_) => ConfigurationCondition::Invalid,
                }
            });
    let compatibility_basis =
        (package_condition == PackageCondition::Ready).then(|| ProductCompatibilityBasis {
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
        });
    StartupInspection {
        observation: ProductStartupObservation {
            package_version: PRODUCT_VERSION.to_owned(),
            package: package_condition,
            configuration,
            compatibility_basis,
        },
        package,
        canonical_root,
        launcher,
    }
}

fn package_condition(code: DiagnosticCode) -> PackageCondition {
    match code {
        DiagnosticCode::Ready => PackageCondition::Ready,
        DiagnosticCode::BinaryMissing => PackageCondition::Missing,
        DiagnosticCode::BinaryNotExecutable => PackageCondition::NotExecutable,
        DiagnosticCode::BinaryArchMismatch => PackageCondition::ArchitectureMismatch,
        _ => PackageCondition::Invalid,
    }
}

fn check_package(
    directory: &Path,
    matrix: &godot_codex_product::CompatibilityMatrix,
) -> PackageCheck {
    let failure = |code| PackageCheck {
        code,
        godot_prerequisite: None,
        package_root: None,
    };
    let expected_target = &matrix.package.target;
    if runtime_target().as_ref() != Some(expected_target) {
        return failure(DiagnosticCode::BinaryArchMismatch);
    }
    let package_root = if directory.join(PACKAGE_MANIFEST_NAME).is_file() {
        directory.to_path_buf()
    } else {
        let Some(parent) = directory.parent() else {
            return failure(DiagnosticCode::PackageInvalid);
        };
        if !parent.join(PACKAGE_MANIFEST_NAME).is_file() {
            return failure(DiagnosticCode::PackageInvalid);
        }
        parent.to_path_buf()
    };
    let Ok(package_root) = fs::canonicalize(package_root) else {
        return failure(DiagnosticCode::PackageInvalid);
    };
    let operations = package_root.join("bin/godot-codex");
    let sidecar = package_root.join("bin/godot-codex-mcp");
    for binary in [&operations, &sidecar] {
        if !binary.is_file() {
            return failure(DiagnosticCode::BinaryMissing);
        }
        if !is_executable(binary) {
            return failure(DiagnosticCode::BinaryNotExecutable);
        }
    }
    let manifest_path = package_root.join(PACKAGE_MANIFEST_NAME);
    let Ok(bytes) = read_bounded(&manifest_path, MAX_MANIFEST_BYTES) else {
        return failure(DiagnosticCode::PackageInvalid);
    };
    let Ok(manifest) = serde_json::from_slice::<PackageManifest>(&bytes) else {
        return failure(DiagnosticCode::PackageInvalid);
    };
    let expected_architecture = manifest_target(expected_target).architecture;
    let expected_godot = GodotPrerequisite {
        architecture: expected_architecture,
        commit: matrix.godot.source_commit.clone(),
        expected_install_path: GODOT_EXPECTED_INSTALL_PATH.to_owned(),
        sha256: format!("sha256:{}", matrix.godot.artifact_sha256),
        verification: GodotVerification {
            sha256: vec![
                "/usr/bin/shasum".to_owned(),
                "-a".to_owned(),
                "256".to_owned(),
                "<godot-binary>".to_owned(),
            ],
            version: vec!["<godot-binary>".to_owned(), "--version".to_owned()],
        },
        version: matrix.godot.build_id.clone(),
    };
    if manifest.schema_version != PACKAGE_MANIFEST_SCHEMA
        || manifest.package_version != PRODUCT_VERSION
        || !valid_source_commit(&manifest.source_commit)
        || !valid_build_provenance(&manifest.build_provenance)
        || manifest.target != manifest_target(expected_target)
        || manifest.compatibility_matrix_sha256
            != format!("sha256:{}", sha256(COMPATIBILITY_MATRIX_JSON.as_bytes()))
        || manifest.registry_sha256
            != format!("sha256:{}", sha256(REGISTRY_PROFILE_JSON.as_bytes()))
        || !valid_prefixed_sha256(&manifest.third_party_licenses_sha256)
        || manifest.checksums_path != PACKAGE_CHECKSUMS_NAME
        || manifest.godot_prerequisite != expected_godot
    {
        return failure(if manifest.target != manifest_target(expected_target) {
            DiagnosticCode::BinaryArchMismatch
        } else {
            DiagnosticCode::PackageInvalid
        });
    }
    if manifest.contents.is_empty() || manifest.contents.len() > 512 {
        return failure(DiagnosticCode::PackageInvalid);
    }
    let mut content_paths = BTreeMap::new();
    let mut total_bytes = 0_u64;
    if manifest.contents.iter().any(|content| {
        content_paths
            .insert(content.path.as_str(), content)
            .is_some()
            || !safe_package_path(&content.path)
            || matches!(
                content.path.as_str(),
                PACKAGE_MANIFEST_NAME | PACKAGE_CHECKSUMS_NAME
            )
            || !valid_prefixed_sha256(&content.sha256)
            || content.bytes > MAX_BINARY_BYTES
            || {
                total_bytes = total_bytes.saturating_add(content.bytes);
                total_bytes > MAX_PACKAGE_BYTES
            }
            || !matches!(content.mode.as_str(), "0644" | "0755")
    }) {
        return failure(DiagnosticCode::PackageInvalid);
    }
    for content in &manifest.contents {
        let path = package_root.join(&content.path);
        let Ok(file_bytes) = read_bounded(&path, MAX_BINARY_BYTES) else {
            return failure(DiagnosticCode::PackageInvalid);
        };
        if content.bytes != file_bytes.len() as u64
            || content.sha256 != format!("sha256:{}", sha256(&file_bytes))
            || actual_mode(&path).as_deref() != Some(content.mode.as_str())
        {
            return failure(DiagnosticCode::PackageInvalid);
        }
    }
    let version_bytes = format!("{PRODUCT_VERSION}\n").into_bytes();
    let source_commit_bytes = format!("{}\n", manifest.source_commit).into_bytes();
    for (name, path, expected_mode, embedded) in [
        ("bin/godot-codex", operations, "0755", None),
        ("bin/godot-codex-mcp", sidecar, "0755", None),
        (
            "VERSION",
            package_root.join("VERSION"),
            "0644",
            Some(version_bytes.as_slice()),
        ),
        (
            "SOURCE_COMMIT",
            package_root.join("SOURCE_COMMIT"),
            "0644",
            Some(source_commit_bytes.as_slice()),
        ),
        (
            "share/godot-codex/product/compatibility-matrix.v1.json",
            package_root.join("share/godot-codex/product/compatibility-matrix.v1.json"),
            "0644",
            Some(COMPATIBILITY_MATRIX_JSON.as_bytes()),
        ),
        (
            "share/godot-codex/product/registry-profile.v1.json",
            package_root.join("share/godot-codex/product/registry-profile.v1.json"),
            "0644",
            Some(REGISTRY_PROFILE_JSON.as_bytes()),
        ),
        (
            "share/godot-codex/product/host-coordinate-profile.v1.json",
            package_root.join("share/godot-codex/product/host-coordinate-profile.v1.json"),
            "0644",
            Some(HOST_COORDINATE_PROFILE_JSON.as_bytes()),
        ),
        (
            "share/godot-codex/product/server-instructions.v1.txt",
            package_root.join("share/godot-codex/product/server-instructions.v1.txt"),
            "0644",
            Some(SERVER_INSTRUCTIONS_TEXT.as_bytes()),
        ),
        (
            "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt",
            package_root.join("share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt"),
            "0644",
            None,
        ),
    ] {
        let Some(expected) = manifest
            .contents
            .iter()
            .find(|content| content.path == name)
        else {
            return failure(DiagnosticCode::PackageInvalid);
        };
        let Ok(bytes) = read_bounded(&path, MAX_BINARY_BYTES) else {
            return failure(DiagnosticCode::PackageInvalid);
        };
        if expected.mode != expected_mode
            || expected.bytes != bytes.len() as u64
            || expected.sha256 != format!("sha256:{}", sha256(&bytes))
            || embedded.is_some_and(|canonical| canonical != bytes)
        {
            return failure(DiagnosticCode::PackageInvalid);
        }
        if name == "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt"
            && manifest.third_party_licenses_sha256 != expected.sha256
        {
            return failure(DiagnosticCode::PackageInvalid);
        }
    }
    let checksums_path = package_root.join(PACKAGE_CHECKSUMS_NAME);
    if actual_mode(&checksums_path).as_deref() != Some("0644") {
        return failure(DiagnosticCode::PackageInvalid);
    }
    let Ok(checksum_bytes) = read_bounded(&checksums_path, MAX_CHECKSUMS_BYTES) else {
        return failure(DiagnosticCode::PackageInvalid);
    };
    let Some(checksums) = parse_checksums(&checksum_bytes) else {
        return failure(DiagnosticCode::PackageInvalid);
    };
    let mut expected_checksums = manifest
        .contents
        .iter()
        .map(|content| {
            (
                content.path.as_str(),
                content.sha256.trim_start_matches("sha256:"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let manifest_digest = sha256(&bytes);
    expected_checksums.insert(PACKAGE_MANIFEST_NAME, &manifest_digest);
    if checksums != expected_checksums {
        return failure(DiagnosticCode::PackageInvalid);
    }
    PackageCheck {
        code: DiagnosticCode::Ready,
        godot_prerequisite: Some(manifest.godot_prerequisite),
        package_root: Some(package_root),
    }
}

fn parse_checksums(bytes: &[u8]) -> Option<BTreeMap<&str, &str>> {
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let (digest, path) = line.split_once("  ")?;
        if !valid_raw_sha256(digest)
            || !safe_package_path(path)
            || values.insert(path, digest).is_some()
        {
            return None;
        }
    }
    Some(values)
}

fn canonical_project_root(path: &Path) -> Option<PathBuf> {
    let canonical = fs::canonicalize(path).ok()?;
    (canonical.is_dir() && canonical.join("project.godot").is_file()).then_some(canonical)
}

fn check_config(
    project_root: &Path,
    expected_launcher: &Path,
    expected_data_root: Option<&Path>,
) -> Result<(), DiagnosticCode> {
    let path = project_root.join(".codex/config.toml");
    if !path.exists() {
        return Err(DiagnosticCode::ProjectConfigMissing);
    }
    if path.is_symlink() {
        return Err(DiagnosticCode::ProjectConfigInvalid);
    }
    let bytes =
        read_bounded(&path, MAX_CONFIG_BYTES).map_err(|_| DiagnosticCode::ProjectConfigInvalid)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| DiagnosticCode::ProjectConfigInvalid)?;
    let document = text
        .parse::<DocumentMut>()
        .map_err(|_| DiagnosticCode::ProjectConfigInvalid)?;
    let table = document
        .get("mcp_servers")
        .and_then(|item| item.get("godot_editor"))
        .and_then(toml_edit::Item::as_table_like)
        .ok_or(DiagnosticCode::ProjectConfigInvalid)?;
    let expected_launcher = expected_launcher
        .to_str()
        .ok_or(DiagnosticCode::ProjectConfigInvalid)?;
    if table.get("command").and_then(|item| item.as_str()) != Some(expected_launcher)
        || string_array(table.get("args")) != Some(vec!["--project-root", "."])
        || table.get("cwd").and_then(|item| item.as_str()) != project_root.to_str()
        || config_data_root(table.get("env")) != expected_data_root.and_then(Path::to_str)
        || table.get("required").and_then(|item| item.as_bool()) != Some(true)
        || table
            .get("startup_timeout_sec")
            .and_then(|item| item.as_integer())
            != Some(10)
        || table
            .get("tool_timeout_sec")
            .and_then(|item| item.as_integer())
            != Some(60)
    {
        return Err(DiagnosticCode::ProjectConfigInvalid);
    }
    let enabled =
        string_array(table.get("enabled_tools")).ok_or(DiagnosticCode::ProjectConfigInvalid)?;
    let full = godot_codex_product::FULL_BETA_TOOLS.to_vec();
    let read_only = READ_ONLY_TOOLS.to_vec();
    let approval = table
        .get("default_tools_approval_mode")
        .and_then(|item| item.as_str());
    let exact_profile = (enabled == read_only && approval.is_none())
        || (enabled == full && approval == Some("writes"));
    if !exact_profile {
        return Err(DiagnosticCode::ProjectConfigInvalid);
    }
    let expected_keys =
        if approval.is_some() { 8 } else { 7 } + usize::from(expected_data_root.is_some());
    if table.len() != expected_keys {
        return Err(DiagnosticCode::ProjectConfigInvalid);
    }
    Ok(())
}

fn config_data_root(item: Option<&toml_edit::Item>) -> Option<&str> {
    item?
        .as_value()?
        .as_inline_table()?
        .get("GODOT_CODEX_DATA_ROOT")?
        .as_str()
}

fn string_array(item: Option<&toml_edit::Item>) -> Option<Vec<&str>> {
    item?
        .as_array()?
        .iter()
        .map(toml_edit::Value::as_str)
        .collect()
}

fn check_host_probe(
    options: &DoctorOptions,
    observation: Option<&HostProbeObservation>,
    matrix: &godot_codex_product::CompatibilityMatrix,
    checks: &mut Vec<DoctorCheck>,
    exit_code: &mut u8,
) -> Option<SurfaceKind> {
    let Some(observation) = observation else {
        if options.surface == SurfaceSelection::Auto {
            *exit_code = (*exit_code).max(1);
            checks.push(DoctorCheck::from_code(
                "host.surface",
                DoctorCheckStatus::Warn,
                DiagnosticCode::ProjectUntrustedOrRestartRequired,
                None,
                None,
            ));
        } else {
            *exit_code = (*exit_code).max(5);
            checks.push(DoctorCheck::from_code(
                "host.surface",
                DoctorCheckStatus::Fail,
                DiagnosticCode::SurfaceUnsupported,
                None,
                None,
            ));
        }
        return options.surface.explicit();
    };
    let selected = options.surface.explicit().unwrap_or(observation.surface);
    let versions_safe = safe_coordinate(&observation.host_version)
        && observation
            .ide_host_version
            .as_deref()
            .is_none_or(safe_coordinate);
    let supported = versions_safe
        && selected == observation.surface
        && matrix
            .surface_qualification(
                observation.surface,
                &observation.host_version,
                observation.ide_host_version.as_deref(),
                matrix.package.target,
            )
            .is_ok_and(|qualification| {
                qualification == Some(godot_codex_product::SurfaceQualification::Supported)
            });
    if supported {
        checks.push(DoctorCheck::from_code(
            "host.surface",
            DoctorCheckStatus::Pass,
            DiagnosticCode::Ready,
            Some(&observation.host_version),
            None,
        ));
    } else {
        *exit_code = (*exit_code).max(5);
        checks.push(DoctorCheck::from_code(
            "host.surface",
            DoctorCheckStatus::Fail,
            DiagnosticCode::SurfaceUnsupported,
            Some(&observation.host_version),
            None,
        ));
    }
    match observation.effective_project_config {
        Some(true) => checks.push(pass("host.effective_config", Component::ProjectConfig)),
        Some(false) => {
            *exit_code = (*exit_code).max(2);
            checks.push(DoctorCheck::from_code(
                "host.effective_config",
                DoctorCheckStatus::Fail,
                DiagnosticCode::ProjectConfigNotEffective,
                None,
                None,
            ));
        }
        None => {
            *exit_code = (*exit_code).max(1);
            checks.push(DoctorCheck::from_code(
                "host.effective_config",
                DoctorCheckStatus::Warn,
                DiagnosticCode::ProjectUntrustedOrRestartRequired,
                None,
                None,
            ));
        }
    }
    match observation.restart_required {
        Some(true) => {
            *exit_code = (*exit_code).max(1);
            checks.push(DoctorCheck::from_code(
                "host.restart",
                DoctorCheckStatus::Warn,
                DiagnosticCode::SurfaceRestartRequired,
                None,
                None,
            ));
        }
        Some(false) => checks.push(pass("host.restart", Component::HostSurface)),
        None => {
            *exit_code = (*exit_code).max(1);
            checks.push(DoctorCheck::from_code(
                "host.restart",
                DoctorCheckStatus::Warn,
                DiagnosticCode::ProjectUntrustedOrRestartRequired,
                None,
                None,
            ));
        }
    }
    match (
        matrix.protocols.requires_form_elicitation,
        observation.supports_form_elicitation,
    ) {
        (false, _) | (true, Some(true)) => {
            checks.push(pass("host.form", Component::HostSurface));
        }
        (true, Some(false)) => {
            *exit_code = (*exit_code).max(5);
            checks.push(DoctorCheck::from_code(
                "host.form",
                DoctorCheckStatus::Fail,
                DiagnosticCode::HostInteractionUnsupported,
                None,
                Some(&matrix.protocols.approval_protocol_floor),
            ));
        }
        (true, None) => {
            *exit_code = (*exit_code).max(1);
            checks.push(DoctorCheck::from_code(
                "host.form",
                DoctorCheckStatus::Warn,
                DiagnosticCode::HostInteractionUnsupported,
                None,
                Some(&matrix.protocols.approval_protocol_floor),
            ));
        }
    }
    Some(selected)
}

fn check_godot_probe(
    observation: Option<&GodotProbeObservation>,
    prerequisite: Option<&GodotPrerequisite>,
    matrix: &godot_codex_product::CompatibilityMatrix,
    checks: &mut Vec<DoctorCheck>,
    exit_code: &mut u8,
) {
    let Some(observation) = observation else {
        *exit_code = (*exit_code).max(5);
        checks.push(DoctorCheck::from_code(
            "godot.prerequisite",
            DoctorCheckStatus::Fail,
            DiagnosticCode::BridgeVersionIncompatible,
            None,
            Some(&matrix.godot.build_id),
        ));
        return;
    };
    let safe = [
        observation.architecture.as_str(),
        observation.source_commit.as_str(),
        observation.build_id.as_str(),
        observation.artifact_sha256.as_str(),
    ]
    .into_iter()
    .all(safe_coordinate);
    let matches = prerequisite.is_some_and(|expected| {
        safe && observation.architecture == expected.architecture
            && observation.source_commit == expected.commit
            && observation.build_id == expected.version
            && observation.artifact_sha256 == expected.sha256
            && observation.source_commit == matrix.godot.source_commit
            && observation.build_id == matrix.godot.build_id
            && observation.artifact_sha256 == format!("sha256:{}", matrix.godot.artifact_sha256)
    });
    if matches {
        checks.push(DoctorCheck::from_code(
            "godot.prerequisite",
            DoctorCheckStatus::Pass,
            DiagnosticCode::Ready,
            Some(&observation.build_id),
            Some(&matrix.godot.build_id),
        ));
    } else {
        *exit_code = (*exit_code).max(5);
        checks.push(DoctorCheck::from_code(
            "godot.prerequisite",
            DoctorCheckStatus::Fail,
            DiagnosticCode::BridgeVersionIncompatible,
            Some(&observation.build_id),
            Some(&matrix.godot.build_id),
        ));
    }
}

fn check_mcp_probe(
    observation: Option<&McpProbeObservation>,
    expected_project_id: Option<&str>,
    matrix: &godot_codex_product::CompatibilityMatrix,
    registry_digest: &str,
    checks: &mut Vec<DoctorCheck>,
    exit_code: &mut u8,
) {
    let Some(observation) = observation else {
        *exit_code = (*exit_code).max(1);
        checks.push(DoctorCheck::from_code(
            "mcp.initialize",
            DoctorCheckStatus::Warn,
            DiagnosticCode::BridgeUnreachable,
            None,
            Some(&matrix.protocols.mcp_protocol),
        ));
        return;
    };
    if !observation.initialized {
        *exit_code = (*exit_code).max(4);
        checks.push(DoctorCheck::from_code(
            "mcp.initialize",
            DoctorCheckStatus::Fail,
            DiagnosticCode::BridgeUnreachable,
            None,
            Some(&matrix.protocols.mcp_protocol),
        ));
        return;
    }
    let safe = [
        observation.protocol_version.as_str(),
        observation.project_id.as_str(),
        observation.registry_digest.as_str(),
    ]
    .into_iter()
    .all(safe_coordinate);
    let protocol_supported = observation.protocol_version == matrix.protocols.mcp_protocol
        || observation.protocol_version == matrix.protocols.approval_protocol_floor;
    if safe && protocol_supported {
        checks.push(DoctorCheck::from_code(
            "mcp.initialize",
            DoctorCheckStatus::Pass,
            DiagnosticCode::Ready,
            Some(&observation.protocol_version),
            Some(&matrix.protocols.mcp_protocol),
        ));
    } else {
        *exit_code = (*exit_code).max(if protocol_supported { 4 } else { 5 });
        checks.push(DoctorCheck::from_code(
            "mcp.initialize",
            DoctorCheckStatus::Fail,
            if protocol_supported {
                DiagnosticCode::BridgeUnreachable
            } else {
                DiagnosticCode::BridgeVersionIncompatible
            },
            Some(&observation.protocol_version),
            Some(&matrix.protocols.mcp_protocol),
        ));
    }
    if expected_project_id == Some(observation.project_id.as_str()) {
        checks.push(pass("mcp.status", Component::Sidecar));
    } else {
        *exit_code = (*exit_code).max(6);
        checks.push(DoctorCheck::from_code(
            "mcp.status",
            DoctorCheckStatus::Fail,
            DiagnosticCode::ProjectBindingMismatch,
            None,
            None,
        ));
    }
    let registry_matches = observation.registry_digest == registry_digest
        && observation.tool_count == matrix.registry.tool_count
        && observation.fixed_resource_count == matrix.registry.fixed_resource_count
        && observation.resource_template_count == matrix.registry.resource_template_count;
    for (check_id, matches) in [
        ("mcp.status_tool", observation.status_tool_available),
        ("mcp.registry", registry_matches),
        ("mcp.instructions", observation.instructions_present),
    ] {
        if matches {
            checks.push(pass(check_id, Component::Sidecar));
        } else {
            *exit_code = (*exit_code).max(5);
            checks.push(DoctorCheck::from_code(
                check_id,
                DoctorCheckStatus::Fail,
                DiagnosticCode::CapabilityUnavailable,
                None,
                None,
            ));
        }
    }
    match (
        matrix.protocols.requires_form_elicitation,
        observation.supports_form_elicitation,
    ) {
        (false, _) | (true, Some(true)) => {
            checks.push(pass("mcp.form", Component::HostSurface));
        }
        (true, Some(false)) => {
            *exit_code = (*exit_code).max(5);
            checks.push(DoctorCheck::from_code(
                "mcp.form",
                DoctorCheckStatus::Fail,
                DiagnosticCode::HostInteractionUnsupported,
                None,
                Some(&matrix.protocols.approval_protocol_floor),
            ));
        }
        (true, None) => {
            *exit_code = (*exit_code).max(1);
            checks.push(DoctorCheck::from_code(
                "mcp.form",
                DoctorCheckStatus::Warn,
                DiagnosticCode::HostInteractionUnsupported,
                None,
                Some(&matrix.protocols.approval_protocol_floor),
            ));
        }
    }
}

fn safe_coordinate(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_COORDINATE_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'_' | b'-' | b'+')
        })
}

fn check_bridge_probe(
    options: &DoctorOptions,
    observation: Option<&BridgeProbeObservation>,
    matrix: &godot_codex_product::CompatibilityMatrix,
    checks: &mut Vec<DoctorCheck>,
    exit_code: &mut u8,
) {
    let status = observation.map_or(BridgeProbeStatus::TimedOut, |probe| probe.status);
    if status == BridgeProbeStatus::Ready {
        let protocol = observation
            .and_then(|probe| probe.negotiated_protocol.as_deref())
            .and_then(parse_bridge_version);
        let compatible = protocol.is_some_and(|(major, minor)| {
            major == u32::from(matrix.protocols.bridge_major)
                && minor >= u32::from(matrix.protocols.bridge_min_minor)
                && minor <= u32::from(matrix.protocols.bridge_current_minor)
        });
        if compatible {
            checks.push(DoctorCheck {
                check_id: "bridge.handshake".to_owned(),
                component: Component::Bridge,
                status: DoctorCheckStatus::Pass,
                code: DiagnosticCode::Ready,
                summary: Diagnostic::new(DiagnosticCode::Ready, None, None).summary,
                observed_version: observation
                    .and_then(|probe| probe.negotiated_protocol.clone())
                    .map(|version| format!("bridge-rpc/{version}")),
                supported: Some("bridge-rpc/1.0-1.8".to_owned()),
                remediation_id: None,
                retryable: false,
            });
            return;
        }
    }

    let (code, class, retryable_transport) =
        status
            .failure_policy()
            .unwrap_or((DiagnosticCode::BridgeVersionIncompatible, 5, false));
    let required_transport_failure = retryable_transport && options.require_editor;
    if retryable_transport && !required_transport_failure {
        *exit_code = (*exit_code).max(1);
    } else {
        *exit_code = (*exit_code).max(class);
    }
    checks.push(DoctorCheck::from_code(
        "bridge.handshake",
        if retryable_transport && !required_transport_failure {
            DoctorCheckStatus::Warn
        } else {
            DoctorCheckStatus::Fail
        },
        code,
        None,
        Some("bridge-rpc/1.0-1.8"),
    ));
}

enum DiscoveryOutcome {
    Online { protocol: String },
    Offline(DiagnosticCode),
    Failed { code: DiagnosticCode, class: u8 },
}

fn check_discovery(
    project_root: &Path,
    matrix: &godot_codex_product::CompatibilityMatrix,
) -> DiscoveryOutcome {
    let codex = project_root.join(".godot/codex");
    let path = codex.join("bridge.json");
    if !path.exists() {
        return DiscoveryOutcome::Offline(DiagnosticCode::BridgeDiscoveryMissing);
    }
    if !private_directory(&codex) || !private_file(&path) {
        return DiscoveryOutcome::Failed {
            code: DiagnosticCode::PermissionsInvalid,
            class: 4,
        };
    }
    let Ok(bytes) = read_bounded(&path, MAX_DISCOVERY_BYTES) else {
        return DiscoveryOutcome::Failed {
            code: DiagnosticCode::BridgeDiscoveryStale,
            class: 4,
        };
    };
    let Ok(record) = serde_json::from_slice::<DiscoveryRecord>(&bytes) else {
        return DiscoveryOutcome::Failed {
            code: DiagnosticCode::BridgeDiscoveryStale,
            class: 4,
        };
    };
    if record.discovery_schema != 1
        || record.created_at.len() > 64
        || record.transport != expected_transport()
        || !valid_session_id(&record.editor_session_id)
        || record.token_file != ".godot/codex/session.token"
        || !safe_relative_path(&record.endpoint, Path::new(".godot/codex/run"))
    {
        return DiscoveryOutcome::Failed {
            code: DiagnosticCode::BridgeDiscoveryStale,
            class: 4,
        };
    }
    let Ok(expected_project_id) = project_id_for_path(project_root) else {
        return DiscoveryOutcome::Failed {
            code: DiagnosticCode::ProjectBindingMismatch,
            class: 6,
        };
    };
    if record.project_id != expected_project_id {
        return DiscoveryOutcome::Failed {
            code: DiagnosticCode::ProjectBindingMismatch,
            class: 6,
        };
    }
    let token_path = project_root.join(&record.token_file);
    let lock_path = codex.join("bridge.lock");
    let endpoint_path = project_root.join(&record.endpoint);
    if !private_file(&token_path) || !private_file(&lock_path) || !private_endpoint(&endpoint_path)
    {
        return DiscoveryOutcome::Failed {
            code: DiagnosticCode::PermissionsInvalid,
            class: 4,
        };
    }
    let Ok(token) = read_bounded(&token_path, 32) else {
        return DiscoveryOutcome::Failed {
            code: DiagnosticCode::BridgeAuthenticationFailed,
            class: 6,
        };
    };
    if token.len() != 32 {
        return DiscoveryOutcome::Failed {
            code: DiagnosticCode::BridgeAuthenticationFailed,
            class: 6,
        };
    }
    let Ok(lock_bytes) = read_bounded(&lock_path, 4096) else {
        return DiscoveryOutcome::Offline(DiagnosticCode::BridgeDiscoveryStale);
    };
    let lock_matches = serde_json::from_slice::<serde_json::Value>(&lock_bytes)
        .ok()
        .is_some_and(|value| {
            value
                .get("editor_session_id")
                .and_then(serde_json::Value::as_str)
                == Some(record.editor_session_id.as_str())
                && value.get("pid").and_then(serde_json::Value::as_i64) == Some(record.pid)
        });
    if !lock_matches || !process_is_alive(record.pid) {
        return DiscoveryOutcome::Offline(DiagnosticCode::BridgeDiscoveryStale);
    }
    let versions = record
        .protocol_versions
        .iter()
        .filter_map(|value| parse_bridge_version(value))
        .filter(|(major, minor)| {
            *major == u32::from(matrix.protocols.bridge_major)
                && *minor >= u32::from(matrix.protocols.bridge_min_minor)
                && *minor <= u32::from(matrix.protocols.bridge_current_minor)
        })
        .collect::<BTreeSet<_>>();
    let Some((major, minor)) = versions.last().copied() else {
        return DiscoveryOutcome::Failed {
            code: DiagnosticCode::BridgeVersionIncompatible,
            class: 5,
        };
    };
    DiscoveryOutcome::Online {
        protocol: format!("bridge-rpc/{major}.{minor}"),
    }
}

fn finish_report(
    options: &DoctorOptions,
    matrix_digest: &str,
    registry_digest: &str,
    checks: Vec<DoctorCheck>,
    exit_code: u8,
    canonical_root: Option<&Path>,
    surface: Option<SurfaceKind>,
) -> DoctorReport {
    let status = if checks
        .iter()
        .any(|check| check.status == DoctorCheckStatus::Fail)
    {
        DoctorStatus::Error
    } else if checks
        .iter()
        .any(|check| check.status == DoctorCheckStatus::Warn)
    {
        DoctorStatus::Warning
    } else {
        DoctorStatus::Ready
    };
    let report = DoctorReport {
        schema_version: REPORT_SCHEMA.to_owned(),
        status,
        exit_code,
        editor_required: options.require_editor,
        requested_surface: options.surface,
        surface,
        project_root: (options.show_paths)
            .then(|| canonical_root.map(|root| root.display().to_string()))
            .flatten(),
        package_version: PRODUCT_VERSION.to_owned(),
        compatibility_matrix_digest: format!("sha256:{matrix_digest}"),
        registry_digest: format!("sha256:{registry_digest}"),
        checks,
    };
    debug_assert!(
        serde_json::to_vec(&report).is_ok_and(|bytes| bytes.len() <= MAX_REPORT_BYTES),
        "the closed doctor report must remain bounded"
    );
    report
}

fn pass(check_id: &'static str, component: Component) -> DoctorCheck {
    let diagnostic = Diagnostic::new(DiagnosticCode::Ready, None, None);
    DoctorCheck {
        check_id: check_id.to_owned(),
        component,
        status: DoctorCheckStatus::Pass,
        code: DiagnosticCode::Ready,
        summary: diagnostic.summary,
        observed_version: None,
        supported: None,
        remediation_id: None,
        retryable: false,
    }
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, ()> {
    crate::launcher::read_plain_bounded(path, max).map_err(|_| ())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_source_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_build_provenance(value: &BuildProvenance) -> bool {
    valid_prefixed_sha256(&value.cargo_lock_sha256)
        && value.cargo_version.starts_with("cargo ")
        && value.cargo_version.len() <= 256
        && value
            .cargo_version
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        && value.fresh_target
        && valid_prefixed_sha256(&value.rust_toolchain_sha256)
        && valid_source_commit(&value.rustc_commit)
        && safe_coordinate(&value.rustc_release)
        && value.target_triple == "aarch64-apple-darwin"
}

fn valid_prefixed_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(valid_raw_sha256)
}

fn valid_raw_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn safe_package_path(value: &str) -> bool {
    let path = Path::new(value);
    !path.is_absolute()
        && !value.contains(['\0', '\r', '\n', '\\'])
        && path
            .components()
            .all(|component| matches!(component, PathComponent::Normal(_)))
}

fn manifest_target(target: &TargetCoordinate) -> ManifestTarget {
    let os = match target.os {
        godot_codex_product::OperatingSystem::Macos => "macos",
        godot_codex_product::OperatingSystem::Windows => "windows",
        godot_codex_product::OperatingSystem::Linux => "linux",
    };
    let architecture = match target.architecture {
        godot_codex_product::Architecture::Arm64 => "arm64",
        godot_codex_product::Architecture::X86_64 => "x86_64",
    };
    ManifestTarget {
        architecture: architecture.to_owned(),
        os: os.to_owned(),
    }
}

#[cfg(unix)]
fn actual_mode(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;

    fs::symlink_metadata(path).ok().and_then(|metadata| {
        (!metadata.file_type().is_symlink() && metadata.is_file())
            .then(|| format!("{:04o}", metadata.permissions().mode() & 0o777))
    })
}

#[cfg(not(unix))]
fn actual_mode(path: &Path) -> Option<String> {
    path.is_file().then(|| {
        if is_executable(path) {
            "0755".to_owned()
        } else {
            "0644".to_owned()
        }
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn private_directory(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        !metadata.file_type().is_symlink()
            && metadata.is_dir()
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.permissions().mode() & 0o777 == 0o700
    })
}

#[cfg(not(unix))]
fn private_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_dir())
}

#[cfg(unix)]
fn private_file(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        !metadata.file_type().is_symlink()
            && metadata.is_file()
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.permissions().mode() & 0o777 == 0o600
    })
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_file())
}

#[cfg(unix)]
fn private_endpoint(path: &Path) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        !metadata.file_type().is_symlink()
            && metadata.file_type().is_socket()
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.permissions().mode() & 0o777 == 0o600
    })
}

#[cfg(windows)]
fn private_endpoint(path: &Path) -> bool {
    !path.as_os_str().is_empty()
}

#[cfg(all(not(unix), not(windows)))]
fn private_endpoint(_path: &Path) -> bool {
    false
}

#[cfg(unix)]
fn process_is_alive(raw_pid: i64) -> bool {
    i32::try_from(raw_pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

#[cfg(not(unix))]
fn process_is_alive(raw_pid: i64) -> bool {
    raw_pid > 0
}

fn valid_session_id(value: &str) -> bool {
    value.len() == 39
        && value.starts_with("editor:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn expected_transport() -> &'static str {
    if cfg!(windows) { "tcp_loopback" } else { "uds" }
}

fn safe_relative_path(value: &str, expected_prefix: &Path) -> bool {
    let path = Path::new(value);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, PathComponent::Normal(_)))
        && path.starts_with(expected_prefix)
}

fn parse_bridge_version(value: &str) -> Option<(u32, u32)> {
    let (major, minor) = value.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn runtime_target() -> Option<TargetCoordinate> {
    let os = match std::env::consts::OS {
        "macos" => godot_codex_product::OperatingSystem::Macos,
        "windows" => godot_codex_product::OperatingSystem::Windows,
        "linux" => godot_codex_product::OperatingSystem::Linux,
        _ => return None,
    };
    let architecture = match std::env::consts::ARCH {
        "aarch64" => godot_codex_product::Architecture::Arm64,
        "x86_64" => godot_codex_product::Architecture::X86_64,
        _ => return None,
    };
    Some(TargetCoordinate { os, architecture })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::TempDir;

    use super::*;

    fn project() -> TempDir {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("project.godot"), "[application]\n").unwrap();
        temp
    }

    fn write_config(root: &Path, tools: &[&str], full: bool, launcher: &Path) {
        fs::create_dir_all(root.join(".codex")).unwrap();
        let mut enabled = String::new();
        for tool in tools {
            enabled.push_str(&format!("  \"{tool}\",\n"));
        }
        let approval = if full {
            "default_tools_approval_mode = \"writes\"\n"
        } else {
            ""
        };
        fs::write(
            root.join(".codex/config.toml"),
            format!(
                "[mcp_servers.godot_editor]\ncommand = {:?}\nargs = [\"--project-root\", \".\"]\ncwd = {:?}\nrequired = true\nstartup_timeout_sec = 10\ntool_timeout_sec = 60\nenabled_tools = [\n{enabled}]\n{approval}",
                launcher.to_str().unwrap(),
                root.to_str().unwrap(),
            ),
        )
        .unwrap();
    }

    fn matching_probes(root: &Path) -> DoctorProbeObservations {
        let matrix = embedded_compatibility_matrix().unwrap();
        let registry = canonical_registry_profile();
        DoctorProbeObservations {
            host: None,
            godot: Some(GodotProbeObservation {
                architecture: manifest_target(&matrix.godot.target).architecture,
                source_commit: matrix.godot.source_commit,
                build_id: matrix.godot.build_id,
                artifact_sha256: format!("sha256:{}", matrix.godot.artifact_sha256),
            }),
            mcp: Some(McpProbeObservation {
                initialized: true,
                protocol_version: matrix.protocols.mcp_protocol,
                project_id: project_id_for_path(root).unwrap(),
                status_tool_available: true,
                registry_digest: registry.digest,
                tool_count: registry.tool_count,
                fixed_resource_count: registry.fixed_resource_count,
                resource_template_count: registry.resource_template_count,
                instructions_present: true,
                supports_form_elicitation: Some(true),
            }),
            bridge: Some(BridgeProbeObservation::ready("1.8".to_owned())),
        }
    }

    #[test]
    fn doctor_uses_closed_bridge_classifier_and_preserves_timeout() {
        let malformed =
            serde_json::from_str::<serde_json::Value>("{").expect_err("fixture must be malformed");
        let cases = [
            (
                BridgeError::Timeout,
                BridgeProbeStatus::TimedOut,
                DiagnosticCode::BridgeUnreachable,
            ),
            (
                BridgeError::Json(malformed),
                BridgeProbeStatus::Unreachable,
                DiagnosticCode::BridgeUnreachable,
            ),
            (
                BridgeError::Invalid("private /Users/alice/project token=top-secret".to_owned()),
                BridgeProbeStatus::Unreachable,
                DiagnosticCode::BridgeUnreachable,
            ),
            (
                BridgeError::Replica(godot_codex_semantic_model::ReplicaError::InvalidSnapshot(
                    "malformed fixture",
                )),
                BridgeProbeStatus::Unreachable,
                DiagnosticCode::BridgeUnreachable,
            ),
            (
                BridgeError::Rpc {
                    code: "authentication_failed".to_owned(),
                    message: "/Users/alice/private token=top-secret".to_owned(),
                    retryable: false,
                    data: serde_json::json!({"native_handle": 42}),
                },
                BridgeProbeStatus::Unreachable,
                DiagnosticCode::BridgeUnreachable,
            ),
            (
                BridgeError::Rpc {
                    code: "session_mismatch".to_owned(),
                    message: "/Users/alice/private token=top-secret".to_owned(),
                    retryable: true,
                    data: serde_json::json!({"native_handle": 42}),
                },
                BridgeProbeStatus::Unreachable,
                DiagnosticCode::BridgeUnreachable,
            ),
        ];
        for (error, status, diagnostic) in cases {
            let observation = BridgeProbeObservation::failed(&error);
            assert_eq!(observation.status, status);
            assert_eq!(observation.status.diagnostic_code(), Some(diagnostic));
            let serialized = serde_json::to_string(&observation).unwrap();
            assert!(!serialized.contains("/Users/"));
            assert!(!serialized.contains("top-secret"));
            assert!(!serialized.contains("native_handle"));
        }
    }

    fn validate_schema_instance(
        instance: &serde_json::Value,
        schema: &serde_json::Value,
        root: &serde_json::Value,
    ) -> Result<(), String> {
        if let Some(reference) = schema.get("$ref").and_then(serde_json::Value::as_str) {
            let pointer = reference
                .strip_prefix('#')
                .ok_or_else(|| "external schema reference".to_owned())?;
            let resolved = root
                .pointer(pointer)
                .ok_or_else(|| format!("unresolved schema reference {reference}"))?;
            return validate_schema_instance(instance, resolved, root);
        }
        if let Some(expected) = schema.get("const")
            && instance != expected
        {
            return Err(format!("const mismatch: {instance} != {expected}"));
        }
        if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array)
            && !values.contains(instance)
        {
            return Err(format!("enum mismatch: {instance}"));
        }
        if let Some(alternatives) = schema.get("oneOf").and_then(serde_json::Value::as_array) {
            let matches = alternatives
                .iter()
                .filter(|candidate| validate_schema_instance(instance, candidate, root).is_ok())
                .count();
            if matches != 1 {
                return Err(format!("oneOf matched {matches} alternatives"));
            }
            return Ok(());
        }
        let Some(kind) = schema.get("type").and_then(serde_json::Value::as_str) else {
            return Ok(());
        };
        match kind {
            "object" => {
                let object = instance
                    .as_object()
                    .ok_or_else(|| format!("expected object, got {instance}"))?;
                let properties = schema
                    .get("properties")
                    .and_then(serde_json::Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array)
                {
                    for key in required.iter().filter_map(serde_json::Value::as_str) {
                        if !object.contains_key(key) {
                            return Err(format!("missing required property {key}"));
                        }
                    }
                }
                if schema.get("additionalProperties") == Some(&serde_json::Value::Bool(false))
                    && object.keys().any(|key| !properties.contains_key(key))
                {
                    return Err("unexpected object property".to_owned());
                }
                for (key, value) in object {
                    if let Some(property_schema) = properties.get(key) {
                        validate_schema_instance(value, property_schema, root)
                            .map_err(|error| format!("{key}: {error}"))?;
                    }
                }
            }
            "array" => {
                let values = instance
                    .as_array()
                    .ok_or_else(|| format!("expected array, got {instance}"))?;
                if let Some(minimum) = schema.get("minItems").and_then(serde_json::Value::as_u64)
                    && values.len() < usize::try_from(minimum).unwrap()
                {
                    return Err("array below minItems".to_owned());
                }
                if let Some(maximum) = schema.get("maxItems").and_then(serde_json::Value::as_u64)
                    && values.len() > usize::try_from(maximum).unwrap()
                {
                    return Err("array above maxItems".to_owned());
                }
                if let Some(item_schema) = schema.get("items") {
                    for (index, value) in values.iter().enumerate() {
                        validate_schema_instance(value, item_schema, root)
                            .map_err(|error| format!("[{index}]: {error}"))?;
                    }
                }
            }
            "string" => {
                let value = instance
                    .as_str()
                    .ok_or_else(|| format!("expected string, got {instance}"))?;
                let characters = value.chars().count();
                if let Some(minimum) = schema.get("minLength").and_then(serde_json::Value::as_u64)
                    && characters < usize::try_from(minimum).unwrap()
                {
                    return Err("string below minLength".to_owned());
                }
                if let Some(maximum) = schema.get("maxLength").and_then(serde_json::Value::as_u64)
                    && characters > usize::try_from(maximum).unwrap()
                {
                    return Err("string above maxLength".to_owned());
                }
                match schema.get("pattern").and_then(serde_json::Value::as_str) {
                    Some("^sha256:[0-9a-f]{64}$") if !valid_prefixed_sha256(value) => {
                        return Err("digest pattern mismatch".to_owned());
                    }
                    Some("^[A-Za-z0-9][A-Za-z0-9._+:/-]{0,127}$")
                        if !value.bytes().enumerate().all(|(index, byte)| {
                            byte.is_ascii_alphanumeric()
                                || (index > 0
                                    && matches!(byte, b'.' | b'_' | b'+' | b':' | b'/' | b'-'))
                        }) =>
                    {
                        return Err("coordinate pattern mismatch".to_owned());
                    }
                    Some("^[a-z][a-z0-9_.-]*$")
                        if !value.bytes().enumerate().all(|(index, byte)| {
                            if index == 0 {
                                byte.is_ascii_lowercase()
                            } else {
                                byte.is_ascii_lowercase()
                                    || byte.is_ascii_digit()
                                    || matches!(byte, b'_' | b'.' | b'-')
                            }
                        }) =>
                    {
                        return Err("check ID pattern mismatch".to_owned());
                    }
                    _ => {}
                }
            }
            "integer" => {
                let value = instance
                    .as_u64()
                    .ok_or_else(|| format!("expected nonnegative integer, got {instance}"))?;
                if schema
                    .get("minimum")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|minimum| value < minimum)
                    || schema
                        .get("maximum")
                        .and_then(serde_json::Value::as_u64)
                        .is_some_and(|maximum| value > maximum)
                {
                    return Err("integer bound mismatch".to_owned());
                }
            }
            "boolean" if !instance.is_boolean() => return Err("expected boolean".to_owned()),
            "null" if !instance.is_null() => return Err("expected null".to_owned()),
            "boolean" | "null" => {}
            other => return Err(format!("unsupported test schema type {other}")),
        }
        Ok(())
    }

    #[test]
    fn serialized_doctor_reports_validate_against_the_published_closed_schema() {
        let schema: serde_json::Value = serde_json::from_str(REPORT_SCHEMA_JSON).unwrap();
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(schema["additionalProperties"], false);

        let missing = TempDir::new().unwrap();
        let missing_report = run_doctor_with_probes(
            &DoctorOptions::new(missing.path()),
            &DoctorProbeObservations::default(),
        );
        let project = project();
        let package = fake_package();
        let launcher = package.path().join("bin/godot-codex-mcp");
        write_config(project.path(), READ_ONLY_TOOLS, false, &launcher);
        let mut options = DoctorOptions::new(project.path());
        options.package_directory = Some(package.path().to_path_buf());
        options.show_paths = true;
        let project_report = run_doctor_with_probes(&options, &matching_probes(project.path()));
        let mut redacted_probes = matching_probes(project.path());
        redacted_probes.host = Some(HostProbeObservation {
            surface: SurfaceKind::Cli,
            host_version: "/private/secret-host-coordinate".to_owned(),
            ide_host_version: None,
            effective_project_config: Some(true),
            restart_required: Some(false),
            supports_form_elicitation: Some(true),
        });
        let redacted_report = run_doctor_with_probes(&options, &redacted_probes);

        for report in [missing_report, project_report, redacted_report] {
            let serialized = serde_json::to_value(report).unwrap();
            validate_schema_instance(&serialized, &schema, &schema)
                .unwrap_or_else(|error| panic!("doctor report violates schema: {error}"));
        }
    }

    #[test]
    fn config_accepts_both_exact_profiles_and_preserves_no_aliases() {
        let temp = project();
        let package = fake_package();
        let launcher = package.path().join("bin/godot-codex-mcp");
        write_config(temp.path(), READ_ONLY_TOOLS, false, &launcher);
        assert_eq!(check_config(temp.path(), &launcher, None), Ok(()));
        write_config(
            temp.path(),
            godot_codex_product::FULL_BETA_TOOLS,
            true,
            &launcher,
        );
        assert_eq!(check_config(temp.path(), &launcher, None), Ok(()));
        write_config(temp.path(), READ_ONLY_TOOLS, false, &launcher);
        let path = temp.path().join(".codex/config.toml");
        let data_root = temp.path().join("installed-data");
        let with_environment = fs::read_to_string(&path).unwrap().replacen(
            "required = true",
            &format!(
                "env = {{ GODOT_CODEX_DATA_ROOT = {:?} }}\nrequired = true",
                data_root.to_str().unwrap()
            ),
            1,
        );
        fs::write(&path, with_environment).unwrap();
        assert_eq!(
            check_config(temp.path(), &launcher, Some(&data_root)),
            Ok(())
        );
        assert_eq!(
            check_config(temp.path(), &launcher, None),
            Err(DiagnosticCode::ProjectConfigInvalid)
        );
        write_config(
            temp.path(),
            godot_codex_product::FULL_BETA_TOOLS,
            true,
            &launcher,
        );
        let path = temp.path().join(".codex/config.toml");
        let changed = fs::read_to_string(&path)
            .unwrap()
            .replace(temp.path().to_str().unwrap(), ".");
        fs::write(&path, changed).unwrap();
        assert_eq!(
            check_config(temp.path(), &launcher, None),
            Err(DiagnosticCode::ProjectConfigInvalid)
        );
        write_config(temp.path(), READ_ONLY_TOOLS, false, &launcher);
        let basename = fs::read_to_string(&path)
            .unwrap()
            .replace(launcher.to_str().unwrap(), "godot-codex-mcp");
        fs::write(&path, basename).unwrap();
        assert_eq!(
            check_config(temp.path(), &launcher, None),
            Err(DiagnosticCode::ProjectConfigInvalid)
        );
    }

    #[test]
    fn auto_surface_is_unknown_instead_of_guessing_cli() {
        let matrix = embedded_compatibility_matrix().unwrap();
        let options = DoctorOptions::new(".");
        let mut checks = Vec::new();
        let mut exit_code = 0;
        assert_eq!(
            check_host_probe(&options, None, &matrix, &mut checks, &mut exit_code),
            None
        );
        assert_eq!(exit_code, 1);
        assert!(checks.iter().any(|check| {
            check.check_id == "host.surface"
                && check.status == DoctorCheckStatus::Warn
                && check.code == DiagnosticCode::ProjectUntrustedOrRestartRequired
        }));
    }

    #[test]
    fn unavailable_host_private_state_is_reported_as_distinct_warnings() {
        let matrix = embedded_compatibility_matrix().unwrap();
        let options = DoctorOptions::new(".");
        let candidate = matrix
            .surfaces
            .iter()
            .find(|rule| rule.surface == SurfaceKind::Cli)
            .unwrap();
        let observation = HostProbeObservation {
            surface: SurfaceKind::Cli,
            host_version: candidate.host_version.clone(),
            ide_host_version: None,
            effective_project_config: None,
            restart_required: None,
            supports_form_elicitation: None,
        };
        let mut checks = Vec::new();
        let mut exit_code = 0;
        check_host_probe(
            &options,
            Some(&observation),
            &matrix,
            &mut checks,
            &mut exit_code,
        );
        for check_id in ["host.effective_config", "host.restart", "host.form"] {
            assert!(checks.iter().any(|check| {
                check.check_id == check_id && check.status == DoctorCheckStatus::Warn
            }));
        }
        assert!(!checks.iter().any(|check| {
            matches!(
                check.check_id.as_str(),
                "host.effective_config" | "host.restart" | "host.form"
            ) && check.status == DoctorCheckStatus::Pass
        }));
    }

    #[test]
    fn missing_project_is_safe_and_closed() {
        let temp = TempDir::new().unwrap();
        let mut options = DoctorOptions::new(temp.path());
        options.show_paths = false;
        let report = run_doctor_with_probes(&options, &DoctorProbeObservations::default());
        assert_eq!(report.exit_code, 3);
        assert_eq!(report.status, DoctorStatus::Error);
        assert!(report.project_root.is_none());
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains(temp.path().to_str().unwrap()));
    }

    #[test]
    fn config_fault_has_distinct_exit_and_code_when_package_is_valid() {
        let temp = project();
        let package = fake_package();
        let mut options = DoctorOptions::new(temp.path());
        options.package_directory = Some(package.path().to_path_buf());
        let report = run_doctor_with_probes(&options, &matching_probes(temp.path()));
        assert!(report.checks.iter().any(|check| {
            check.code == DiagnosticCode::ProjectConfigMissing
                && check.status == DoctorCheckStatus::Fail
        }));
        assert_eq!(report.exit_code, 2);
    }

    #[test]
    fn doctor_reports_same_project_owner_as_busy_instead_of_rebuilding() {
        let temp = project();
        let package = fake_package();
        write_config(
            temp.path(),
            READ_ONLY_TOOLS,
            false,
            &package.path().join("bin/godot-codex-mcp"),
        );
        let project_id = project_id_for_path(temp.path()).unwrap();
        let _owner = SegmentStore::open(temp.path(), &project_id).unwrap();
        let mut options = DoctorOptions::new(temp.path());
        options.package_directory = Some(package.path().to_path_buf());

        let report = run_doctor_with_probes(&options, &matching_probes(temp.path()));
        assert!(report.checks.iter().any(|check| {
            check.check_id == "cache.integrity"
                && check.code == DiagnosticCode::ProjectSessionBusy
                && check.status == DoctorCheckStatus::Warn
        }));
        assert!(!report.checks.iter().any(|check| {
            check.check_id == "cache.integrity"
                && check.code == DiagnosticCode::StaticCacheRebuilding
        }));
    }

    #[test]
    fn require_editor_promotes_offline_to_transport_failure() {
        let temp = project();
        let package = fake_package();
        write_config(
            temp.path(),
            READ_ONLY_TOOLS,
            false,
            &package.path().join("bin/godot-codex-mcp"),
        );
        let mut options = DoctorOptions::new(temp.path());
        options.package_directory = Some(package.path().to_path_buf());
        options.require_editor = true;
        let report = run_doctor_with_probes(&options, &matching_probes(temp.path()));
        assert_eq!(report.exit_code, 4);
        assert!(report.checks.iter().any(|check| {
            check.code == DiagnosticCode::BridgeDiscoveryMissing
                && check.status == DoctorCheckStatus::Fail
        }));
    }

    #[test]
    fn package_hash_mismatch_is_not_reported_as_missing_binary() {
        let temp = project();
        let package = fake_package();
        fs::OpenOptions::new()
            .append(true)
            .open(package.path().join("bin/godot-codex-mcp"))
            .unwrap()
            .write_all(b"x")
            .unwrap();
        let mut options = DoctorOptions::new(temp.path());
        options.package_directory = Some(package.path().to_path_buf());
        let report = run_doctor_with_probes(&options, &matching_probes(temp.path()));
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.code == DiagnosticCode::PackageInvalid)
        );
    }

    #[test]
    fn report_json_is_bounded_path_redacted_and_ansi_free() {
        let temp = project();
        let options = DoctorOptions::new(temp.path());
        let report = run_doctor_with_probes(&options, &matching_probes(temp.path()));
        let serialized = serde_json::to_vec(&report).unwrap();
        assert!(serialized.len() <= MAX_REPORT_BYTES);
        let text = String::from_utf8(serialized).unwrap();
        assert!(!text.contains(temp.path().to_str().unwrap()));
        assert!(!text.contains('\u{1b}'));
        assert!(!text.to_ascii_lowercase().contains("session.token"));
    }

    #[test]
    fn show_paths_is_the_only_absolute_root_surface() {
        let temp = project();
        let mut options = DoctorOptions::new(temp.path());
        options.show_paths = true;
        let report = run_doctor_with_probes(&options, &matching_probes(temp.path()));
        assert_eq!(
            report.project_root.as_deref(),
            Some(fs::canonicalize(temp.path()).unwrap().to_str().unwrap())
        );
    }

    #[test]
    fn safe_relative_discovery_paths_reject_escape_and_absolute_values() {
        assert!(safe_relative_path(
            ".godot/codex/run/bridge.sock",
            Path::new(".godot/codex/run")
        ));
        assert!(!safe_relative_path(
            ".godot/codex/run/../token",
            Path::new(".godot/codex/run")
        ));
        assert!(!safe_relative_path(
            "/tmp/bridge.sock",
            Path::new(".godot/codex/run")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn discovery_faults_keep_stale_version_auth_binding_and_permissions_distinct() {
        use std::os::unix::fs::PermissionsExt;

        let matrix = embedded_compatibility_matrix().unwrap();

        let stale = project();
        let _listener = write_discovery(stale.path(), i64::from(i32::MAX), &["1.8"], None, 32);
        assert!(matches!(
            check_discovery(stale.path(), &matrix),
            DiscoveryOutcome::Offline(DiagnosticCode::BridgeDiscoveryStale)
        ));

        let version = project();
        let _listener = write_discovery(
            version.path(),
            i64::from(std::process::id()),
            &["2.0"],
            None,
            32,
        );
        assert!(matches!(
            check_discovery(version.path(), &matrix),
            DiscoveryOutcome::Failed {
                code: DiagnosticCode::BridgeVersionIncompatible,
                class: 5
            }
        ));

        let auth = project();
        let _listener = write_discovery(
            auth.path(),
            i64::from(std::process::id()),
            &["1.8"],
            None,
            31,
        );
        assert!(matches!(
            check_discovery(auth.path(), &matrix),
            DiscoveryOutcome::Failed {
                code: DiagnosticCode::BridgeAuthenticationFailed,
                class: 6
            }
        ));

        let binding = project();
        let _listener = write_discovery(
            binding.path(),
            i64::from(std::process::id()),
            &["1.8"],
            Some("project:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            32,
        );
        assert!(matches!(
            check_discovery(binding.path(), &matrix),
            DiscoveryOutcome::Failed {
                code: DiagnosticCode::ProjectBindingMismatch,
                class: 6
            }
        ));

        let permissions = project();
        let _listener = write_discovery(
            permissions.path(),
            i64::from(std::process::id()),
            &["1.8"],
            None,
            32,
        );
        fs::set_permissions(
            permissions.path().join(".godot/codex/bridge.json"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(matches!(
            check_discovery(permissions.path(), &matrix),
            DiscoveryOutcome::Failed {
                code: DiagnosticCode::PermissionsInvalid,
                class: 4
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn authenticated_bridge_gate_never_promotes_failed_handshakes_to_ready() {
        let project = project();
        let package = fake_package();
        let launcher = package.path().join("bin/godot-codex-mcp");
        write_config(project.path(), READ_ONLY_TOOLS, false, &launcher);
        let _listener = write_discovery(
            project.path(),
            i64::from(std::process::id()),
            &["1.8"],
            None,
            32,
        );
        let mut options = DoctorOptions::new(project.path());
        options.package_directory = Some(package.path().to_path_buf());
        let cases = [
            (
                BridgeProbeStatus::Unreachable,
                DiagnosticCode::BridgeUnreachable,
                DoctorCheckStatus::Warn,
            ),
            (
                BridgeProbeStatus::TimedOut,
                DiagnosticCode::BridgeUnreachable,
                DoctorCheckStatus::Warn,
            ),
            (
                BridgeProbeStatus::AuthenticationFailed,
                DiagnosticCode::BridgeAuthenticationFailed,
                DoctorCheckStatus::Fail,
            ),
            (
                BridgeProbeStatus::ProjectBindingMismatch,
                DiagnosticCode::ProjectBindingMismatch,
                DoctorCheckStatus::Fail,
            ),
            (
                BridgeProbeStatus::ProtocolVersionMismatch,
                DiagnosticCode::BridgeVersionIncompatible,
                DoctorCheckStatus::Fail,
            ),
        ];
        for (probe_status, diagnostic_code, check_status) in cases {
            let mut probes = matching_probes(project.path());
            probes.bridge = Some(BridgeProbeObservation {
                status: probe_status,
                negotiated_protocol: None,
            });
            let report = run_doctor_with_probes(&options, &probes);
            assert_ne!(report.status, DoctorStatus::Ready);
            assert!(report.checks.iter().any(|check| {
                check.check_id == "bridge.handshake"
                    && check.code == diagnostic_code
                    && check.status == check_status
            }));
        }

        let report = run_doctor_with_probes(&options, &matching_probes(project.path()));
        assert!(report.checks.iter().any(|check| {
            check.check_id == "bridge.handshake"
                && check.code == DiagnosticCode::Ready
                && check.status == DoctorCheckStatus::Pass
                && check.observed_version.as_deref() == Some("bridge-rpc/1.8")
        }));
    }

    fn fake_package() -> TempDir {
        let temp = TempDir::new().unwrap();
        let matrix = embedded_compatibility_matrix().unwrap();
        let target = manifest_target(&runtime_target().unwrap());
        let source_commit = "a".repeat(40);
        fs::create_dir(temp.path().join("bin")).unwrap();
        for name in ["godot-codex", "godot-codex-mcp"] {
            let path = temp.path().join("bin").join(name);
            fs::write(&path, format!("#!stub\n{name}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let mut contents = ["godot-codex", "godot-codex-mcp"]
            .into_iter()
            .map(|name| {
                let bytes = fs::read(temp.path().join("bin").join(name)).unwrap();
                serde_json::json!({
                    "mode": "0755",
                    "path": format!("bin/{name}"),
                    "sha256": format!("sha256:{}", sha256(&bytes)),
                    "bytes": bytes.len(),
                })
            })
            .collect::<Vec<_>>();
        let version_bytes = format!("{PRODUCT_VERSION}\n").into_bytes();
        let source_commit_bytes = format!("{source_commit}\n").into_bytes();
        let third_party_licenses = b"THIRD-PARTY LICENSE TEST FIXTURE\n".as_slice();
        for (relative, bytes) in [
            (
                "share/godot-codex/product/compatibility-matrix.v1.json",
                COMPATIBILITY_MATRIX_JSON.as_bytes(),
            ),
            (
                "share/godot-codex/product/registry-profile.v1.json",
                REGISTRY_PROFILE_JSON.as_bytes(),
            ),
            (
                "share/godot-codex/product/host-coordinate-profile.v1.json",
                HOST_COORDINATE_PROFILE_JSON.as_bytes(),
            ),
            (
                "share/godot-codex/product/server-instructions.v1.txt",
                SERVER_INSTRUCTIONS_TEXT.as_bytes(),
            ),
            (
                "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt",
                third_party_licenses,
            ),
            ("VERSION", version_bytes.as_slice()),
            ("SOURCE_COMMIT", source_commit_bytes.as_slice()),
        ] {
            let path = temp.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            contents.push(serde_json::json!({
                "mode": "0644",
                "path": relative,
                "sha256": format!("sha256:{}", sha256(bytes)),
                "bytes": bytes.len(),
            }));
        }
        let manifest = serde_json::json!({
            "schema_version": PACKAGE_MANIFEST_SCHEMA,
            "package_version": PRODUCT_VERSION,
            "source_commit": source_commit,
            "target": target,
            "build_provenance": {
                "cargo_lock_sha256": format!("sha256:{}", "1".repeat(64)),
                "cargo_version": "cargo 1.94.1 (test)",
                "fresh_target": true,
                "rust_toolchain_sha256": format!("sha256:{}", "2".repeat(64)),
                "rustc_commit": "b".repeat(40),
                "rustc_release": "1.94.1",
                "target_triple": "aarch64-apple-darwin",
            },
            "godot_prerequisite": {
                "architecture": target.architecture,
                "commit": matrix.godot.source_commit,
                "expected_install_path": GODOT_EXPECTED_INSTALL_PATH,
                "sha256": format!("sha256:{}", matrix.godot.artifact_sha256),
                "verification": {
                    "sha256": ["/usr/bin/shasum", "-a", "256", "<godot-binary>"],
                    "version": ["<godot-binary>", "--version"],
                },
                "version": matrix.godot.build_id,
            },
            "compatibility_matrix_sha256": format!("sha256:{}", sha256(COMPATIBILITY_MATRIX_JSON.as_bytes())),
            "registry_sha256": format!("sha256:{}", sha256(REGISTRY_PROFILE_JSON.as_bytes())),
            "third_party_licenses_sha256": format!("sha256:{}", sha256(third_party_licenses)),
            "contents": contents,
            "checksums_path": "checksums.sha256",
        });
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(temp.path().join(PACKAGE_MANIFEST_NAME), &manifest_bytes).unwrap();
        let mut checksums = manifest["contents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|content| {
                format!(
                    "{}  {}\n",
                    content["sha256"]
                        .as_str()
                        .unwrap()
                        .trim_start_matches("sha256:"),
                    content["path"].as_str().unwrap()
                )
            })
            .collect::<Vec<_>>();
        checksums.push(format!(
            "{}  {PACKAGE_MANIFEST_NAME}\n",
            sha256(&manifest_bytes)
        ));
        checksums.sort();
        let checksums_path = temp.path().join(PACKAGE_CHECKSUMS_NAME);
        fs::write(&checksums_path, checksums.concat()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&checksums_path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        temp
    }

    #[cfg(unix)]
    fn write_discovery(
        root: &Path,
        pid: i64,
        versions: &[&str],
        project_id_override: Option<&str>,
        token_len: usize,
    ) -> std::os::unix::net::UnixListener {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;

        let codex = root.join(".godot/codex");
        let run = codex.join("run");
        fs::create_dir_all(&run).unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint_relative = ".godot/codex/run/bridge-test.sock";
        let endpoint = root.join(endpoint_relative);
        let listener = UnixListener::bind(&endpoint).unwrap();
        fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)).unwrap();
        let project_id = project_id_override
            .map_or_else(|| project_id_for_path(root).unwrap(), ToOwned::to_owned);
        let editor_session_id = format!("editor:{}", "a".repeat(32));
        let record = serde_json::json!({
            "discovery_schema": 1,
            "created_at": "2026-07-24T00:00:00Z",
            "transport": "uds",
            "endpoint": endpoint_relative,
            "token_file": ".godot/codex/session.token",
            "project_id": project_id,
            "editor_session_id": editor_session_id,
            "pid": pid,
            "protocol_versions": versions,
        });
        fs::write(
            codex.join("bridge.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        fs::write(codex.join("session.token"), vec![0_u8; token_len]).unwrap();
        fs::write(
            codex.join("bridge.lock"),
            serde_json::to_vec(&serde_json::json!({
                "editor_session_id": editor_session_id,
                "pid": pid,
            }))
            .unwrap(),
        )
        .unwrap();
        for name in ["bridge.json", "session.token", "bridge.lock"] {
            fs::set_permissions(codex.join(name), fs::Permissions::from_mode(0o600)).unwrap();
        }
        listener
    }
}
