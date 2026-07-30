use std::collections::{BTreeMap, BTreeSet};
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use godot_codex_product::{
    COMPATIBILITY_MATRIX_JSON, CompatibilityMatrix, HOST_COORDINATE_PROFILE_JSON,
    SurfaceCompatibilityBundle, SurfaceKind, SurfaceQualification, embedded_compatibility_matrix,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::PRODUCT_VERSION;
use crate::launcher::resolve_launcher;

const PLAN_SCHEMA: &str = "godot-codex-surface-compatibility-plan/1.0";
const PREVIEW_SCHEMA: &str = "godot-codex-surface-compatibility-preview/1.0";
const REPORT_SCHEMA: &str = "godot-codex-surface-compatibility-report/1.0";
const STATUS_SCHEMA: &str = "godot-codex-surface-compatibility-status/1.0";
const ACTIVE_SCHEMA: &str = "godot-codex-active-surface-compatibility/1.0";
const PLAN_LIFETIME_SECONDS: u64 = 10 * 60;
const MAX_BUNDLE_BYTES: u64 = 256 * 1024;
const MAX_HOST_PROFILE_BYTES: u64 = 512 * 1024;
const MAX_ACTIVE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PLAN_BYTES: u64 = 2 * 1024 * 1024;
const MAX_STORED_PLANS: usize = 32;
const COMPATIBILITY_DIRECTORY: &str = "compatibility";
const ACTIVE_BUNDLE_NAME: &str = "active-surface-bundle.json";
const OPERATION_LOCK_NAME: &str = "surface-bundle.lock";

/// Source selected by the effective compatibility resolver.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityMatrixSource {
    Embedded,
    InstalledBundle,
}

/// A validated effective matrix and its bounded provenance.
#[derive(Clone, Debug)]
pub struct EffectiveCompatibilityMatrix {
    pub matrix: CompatibilityMatrix,
    pub source: CompatibilityMatrixSource,
    pub bundle_id: Option<String>,
    pub sequence: Option<u64>,
    pub bundle_sha256: Option<String>,
    pub host_coordinate_profile_sha256: Option<String>,
}

/// Closed compatibility-bundle operation failures.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CompatibilityBundleError {
    #[error("compatibility_data_root_unavailable")]
    DataRootUnavailable,
    #[error("compatibility_path_unsafe")]
    PathUnsafe,
    #[error("compatibility_bundle_unavailable")]
    BundleUnavailable,
    #[error("compatibility_bundle_too_large")]
    BundleTooLarge,
    #[error("compatibility_bundle_digest_invalid")]
    BundleDigestInvalid,
    #[error("compatibility_bundle_digest_mismatch")]
    BundleDigestMismatch,
    #[error("compatibility_bundle_invalid")]
    BundleInvalid,
    #[error("compatibility_host_profile_unavailable")]
    HostProfileUnavailable,
    #[error("compatibility_host_profile_too_large")]
    HostProfileTooLarge,
    #[error("compatibility_host_profile_digest_mismatch")]
    HostProfileDigestMismatch,
    #[error("compatibility_host_profile_invalid")]
    HostProfileInvalid,
    #[error("compatibility_bundle_sequence_not_advanced")]
    SequenceNotAdvanced,
    #[error("compatibility_plan_store_unavailable")]
    PlanStoreUnavailable,
    #[error("compatibility_plan_limit_reached")]
    PlanLimitReached,
    #[error("compatibility_plan_digest_invalid")]
    PlanDigestInvalid,
    #[error("compatibility_plan_not_found")]
    PlanNotFound,
    #[error("compatibility_plan_expired")]
    PlanExpired,
    #[error("compatibility_plan_inputs_changed")]
    PlanInputsChanged,
    #[error("compatibility_operation_in_progress")]
    OperationInProgress,
    #[error("compatibility_atomic_write_failed")]
    AtomicWriteFailed,
}

/// Inputs for a digest-authorized, two-phase bundle installation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityInstallOptions {
    pub bundle_path: PathBuf,
    pub host_profile_path: PathBuf,
    pub expected_sha256: String,
    data_root: Option<PathBuf>,
    plan_store: Option<PathBuf>,
}

impl CompatibilityInstallOptions {
    #[must_use]
    pub fn new(
        bundle_path: impl Into<PathBuf>,
        host_profile_path: impl Into<PathBuf>,
        expected_sha256: impl Into<String>,
    ) -> Self {
        Self {
            bundle_path: bundle_path.into(),
            host_profile_path: host_profile_path.into(),
            expected_sha256: expected_sha256.into(),
            data_root: None,
            plan_store: None,
        }
    }

    /// Overrides installed locations for a bounded validator or test harness.
    #[must_use]
    pub fn with_locations(
        mut self,
        data_root: impl Into<PathBuf>,
        plan_store: impl Into<PathBuf>,
    ) -> Self {
        self.data_root = Some(data_root.into());
        self.plan_store = Some(plan_store.into());
        self
    }
}

/// Redacted immutable preview. No filesystem path or bundle body is exposed.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityInstallPreview {
    pub schema_version: String,
    pub status: String,
    pub plan_digest: String,
    pub expires_in_seconds: u64,
    pub package_version: String,
    pub bundle_id: String,
    pub sequence: u64,
    pub bundle_sha256: String,
    pub baseline_matrix_sha256: String,
    pub host_coordinate_profile_sha256: String,
    pub previous_sequence: Option<u64>,
}

/// Durable result of applying one exact previewed bundle.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityInstallReport {
    pub schema_version: String,
    pub status: String,
    pub plan_digest: String,
    pub package_version: String,
    pub bundle_id: String,
    pub sequence: u64,
    pub bundle_sha256: String,
    pub host_coordinate_profile_sha256: String,
}

/// Read-only status for the active effective matrix.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityBundleStatus {
    pub schema_version: String,
    pub status: String,
    pub package_version: String,
    pub source: CompatibilityMatrixSource,
    pub effective_matrix_sha256: String,
    pub bundle_id: Option<String>,
    pub sequence: Option<u64>,
    pub bundle_sha256: Option<String>,
    pub host_coordinate_profile_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct CompatibilityInstallPlan {
    schema_version: String,
    created_at_epoch_seconds: u64,
    expires_at_epoch_seconds: u64,
    package_version: String,
    data_root: PathBuf,
    source_path: PathBuf,
    host_profile_source_path: PathBuf,
    expected_sha256: String,
    bundle_json: String,
    host_profile_json: String,
    bundle_id: String,
    sequence: u64,
    baseline_matrix_sha256: String,
    host_coordinate_profile_sha256: String,
    active_before_sha256: Option<String>,
    previous_sequence: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HostCoordinateProfile {
    schema_version: String,
    profile_id: String,
    compatibility_matrix: HostProfileBinding,
    server_instructions: HostInstructionsBinding,
    surfaces: Vec<HostSurfaceCoordinate>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HostProfileBinding {
    path: String,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HostInstructionsBinding {
    path: String,
    file_sha256: String,
    wire_sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum HostArtifactKind {
    MacosBundleExecutable,
    StandaloneExecutable,
    ExtensionTree,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HostCodeSignature {
    mode: String,
    identifier: String,
    team_id: String,
    cdhash: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HostSurfaceCoordinate {
    surface: SurfaceKind,
    host_name: String,
    host_identifier: String,
    host_artifact_kind: HostArtifactKind,
    host_version: String,
    host_build: String,
    host_commit: Option<String>,
    host_artifact_sha256: String,
    host_metadata_sha256: Option<String>,
    host_code_signature: Option<HostCodeSignature>,
    client_version: String,
    client_artifact_sha256: String,
    client_code_signature: Option<HostCodeSignature>,
    ide_host_version: Option<String>,
    ide_shell_identifier: Option<String>,
    ide_shell_artifact_sha256: Option<String>,
    ide_shell_team_id: Option<String>,
    ide_shell_code_signature: Option<HostCodeSignature>,
    qualification: SurfaceQualification,
}

/// Returns the private per-user compatibility plan store.
pub fn default_compatibility_plan_store() -> Result<PathBuf, CompatibilityBundleError> {
    if cfg!(target_os = "macos") {
        return std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|home| home.join("Library/Caches/GodotCodex/compatibility-plans"))
            .ok_or(CompatibilityBundleError::PlanStoreUnavailable);
    }
    if let Some(state) = std::env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(state).join("godot-codex/compatibility-plans"));
    }
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|base| base.join("GodotCodex/compatibility-plans"))
            .ok_or(CompatibilityBundleError::PlanStoreUnavailable);
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".local/state/godot-codex/compatibility-plans"))
        .ok_or(CompatibilityBundleError::PlanStoreUnavailable)
}

/// Validates and persists an immutable, expiring installation preview.
pub fn prepare_compatibility_install(
    options: &CompatibilityInstallOptions,
) -> Result<CompatibilityInstallPreview, CompatibilityBundleError> {
    parse_prefixed_digest(&options.expected_sha256)
        .ok_or(CompatibilityBundleError::BundleDigestInvalid)?;
    let source_path = canonical_plain_file(&options.bundle_path)?;
    let bundle_bytes = read_plain_bounded(&source_path, MAX_BUNDLE_BYTES)?;
    if digest_bytes(&bundle_bytes) != options.expected_sha256 {
        return Err(CompatibilityBundleError::BundleDigestMismatch);
    }
    let bundle_json =
        String::from_utf8(bundle_bytes).map_err(|_| CompatibilityBundleError::BundleInvalid)?;
    let baseline =
        embedded_compatibility_matrix().map_err(|_| CompatibilityBundleError::BundleInvalid)?;
    let bundle = SurfaceCompatibilityBundle::parse(bundle_json.as_bytes(), &baseline)
        .map_err(|_| CompatibilityBundleError::BundleInvalid)?;
    let host_profile_source_path = canonical_plain_file_with_error(
        &options.host_profile_path,
        CompatibilityBundleError::HostProfileUnavailable,
    )?;
    let host_profile_bytes = read_plain_bounded_with_errors(
        &host_profile_source_path,
        MAX_HOST_PROFILE_BYTES,
        CompatibilityBundleError::HostProfileUnavailable,
        CompatibilityBundleError::HostProfileTooLarge,
    )?;
    if digest_bytes(&host_profile_bytes) != bundle.host_coordinate_profile_sha256 {
        return Err(CompatibilityBundleError::HostProfileDigestMismatch);
    }
    let host_profile_json = String::from_utf8(host_profile_bytes)
        .map_err(|_| CompatibilityBundleError::HostProfileInvalid)?;
    validate_host_coordinate_profile(&host_profile_json, &bundle)?;
    let data_root = resolve_data_root(options.data_root.as_deref())?;
    let active = read_active_bundle(&data_root, &baseline)?;
    if active
        .as_ref()
        .is_some_and(|active| bundle.sequence <= active.bundle.sequence)
    {
        return Err(CompatibilityBundleError::SequenceNotAdvanced);
    }
    let now = now_epoch_seconds()?;
    let plan = CompatibilityInstallPlan {
        schema_version: PLAN_SCHEMA.to_owned(),
        created_at_epoch_seconds: now,
        expires_at_epoch_seconds: now.saturating_add(PLAN_LIFETIME_SECONDS),
        package_version: PRODUCT_VERSION.to_owned(),
        data_root,
        source_path,
        host_profile_source_path,
        expected_sha256: options.expected_sha256.clone(),
        bundle_json,
        host_profile_json,
        bundle_id: bundle.bundle_id.clone(),
        sequence: bundle.sequence,
        baseline_matrix_sha256: bundle.baseline_matrix_sha256.clone(),
        host_coordinate_profile_sha256: bundle.host_coordinate_profile_sha256.clone(),
        active_before_sha256: active.as_ref().map(|active| active.state_sha256.clone()),
        previous_sequence: active.as_ref().map(|active| active.bundle.sequence),
    };
    let plan_digest = plan_digest(&plan)?;
    let plan_store = options
        .plan_store
        .clone()
        .map_or_else(default_compatibility_plan_store, Ok)?;
    persist_plan(&plan_store, &plan_digest, &plan)?;
    Ok(CompatibilityInstallPreview {
        schema_version: PREVIEW_SCHEMA.to_owned(),
        status: "planned".to_owned(),
        plan_digest,
        expires_in_seconds: PLAN_LIFETIME_SECONDS,
        package_version: PRODUCT_VERSION.to_owned(),
        bundle_id: bundle.bundle_id,
        sequence: bundle.sequence,
        bundle_sha256: options.expected_sha256.clone(),
        baseline_matrix_sha256: bundle.baseline_matrix_sha256,
        host_coordinate_profile_sha256: bundle.host_coordinate_profile_sha256,
        previous_sequence: plan.previous_sequence,
    })
}

/// Applies one exact, unexpired plan. The active bundle is a single atomic
/// commit point and sequence rollback is rechecked while holding the lock.
pub fn apply_compatibility_install(
    digest: &str,
    plan_store: Option<&Path>,
) -> Result<CompatibilityInstallReport, CompatibilityBundleError> {
    parse_prefixed_digest(digest).ok_or(CompatibilityBundleError::PlanDigestInvalid)?;
    let plan_store = plan_store
        .map(Path::to_path_buf)
        .map_or_else(default_compatibility_plan_store, Ok)?;
    let plan_path = plan_store.join(format!("{}.json", digest.trim_start_matches("sha256:")));
    let bytes = read_plain_bounded(&plan_path, MAX_PLAN_BYTES).map_err(|error| match error {
        CompatibilityBundleError::BundleUnavailable => CompatibilityBundleError::PlanNotFound,
        CompatibilityBundleError::BundleTooLarge => CompatibilityBundleError::PlanDigestInvalid,
        _ => CompatibilityBundleError::PlanStoreUnavailable,
    })?;
    let plan = serde_json::from_slice::<CompatibilityInstallPlan>(&bytes)
        .map_err(|_| CompatibilityBundleError::PlanDigestInvalid)?;
    if plan_digest(&plan)? != digest
        || plan.schema_version != PLAN_SCHEMA
        || plan.package_version != PRODUCT_VERSION
        || plan.expires_at_epoch_seconds
            != plan
                .created_at_epoch_seconds
                .saturating_add(PLAN_LIFETIME_SECONDS)
    {
        return Err(CompatibilityBundleError::PlanDigestInvalid);
    }
    if now_epoch_seconds()? > plan.expires_at_epoch_seconds {
        return Err(CompatibilityBundleError::PlanExpired);
    }
    let data_root = canonical_plain_directory(&plan.data_root)?;
    let baseline =
        embedded_compatibility_matrix().map_err(|_| CompatibilityBundleError::BundleInvalid)?;
    let source_bytes = read_plain_bounded(&plan.source_path, MAX_BUNDLE_BYTES)?;
    if digest_bytes(&source_bytes) != plan.expected_sha256
        || source_bytes != plan.bundle_json.as_bytes()
    {
        return Err(CompatibilityBundleError::PlanInputsChanged);
    }
    let host_profile_bytes = read_plain_bounded_with_errors(
        &plan.host_profile_source_path,
        MAX_HOST_PROFILE_BYTES,
        CompatibilityBundleError::PlanInputsChanged,
        CompatibilityBundleError::PlanInputsChanged,
    )?;
    if host_profile_bytes != plan.host_profile_json.as_bytes()
        || digest_bytes(&host_profile_bytes) != plan.host_coordinate_profile_sha256
    {
        return Err(CompatibilityBundleError::PlanInputsChanged);
    }
    let bundle = SurfaceCompatibilityBundle::parse(plan.bundle_json.as_bytes(), &baseline)
        .map_err(|_| CompatibilityBundleError::PlanInputsChanged)?;
    if bundle.bundle_id != plan.bundle_id
        || bundle.sequence != plan.sequence
        || bundle.baseline_matrix_sha256 != plan.baseline_matrix_sha256
        || bundle.host_coordinate_profile_sha256 != plan.host_coordinate_profile_sha256
    {
        return Err(CompatibilityBundleError::PlanInputsChanged);
    }

    let version_root = prepare_version_root(&data_root)?;
    let _lock = OperationLock::acquire(&version_root)?;
    if now_epoch_seconds()? > plan.expires_at_epoch_seconds {
        return Err(CompatibilityBundleError::PlanExpired);
    }
    let active = read_active_bundle(&data_root, &baseline)?;
    if active
        .as_ref()
        .is_some_and(|active| active.bundle_sha256 == plan.expected_sha256)
    {
        return Ok(install_report(&plan, digest, "already_installed"));
    }
    if active.as_ref().map(|active| active.state_sha256.clone()) != plan.active_before_sha256 {
        return Err(CompatibilityBundleError::PlanInputsChanged);
    }
    if active
        .as_ref()
        .is_some_and(|active| bundle.sequence <= active.bundle.sequence)
    {
        return Err(CompatibilityBundleError::SequenceNotAdvanced);
    }
    let active_document = ActiveCompatibilityDocument {
        schema_version: ACTIVE_SCHEMA.to_owned(),
        bundle_sha256: plan.expected_sha256.clone(),
        bundle_json: plan.bundle_json.clone(),
        host_coordinate_profile_sha256: plan.host_coordinate_profile_sha256.clone(),
        host_coordinate_profile_json: plan.host_profile_json.clone(),
    };
    let active_bytes = serde_json::to_vec(&active_document)
        .map_err(|_| CompatibilityBundleError::AtomicWriteFailed)?;
    atomic_write_private(&version_root, ACTIVE_BUNDLE_NAME, &active_bytes)?;
    let _ = fs::remove_file(&plan_path);
    Ok(install_report(&plan, digest, "installed"))
}

/// Resolves the embedded matrix plus an optional valid active surface bundle.
/// Any malformed active file is an error; callers must not silently ignore it.
pub fn load_effective_compatibility_matrix(
    data_root: &Path,
) -> Result<EffectiveCompatibilityMatrix, CompatibilityBundleError> {
    let data_root = canonical_plain_directory(data_root)?;
    let baseline =
        embedded_compatibility_matrix().map_err(|_| CompatibilityBundleError::BundleInvalid)?;
    let Some(active) = read_active_bundle(&data_root, &baseline)? else {
        return Ok(EffectiveCompatibilityMatrix {
            matrix: baseline,
            source: CompatibilityMatrixSource::Embedded,
            bundle_id: None,
            sequence: None,
            bundle_sha256: None,
            host_coordinate_profile_sha256: None,
        });
    };
    let matrix = active
        .bundle
        .apply_to(&baseline)
        .map_err(|_| CompatibilityBundleError::BundleInvalid)?;
    Ok(EffectiveCompatibilityMatrix {
        matrix,
        source: CompatibilityMatrixSource::InstalledBundle,
        bundle_id: Some(active.bundle.bundle_id),
        sequence: Some(active.bundle.sequence),
        bundle_sha256: Some(active.bundle_sha256),
        host_coordinate_profile_sha256: Some(active.bundle.host_coordinate_profile_sha256),
    })
}

/// Returns a bounded read-only status for the effective matrix.
pub fn compatibility_bundle_status(
    data_root: Option<&Path>,
) -> Result<CompatibilityBundleStatus, CompatibilityBundleError> {
    let data_root = resolve_data_root(data_root)?;
    let effective = load_effective_compatibility_matrix(&data_root)?;
    let effective_matrix_sha256 = format!(
        "sha256:{}",
        effective
            .matrix
            .canonical_digest()
            .map_err(|_| CompatibilityBundleError::BundleInvalid)?
    );
    Ok(CompatibilityBundleStatus {
        schema_version: STATUS_SCHEMA.to_owned(),
        status: "ready".to_owned(),
        package_version: PRODUCT_VERSION.to_owned(),
        source: effective.source,
        effective_matrix_sha256,
        bundle_id: effective.bundle_id,
        sequence: effective.sequence,
        bundle_sha256: effective.bundle_sha256,
        host_coordinate_profile_sha256: effective.host_coordinate_profile_sha256,
    })
}

fn install_report(
    plan: &CompatibilityInstallPlan,
    plan_digest: &str,
    status: &str,
) -> CompatibilityInstallReport {
    CompatibilityInstallReport {
        schema_version: REPORT_SCHEMA.to_owned(),
        status: status.to_owned(),
        plan_digest: plan_digest.to_owned(),
        package_version: PRODUCT_VERSION.to_owned(),
        bundle_id: plan.bundle_id.clone(),
        sequence: plan.sequence,
        bundle_sha256: plan.expected_sha256.clone(),
        host_coordinate_profile_sha256: plan.host_coordinate_profile_sha256.clone(),
    }
}

struct ActiveBundle {
    bundle: SurfaceCompatibilityBundle,
    bundle_sha256: String,
    state_sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ActiveCompatibilityDocument {
    schema_version: String,
    bundle_sha256: String,
    bundle_json: String,
    host_coordinate_profile_sha256: String,
    host_coordinate_profile_json: String,
}

fn read_active_bundle(
    data_root: &Path,
    baseline: &CompatibilityMatrix,
) -> Result<Option<ActiveBundle>, CompatibilityBundleError> {
    let path = active_bundle_path(data_root);
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(CompatibilityBundleError::PathUnsafe),
    }
    let (bytes, metadata) = read_plain_bounded_with_metadata(
        &path,
        MAX_ACTIVE_BYTES,
        CompatibilityBundleError::BundleInvalid,
        CompatibilityBundleError::BundleTooLarge,
    )?;
    if !private_file_mode_matches(&metadata) {
        return Err(CompatibilityBundleError::BundleInvalid);
    }
    let document = serde_json::from_slice::<ActiveCompatibilityDocument>(&bytes)
        .map_err(|_| CompatibilityBundleError::BundleInvalid)?;
    if document.schema_version != ACTIVE_SCHEMA
        || digest_bytes(document.bundle_json.as_bytes()) != document.bundle_sha256
        || digest_bytes(document.host_coordinate_profile_json.as_bytes())
            != document.host_coordinate_profile_sha256
    {
        return Err(CompatibilityBundleError::BundleInvalid);
    }
    let bundle = SurfaceCompatibilityBundle::parse(document.bundle_json.as_bytes(), baseline)
        .map_err(|_| CompatibilityBundleError::BundleInvalid)?;
    if bundle.host_coordinate_profile_sha256 != document.host_coordinate_profile_sha256 {
        return Err(CompatibilityBundleError::BundleInvalid);
    }
    validate_host_coordinate_profile(&document.host_coordinate_profile_json, &bundle)
        .map_err(|_| CompatibilityBundleError::BundleInvalid)?;
    Ok(Some(ActiveBundle {
        bundle,
        bundle_sha256: document.bundle_sha256,
        state_sha256: digest_bytes(&bytes),
    }))
}

fn validate_host_coordinate_profile(
    profile_json: &str,
    bundle: &SurfaceCompatibilityBundle,
) -> Result<(), CompatibilityBundleError> {
    let profile = serde_json::from_str::<HostCoordinateProfile>(profile_json)
        .map_err(|_| CompatibilityBundleError::HostProfileInvalid)?;
    let embedded = serde_json::from_str::<HostCoordinateProfile>(HOST_COORDINATE_PROFILE_JSON)
        .map_err(|_| CompatibilityBundleError::HostProfileInvalid)?;
    if profile.schema_version != "godot-codex-host-coordinate-profile/1.0"
        || !safe_token(&profile.profile_id)
        || profile.compatibility_matrix != embedded.compatibility_matrix
        || profile.server_instructions != embedded.server_instructions
        || profile.compatibility_matrix.path
            != "godot-codex-mcp/product/compatibility-matrix.v1.json"
        || profile.compatibility_matrix.sha256 != digest_bytes(COMPATIBILITY_MATRIX_JSON.as_bytes())
        || profile.surfaces.len() != bundle.surfaces.len()
        || profile.surfaces.len() < 3
        || profile.surfaces.len() > 32
    {
        return Err(CompatibilityBundleError::HostProfileInvalid);
    }

    let mut coordinates = BTreeMap::new();
    let mut represented = BTreeSet::new();
    for coordinate in &profile.surfaces {
        validate_host_surface_coordinate(coordinate)?;
        represented.insert(coordinate.surface);
        let key = (
            coordinate.surface,
            coordinate.host_version.clone(),
            coordinate.ide_host_version.clone(),
        );
        if coordinates.insert(key, coordinate).is_some() {
            return Err(CompatibilityBundleError::HostProfileInvalid);
        }
    }
    if represented != BTreeSet::from([SurfaceKind::App, SurfaceKind::Cli, SurfaceKind::Ide]) {
        return Err(CompatibilityBundleError::HostProfileInvalid);
    }
    for rule in &bundle.surfaces {
        let key = (
            rule.surface,
            rule.host_version.clone(),
            rule.ide_host_version.clone(),
        );
        if coordinates
            .get(&key)
            .is_none_or(|coordinate| coordinate.qualification != rule.qualification)
        {
            return Err(CompatibilityBundleError::HostProfileInvalid);
        }
    }
    Ok(())
}

fn validate_host_surface_coordinate(
    coordinate: &HostSurfaceCoordinate,
) -> Result<(), CompatibilityBundleError> {
    if [
        coordinate.host_name.as_str(),
        coordinate.host_identifier.as_str(),
        coordinate.host_version.as_str(),
        coordinate.host_build.as_str(),
        coordinate.client_version.as_str(),
    ]
    .into_iter()
    .any(|value| !safe_token(value))
        || coordinate
            .host_commit
            .as_deref()
            .is_some_and(|value| !lowercase_hex(value, 40))
        || !valid_prefixed_digest(&coordinate.host_artifact_sha256)
        || !valid_prefixed_digest(&coordinate.client_artifact_sha256)
        || coordinate
            .host_metadata_sha256
            .as_deref()
            .is_some_and(|value| !valid_prefixed_digest(value))
        || coordinate
            .ide_host_version
            .as_deref()
            .is_some_and(|value| !safe_token(value))
        || coordinate
            .ide_shell_identifier
            .as_deref()
            .is_some_and(|value| !safe_token(value))
        || coordinate
            .ide_shell_artifact_sha256
            .as_deref()
            .is_some_and(|value| !valid_prefixed_digest(value))
        || coordinate
            .ide_shell_team_id
            .as_deref()
            .is_some_and(|value| !safe_token(value))
        || coordinate
            .host_code_signature
            .as_ref()
            .is_some_and(|value| !valid_host_code_signature(value))
        || coordinate
            .client_code_signature
            .as_ref()
            .is_some_and(|value| !valid_host_code_signature(value))
        || coordinate
            .ide_shell_code_signature
            .as_ref()
            .is_some_and(|value| !valid_host_code_signature(value))
    {
        return Err(CompatibilityBundleError::HostProfileInvalid);
    }

    let ide_fields_present = coordinate.ide_host_version.is_some()
        && coordinate.ide_shell_identifier.is_some()
        && coordinate.ide_shell_artifact_sha256.is_some()
        && coordinate.ide_shell_team_id.is_some()
        && coordinate.ide_shell_code_signature.is_some();
    let ide_fields_absent = coordinate.ide_host_version.is_none()
        && coordinate.ide_shell_identifier.is_none()
        && coordinate.ide_shell_artifact_sha256.is_none()
        && coordinate.ide_shell_team_id.is_none()
        && coordinate.ide_shell_code_signature.is_none();
    let shape_valid = match coordinate.surface {
        SurfaceKind::App => {
            coordinate.host_artifact_kind == HostArtifactKind::MacosBundleExecutable
                && coordinate.host_metadata_sha256.is_none()
                && coordinate.host_code_signature.is_some()
                && coordinate.client_code_signature.is_some()
                && ide_fields_absent
        }
        SurfaceKind::Cli => {
            coordinate.host_artifact_kind == HostArtifactKind::StandaloneExecutable
                && coordinate.host_metadata_sha256.is_none()
                && coordinate.host_code_signature.is_some()
                && coordinate.client_code_signature.is_some()
                && ide_fields_absent
        }
        SurfaceKind::Ide => {
            coordinate.host_artifact_kind == HostArtifactKind::ExtensionTree
                && coordinate.host_metadata_sha256.is_some()
                && coordinate.host_code_signature.is_none()
                && coordinate.client_code_signature.is_some()
                && ide_fields_present
        }
        SurfaceKind::Cursor => false,
    };
    if !shape_valid {
        return Err(CompatibilityBundleError::HostProfileInvalid);
    }
    Ok(())
}

fn valid_host_code_signature(signature: &HostCodeSignature) -> bool {
    matches!(signature.mode.as_str(), "strict" | "deep_strict")
        && safe_token(&signature.identifier)
        && safe_token(&signature.team_id)
        && (lowercase_hex(&signature.cdhash, 40) || lowercase_hex(&signature.cdhash, 64))
}

fn safe_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'+' | b':' | b'/' | b'-'))
        })
}

fn lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_prefixed_digest(value: &str) -> bool {
    parse_prefixed_digest(value).is_some()
}

fn resolve_data_root(override_root: Option<&Path>) -> Result<PathBuf, CompatibilityBundleError> {
    if let Some(root) = override_root {
        return canonical_plain_directory(root);
    }
    let launcher =
        resolve_launcher(None).map_err(|_| CompatibilityBundleError::DataRootUnavailable)?;
    launcher
        .installed_data_root()
        .ok_or(CompatibilityBundleError::DataRootUnavailable)
        .and_then(canonical_plain_directory)
}

fn active_bundle_path(data_root: &Path) -> PathBuf {
    data_root
        .join(COMPATIBILITY_DIRECTORY)
        .join(PRODUCT_VERSION)
        .join(ACTIVE_BUNDLE_NAME)
}

fn prepare_version_root(data_root: &Path) -> Result<PathBuf, CompatibilityBundleError> {
    let compatibility = data_root.join(COMPATIBILITY_DIRECTORY);
    ensure_private_directory(&compatibility)?;
    let version = compatibility.join(PRODUCT_VERSION);
    ensure_private_directory(&version)?;
    Ok(version)
}

fn ensure_private_directory(path: &Path) -> Result<(), CompatibilityBundleError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || !private_directory_mode_matches(&metadata)
            {
                return Err(CompatibilityBundleError::PathUnsafe);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| CompatibilityBundleError::PathUnsafe)?;
            set_private_directory_mode(path)?;
            File::open(path)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| CompatibilityBundleError::AtomicWriteFailed)?;
        }
        Err(_) => return Err(CompatibilityBundleError::PathUnsafe),
    }
    Ok(())
}

fn canonical_plain_file(path: &Path) -> Result<PathBuf, CompatibilityBundleError> {
    canonical_plain_file_with_error(path, CompatibilityBundleError::BundleUnavailable)
}

fn canonical_plain_file_with_error(
    path: &Path,
    unavailable: CompatibilityBundleError,
) -> Result<PathBuf, CompatibilityBundleError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| unavailable.clone())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CompatibilityBundleError::PathUnsafe);
    }
    let canonical = fs::canonicalize(path).map_err(|_| CompatibilityBundleError::PathUnsafe)?;
    let canonical_metadata =
        fs::symlink_metadata(&canonical).map_err(|_| CompatibilityBundleError::PathUnsafe)?;
    if !same_file_identity(&metadata, &canonical_metadata) {
        return Err(CompatibilityBundleError::PathUnsafe);
    }
    Ok(canonical)
}

fn canonical_plain_directory(path: &Path) -> Result<PathBuf, CompatibilityBundleError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| CompatibilityBundleError::DataRootUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CompatibilityBundleError::PathUnsafe);
    }
    let canonical = fs::canonicalize(path).map_err(|_| CompatibilityBundleError::PathUnsafe)?;
    if !canonical.is_absolute() || canonical == Path::new("/") {
        return Err(CompatibilityBundleError::PathUnsafe);
    }
    Ok(canonical)
}

fn read_plain_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, CompatibilityBundleError> {
    read_plain_bounded_with_errors(
        path,
        maximum,
        CompatibilityBundleError::BundleUnavailable,
        CompatibilityBundleError::BundleTooLarge,
    )
}

fn read_plain_bounded_with_errors(
    path: &Path,
    maximum: u64,
    unavailable: CompatibilityBundleError,
    too_large: CompatibilityBundleError,
) -> Result<Vec<u8>, CompatibilityBundleError> {
    read_plain_bounded_with_metadata(path, maximum, unavailable, too_large).map(|(bytes, _)| bytes)
}

fn read_plain_bounded_with_metadata(
    path: &Path,
    maximum: u64,
    unavailable: CompatibilityBundleError,
    too_large: CompatibilityBundleError,
) -> Result<(Vec<u8>, fs::Metadata), CompatibilityBundleError> {
    let path_metadata = fs::symlink_metadata(path).map_err(|_| unavailable.clone())?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(CompatibilityBundleError::PathUnsafe);
    }
    if path_metadata.len() > maximum {
        return Err(too_large.clone());
    }
    let mut file = open_plain_readonly(path).map_err(|_| unavailable.clone())?;
    let before = file.metadata().map_err(|_| unavailable.clone())?;
    if !before.is_file() || !same_file_identity(&path_metadata, &before) || before.len() > maximum {
        return Err(CompatibilityBundleError::PathUnsafe);
    }
    let mut bytes = Vec::with_capacity((before.len().min(maximum)) as usize);
    Read::by_ref(&mut file)
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable.clone())?;
    if bytes.len() as u64 > maximum {
        return Err(too_large);
    }
    let after = file.metadata().map_err(|_| unavailable.clone())?;
    if !same_file_snapshot(&before, &after) || bytes.len() as u64 != after.len() {
        return Err(unavailable);
    }
    Ok((bytes, after))
}

#[cfg(unix)]
fn open_plain_readonly(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};

    rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)
}

#[cfg(not(unix))]
fn open_plain_readonly(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

#[cfg(unix)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.is_file() && right.is_file() && left.len() == right.len()
}

#[cfg(unix)]
fn same_file_snapshot(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    same_file_identity(left, right)
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

#[cfg(not(unix))]
fn same_file_snapshot(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    same_file_identity(left, right)
        && left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
}

fn persist_plan(
    store: &Path,
    digest: &str,
    plan: &CompatibilityInstallPlan,
) -> Result<(), CompatibilityBundleError> {
    let store = prepare_plan_store(store)?;
    let bytes =
        serde_json::to_vec(plan).map_err(|_| CompatibilityBundleError::PlanDigestInvalid)?;
    if bytes.len() as u64 > MAX_PLAN_BYTES {
        return Err(CompatibilityBundleError::PlanDigestInvalid);
    }
    let path = store.join(format!("{}.json", digest.trim_start_matches("sha256:")));
    match write_new_private(&path, &bytes) {
        Ok(()) => {
            File::open(&store)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| CompatibilityBundleError::PlanStoreUnavailable)?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = read_plain_bounded(&path, MAX_PLAN_BYTES)
                .map_err(|_| CompatibilityBundleError::PlanStoreUnavailable)?;
            if existing == bytes {
                Ok(())
            } else {
                Err(CompatibilityBundleError::PlanDigestInvalid)
            }
        }
        Err(_) => Err(CompatibilityBundleError::PlanStoreUnavailable),
    }
}

fn prepare_plan_store(store: &Path) -> Result<PathBuf, CompatibilityBundleError> {
    if !store.is_absolute() || store == Path::new("/") {
        return Err(CompatibilityBundleError::PlanStoreUnavailable);
    }
    if !store.exists() {
        fs::create_dir_all(store).map_err(|_| CompatibilityBundleError::PlanStoreUnavailable)?;
        set_private_directory_mode(store)?;
    }
    let canonical = canonical_plain_directory(store)
        .map_err(|_| CompatibilityBundleError::PlanStoreUnavailable)?;
    let count = fs::read_dir(&canonical)
        .map_err(|_| CompatibilityBundleError::PlanStoreUnavailable)?
        .filter_map(Result::ok)
        .count();
    if count >= MAX_STORED_PLANS {
        return Err(CompatibilityBundleError::PlanLimitReached);
    }
    Ok(canonical)
}

fn plan_digest(plan: &CompatibilityInstallPlan) -> Result<String, CompatibilityBundleError> {
    let bytes =
        serde_json::to_vec(plan).map_err(|_| CompatibilityBundleError::PlanDigestInvalid)?;
    Ok(digest_bytes(&bytes))
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn parse_prefixed_digest(value: &str) -> Option<&str> {
    let hex = value.strip_prefix("sha256:")?;
    (hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
    .then_some(hex)
}

fn now_epoch_seconds() -> Result<u64, CompatibilityBundleError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| CompatibilityBundleError::PlanStoreUnavailable)
}

struct OperationLock {
    #[allow(dead_code)]
    file: File,
}

impl OperationLock {
    fn acquire(version_root: &Path) -> Result<Self, CompatibilityBundleError> {
        let path = version_root.join(OPERATION_LOCK_NAME);
        let metadata = fs::symlink_metadata(&path);
        if let Ok(metadata) = &metadata
            && (metadata.file_type().is_symlink()
                || !metadata.is_file()
                || !private_file_mode_matches(metadata))
        {
            return Err(CompatibilityBundleError::PathUnsafe);
        }
        if metadata
            .as_ref()
            .is_err_and(|error| error.kind() != io::ErrorKind::NotFound)
        {
            return Err(CompatibilityBundleError::PathUnsafe);
        }
        let file = open_lock_file(&path)?;
        #[cfg(unix)]
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| CompatibilityBundleError::OperationInProgress)?;
        Ok(Self { file })
    }
}

#[cfg(unix)]
fn open_lock_file(path: &Path) -> Result<File, CompatibilityBundleError> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| CompatibilityBundleError::AtomicWriteFailed)?;
    let file = File::from(descriptor);
    rustix::fs::fchmod(&file, Mode::from_raw_mode(0o600))
        .map_err(|_| CompatibilityBundleError::AtomicWriteFailed)?;
    Ok(file)
}

#[cfg(not(unix))]
fn open_lock_file(path: &Path) -> Result<File, CompatibilityBundleError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| CompatibilityBundleError::AtomicWriteFailed)
}

fn atomic_write_private(
    parent: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), CompatibilityBundleError> {
    let target = parent.join(name);
    if fs::symlink_metadata(&target)
        .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(CompatibilityBundleError::PathUnsafe);
    }
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|_| CompatibilityBundleError::AtomicWriteFailed)?;
    let temporary_name = format!(
        ".{name}.{}.tmp",
        random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let temporary = parent.join(&temporary_name);
    let result = (|| {
        let mut file = open_new_private(&temporary)?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| CompatibilityBundleError::AtomicWriteFailed)?;
        fs::rename(&temporary, &target).map_err(|_| CompatibilityBundleError::AtomicWriteFailed)?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| CompatibilityBundleError::AtomicWriteFailed)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn open_new_private(path: &Path) -> Result<File, CompatibilityBundleError> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| CompatibilityBundleError::AtomicWriteFailed)?;
    Ok(File::from(descriptor))
}

#[cfg(not(unix))]
fn open_new_private(path: &Path) -> Result<File, CompatibilityBundleError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| CompatibilityBundleError::AtomicWriteFailed)
}

#[cfg(unix)]
fn write_new_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(io::Error::from)?;
    let mut file = File::from(descriptor);
    rustix::fs::fchmod(&file, Mode::from_raw_mode(0o600)).map_err(io::Error::from)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_new_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn set_private_directory_mode(path: &Path) -> Result<(), CompatibilityBundleError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| CompatibilityBundleError::PathUnsafe)
}

#[cfg(not(unix))]
fn set_private_directory_mode(_path: &Path) -> Result<(), CompatibilityBundleError> {
    Ok(())
}

#[cfg(unix)]
fn private_file_mode_matches(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    metadata.permissions().mode() & 0o777 == 0o600
        && metadata.uid() == rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn private_file_mode_matches(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn private_directory_mode_matches(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    metadata.permissions().mode() & 0o777 == 0o700
        && metadata.uid() == rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn private_directory_mode_matches(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use godot_codex_product::{
        SURFACE_COMPATIBILITY_BUNDLE_SCHEMA, SurfaceCompatibilityBundle, SurfaceKind,
        SurfaceQualification,
    };
    use tempfile::TempDir;

    use super::*;

    fn private_directory(path: &Path) {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn fixture() -> (TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
        let private = TempDir::new().unwrap();
        let data_root = private.path().join("data");
        let plan_store = private.path().join("plans");
        private_directory(&data_root);
        private_directory(&plan_store);
        let bundle_path = private.path().join("bundle.json");
        let host_profile_path = private.path().join("host-profile.json");
        let mut host_profile =
            serde_json::from_str::<serde_json::Value>(HOST_COORDINATE_PROFILE_JSON).unwrap();
        for surface in host_profile["surfaces"].as_array_mut().unwrap() {
            surface["qualification"] = serde_json::json!("supported");
        }
        fs::write(
            &host_profile_path,
            serde_json::to_vec_pretty(&host_profile).unwrap(),
        )
        .unwrap();
        (
            private,
            data_root,
            plan_store,
            bundle_path,
            host_profile_path,
        )
    }

    fn bundle_bytes(sequence: u64, host_profile_sha256: &str) -> Vec<u8> {
        let matrix = embedded_compatibility_matrix().unwrap();
        let mut surfaces = matrix
            .surfaces
            .iter()
            .filter(|rule| rule.surface != SurfaceKind::Cursor)
            .cloned()
            .collect::<Vec<_>>();
        for rule in &mut surfaces {
            rule.qualification = SurfaceQualification::Supported;
        }
        let bundle = SurfaceCompatibilityBundle {
            schema_version: SURFACE_COMPATIBILITY_BUNDLE_SCHEMA.to_owned(),
            bundle_id: format!("qualified-hosts-{sequence}"),
            sequence,
            package_version: PRODUCT_VERSION.to_owned(),
            baseline_matrix_sha256: format!("sha256:{}", matrix.canonical_digest().unwrap()),
            host_coordinate_profile_sha256: host_profile_sha256.to_owned(),
            target: matrix.package.target,
            surfaces,
        };
        serde_json::to_vec_pretty(&bundle).unwrap()
    }

    fn write_bundle(path: &Path, sequence: u64, host_profile_sha256: &str) -> String {
        let bytes = bundle_bytes(sequence, host_profile_sha256);
        fs::write(path, &bytes).unwrap();
        digest_bytes(&bytes)
    }

    #[test]
    fn exact_preview_apply_and_status_use_the_installed_overlay() {
        let (_private, data_root, plan_store, bundle_path, host_profile_path) = fixture();
        let host_profile_digest = digest_bytes(&fs::read(&host_profile_path).unwrap());
        let digest = write_bundle(&bundle_path, 1, &host_profile_digest);
        let options = CompatibilityInstallOptions::new(&bundle_path, &host_profile_path, &digest)
            .with_locations(&data_root, &plan_store);
        let preview = prepare_compatibility_install(&options).unwrap();
        assert_eq!(preview.sequence, 1);
        assert_eq!(preview.previous_sequence, None);

        let report = apply_compatibility_install(&preview.plan_digest, Some(&plan_store)).unwrap();
        assert_eq!(report.status, "installed");
        let effective = load_effective_compatibility_matrix(&data_root).unwrap();
        assert_eq!(effective.source, CompatibilityMatrixSource::InstalledBundle);
        assert_eq!(effective.sequence, Some(1));
        assert!(effective.matrix.surfaces.iter().any(|rule| {
            rule.surface == SurfaceKind::App
                && rule.qualification == SurfaceQualification::Supported
        }));

        let status = compatibility_bundle_status(Some(&data_root)).unwrap();
        assert_eq!(status.source, CompatibilityMatrixSource::InstalledBundle);
        assert_eq!(status.sequence, Some(1));
        assert_eq!(status.bundle_sha256.as_deref(), Some(digest.as_str()));
    }

    #[test]
    fn expected_digest_tampering_and_sequence_rollback_fail_closed() {
        let (_private, data_root, plan_store, bundle_path, host_profile_path) = fixture();
        let host_profile_digest = digest_bytes(&fs::read(&host_profile_path).unwrap());
        let digest = write_bundle(&bundle_path, 1, &host_profile_digest);
        let wrong = format!("sha256:{}", "f".repeat(64));
        let options = CompatibilityInstallOptions::new(&bundle_path, &host_profile_path, wrong)
            .with_locations(&data_root, &plan_store);
        assert_eq!(
            prepare_compatibility_install(&options),
            Err(CompatibilityBundleError::BundleDigestMismatch)
        );

        let original_profile = fs::read(&host_profile_path).unwrap();
        fs::write(&host_profile_path, br#"{"schema_version":"changed"}"#).unwrap();
        let options = CompatibilityInstallOptions::new(&bundle_path, &host_profile_path, &digest)
            .with_locations(&data_root, &plan_store);
        assert_eq!(
            prepare_compatibility_install(&options),
            Err(CompatibilityBundleError::HostProfileDigestMismatch)
        );
        fs::write(&host_profile_path, original_profile).unwrap();

        let options = CompatibilityInstallOptions::new(&bundle_path, &host_profile_path, &digest)
            .with_locations(&data_root, &plan_store);
        let preview = prepare_compatibility_install(&options).unwrap();
        apply_compatibility_install(&preview.plan_digest, Some(&plan_store)).unwrap();
        let options = CompatibilityInstallOptions::new(&bundle_path, &host_profile_path, &digest)
            .with_locations(&data_root, &plan_store);
        assert_eq!(
            prepare_compatibility_install(&options),
            Err(CompatibilityBundleError::SequenceNotAdvanced)
        );
    }

    #[test]
    fn digest_bound_but_malformed_or_cross_mismatched_host_profile_fails_closed() {
        let (_private, data_root, plan_store, bundle_path, host_profile_path) = fixture();
        fs::write(&host_profile_path, br#"{"schema_version":"unexpected"}"#).unwrap();
        let host_profile_digest = digest_bytes(&fs::read(&host_profile_path).unwrap());
        let digest = write_bundle(&bundle_path, 1, &host_profile_digest);
        let options = CompatibilityInstallOptions::new(&bundle_path, &host_profile_path, &digest)
            .with_locations(&data_root, &plan_store);
        assert_eq!(
            prepare_compatibility_install(&options),
            Err(CompatibilityBundleError::HostProfileInvalid)
        );

        let mut host_profile =
            serde_json::from_str::<serde_json::Value>(HOST_COORDINATE_PROFILE_JSON).unwrap();
        for surface in host_profile["surfaces"].as_array_mut().unwrap() {
            surface["qualification"] = serde_json::json!("supported");
        }
        host_profile["surfaces"][0]["host_version"] = serde_json::json!("different");
        fs::write(
            &host_profile_path,
            serde_json::to_vec(&host_profile).unwrap(),
        )
        .unwrap();
        let host_profile_digest = digest_bytes(&fs::read(&host_profile_path).unwrap());
        let digest = write_bundle(&bundle_path, 1, &host_profile_digest);
        let options = CompatibilityInstallOptions::new(&bundle_path, &host_profile_path, &digest)
            .with_locations(&data_root, &plan_store);
        assert_eq!(
            prepare_compatibility_install(&options),
            Err(CompatibilityBundleError::HostProfileInvalid)
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_bundle_or_host_profile_is_rejected_before_install_planning() {
        use std::os::unix::fs::symlink;

        let (private, data_root, plan_store, bundle_path, host_profile_path) = fixture();
        let host_profile_digest = digest_bytes(&fs::read(&host_profile_path).unwrap());
        let digest = write_bundle(&bundle_path, 1, &host_profile_digest);
        let bundle_target = private.path().join("bundle-target.json");
        fs::copy(&bundle_path, &bundle_target).unwrap();
        fs::remove_file(&bundle_path).unwrap();
        symlink(&bundle_target, &bundle_path).unwrap();
        let options = CompatibilityInstallOptions::new(&bundle_path, &host_profile_path, &digest)
            .with_locations(&data_root, &plan_store);
        assert_eq!(
            prepare_compatibility_install(&options),
            Err(CompatibilityBundleError::PathUnsafe)
        );

        fs::remove_file(&bundle_path).unwrap();
        fs::copy(&bundle_target, &bundle_path).unwrap();
        let profile_target = private.path().join("profile-target.json");
        fs::copy(&host_profile_path, &profile_target).unwrap();
        fs::remove_file(&host_profile_path).unwrap();
        symlink(&profile_target, &host_profile_path).unwrap();
        let options = CompatibilityInstallOptions::new(&bundle_path, &host_profile_path, &digest)
            .with_locations(&data_root, &plan_store);
        assert_eq!(
            prepare_compatibility_install(&options),
            Err(CompatibilityBundleError::PathUnsafe)
        );
    }

    #[test]
    fn changed_source_and_invalid_active_bundle_never_fall_back_silently() {
        let (_private, data_root, plan_store, bundle_path, host_profile_path) = fixture();
        let host_profile_digest = digest_bytes(&fs::read(&host_profile_path).unwrap());
        let digest = write_bundle(&bundle_path, 1, &host_profile_digest);
        let options = CompatibilityInstallOptions::new(&bundle_path, &host_profile_path, &digest)
            .with_locations(&data_root, &plan_store);
        let preview = prepare_compatibility_install(&options).unwrap();
        fs::write(&bundle_path, bundle_bytes(2, &host_profile_digest)).unwrap();
        assert_eq!(
            apply_compatibility_install(&preview.plan_digest, Some(&plan_store)),
            Err(CompatibilityBundleError::PlanInputsChanged)
        );

        let version_root = prepare_version_root(&data_root).unwrap();
        fs::write(version_root.join(ACTIVE_BUNDLE_NAME), b"{}").unwrap();
        fs::set_permissions(
            version_root.join(ACTIVE_BUNDLE_NAME),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(matches!(
            load_effective_compatibility_matrix(&data_root),
            Err(CompatibilityBundleError::BundleInvalid)
        ));
    }
}
