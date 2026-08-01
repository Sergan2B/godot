use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use godot_codex_surface_capture::{
    ArmDisposition, ArmRequest, ArmedLease, BindingDigests, LeaseError, LeaseState, LeaseStore,
    MetadataBinding, Surface, project_identity_for_path as surface_project_identity,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::PRODUCT_VERSION;
use crate::launcher::{read_plain_bounded, resolve_launcher};
use crate::setup::{SetupError, verify_surface_capture_setup_state};

const RESULT_SCHEMA: &str = "godot-codex-surface-capture-result/1.0";
const ARM_RESULT_SCHEMA: &str = "godot-codex-surface-capture-result/1.1";
const PACKAGE_MANIFEST_NAME: &str = "package-manifest.json";
const MAX_MANIFEST_BYTES: u64 = 512 * 1024;
const MAX_METADATA_BYTES: u64 = 64 * 1024;
const MAX_CAPTURE_TTL_SECONDS: u64 = 30 * 60;
const MAX_CAPTURE_EXECUTABLE_BYTES: u64 = 256 * 1024 * 1024;
const CAPTURE_CONFIG_PATH: &str = ".codex/config.toml";
const CAPTURE_RECEIPT_PATH: &str = ".godot/codex/setup-receipt-v1.json";
const SURFACE_METADATA_SCHEMA: &str =
    include_str!("../../../schemas/godot_codex/sprint11-surface-metadata.schema.json");

static SURFACE_METADATA_VALIDATOR: LazyLock<Option<jsonschema::Validator>> = LazyLock::new(|| {
    serde_json::from_str::<Value>(SURFACE_METADATA_SCHEMA)
        .ok()
        .and_then(|schema| jsonschema::draft202012::options().build(&schema).ok())
});

/// Closed official-host surface accepted by `surface-capture arm`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceCaptureSurface {
    App,
    Cli,
    Ide,
}

impl SurfaceCaptureSurface {
    const fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Cli => "cli",
            Self::Ide => "ide",
        }
    }
}

impl From<SurfaceCaptureSurface> for Surface {
    fn from(value: SurfaceCaptureSurface) -> Self {
        match value {
            SurfaceCaptureSurface::App => Self::App,
            SurfaceCaptureSurface::Cli => Self::Cli,
            SurfaceCaptureSurface::Ide => Self::Ide,
        }
    }
}

/// Inputs for one one-shot official-host capture lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceCaptureArmOptions {
    pub project_root: PathBuf,
    pub surface: SurfaceCaptureSurface,
    pub metadata: PathBuf,
    pub ttl_seconds: u64,
}

impl SurfaceCaptureArmOptions {
    #[must_use]
    pub fn new(
        project_root: impl Into<PathBuf>,
        surface: SurfaceCaptureSurface,
        metadata: impl Into<PathBuf>,
        ttl_seconds: u64,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            surface,
            metadata: metadata.into(),
            ttl_seconds,
        }
    }
}

/// Read-only, exact installed-package/project bindings used to arm a lease.
///
/// Canonical filesystem paths remain private. Public accessors expose only
/// stable hashed identities and exact file-content digests.
pub struct SurfaceCaptureProjectContext {
    project_root: PathBuf,
    data_root: PathBuf,
    project_id: String,
    project_identity_sha256: String,
    package_launcher_sha256: String,
    project_config_sha256: String,
    setup_receipt_sha256: String,
    internal_manifest_sha256: String,
}

/// Minimal claim-time context used by the sidecar before the MCP handshake.
///
/// A lease can only exist after `arm_surface_capture` performed the complete
/// package/setup verification. Claim-time work therefore re-measures the
/// exact running sidecar, project config, and setup receipt and compares
/// those digests with the private one-shot lease. It deliberately does not
/// rescan the complete installed package; the normal product-startup check
/// still performs that scan once before the server is exposed.
pub struct SurfaceCaptureClaimContext {
    project_root: PathBuf,
    data_root: PathBuf,
    bindings: BindingDigests,
}

impl fmt::Debug for SurfaceCaptureClaimContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SurfaceCaptureClaimContext")
            .field("bindings", &self.bindings)
            .finish_non_exhaustive()
    }
}

impl SurfaceCaptureClaimContext {
    #[must_use]
    pub fn canonical_project_root(&self) -> &Path {
        &self.project_root
    }

    #[must_use]
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    #[must_use]
    pub fn binding_digests(&self) -> BindingDigests {
        self.bindings.clone()
    }
}

impl fmt::Debug for SurfaceCaptureProjectContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SurfaceCaptureProjectContext")
            .field("project_id", &self.project_id)
            .field("project_identity_sha256", &self.project_identity_sha256)
            .field("package_launcher_sha256", &self.package_launcher_sha256)
            .field("project_config_sha256", &self.project_config_sha256)
            .field("setup_receipt_sha256", &self.setup_receipt_sha256)
            .field("internal_manifest_sha256", &self.internal_manifest_sha256)
            .finish_non_exhaustive()
    }
}

impl SurfaceCaptureProjectContext {
    /// Canonical exact project root. This accessor is for in-process binding;
    /// reports and `Debug` deliberately omit the path.
    #[must_use]
    pub fn canonical_project_root(&self) -> &Path {
        &self.project_root
    }

    /// Canonical installed data root. This accessor is for the private lease
    /// store; reports and `Debug` deliberately omit the path.
    #[must_use]
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    #[must_use]
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    #[must_use]
    pub fn project_identity_sha256(&self) -> &str {
        &self.project_identity_sha256
    }

    #[must_use]
    pub fn package_launcher_sha256(&self) -> &str {
        &self.package_launcher_sha256
    }

    #[must_use]
    pub fn project_config_sha256(&self) -> &str {
        &self.project_config_sha256
    }

    #[must_use]
    pub fn setup_receipt_sha256(&self) -> &str {
        &self.setup_receipt_sha256
    }

    #[must_use]
    pub fn internal_manifest_sha256(&self) -> &str {
        &self.internal_manifest_sha256
    }

    #[must_use]
    pub fn binding_digests(&self) -> BindingDigests {
        BindingDigests {
            package_launcher_sha256: self.package_launcher_sha256.clone(),
            project_config_sha256: self.project_config_sha256.clone(),
            setup_receipt_sha256: self.setup_receipt_sha256.clone(),
        }
    }
}

/// Safe result of arming a one-shot lease. No filesystem path is serialized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceCaptureArmReport {
    pub schema_version: String,
    pub action: String,
    pub run_id: String,
    pub state: LeaseState,
    pub disposition: ArmDisposition,
    pub surface: SurfaceCaptureSurface,
    pub project_id: String,
    pub project_identity: String,
    pub metadata_sha256: String,
    pub package_launcher_sha256: String,
    pub project_config_sha256: String,
    pub setup_receipt_sha256: String,
    pub internal_manifest_sha256: String,
}

impl SurfaceCaptureArmReport {
    /// Only a new or exact recovered non-expired lease authorizes host start.
    #[must_use]
    pub fn authorizes_host_start(&self) -> bool {
        self.state == LeaseState::Armed
            && matches!(
                self.disposition,
                ArmDisposition::Created | ArmDisposition::Recovered
            )
    }
}

/// Safe read-only state of an exact opaque run id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceCaptureStatusReport {
    pub schema_version: String,
    pub action: String,
    pub run_id: String,
    pub state: LeaseState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<SurfaceCaptureSurface>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_identity: Option<String>,
}

/// Safe result of cancelling one exact, still-unclaimed run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceCaptureCancelReport {
    pub schema_version: String,
    pub action: String,
    pub run_id: String,
    pub state: LeaseState,
}

/// Safe result of digest-bound consumption of one finalized private run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceCaptureConsumeReport {
    pub schema_version: String,
    pub action: String,
    pub run_id: String,
    pub state: LeaseState,
}

/// Safe result of explicitly abandoning one exact inactive eligible Claimed run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceCaptureAbandonReport {
    pub schema_version: String,
    pub action: String,
    pub run_id: String,
    pub state: LeaseState,
}

/// Fail-closed surface-capture operation error.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SurfaceCaptureError {
    #[error("surface_capture_project_invalid")]
    ProjectInvalid,
    #[error("surface_capture_package_invalid")]
    PackageInvalid,
    #[error("surface_capture_project_config_invalid")]
    ProjectConfigInvalid,
    #[error("surface_capture_setup_receipt_invalid")]
    SetupReceiptInvalid,
    #[error("surface_capture_metadata_invalid")]
    MetadataInvalid,
    #[error("surface_capture_ttl_invalid")]
    TtlInvalid,
    #[error("surface_capture_run_id_invalid")]
    RunIdInvalid,
    #[error("surface_capture_store_unavailable")]
    StoreUnavailable,
    #[error("surface_capture_run_not_found")]
    RunNotFound,
    #[error("surface_capture_run_already_claimed")]
    AlreadyClaimed,
    #[error("surface_capture_run_expired")]
    Expired,
    #[error("surface_capture_digest_mismatch")]
    DigestMismatch,
    #[error("surface_capture_metadata_digest_mismatch")]
    MetadataDigestMismatch,
    #[error("surface_capture_claim_active")]
    ClaimActive,
    #[error("surface_capture_recovery_required")]
    RecoveryRequired,
    #[error("surface_capture_consume_required")]
    ConsumeRequired,
    #[error("surface_capture_unsupported")]
    Unsupported,
    #[error("surface_capture_state_invalid")]
    StateInvalid,
}

/// Verify the exact installed package/current launcher and setup-owned project
/// state without changing project configuration, trust, or capture state.
pub fn verify_surface_capture_project(
    project_root: &Path,
) -> Result<SurfaceCaptureProjectContext, SurfaceCaptureError> {
    verify_surface_capture_project_with(project_root, None, None)
}

/// Re-measure only the exact lease-bound files needed to claim a capture.
///
/// This is intentionally a fast path: official hosts impose a bounded MCP
/// initialization deadline, so repeating the complete package checksum scan
/// before the normal startup inspection would make the recorder unusable.
pub fn verify_surface_capture_claim(
    project_root: &Path,
    data_root: &Path,
) -> Result<SurfaceCaptureClaimContext, SurfaceCaptureError> {
    let running_sidecar =
        std::env::current_exe().map_err(|_| SurfaceCaptureError::PackageInvalid)?;
    verify_surface_capture_claim_with(project_root, data_root, &running_sidecar)
}

fn verify_surface_capture_claim_with(
    project_root: &Path,
    data_root: &Path,
    running_sidecar: &Path,
) -> Result<SurfaceCaptureClaimContext, SurfaceCaptureError> {
    let project_root =
        fs::canonicalize(project_root).map_err(|_| SurfaceCaptureError::ProjectInvalid)?;
    require_plain_directory(&project_root, SurfaceCaptureError::ProjectInvalid)?;
    require_plain_file(
        &project_root.join("project.godot"),
        SurfaceCaptureError::ProjectInvalid,
    )?;

    let data_root =
        fs::canonicalize(data_root).map_err(|_| SurfaceCaptureError::StoreUnavailable)?;
    LeaseStore::open(&data_root).map_err(map_lease_error)?;
    let version_root = data_root.join("versions").join(PRODUCT_VERSION);
    require_plain_directory(
        &data_root.join("versions"),
        SurfaceCaptureError::PackageInvalid,
    )?;
    require_plain_directory(&version_root, SurfaceCaptureError::PackageInvalid)?;
    require_plain_directory(
        &version_root.join("bin"),
        SurfaceCaptureError::PackageInvalid,
    )?;
    let current = data_root.join("current");
    let current_metadata =
        fs::symlink_metadata(&current).map_err(|_| SurfaceCaptureError::PackageInvalid)?;
    let current_target =
        fs::read_link(&current).map_err(|_| SurfaceCaptureError::PackageInvalid)?;
    if !current_metadata.file_type().is_symlink()
        || !current_target.is_absolute()
        || fs::canonicalize(&current_target).map_err(|_| SurfaceCaptureError::PackageInvalid)?
            != fs::canonicalize(&version_root).map_err(|_| SurfaceCaptureError::PackageInvalid)?
        || fs::canonicalize(&current).map_err(|_| SurfaceCaptureError::PackageInvalid)?
            != fs::canonicalize(&version_root).map_err(|_| SurfaceCaptureError::PackageInvalid)?
    {
        return Err(SurfaceCaptureError::PackageInvalid);
    }
    let expected_sidecar = version_root.join("bin/godot-codex-mcp");
    require_plain_file(&expected_sidecar, SurfaceCaptureError::PackageInvalid)?;
    let running_sidecar =
        fs::canonicalize(running_sidecar).map_err(|_| SurfaceCaptureError::PackageInvalid)?;
    if running_sidecar
        != fs::canonicalize(&expected_sidecar).map_err(|_| SurfaceCaptureError::PackageInvalid)?
    {
        return Err(SurfaceCaptureError::PackageInvalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if fs::symlink_metadata(&expected_sidecar)
            .map_err(|_| SurfaceCaptureError::PackageInvalid)?
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return Err(SurfaceCaptureError::PackageInvalid);
        }
    }
    let sidecar = read_plain_bounded(&expected_sidecar, MAX_CAPTURE_EXECUTABLE_BYTES)
        .map_err(|_| SurfaceCaptureError::PackageInvalid)?;

    require_plain_project_ancestors(&project_root, CAPTURE_CONFIG_PATH)?;
    require_plain_project_ancestors(&project_root, CAPTURE_RECEIPT_PATH)?;
    let config_path = project_root.join(CAPTURE_CONFIG_PATH);
    let receipt_path = project_root.join(CAPTURE_RECEIPT_PATH);
    let config = read_plain_bounded(&config_path, 256 * 1024)
        .map_err(|_| SurfaceCaptureError::ProjectConfigInvalid)?;
    let receipt = read_plain_bounded(&receipt_path, 64 * 1024)
        .map_err(|_| SurfaceCaptureError::SetupReceiptInvalid)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if fs::symlink_metadata(&receipt_path)
            .map_err(|_| SurfaceCaptureError::SetupReceiptInvalid)?
            .permissions()
            .mode()
            & 0o777
            != 0o600
        {
            return Err(SurfaceCaptureError::SetupReceiptInvalid);
        }
    }
    if read_plain_bounded(&config_path, 256 * 1024)
        .map_err(|_| SurfaceCaptureError::ProjectConfigInvalid)?
        != config
        || read_plain_bounded(&receipt_path, 64 * 1024)
            .map_err(|_| SurfaceCaptureError::SetupReceiptInvalid)?
            != receipt
    {
        return Err(SurfaceCaptureError::StateInvalid);
    }
    Ok(SurfaceCaptureClaimContext {
        project_root,
        data_root,
        bindings: BindingDigests {
            package_launcher_sha256: sha256_bytes(&sidecar),
            project_config_sha256: sha256_bytes(&config),
            setup_receipt_sha256: sha256_bytes(&receipt),
        },
    })
}

/// Arm exactly one bounded, one-shot capture lease.
pub fn arm_surface_capture(
    options: &SurfaceCaptureArmOptions,
) -> Result<SurfaceCaptureArmReport, SurfaceCaptureError> {
    if !(1..=MAX_CAPTURE_TTL_SECONDS).contains(&options.ttl_seconds) {
        return Err(SurfaceCaptureError::TtlInvalid);
    }
    let context = verify_surface_capture_project(&options.project_root)?;
    let metadata = read_metadata(&options.metadata)?;
    validate_metadata_context(&metadata.document, options.surface, &context)?;
    let now = unix_seconds()?;
    let expires_at_unix = now
        .checked_add(options.ttl_seconds)
        .ok_or(SurfaceCaptureError::TtlInvalid)?;
    let store = LeaseStore::open(&context.data_root).map_err(map_lease_error)?;
    let armed = store
        .arm(ArmRequest {
            project_root: context.project_root.clone(),
            surface: options.surface.into(),
            metadata,
            bindings: context.binding_digests(),
            expires_at_unix,
        })
        .map_err(map_lease_error)?;
    let stored_internal_manifest_sha256 = stored_internal_manifest_sha256(&armed)?;
    let stored_bindings = armed.bindings;
    Ok(SurfaceCaptureArmReport {
        schema_version: ARM_RESULT_SCHEMA.to_owned(),
        action: "arm".to_owned(),
        run_id: armed.run_id,
        state: armed.state,
        disposition: armed.disposition,
        surface: surface_from_capture(armed.surface),
        project_id: context.project_id,
        project_identity: armed.project_identity,
        metadata_sha256: armed.metadata_sha256,
        package_launcher_sha256: stored_bindings.package_launcher_sha256,
        project_config_sha256: stored_bindings.project_config_sha256,
        setup_receipt_sha256: stored_bindings.setup_receipt_sha256,
        internal_manifest_sha256: stored_internal_manifest_sha256,
    })
}

fn stored_internal_manifest_sha256(armed: &ArmedLease) -> Result<String, SurfaceCaptureError> {
    let digest = armed
        .metadata_document()
        .pointer("/package/internal_manifest_sha256")
        .and_then(Value::as_str)
        .ok_or(SurfaceCaptureError::MetadataInvalid)?;
    if digest.len() != 71
        || !digest.starts_with("sha256:")
        || !digest[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(SurfaceCaptureError::MetadataInvalid);
    }
    Ok(digest.to_owned())
}

/// Read the state of one exact opaque run id from the installed data root.
pub fn surface_capture_status(
    run_id: &str,
) -> Result<SurfaceCaptureStatusReport, SurfaceCaptureError> {
    validate_run_id(run_id)?;
    let store = installed_store()?;
    let status = store.status(run_id).map_err(map_lease_error)?;
    Ok(SurfaceCaptureStatusReport {
        schema_version: RESULT_SCHEMA.to_owned(),
        action: "status".to_owned(),
        run_id: status.run_id,
        state: status.state,
        surface: status.surface.map(surface_from_capture),
        project_identity: status.project_identity,
    })
}

/// Cancel one exact, unclaimed capture run in the installed data root.
pub fn cancel_surface_capture(
    run_id: &str,
) -> Result<SurfaceCaptureCancelReport, SurfaceCaptureError> {
    validate_run_id(run_id)?;
    let store = installed_store()?;
    store.cancel(run_id).map_err(map_lease_error)?;
    Ok(SurfaceCaptureCancelReport {
        schema_version: RESULT_SCHEMA.to_owned(),
        action: "cancel".to_owned(),
        run_id: run_id.to_owned(),
        state: LeaseState::Absent,
    })
}

/// Consume one exact finalized capture from the installed private data root.
pub fn consume_surface_capture(
    run_id: &str,
    capture_sha256: &str,
) -> Result<SurfaceCaptureConsumeReport, SurfaceCaptureError> {
    validate_run_id(run_id)?;
    validate_capture_digest(capture_sha256)?;
    let store = installed_store()?;
    store
        .consume_finalized(run_id, capture_sha256)
        .map_err(map_lease_error)?;
    Ok(SurfaceCaptureConsumeReport {
        schema_version: RESULT_SCHEMA.to_owned(),
        action: "consume".to_owned(),
        run_id: run_id.to_owned(),
        state: LeaseState::Absent,
    })
}

/// Explicitly abandon one exact inactive bare Claimed or corrupt partial capture.
pub fn abandon_surface_capture(
    run_id: &str,
    metadata_sha256: &str,
) -> Result<SurfaceCaptureAbandonReport, SurfaceCaptureError> {
    validate_run_id(run_id)?;
    validate_metadata_digest(metadata_sha256)?;
    let store = installed_store()?;
    store
        .abandon_claimed(run_id, metadata_sha256)
        .map_err(map_lease_error)?;
    Ok(SurfaceCaptureAbandonReport {
        schema_version: RESULT_SCHEMA.to_owned(),
        action: "abandon".to_owned(),
        run_id: run_id.to_owned(),
        state: LeaseState::Absent,
    })
}

fn verify_surface_capture_project_with(
    project_root: &Path,
    package_directory: Option<&Path>,
    data_root_override: Option<&Path>,
) -> Result<SurfaceCaptureProjectContext, SurfaceCaptureError> {
    let project_root =
        fs::canonicalize(project_root).map_err(|_| SurfaceCaptureError::ProjectInvalid)?;
    require_plain_directory(&project_root, SurfaceCaptureError::ProjectInvalid)?;
    require_plain_file(
        &project_root.join("project.godot"),
        SurfaceCaptureError::ProjectInvalid,
    )?;
    let launcher =
        resolve_launcher(package_directory).map_err(|_| SurfaceCaptureError::PackageInvalid)?;
    let data_root = launcher
        .installed_data_root()
        .or(data_root_override)
        .ok_or(SurfaceCaptureError::PackageInvalid)?;
    let data_root =
        fs::canonicalize(data_root).map_err(|_| SurfaceCaptureError::StoreUnavailable)?;
    LeaseStore::open(&data_root).map_err(map_lease_error)?;
    let project_id = godot_codex_bridge_client::project_id_for_path(&project_root)
        .map_err(|_| SurfaceCaptureError::ProjectInvalid)?;
    let state = verify_surface_capture_setup_state(
        &project_root,
        &project_id,
        &launcher,
        package_directory,
    )
    .map_err(map_capture_setup_error)?;
    let manifest = read_plain_bounded(
        &launcher.package_root().join(PACKAGE_MANIFEST_NAME),
        MAX_MANIFEST_BYTES,
    )
    .map_err(|_| SurfaceCaptureError::PackageInvalid)?;
    let project_identity_sha256 =
        surface_project_identity(&project_root).map_err(|_| SurfaceCaptureError::ProjectInvalid)?;
    Ok(SurfaceCaptureProjectContext {
        project_root,
        data_root,
        project_id,
        project_identity_sha256,
        package_launcher_sha256: launcher.file_digest().to_owned(),
        project_config_sha256: sha256_bytes(&state.config),
        setup_receipt_sha256: sha256_bytes(&state.receipt),
        internal_manifest_sha256: sha256_bytes(&manifest),
    })
}

fn installed_store() -> Result<LeaseStore, SurfaceCaptureError> {
    let launcher = resolve_launcher(None).map_err(|_| SurfaceCaptureError::PackageInvalid)?;
    let data_root = launcher
        .installed_data_root()
        .ok_or(SurfaceCaptureError::PackageInvalid)?;
    LeaseStore::open(data_root).map_err(map_lease_error)
}

fn read_metadata(path: &Path) -> Result<MetadataBinding, SurfaceCaptureError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| SurfaceCaptureError::MetadataInvalid)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_METADATA_BYTES
    {
        return Err(SurfaceCaptureError::MetadataInvalid);
    }
    let bytes = read_plain_bounded(path, MAX_METADATA_BYTES)
        .map_err(|_| SurfaceCaptureError::MetadataInvalid)?;
    let document = serde_json::from_slice::<Value>(&bytes)
        .map_err(|_| SurfaceCaptureError::MetadataInvalid)?;
    let mut canonical =
        serde_json::to_vec(&document).map_err(|_| SurfaceCaptureError::MetadataInvalid)?;
    canonical.push(b'\n');
    if canonical != bytes {
        return Err(SurfaceCaptureError::MetadataInvalid);
    }
    MetadataBinding::from_document(document).map_err(|_| SurfaceCaptureError::MetadataInvalid)
}

fn validate_metadata_context(
    metadata: &Value,
    surface: SurfaceCaptureSurface,
    context: &SurfaceCaptureProjectContext,
) -> Result<(), SurfaceCaptureError> {
    if !SURFACE_METADATA_VALIDATOR
        .as_ref()
        .is_some_and(|validator| validator.is_valid(metadata))
        || metadata_contains_private_string(metadata)
    {
        return Err(SurfaceCaptureError::MetadataInvalid);
    }
    let package = metadata
        .get("package")
        .and_then(Value::as_object)
        .ok_or(SurfaceCaptureError::MetadataInvalid)?;
    let bindings = metadata
        .get("bindings")
        .and_then(Value::as_object)
        .ok_or(SurfaceCaptureError::MetadataInvalid)?;
    let machine = metadata
        .get("machine_bindings")
        .and_then(Value::as_object)
        .ok_or(SurfaceCaptureError::MetadataInvalid)?;
    if metadata.get("schema_version").and_then(Value::as_str) != Some("s11-surface-metadata/1.1")
        || metadata.get("surface").and_then(Value::as_str) != Some(surface.as_str())
        || package.get("version").and_then(Value::as_str) != Some(PRODUCT_VERSION)
        || package
            .get("internal_manifest_sha256")
            .and_then(Value::as_str)
            != Some(context.internal_manifest_sha256())
        || package
            .get("detached_manifest_sha256")
            .and_then(Value::as_str)
            != bindings
                .get("package_manifest_sha256")
                .and_then(Value::as_str)
        || package.get("mcp_binary_sha256").and_then(Value::as_str)
            != Some(context.package_launcher_sha256())
        || bindings.get("mcp_binary_sha256").and_then(Value::as_str)
            != Some(context.package_launcher_sha256())
        || machine
            .get("project_identity_sha256")
            .and_then(Value::as_str)
            != Some(context.project_identity_sha256())
        || machine
            .get("package_launcher_sha256")
            .and_then(Value::as_str)
            != Some(context.package_launcher_sha256())
        || machine.get("project_config_sha256").and_then(Value::as_str)
            != Some(context.project_config_sha256())
        || machine.get("setup_receipt_sha256").and_then(Value::as_str)
            != Some(context.setup_receipt_sha256())
    {
        return Err(SurfaceCaptureError::MetadataInvalid);
    }
    Ok(())
}

fn metadata_contains_private_string(value: &Value) -> bool {
    match value {
        Value::String(value) => {
            contains_absolute_private_path(value)
                || contains_secret_marker(value)
                || contains_account_identity(value)
        }
        Value::Array(values) => values.iter().any(metadata_contains_private_string),
        Value::Object(values) => values.values().any(metadata_contains_private_string),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn contains_absolute_private_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with("~/")
        || value.contains("file://")
        || [
            "/Users",
            "/home",
            "/root",
            "/private",
            "/tmp",
            "/var/folders",
            "/opt",
            "/Applications",
            "/Volumes",
        ]
        .into_iter()
        .any(|prefix| {
            value.match_indices(prefix).any(|(index, _)| {
                let boundary = index == 0
                    || bytes[index - 1].is_ascii_whitespace()
                    || matches!(
                        bytes[index - 1],
                        b'=' | b':' | b'(' | b'[' | b'{' | b'"' | b'\'' | b',' | b';'
                    );
                let end = index + prefix.len();
                boundary && (end == bytes.len() || bytes.get(end) == Some(&b'/'))
            })
        })
        || value
            .as_bytes()
            .windows(3)
            .enumerate()
            .any(|(index, bytes)| {
                (index == 0 || !value.as_bytes()[index - 1].is_ascii_alphanumeric())
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && matches!(bytes[2], b'/' | b'\\')
            })
        || value.contains('\\')
}

fn contains_secret_marker(value: &str) -> bool {
    let lowercase = value.to_ascii_lowercase();
    contains_token_with_prefix(&lowercase, "bearer ", 12, |byte| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
    }) || contains_token_with_prefix(&lowercase, "sk-", 12, |byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
    }) || ["ghp_", "gho_", "ghs_", "ghu_"].into_iter().any(|prefix| {
        contains_token_with_prefix(&lowercase, prefix, 20, |byte| byte.is_ascii_alphanumeric())
    }) || ["s11_secret_", "s11_token_", "s11_proof_", "s11_canary_"]
        .into_iter()
        .any(|prefix| {
            contains_token_with_prefix(&lowercase, prefix, 1, |byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
            })
        })
}

fn contains_account_identity(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.iter().enumerate().any(|(at, byte)| {
        if *byte != b'@' || at == 0 || at + 1 >= bytes.len() {
            return false;
        }
        let local_start = bytes[..at]
            .iter()
            .rposition(|byte| {
                !(byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b'%' | b'+' | b'-'))
            })
            .map_or(0, |index| index + 1);
        if local_start == at {
            return false;
        }
        let domain_end = bytes[at + 1..]
            .iter()
            .position(|byte| !(byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'-')))
            .map_or(bytes.len(), |index| at + 1 + index);
        let domain = &bytes[at + 1..domain_end];
        let Some(dot) = domain.iter().rposition(|byte| *byte == b'.') else {
            return false;
        };
        dot > 0
            && domain.len().saturating_sub(dot + 1) >= 2
            && domain[dot + 1..]
                .iter()
                .all(|byte| byte.is_ascii_alphabetic())
    })
}

fn contains_token_with_prefix(
    value: &str,
    prefix: &str,
    minimum_suffix_bytes: usize,
    allowed: impl Fn(u8) -> bool,
) -> bool {
    value.match_indices(prefix).any(|(index, _)| {
        (index == 0 || !value.as_bytes()[index - 1].is_ascii_alphanumeric())
            && value.as_bytes()[index + prefix.len()..]
                .iter()
                .copied()
                .take_while(|byte| allowed(*byte))
                .count()
                >= minimum_suffix_bytes
    })
}

fn validate_run_id(run_id: &str) -> Result<(), SurfaceCaptureError> {
    if run_id.len() == 64
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(SurfaceCaptureError::RunIdInvalid)
    }
}

fn validate_capture_digest(digest: &str) -> Result<(), SurfaceCaptureError> {
    if digest.len() == 71
        && digest.starts_with("sha256:")
        && digest[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(SurfaceCaptureError::DigestMismatch)
    }
}

fn validate_metadata_digest(digest: &str) -> Result<(), SurfaceCaptureError> {
    if digest.len() == 71
        && digest.starts_with("sha256:")
        && digest[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(SurfaceCaptureError::MetadataDigestMismatch)
    }
}

fn unix_seconds() -> Result<u64, SurfaceCaptureError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| SurfaceCaptureError::TtlInvalid)
}

fn surface_from_capture(surface: Surface) -> SurfaceCaptureSurface {
    match surface {
        Surface::App => SurfaceCaptureSurface::App,
        Surface::Cli => SurfaceCaptureSurface::Cli,
        Surface::Ide => SurfaceCaptureSurface::Ide,
    }
}

fn map_capture_setup_error(error: SetupError) -> SurfaceCaptureError {
    match error {
        SetupError::ConfigInvalid => SurfaceCaptureError::ProjectConfigInvalid,
        SetupError::PackageInvalid | SetupError::PlatformIncompatible => {
            SurfaceCaptureError::PackageInvalid
        }
        SetupError::ReceiptInvalid | SetupError::OwnershipConflict => {
            SurfaceCaptureError::SetupReceiptInvalid
        }
        SetupError::ProjectInvalid => SurfaceCaptureError::ProjectInvalid,
        SetupError::PathUnsafe
        | SetupError::InputTooLarge
        | SetupError::GuidanceConflict
        | SetupError::PlanStoreUnavailable
        | SetupError::PlanLimitReached
        | SetupError::PlanDigestInvalid
        | SetupError::PlanNotFound
        | SetupError::PlanExpired
        | SetupError::PlanInputsChanged
        | SetupError::RepairNotNeeded
        | SetupError::AtomicWriteFailed
        | SetupError::OperationInProgress
        | SetupError::TransactionRecoveryRequired => SurfaceCaptureError::StateInvalid,
    }
}

fn map_lease_error(error: LeaseError) -> SurfaceCaptureError {
    match error {
        LeaseError::InvalidLease => SurfaceCaptureError::StateInvalid,
        LeaseError::InvalidMetadata => SurfaceCaptureError::MetadataInvalid,
        LeaseError::InvalidExpiry => SurfaceCaptureError::TtlInvalid,
        LeaseError::NotFound => SurfaceCaptureError::RunNotFound,
        LeaseError::AlreadyClaimed | LeaseError::AlreadyFinalized => {
            SurfaceCaptureError::AlreadyClaimed
        }
        LeaseError::Expired => SurfaceCaptureError::Expired,
        LeaseError::DigestMismatch => SurfaceCaptureError::DigestMismatch,
        LeaseError::MetadataDigestMismatch => SurfaceCaptureError::MetadataDigestMismatch,
        LeaseError::ClaimActive => SurfaceCaptureError::ClaimActive,
        LeaseError::RecoveryRequired => SurfaceCaptureError::RecoveryRequired,
        LeaseError::ConsumeRequired => SurfaceCaptureError::ConsumeRequired,
        LeaseError::Unsupported => SurfaceCaptureError::Unsupported,
        LeaseError::Io(_) | LeaseError::UnsafeMetadata | LeaseError::RandomUnavailable => {
            SurfaceCaptureError::StoreUnavailable
        }
        LeaseError::BindingMismatch
        | LeaseError::Ambiguous
        | LeaseError::StoreBoundExceeded
        | LeaseError::InvalidJournal
        | LeaseError::NotFinalized
        | LeaseError::StoreBusy => SurfaceCaptureError::StateInvalid,
    }
}

fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn require_plain_directory(
    path: &Path,
    error: SurfaceCaptureError,
) -> Result<(), SurfaceCaptureError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(error);
    }
    Ok(())
}

fn require_plain_file(path: &Path, error: SurfaceCaptureError) -> Result<(), SurfaceCaptureError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(error);
    }
    Ok(())
}

fn require_plain_project_ancestors(
    project_root: &Path,
    relative: &str,
) -> Result<(), SurfaceCaptureError> {
    let relative = Path::new(relative);
    let Some(parent) = relative.parent() else {
        return Err(SurfaceCaptureError::ProjectInvalid);
    };
    let mut current = project_root.to_path_buf();
    for component in parent.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(SurfaceCaptureError::ProjectInvalid);
        };
        current.push(component);
        require_plain_directory(&current, SurfaceCaptureError::ProjectInvalid)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::setup::{SetupOptions, SetupProfile, apply_setup_plan, prepare_setup};

    fn setup_owned_fixture() -> (TempDir, TempDir, TempDir, PathBuf) {
        let project = TempDir::new().unwrap();
        fs::write(project.path().join("project.godot"), "[application]\n").unwrap();
        let package = TempDir::new().unwrap();
        fs::create_dir(package.path().join("bin")).unwrap();
        fs::write(
            package.path().join(PACKAGE_MANIFEST_NAME),
            format!(r#"{{"package_version":"{PRODUCT_VERSION}"}}"#),
        )
        .unwrap();
        let sidecar = package.path().join("bin/godot-codex-mcp");
        let operations = package.path().join("bin/godot-codex");
        fs::write(&sidecar, b"surface-capture-sidecar").unwrap();
        fs::write(&operations, b"surface-capture-operations").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&operations, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let private = TempDir::new().unwrap();
        let data_root = private.path().join("data");
        fs::create_dir(&data_root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&data_root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut setup = SetupOptions::new(project.path(), SetupProfile::ReadOnly);
        setup.package_directory = Some(package.path().to_path_buf());
        setup.plan_store = Some(private.path().join("plans"));
        let preview = prepare_setup(&setup).unwrap();
        apply_setup_plan(&preview.plan_digest, setup.plan_store.as_deref()).unwrap();
        (project, package, private, data_root)
    }

    fn fixture_digest(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }

    fn valid_metadata(context: &SurfaceCaptureProjectContext) -> Value {
        let package_manifest_sha256 = fixture_digest('7');
        serde_json::json!({
            "schema_version": "s11-surface-metadata/1.1",
            "surface": "cli",
            "package": {
                "version": PRODUCT_VERSION,
                "detached_manifest_sha256": package_manifest_sha256,
                "internal_manifest_sha256": context.internal_manifest_sha256(),
                "mcp_binary_sha256": context.package_launcher_sha256(),
            },
            "host": {
                "name": "Codex",
                "identifier": "com.openai.codex",
                "artifact_kind": "macos_application",
                "version": "1.2.3",
                "build": "123",
                "commit": null,
                "client_version": "1.2.3",
                "host_metadata_sha256": null,
                "host_code_signature": null,
                "client_code_signature": null,
                "ide_host_version": null,
                "ide_shell_identifier": null,
                "ide_shell_artifact_sha256": null,
                "ide_shell_team_id": null,
                "ide_shell_code_signature": null,
                "architecture": "arm64",
            },
            "bindings": {
                "package_source_commit": "8".repeat(40),
                "package_manifest_sha256": package_manifest_sha256,
                "compatibility_matrix_sha256": fixture_digest('9'),
                "host_coordinate_profile_sha256": fixture_digest('a'),
                "project_fixture_sha256": fixture_digest('b'),
                "registry_sha256": fixture_digest('c'),
                "prompt_pack_sha256": fixture_digest('d'),
                "host_artifact_sha256": fixture_digest('e'),
                "client_artifact_sha256": fixture_digest('f'),
                "host_provenance_sha256": fixture_digest('0'),
                "mcp_binary_sha256": context.package_launcher_sha256(),
                "godot_artifact_sha256": fixture_digest('1'),
            },
            "machine_bindings": {
                "project_identity_sha256": context.project_identity_sha256(),
                "package_launcher_sha256": context.package_launcher_sha256(),
                "project_config_sha256": context.project_config_sha256(),
                "setup_receipt_sha256": context.setup_receipt_sha256(),
            },
            "registry": {
                "tools": (0..41)
                    .map(|index| format!("godot_tool_{index:02}"))
                    .collect::<Vec<_>>(),
                "fixed_resources": [
                    "godot://one",
                    "godot://two",
                    "godot://three",
                    "godot://four",
                ],
                "resource_templates": ["godot://resource/{id}"],
                "instructions_sha256": fixture_digest('2'),
            },
            "redaction": {
                "absolute_paths_absent": true,
                "account_identity_absent": true,
                "host_controls_absent": true,
                "secrets_absent": true,
            },
        })
    }

    #[test]
    fn run_ids_and_ttls_are_closed() {
        assert!(validate_run_id(&"a".repeat(64)).is_ok());
        for invalid in ["", "A", "../run", &"a".repeat(63), &"g".repeat(64)] {
            assert_eq!(
                validate_run_id(invalid),
                Err(SurfaceCaptureError::RunIdInvalid)
            );
        }
        assert_eq!(MAX_CAPTURE_TTL_SECONDS, 1800);
        for ttl in [0, 1801, u64::MAX] {
            assert_eq!(
                arm_surface_capture(&SurfaceCaptureArmOptions::new(
                    "/unobserved/project",
                    SurfaceCaptureSurface::Cli,
                    "/unobserved/metadata.json",
                    ttl,
                )),
                Err(SurfaceCaptureError::TtlInvalid)
            );
        }
        assert!(validate_capture_digest(&format!("sha256:{}", "a".repeat(64))).is_ok());
        assert!(validate_metadata_digest(&format!("sha256:{}", "b".repeat(64))).is_ok());
        for invalid in [
            "",
            &"a".repeat(64),
            &format!("sha256:{}", "A".repeat(64)),
            &format!("sha256:{}", "a".repeat(63)),
            &format!("sha256:{}", "g".repeat(64)),
        ] {
            assert_eq!(
                validate_capture_digest(invalid),
                Err(SurfaceCaptureError::DigestMismatch)
            );
        }
        assert_eq!(
            map_lease_error(LeaseError::DigestMismatch),
            SurfaceCaptureError::DigestMismatch
        );
        assert_eq!(
            map_lease_error(LeaseError::NotFinalized),
            SurfaceCaptureError::StateInvalid
        );
        assert_eq!(
            map_lease_error(LeaseError::InvalidLease),
            SurfaceCaptureError::StateInvalid
        );
        assert_eq!(
            map_capture_setup_error(SetupError::ConfigInvalid),
            SurfaceCaptureError::ProjectConfigInvalid
        );
        assert_eq!(
            map_capture_setup_error(SetupError::ReceiptInvalid),
            SurfaceCaptureError::SetupReceiptInvalid
        );
        assert_eq!(
            map_capture_setup_error(SetupError::PackageInvalid),
            SurfaceCaptureError::PackageInvalid
        );
        assert_eq!(
            map_lease_error(LeaseError::MetadataDigestMismatch),
            SurfaceCaptureError::MetadataDigestMismatch
        );
        assert_eq!(
            map_lease_error(LeaseError::ClaimActive),
            SurfaceCaptureError::ClaimActive
        );
        assert_eq!(
            map_lease_error(LeaseError::RecoveryRequired),
            SurfaceCaptureError::RecoveryRequired
        );
        assert_eq!(
            map_lease_error(LeaseError::ConsumeRequired),
            SurfaceCaptureError::ConsumeRequired
        );
        assert_eq!(
            map_lease_error(LeaseError::Unsupported),
            SurfaceCaptureError::Unsupported
        );
        let report = SurfaceCaptureConsumeReport {
            schema_version: RESULT_SCHEMA.to_owned(),
            action: "consume".to_owned(),
            run_id: "a".repeat(64),
            state: LeaseState::Absent,
        };
        let rendered = serde_json::to_value(report).unwrap();
        assert_eq!(rendered["action"], "consume");
        assert_eq!(rendered["state"], "absent");
        let abandon = SurfaceCaptureAbandonReport {
            schema_version: RESULT_SCHEMA.to_owned(),
            action: "abandon".to_owned(),
            run_id: "b".repeat(64),
            state: LeaseState::Absent,
        };
        let rendered = serde_json::to_value(abandon).unwrap();
        assert_eq!(rendered["action"], "abandon");
        assert_eq!(rendered["state"], "absent");
    }

    #[test]
    fn project_context_debug_never_contains_paths() {
        let context = SurfaceCaptureProjectContext {
            project_root: PathBuf::from("/private/project"),
            data_root: PathBuf::from("/private/data"),
            project_id: format!("project:sha256:{}", "1".repeat(64)),
            project_identity_sha256: format!("sha256:{}", "6".repeat(64)),
            package_launcher_sha256: format!("sha256:{}", "2".repeat(64)),
            project_config_sha256: format!("sha256:{}", "3".repeat(64)),
            setup_receipt_sha256: format!("sha256:{}", "4".repeat(64)),
            internal_manifest_sha256: format!("sha256:{}", "5".repeat(64)),
        };
        let rendered = format!("{context:?}");
        assert!(!rendered.contains("/private/"));
        assert!(rendered.contains(context.project_id()));
    }

    #[test]
    fn project_context_verifies_and_hashes_exact_setup_owned_state() {
        let (project, package, _private, data_root) = setup_owned_fixture();
        let context = verify_surface_capture_project_with(
            project.path(),
            Some(package.path()),
            Some(&data_root),
        )
        .unwrap();
        assert_eq!(
            context.project_config_sha256(),
            sha256_bytes(&fs::read(project.path().join(".codex/config.toml")).unwrap())
        );
        assert_eq!(
            context.setup_receipt_sha256(),
            sha256_bytes(
                &fs::read(project.path().join(".godot/codex/setup-receipt-v1.json")).unwrap()
            )
        );
        assert_eq!(
            context.internal_manifest_sha256(),
            sha256_bytes(&fs::read(package.path().join(PACKAGE_MANIFEST_NAME)).unwrap())
        );
        assert_eq!(context.data_root(), fs::canonicalize(data_root).unwrap());
        assert_eq!(
            context.canonical_project_root(),
            fs::canonicalize(project.path()).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn claim_context_is_lightweight_and_binds_only_exact_armed_files() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let (project, package, private, data_root) = setup_owned_fixture();
        let versions = data_root.join("versions");
        let version = versions.join(PRODUCT_VERSION);
        let bin = version.join("bin");
        fs::create_dir(&versions).unwrap();
        fs::create_dir(&version).unwrap();
        fs::create_dir(&bin).unwrap();
        let sidecar = bin.join("godot-codex-mcp");
        fs::copy(package.path().join("bin/godot-codex-mcp"), &sidecar).unwrap();
        fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&version, data_root.join("current")).unwrap();

        let context =
            verify_surface_capture_claim_with(project.path(), &data_root, &sidecar).unwrap();
        assert_eq!(
            context.canonical_project_root(),
            fs::canonicalize(project.path()).unwrap()
        );
        assert_eq!(context.data_root(), fs::canonicalize(&data_root).unwrap());
        assert_eq!(
            context.binding_digests().package_launcher_sha256,
            sha256_bytes(&fs::read(&sidecar).unwrap())
        );
        assert_eq!(
            context.binding_digests().project_config_sha256,
            sha256_bytes(&fs::read(project.path().join(CAPTURE_CONFIG_PATH)).unwrap())
        );
        assert_eq!(
            context.binding_digests().setup_receipt_sha256,
            sha256_bytes(&fs::read(project.path().join(CAPTURE_RECEIPT_PATH)).unwrap())
        );

        let wrong = private.path().join("wrong-sidecar");
        fs::write(&wrong, b"wrong").unwrap();
        fs::set_permissions(&wrong, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            verify_surface_capture_claim_with(project.path(), &data_root, &wrong).map(|_| ()),
            Err(SurfaceCaptureError::PackageInvalid)
        );

        let project_file = project.path().join("project.godot");
        let project_target = private.path().join("project.godot.target");
        fs::rename(&project_file, &project_target).unwrap();
        symlink(&project_target, &project_file).unwrap();
        assert_eq!(
            verify_surface_capture_project_with(
                project.path(),
                Some(package.path()),
                Some(&data_root)
            )
            .map(|_| ()),
            Err(SurfaceCaptureError::ProjectInvalid)
        );
        assert_eq!(
            verify_surface_capture_claim_with(project.path(), &data_root, &sidecar).map(|_| ()),
            Err(SurfaceCaptureError::ProjectInvalid)
        );
        fs::remove_file(&project_file).unwrap();
        fs::rename(&project_target, &project_file).unwrap();

        let receipt = project.path().join(CAPTURE_RECEIPT_PATH);
        fs::set_permissions(&receipt, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            verify_surface_capture_claim_with(project.path(), &data_root, &sidecar).map(|_| ()),
            Err(SurfaceCaptureError::SetupReceiptInvalid)
        );
    }

    #[test]
    fn metadata_requires_exact_surface_and_installed_digests() {
        let context = SurfaceCaptureProjectContext {
            project_root: PathBuf::new(),
            data_root: PathBuf::new(),
            project_id: format!("project:sha256:{}", "1".repeat(64)),
            project_identity_sha256: format!("sha256:{}", "6".repeat(64)),
            package_launcher_sha256: format!("sha256:{}", "2".repeat(64)),
            project_config_sha256: format!("sha256:{}", "3".repeat(64)),
            setup_receipt_sha256: format!("sha256:{}", "4".repeat(64)),
            internal_manifest_sha256: format!("sha256:{}", "5".repeat(64)),
        };
        let mut metadata = valid_metadata(&context);
        assert!(validate_metadata_context(&metadata, SurfaceCaptureSurface::Cli, &context).is_ok());
        metadata["surface"] = Value::String("app".to_owned());
        assert_eq!(
            validate_metadata_context(&metadata, SurfaceCaptureSurface::Cli, &context),
            Err(SurfaceCaptureError::MetadataInvalid)
        );
    }

    #[test]
    fn conflict_receipt_uses_the_existing_run_internal_manifest() {
        let project = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(data.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let bindings = BindingDigests {
            package_launcher_sha256: fixture_digest('1'),
            project_config_sha256: fixture_digest('2'),
            setup_receipt_sha256: fixture_digest('3'),
        };
        let metadata = |manifest: &str, surface: &str| {
            MetadataBinding::from_document(serde_json::json!({
                "schema_version": "s11-surface-metadata/1.1",
                "surface": surface,
                "package": {"internal_manifest_sha256": manifest}
            }))
            .unwrap()
        };
        let existing_manifest = fixture_digest('4');
        let requested_manifest = fixture_digest('5');
        let store = LeaseStore::open(data.path()).unwrap();
        let first = store
            .arm(ArmRequest {
                project_root: project.path().to_path_buf(),
                surface: Surface::Cli,
                metadata: metadata(&existing_manifest, "cli"),
                bindings: bindings.clone(),
                expires_at_unix: unix_seconds().unwrap() + 300,
            })
            .unwrap();
        let conflict = store
            .arm(ArmRequest {
                project_root: project.path().to_path_buf(),
                surface: Surface::App,
                metadata: metadata(&requested_manifest, "app"),
                bindings,
                expires_at_unix: unix_seconds().unwrap() + 300,
            })
            .unwrap();

        assert_eq!(conflict.run_id, first.run_id);
        assert_eq!(conflict.disposition, ArmDisposition::Conflict);
        assert_eq!(
            stored_internal_manifest_sha256(&conflict).unwrap(),
            existing_manifest
        );
        assert_ne!(
            stored_internal_manifest_sha256(&conflict).unwrap(),
            requested_manifest
        );
    }

    #[test]
    fn metadata_requires_the_complete_closed_v1_1_schema() {
        let context = SurfaceCaptureProjectContext {
            project_root: PathBuf::new(),
            data_root: PathBuf::new(),
            project_id: format!("project:sha256:{}", "1".repeat(64)),
            project_identity_sha256: fixture_digest('6'),
            package_launcher_sha256: fixture_digest('2'),
            project_config_sha256: fixture_digest('3'),
            setup_receipt_sha256: fixture_digest('4'),
            internal_manifest_sha256: fixture_digest('5'),
        };
        let metadata = valid_metadata(&context);

        for missing in ["host", "registry", "redaction"] {
            let mut invalid = metadata.clone();
            invalid.as_object_mut().unwrap().remove(missing);
            assert_eq!(
                validate_metadata_context(&invalid, SurfaceCaptureSurface::Cli, &context),
                Err(SurfaceCaptureError::MetadataInvalid),
                "missing {missing} was accepted"
            );
        }

        let mut unknown = metadata.clone();
        unknown["unexpected"] = Value::Bool(true);
        assert_eq!(
            validate_metadata_context(&unknown, SurfaceCaptureSurface::Cli, &context),
            Err(SurfaceCaptureError::MetadataInvalid)
        );

        let mut nested_unknown = metadata.clone();
        nested_unknown["host"]["unexpected"] = Value::Bool(true);
        assert_eq!(
            validate_metadata_context(&nested_unknown, SurfaceCaptureSurface::Cli, &context),
            Err(SurfaceCaptureError::MetadataInvalid)
        );

        let mut false_redaction = metadata.clone();
        false_redaction["redaction"]["secrets_absent"] = Value::Bool(false);
        assert_eq!(
            validate_metadata_context(&false_redaction, SurfaceCaptureSurface::Cli, &context),
            Err(SurfaceCaptureError::MetadataInvalid)
        );

        let mut detached_mismatch = metadata;
        detached_mismatch["package"]["detached_manifest_sha256"] =
            Value::String(fixture_digest('3'));
        assert_eq!(
            validate_metadata_context(&detached_mismatch, SurfaceCaptureSurface::Cli, &context),
            Err(SurfaceCaptureError::MetadataInvalid)
        );
    }

    #[test]
    fn metadata_redaction_flags_cannot_hide_private_paths_or_secrets() {
        let context = SurfaceCaptureProjectContext {
            project_root: PathBuf::new(),
            data_root: PathBuf::new(),
            project_id: format!("project:sha256:{}", "1".repeat(64)),
            project_identity_sha256: fixture_digest('6'),
            package_launcher_sha256: fixture_digest('2'),
            project_config_sha256: fixture_digest('3'),
            setup_receipt_sha256: fixture_digest('4'),
            internal_manifest_sha256: fixture_digest('5'),
        };
        let metadata = valid_metadata(&context);

        for leaked_value in [
            "/Users/alice/private-host",
            "/root/private-host",
            "/Applications/ChatGPT.app",
            "path=/private/alice/private-host",
            "file:///Users/alice/private-host",
            "C:\\Users\\alice\\private-host",
            r"path=\Users\alice\private-host",
            "artifact=file:///C:/Users/alice/private-host",
            r"\\private-server\account\host",
            "Bearer abcdefghijklmnop",
            "sk-proj-abcdefghijklmnop",
            "READY:SK-PROJ-ABCDEFGHIJKLMNOP",
            "ghp_abcdefghijklmnopqrst",
            "S11_CANARY_not_public",
            "alice@example.com",
        ] {
            let mut invalid = metadata.clone();
            invalid["host"]["name"] = Value::String(leaked_value.to_owned());
            assert_eq!(
                validate_metadata_context(&invalid, SurfaceCaptureSurface::Cli, &context),
                Err(SurfaceCaptureError::MetadataInvalid),
                "private value was accepted: {leaked_value}"
            );
        }

        let mut system_path = metadata;
        system_path["host"]["name"] = Value::String("/usr/bin/shasum".to_owned());
        assert!(
            validate_metadata_context(&system_path, SurfaceCaptureSurface::Cli, &context).is_ok()
        );
    }

    #[test]
    fn metadata_file_must_be_bounded_canonical_json_with_one_lf() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("metadata.json");
        fs::write(&path, b"{\"schema_version\":\"fixture\"}\n").unwrap();
        let binding = read_metadata(&path).unwrap();
        assert_eq!(
            binding.sha256,
            sha256_bytes(b"{\"schema_version\":\"fixture\"}\n")
        );

        fs::write(&path, b"{ \"schema_version\": \"fixture\" }\n").unwrap();
        assert_eq!(
            read_metadata(&path),
            Err(SurfaceCaptureError::MetadataInvalid)
        );
        fs::write(&path, b"{\"schema_version\":\"fixture\"}").unwrap();
        assert_eq!(
            read_metadata(&path),
            Err(SurfaceCaptureError::MetadataInvalid)
        );
    }

    #[cfg(unix)]
    #[test]
    fn metadata_file_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let actual = temp.path().join("actual.json");
        let alias = temp.path().join("metadata.json");
        fs::write(&actual, b"{}\n").unwrap();
        symlink(&actual, &alias).unwrap();
        assert_eq!(
            read_metadata(&alias),
            Err(SurfaceCaptureError::MetadataInvalid)
        );
    }
}
