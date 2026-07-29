use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use godot_codex_bridge_client::project_id_for_path;
use godot_codex_product::{
    FULL_BETA_TOOLS, READ_ONLY_TOOLS, canonical_registry_profile, embedded_compatibility_matrix,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use toml_edit::{DocumentMut, Item, Table};

use crate::PRODUCT_VERSION;
use crate::launcher::{LauncherResolution, resolve_launcher};

const PLAN_SCHEMA: &str = "godot-codex-setup-plan/1.3";
const RECEIPT_SCHEMA: &str = "godot-codex-setup-receipt/1.1";
const JOURNAL_SCHEMA: &str = "godot-codex-setup-journal/1.0";
const PLAN_LIFETIME_SECONDS: u64 = 10 * 60;
const CLAIM_GC_GRACE_SECONDS: u64 = 10 * 60;
const MAX_STORED_PLANS: usize = 32;
const MAX_PLAN_BYTES: u64 = 512 * 1024;
const MAX_JOURNAL_BYTES: u64 = 2 * 1024 * 1024;
const MAX_OPERATIONS_BINARY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MAX_GUIDANCE_BYTES: u64 = 256 * 1024;
const MAX_RECEIPT_BYTES: u64 = 64 * 1024;
const MAX_DIFF_BYTES: usize = 64 * 1024;
const CONFIG_PATH: &str = ".codex/config.toml";
const RECEIPT_PATH: &str = ".godot/codex/setup-receipt-v1.json";
const JOURNAL_PATH: &str = ".godot/codex/setup-journal-v1.json";
const OPERATION_LOCK_PATH: &str = ".godot/codex/setup-operation-v1.lock";
const AGENTS_PATH: &str = "AGENTS.md";
const SKILL_PATH: &str = ".agents/skills/godot-editor/SKILL.md";
const AGENTS_BEGIN: &str = "<!-- BEGIN GODOT CODEX SETUP v1 -->";
const AGENTS_END: &str = "<!-- END GODOT CODEX SETUP v1 -->";
const CONFIG_OWNERSHIP_PREFIX: &str = "# godot-codex-setup-owner: ";
const MISSING_DIGEST: &str = "sha256:missing";

const CANONICAL_AGENTS_GUIDANCE: &str =
    include_str!("../../../../docs/codex-integration/templates/AGENTS.godot.md");
const CANONICAL_SKILL: &str = include_str!("../../../../.agents/skills/godot-editor/SKILL.md");

/// Exact tool profile written into project-scoped Codex configuration.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SetupProfile {
    ReadOnly,
    FullBeta,
}

impl fmt::Display for SetupProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ReadOnly => "read-only",
            Self::FullBeta => "full-beta",
        })
    }
}

/// Closed operation kind bound into every immutable setup plan.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupMode {
    Configure,
    Repair,
    Remove,
}

impl fmt::Display for SetupMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configure => "configure",
            Self::Repair => "repair",
            Self::Remove => "remove",
        })
    }
}

/// Optional package-owned guidance artifacts.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidanceMode {
    None,
    Agents,
    Skill,
    All,
}

impl GuidanceMode {
    fn agents(self) -> bool {
        matches!(self, Self::Agents | Self::All)
    }

    fn skill(self) -> bool {
        matches!(self, Self::Skill | Self::All)
    }
}

/// Inputs for immutable setup planning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupOptions {
    pub project_root: PathBuf,
    pub profile: SetupProfile,
    pub guidance: GuidanceMode,
    /// Private store override used by installers and tests. The CLI otherwise
    /// uses [`default_plan_store`].
    pub plan_store: Option<PathBuf>,
    /// Optional package root override used by installed-package acceptance
    /// and tests. The root must contain `package-manifest.json` and `bin/`.
    pub package_directory: Option<PathBuf>,
}

impl SetupOptions {
    #[must_use]
    pub fn new(project_root: impl Into<PathBuf>, profile: SetupProfile) -> Self {
        Self {
            project_root: project_root.into(),
            profile,
            guidance: GuidanceMode::None,
            plan_store: None,
            package_directory: None,
        }
    }
}

/// Inputs for a receipt-proven project configuration repair.
///
/// Profile and guidance are intentionally absent: repair derives both from
/// the valid private receipt written by an earlier successful setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepairOptions {
    pub project_root: PathBuf,
    /// Private store override used by installers and tests. The CLI otherwise
    /// uses [`default_plan_store`].
    pub plan_store: Option<PathBuf>,
    /// Optional package root override used by installed-package acceptance
    /// and tests.
    pub package_directory: Option<PathBuf>,
}

impl RepairOptions {
    #[must_use]
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            plan_store: None,
            package_directory: None,
        }
    }
}

/// Inputs for receipt-proven removal planning.
///
/// Removal has no profile or guidance choices: both are bound to the valid
/// private receipt written by the setup plan being removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoveOptions {
    pub project_root: PathBuf,
    /// Private store override used by installers and tests. The CLI otherwise
    /// uses [`default_plan_store`].
    pub plan_store: Option<PathBuf>,
    /// Optional package root override used by installed-package acceptance
    /// and tests.
    pub package_directory: Option<PathBuf>,
}

impl RemoveOptions {
    #[must_use]
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            plan_store: None,
            package_directory: None,
        }
    }
}

/// File operation represented by a setup preview.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupAction {
    Create,
    Update,
    Delete,
}

/// One bounded, project-relative setup change.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetupChange {
    pub path: String,
    pub action: SetupAction,
    pub before_digest: String,
    pub after_digest: String,
    pub diff: String,
}

/// Safe setup preview returned before any project file is changed.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetupPreview {
    pub schema_version: String,
    pub mode: SetupMode,
    pub plan_digest: String,
    pub expires_in_seconds: u64,
    pub package_version: String,
    pub profile: SetupProfile,
    pub guidance: GuidanceMode,
    pub restart_required: bool,
    pub project_trust_unchanged: bool,
    pub changes: Vec<SetupChange>,
}

/// Result after a digest-bound setup plan is applied.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetupReport {
    pub schema_version: String,
    pub mode: SetupMode,
    pub plan_digest: String,
    pub status: String,
    pub profile: SetupProfile,
    pub guidance: GuidanceMode,
    pub changed_paths: Vec<String>,
    pub restart_required: bool,
}

/// Closed setup failures. Error text never includes project roots, file
/// contents, tokens, or host configuration values.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SetupError {
    #[error("project_invalid")]
    ProjectInvalid,
    #[error("platform_incompatible")]
    PlatformIncompatible,
    #[error("package_invalid")]
    PackageInvalid,
    #[error("path_unsafe")]
    PathUnsafe,
    #[error("input_too_large")]
    InputTooLarge,
    #[error("config_invalid")]
    ConfigInvalid,
    #[error("ownership_conflict")]
    OwnershipConflict,
    #[error("guidance_conflict")]
    GuidanceConflict,
    #[error("plan_store_unavailable")]
    PlanStoreUnavailable,
    #[error("plan_limit_reached")]
    PlanLimitReached,
    #[error("plan_digest_invalid")]
    PlanDigestInvalid,
    #[error("plan_not_found")]
    PlanNotFound,
    #[error("plan_expired")]
    PlanExpired,
    #[error("plan_inputs_changed")]
    PlanInputsChanged,
    #[error("receipt_invalid")]
    ReceiptInvalid,
    #[error("repair_not_needed")]
    RepairNotNeeded,
    #[error("atomic_write_failed")]
    AtomicWriteFailed,
    #[error("operation_in_progress")]
    OperationInProgress,
    #[error("transaction_recovery_required")]
    TransactionRecoveryRequired,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct FileState {
    exists: bool,
    digest: String,
    mode: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct PlannedChange {
    path: String,
    before: FileState,
    desired: Option<String>,
    display_before: String,
    display_after: String,
    private: bool,
    desired_mode: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct GuardedInput {
    path: String,
    state: FileState,
    private: bool,
}

type PlannedAgents = (Vec<PlannedChange>, Option<String>, Option<String>);

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ReceiptTemplate {
    schema_version: String,
    project_id: String,
    package_version: String,
    profile: SetupProfile,
    guidance: GuidanceMode,
    package_identity_digest: String,
    launcher_path_sha256: String,
    launcher_file_sha256: String,
    config_ownership_marker: String,
    config_table_digest: String,
    config_created: bool,
    config_file_mode: u32,
    agents_block_digest: Option<String>,
    agents_separator: Option<String>,
    agents_file_mode: Option<u32>,
    skill_file_digest: Option<String>,
    skill_file_mode: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct SetupReceipt {
    schema_version: String,
    project_id: String,
    package_version: String,
    profile: SetupProfile,
    guidance: GuidanceMode,
    package_identity_digest: String,
    launcher_path_sha256: String,
    launcher_file_sha256: String,
    config_ownership_marker: String,
    config_table_digest: String,
    config_created: bool,
    config_file_mode: u32,
    agents_block_digest: Option<String>,
    agents_separator: Option<String>,
    agents_file_mode: Option<u32>,
    skill_file_digest: Option<String>,
    skill_file_mode: Option<u32>,
    plan_digest: String,
}

impl ReceiptTemplate {
    fn from_receipt(receipt: &SetupReceipt) -> Self {
        Self {
            schema_version: receipt.schema_version.clone(),
            project_id: receipt.project_id.clone(),
            package_version: receipt.package_version.clone(),
            profile: receipt.profile,
            guidance: receipt.guidance,
            package_identity_digest: receipt.package_identity_digest.clone(),
            launcher_path_sha256: receipt.launcher_path_sha256.clone(),
            launcher_file_sha256: receipt.launcher_file_sha256.clone(),
            config_ownership_marker: receipt.config_ownership_marker.clone(),
            config_table_digest: receipt.config_table_digest.clone(),
            config_created: receipt.config_created,
            config_file_mode: receipt.config_file_mode,
            agents_block_digest: receipt.agents_block_digest.clone(),
            agents_separator: receipt.agents_separator.clone(),
            agents_file_mode: receipt.agents_file_mode,
            skill_file_digest: receipt.skill_file_digest.clone(),
            skill_file_mode: receipt.skill_file_mode,
        }
    }

    fn has_valid_ownership_metadata(&self, project_id: &str) -> bool {
        self.schema_version == RECEIPT_SCHEMA
            && self.project_id == project_id
            && historical_package_version_is_supported(&self.package_version)
            && parse_digest(&self.package_identity_digest).is_ok()
            && parse_digest(&self.launcher_path_sha256).is_ok()
            && parse_digest(&self.launcher_file_sha256).is_ok()
            && parse_digest(&self.config_ownership_marker).is_ok()
            && parse_digest(&self.config_table_digest).is_ok()
            && self
                .agents_block_digest
                .as_deref()
                .is_none_or(|digest| parse_digest(digest).is_ok())
            && self
                .skill_file_digest
                .as_deref()
                .is_none_or(|digest| parse_digest(digest).is_ok())
            && valid_file_mode(self.config_file_mode)
            && self.agents_block_digest.is_some() == self.agents_separator.is_some()
            && self.agents_block_digest.is_some() == self.agents_file_mode.is_some()
            && self.skill_file_digest.is_some() == self.skill_file_mode.is_some()
            && self.agents_file_mode.is_none_or(valid_file_mode)
            && self.skill_file_mode.is_none_or(valid_file_mode)
            && self
                .agents_separator
                .as_deref()
                .is_none_or(|value| matches!(value, "" | "\n"))
    }
}

impl SetupReceipt {
    fn from_template(template: &ReceiptTemplate, plan_digest: &str) -> Self {
        Self {
            schema_version: template.schema_version.clone(),
            project_id: template.project_id.clone(),
            package_version: template.package_version.clone(),
            profile: template.profile,
            guidance: template.guidance,
            package_identity_digest: template.package_identity_digest.clone(),
            launcher_path_sha256: template.launcher_path_sha256.clone(),
            launcher_file_sha256: template.launcher_file_sha256.clone(),
            config_ownership_marker: template.config_ownership_marker.clone(),
            config_table_digest: template.config_table_digest.clone(),
            config_created: template.config_created,
            config_file_mode: template.config_file_mode,
            agents_block_digest: template.agents_block_digest.clone(),
            agents_separator: template.agents_separator.clone(),
            agents_file_mode: template.agents_file_mode,
            skill_file_digest: template.skill_file_digest.clone(),
            skill_file_mode: template.skill_file_mode,
            plan_digest: plan_digest.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct SetupPlan {
    schema_version: String,
    mode: SetupMode,
    created_at_epoch_seconds: u64,
    expires_at_epoch_seconds: u64,
    project_root: PathBuf,
    project_root_identity: DirectoryIdentity,
    project_id: String,
    package_version: String,
    package_identity_digest: String,
    package_directory: Option<PathBuf>,
    launcher_path_sha256: String,
    launcher_file_sha256: String,
    compatibility_matrix_digest: String,
    registry_digest: String,
    profile: SetupProfile,
    guidance: GuidanceMode,
    receipt_before: FileState,
    receipt_template: ReceiptTemplate,
    changes: Vec<PlannedChange>,
    guarded_inputs: Vec<GuardedInput>,
}

/// An exact setup or repair preview that is not actionable until the caller
/// explicitly accepts it. Dropping this value leaves no reusable plan in the
/// per-user plan store.
pub struct PendingSetup {
    plan_store: PathBuf,
    plan: SetupPlan,
    digest: String,
    preview: SetupPreview,
}

impl PendingSetup {
    /// Returns the exact redacted preview the caller must present for consent.
    #[must_use]
    pub fn preview(&self) -> &SetupPreview {
        &self.preview
    }

    /// Publishes and applies this exact plan only after the caller obtained an
    /// affirmative interactive decision.
    pub fn apply(self) -> Result<SetupReport, SetupError> {
        inspect_pending_transaction(&self.plan.project_root, &self.plan, &self.digest)?;
        preflight_plan_inputs(&self.plan)?;
        prepare_plan_store(&self.plan_store)?;
        persist_plan(&self.plan_store, &self.digest, &self.plan)?;
        apply_setup_plan(&self.digest, Some(&self.plan_store))
    }

    fn persist_preview(self) -> Result<SetupPreview, SetupError> {
        inspect_pending_transaction(&self.plan.project_root, &self.plan, &self.digest)?;
        preflight_plan_inputs(&self.plan)?;
        prepare_plan_store(&self.plan_store)?;
        persist_plan(&self.plan_store, &self.digest, &self.plan)?;
        Ok(self.preview)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum JournalOperation {
    Apply,
    Remove,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalFile {
    path: String,
    before: Option<String>,
    before_mode: Option<u32>,
    after: Option<String>,
    after_mode: Option<u32>,
    commit_marker: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct SetupJournal {
    schema_version: String,
    operation: JournalOperation,
    project_id: String,
    plan_digest: Option<String>,
    files: Vec<JournalFile>,
}

/// Returns the private per-user plan store. This location is outside projects,
/// so a preview does not itself edit the selected project.
pub fn default_plan_store() -> Result<PathBuf, SetupError> {
    if cfg!(target_os = "macos") {
        return std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|home| home.join("Library/Caches/GodotCodex/setup-plans"))
            .ok_or(SetupError::PlanStoreUnavailable);
    }
    if let Some(state) = std::env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(state).join("godot-codex/setup-plans"));
    }
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|base| base.join("GodotCodex/setup-plans"))
            .ok_or(SetupError::PlanStoreUnavailable);
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".local/state/godot-codex/setup-plans"))
        .ok_or(SetupError::PlanStoreUnavailable)
}

/// Computes and persists a short-lived immutable plan without changing any
/// selected-project file.
pub fn prepare_setup(options: &SetupOptions) -> Result<SetupPreview, SetupError> {
    prepare_setup_for_consent(options)?.persist_preview()
}

/// Computes an exact preview without publishing an actionable plan. This is
/// used by the interactive default-no flow.
pub fn prepare_setup_for_consent(options: &SetupOptions) -> Result<PendingSetup, SetupError> {
    let project_root = canonical_project(&options.project_root)?;
    reject_pending_transaction_during_prepare(&project_root)?;
    verify_supported_target()?;
    let project_id = project_id_for_path(&project_root).map_err(|_| SetupError::ProjectInvalid)?;
    let plan_store = options
        .plan_store
        .clone()
        .map_or_else(default_plan_store, Ok)?;

    let receipt_path = project_root.join(RECEIPT_PATH);
    let receipt_before = file_state(&receipt_path, MAX_RECEIPT_BYTES)?;
    let previous_receipt = if receipt_before.exists {
        Some(read_receipt(&receipt_path)?)
    } else {
        None
    };
    if let Some(receipt) = &previous_receipt {
        verify_owned_state(&project_root, &project_id, receipt)?;
    }

    let package_directory = options
        .package_directory
        .as_deref()
        .map(fs::canonicalize)
        .transpose()
        .map_err(|_| SetupError::PackageInvalid)?;
    let launcher =
        resolve_launcher(package_directory.as_deref()).map_err(|_| SetupError::PackageInvalid)?;
    let package_identity_digest = current_package_identity_digest(package_directory.as_deref())?;
    let (config_change, config_ownership_marker, config_table_digest, config_created) =
        plan_config(
            &project_root,
            options.profile,
            previous_receipt.as_ref(),
            &launcher,
        )?;
    let (agents_change, agents_block_digest, agents_separator) = plan_agents(
        &project_root,
        options.guidance.agents(),
        previous_receipt.as_ref(),
    )?;
    let (skill_change, skill_file_digest) = plan_skill(
        &project_root,
        options.guidance.skill(),
        previous_receipt.as_ref(),
    )?;
    let agents_file_mode = agents_change
        .iter()
        .find(|change| change.desired.is_some())
        .map(|change| change.desired_mode);
    let skill_file_mode = skill_change
        .iter()
        .find(|change| change.desired.is_some())
        .map(|change| change.desired_mode);

    let mut changes = vec![config_change];
    changes.extend(agents_change);
    changes.extend(skill_change);
    changes.sort_by(|left, right| left.path.cmp(&right.path));

    let matrix = embedded_compatibility_matrix().map_err(|_| SetupError::PlatformIncompatible)?;
    let registry = canonical_registry_profile();
    let created_at = now_epoch_seconds()?;
    let project_root_identity = directory_identity(&project_root)?;
    let plan = SetupPlan {
        schema_version: PLAN_SCHEMA.to_owned(),
        mode: SetupMode::Configure,
        created_at_epoch_seconds: created_at,
        expires_at_epoch_seconds: created_at + PLAN_LIFETIME_SECONDS,
        project_root,
        project_root_identity,
        project_id: project_id.clone(),
        package_version: PRODUCT_VERSION.to_owned(),
        package_identity_digest: package_identity_digest.clone(),
        package_directory,
        launcher_path_sha256: launcher.path_digest().to_owned(),
        launcher_file_sha256: launcher.file_digest().to_owned(),
        compatibility_matrix_digest: matrix
            .canonical_digest()
            .map_err(|_| SetupError::PlatformIncompatible)?,
        registry_digest: registry.digest,
        profile: options.profile,
        guidance: options.guidance,
        receipt_before,
        receipt_template: ReceiptTemplate {
            schema_version: RECEIPT_SCHEMA.to_owned(),
            project_id,
            package_version: PRODUCT_VERSION.to_owned(),
            profile: options.profile,
            guidance: options.guidance,
            package_identity_digest,
            launcher_path_sha256: launcher.path_digest().to_owned(),
            launcher_file_sha256: launcher.file_digest().to_owned(),
            config_ownership_marker,
            config_table_digest,
            config_created,
            config_file_mode: changes
                .iter()
                .find(|change| change.path == CONFIG_PATH)
                .map(|change| change.desired_mode)
                .ok_or(SetupError::PlanDigestInvalid)?,
            agents_block_digest,
            agents_separator,
            agents_file_mode,
            skill_file_digest,
            skill_file_mode,
        },
        changes,
        guarded_inputs: Vec::new(),
    };
    let digest = plan_digest(&plan)?;
    preflight_plan_inputs(&plan)?;
    let preview = preview(&plan, &digest)?;
    Ok(PendingSetup {
        plan_store,
        plan,
        digest,
        preview,
    })
}

/// Computes a short-lived repair plan for an exact setup-owned MCP stanza.
///
/// Repair is deliberately narrower than setup: a valid private receipt must
/// prove the previous ownership, package, launcher, profile, and guidance.
/// The current config must exist and parse as TOML. Only the
/// `mcp_servers.godot_editor` item and the receipt may change.
pub fn prepare_setup_repair(options: &RepairOptions) -> Result<SetupPreview, SetupError> {
    prepare_setup_repair_for_consent(options)?.persist_preview()
}

/// Computes an exact repair preview without publishing an actionable plan.
pub fn prepare_setup_repair_for_consent(
    options: &RepairOptions,
) -> Result<PendingSetup, SetupError> {
    let project_root = canonical_project(&options.project_root)?;
    reject_pending_transaction_during_prepare(&project_root)?;
    verify_supported_target()?;
    let project_id = project_id_for_path(&project_root).map_err(|_| SetupError::ProjectInvalid)?;
    let plan_store = options
        .plan_store
        .clone()
        .map_or_else(default_plan_store, Ok)?;

    let receipt_path = project_root.join(RECEIPT_PATH);
    validate_safe_parent(&project_root, &receipt_path, true)?;
    let receipt_before = file_state(&receipt_path, MAX_RECEIPT_BYTES)?;
    if !receipt_before.exists {
        return Err(SetupError::ReceiptInvalid);
    }
    let receipt = read_receipt(&receipt_path)?;
    validate_receipt_metadata(&project_id, &receipt)?;
    verify_owned_guidance_state(&project_root, &receipt)?;
    let guarded_inputs = owned_guidance_guards(&project_root, &receipt)?;

    let package_directory = options
        .package_directory
        .as_deref()
        .map(fs::canonicalize)
        .transpose()
        .map_err(|_| SetupError::PackageInvalid)?;
    let launcher =
        resolve_launcher(package_directory.as_deref()).map_err(|_| SetupError::PackageInvalid)?;
    let package_identity_digest = current_package_identity_digest(package_directory.as_deref())?;
    let package_upgrade =
        !receipt_matches_current_package(&receipt, &launcher, &package_identity_digest);
    let (config_change, config_table_digest) =
        plan_repair_config(&project_root, &receipt, &launcher, package_upgrade)?;
    let config_file_mode = config_change
        .before
        .mode
        .filter(|mode| valid_file_mode(*mode))
        .ok_or(SetupError::ConfigInvalid)?;
    let matrix = embedded_compatibility_matrix().map_err(|_| SetupError::PlatformIncompatible)?;
    let registry = canonical_registry_profile();
    let created_at = now_epoch_seconds()?;
    let project_root_identity = directory_identity(&project_root)?;
    let plan = SetupPlan {
        schema_version: PLAN_SCHEMA.to_owned(),
        mode: SetupMode::Repair,
        created_at_epoch_seconds: created_at,
        expires_at_epoch_seconds: created_at + PLAN_LIFETIME_SECONDS,
        project_root,
        project_root_identity,
        project_id: project_id.clone(),
        package_version: PRODUCT_VERSION.to_owned(),
        package_identity_digest: package_identity_digest.clone(),
        package_directory,
        launcher_path_sha256: launcher.path_digest().to_owned(),
        launcher_file_sha256: launcher.file_digest().to_owned(),
        compatibility_matrix_digest: matrix
            .canonical_digest()
            .map_err(|_| SetupError::PlatformIncompatible)?,
        registry_digest: registry.digest,
        profile: receipt.profile,
        guidance: receipt.guidance,
        receipt_before,
        receipt_template: ReceiptTemplate {
            schema_version: RECEIPT_SCHEMA.to_owned(),
            project_id,
            package_version: PRODUCT_VERSION.to_owned(),
            profile: receipt.profile,
            guidance: receipt.guidance,
            package_identity_digest,
            launcher_path_sha256: launcher.path_digest().to_owned(),
            launcher_file_sha256: launcher.file_digest().to_owned(),
            config_ownership_marker: receipt.config_ownership_marker,
            config_table_digest,
            config_created: receipt.config_created,
            config_file_mode,
            agents_block_digest: receipt.agents_block_digest,
            agents_separator: receipt.agents_separator,
            agents_file_mode: receipt.agents_file_mode,
            skill_file_digest: receipt.skill_file_digest,
            skill_file_mode: receipt.skill_file_mode,
        },
        changes: vec![config_change],
        guarded_inputs,
    };
    let digest = plan_digest(&plan)?;
    preflight_plan_inputs(&plan)?;
    let preview = preview(&plan, &digest)?;
    Ok(PendingSetup {
        plan_store,
        plan,
        digest,
        preview,
    })
}

/// Computes and persists a short-lived plan that removes only exact
/// receipt-owned setup content.
pub fn prepare_setup_remove(options: &RemoveOptions) -> Result<SetupPreview, SetupError> {
    prepare_setup_remove_for_consent(options)?.persist_preview()
}

/// Computes an exact removal preview without publishing actionable apply
/// authority. This is used by the interactive default-no flow.
pub fn prepare_setup_remove_for_consent(
    options: &RemoveOptions,
) -> Result<PendingSetup, SetupError> {
    let project_root = canonical_project(&options.project_root)?;
    reject_pending_transaction_during_prepare(&project_root)?;
    verify_supported_target()?;
    let project_id = project_id_for_path(&project_root).map_err(|_| SetupError::ProjectInvalid)?;
    let plan_store = options
        .plan_store
        .clone()
        .map_or_else(default_plan_store, Ok)?;

    let receipt_path = project_root.join(RECEIPT_PATH);
    validate_safe_parent(&project_root, &receipt_path, true)?;
    let receipt_before = file_state(&receipt_path, MAX_RECEIPT_BYTES)?;
    if !receipt_before.exists {
        return Err(SetupError::ReceiptInvalid);
    }
    let receipt = read_receipt(&receipt_path)?;
    verify_owned_state(&project_root, &project_id, &receipt)?;

    let package_directory = options
        .package_directory
        .as_deref()
        .map(fs::canonicalize)
        .transpose()
        .map_err(|_| SetupError::PackageInvalid)?;
    let launcher =
        resolve_launcher(package_directory.as_deref()).map_err(|_| SetupError::PackageInvalid)?;
    let package_identity_digest = current_package_identity_digest(package_directory.as_deref())?;

    let changes = plan_remove_changes(&project_root, &receipt)?;
    let matrix = embedded_compatibility_matrix().map_err(|_| SetupError::PlatformIncompatible)?;
    let registry = canonical_registry_profile();
    let created_at = now_epoch_seconds()?;
    let plan = SetupPlan {
        schema_version: PLAN_SCHEMA.to_owned(),
        mode: SetupMode::Remove,
        created_at_epoch_seconds: created_at,
        expires_at_epoch_seconds: created_at + PLAN_LIFETIME_SECONDS,
        project_root: project_root.clone(),
        project_root_identity: directory_identity(&project_root)?,
        project_id,
        package_version: PRODUCT_VERSION.to_owned(),
        package_identity_digest,
        package_directory,
        launcher_path_sha256: launcher.path_digest().to_owned(),
        launcher_file_sha256: launcher.file_digest().to_owned(),
        compatibility_matrix_digest: matrix
            .canonical_digest()
            .map_err(|_| SetupError::PlatformIncompatible)?,
        registry_digest: registry.digest,
        profile: receipt.profile,
        guidance: receipt.guidance,
        receipt_before,
        receipt_template: ReceiptTemplate::from_receipt(&receipt),
        changes,
        guarded_inputs: Vec::new(),
    };
    let digest = plan_digest(&plan)?;
    preflight_plan_inputs(&plan)?;
    let preview = preview(&plan, &digest)?;
    Ok(PendingSetup {
        plan_store,
        plan,
        digest,
        preview,
    })
}

/// Applies exactly one unexpired plan selected by its full `sha256:` digest.
pub fn apply_setup_plan(
    digest: &str,
    plan_store_override: Option<&Path>,
) -> Result<SetupReport, SetupError> {
    let digest_hex = parse_digest(digest)?;
    let plan_store = plan_store_override
        .map(Path::to_path_buf)
        .map_or_else(default_plan_store, Ok)?;
    if matches!(
        fs::symlink_metadata(&plan_store),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    ) {
        return Err(SetupError::PlanNotFound);
    }
    validate_plan_store(&plan_store)?;
    let claim = claim_plan(&plan_store, digest_hex)?;
    let plan = &claim.plan;
    if plan.schema_version != PLAN_SCHEMA || plan_digest(plan)? != digest {
        claim.invalidate_nonfatal();
        return Err(SetupError::PlanDigestInvalid);
    }
    let result = (|| {
        verify_project_binding(plan)?;
        // Reject a journal that is not the exact durable form of this
        // digest-bound plan before OperationLock can create a project file.
        // The same check is repeated under the lock by recovery.
        let pending_transaction = inspect_pending_transaction(&plan.project_root, plan, digest)?;
        if pending_transaction.is_none() {
            let authority_committed = claim.authority_after_lock()?;
            preflight_fresh_apply(plan, digest, authority_committed)?;
        }
        let _operation_lock = OperationLock::acquire(&plan.project_root)?;
        let authority_committed = claim.authority_after_lock()?;
        let recovery = recover_pending_transaction(&plan.project_root, plan, digest)?;
        match committed_plan_state(plan, digest) {
            Ok(true) => {
                return Ok(setup_report(
                    plan,
                    digest,
                    already_applied_status(plan.mode),
                ));
            }
            Ok(false) if recovery == RecoveryOutcome::Committed => {
                return Err(SetupError::TransactionRecoveryRequired);
            }
            Ok(false) => {}
            Err(_) if recovery == RecoveryOutcome::Committed => {
                return Err(SetupError::TransactionRecoveryRequired);
            }
            Err(error) => return Err(error),
        }
        if authority_committed {
            return Err(SetupError::PlanInputsChanged);
        }
        let now = now_epoch_seconds()?;
        if now < plan.created_at_epoch_seconds || now > plan.expires_at_epoch_seconds {
            return Err(SetupError::PlanExpired);
        }
        verify_plan_bindings(plan)?;
        preflight_plan_inputs(plan)?;
        let journal = build_apply_journal(plan, digest)?;
        execute_transaction(&plan.project_root, &journal, plan, digest)?;
        Ok(setup_report(plan, digest, applied_status(plan.mode)))
    })();

    match result {
        Ok(report) => {
            // The receipt is the commit marker. Plan/journal garbage
            // collection is deliberately non-fatal after that point.
            claim.mark_committed_nonfatal();
            Ok(report)
        }
        Err(error) => {
            if !plan.project_root.join(JOURNAL_PATH).exists() {
                if matches!(
                    error,
                    SetupError::AtomicWriteFailed
                        | SetupError::OperationInProgress
                        | SetupError::TransactionRecoveryRequired
                ) {
                    claim.restore_if_owned()?;
                } else {
                    claim.invalidate_nonfatal();
                }
            }
            Err(error)
        }
    }
}

fn preflight_fresh_apply(
    plan: &SetupPlan,
    digest: &str,
    authority_committed: bool,
) -> Result<(), SetupError> {
    if committed_plan_state(plan, digest)? {
        return Ok(());
    }
    if authority_committed {
        return Err(SetupError::PlanInputsChanged);
    }
    let now = now_epoch_seconds()?;
    if now < plan.created_at_epoch_seconds || now > plan.expires_at_epoch_seconds {
        return Err(SetupError::PlanExpired);
    }
    verify_plan_bindings(plan)?;
    preflight_plan_inputs(plan)
}

struct PlanClaim {
    plan: SetupPlan,
    source_path: PathBuf,
    claimed_path: PathBuf,
    committed_path: PathBuf,
    owned: bool,
    committed: bool,
    #[allow(dead_code)]
    claim_lock: Option<File>,
}

impl PlanClaim {
    fn restore_if_owned(&self) -> Result<(), SetupError> {
        if !self.owned || self.committed || self.source_path.exists() {
            return Ok(());
        }
        fs::rename(&self.claimed_path, &self.source_path)
            .map_err(|_| SetupError::PlanStoreUnavailable)?;
        sync_directory(
            self.source_path
                .parent()
                .ok_or(SetupError::PlanStoreUnavailable)?,
        )
        .map_err(|_| SetupError::PlanStoreUnavailable)
    }

    fn invalidate_nonfatal(&self) {
        if !self.owned || self.committed {
            return;
        }
        let _ = fs::remove_file(&self.claimed_path);
        if let Some(parent) = self.claimed_path.parent() {
            let _ = sync_directory(parent);
        }
    }

    fn mark_committed_nonfatal(&self) {
        if self.committed || self.committed_path.exists() {
            return;
        }
        if fs::rename(&self.claimed_path, &self.committed_path).is_ok()
            && let Some(parent) = self.committed_path.parent()
        {
            let _ = sync_directory(parent);
        }
    }

    fn authority_after_lock(&self) -> Result<bool, SetupError> {
        let claimed_exists = regular_plan_path_exists(&self.claimed_path)?;
        let committed_exists = regular_plan_path_exists(&self.committed_path)?;
        if claimed_exists == committed_exists {
            return Err(if claimed_exists {
                SetupError::PlanStoreUnavailable
            } else {
                SetupError::PlanNotFound
            });
        }
        let path = if committed_exists {
            &self.committed_path
        } else {
            &self.claimed_path
        };
        let bytes =
            read_bounded(path, MAX_PLAN_BYTES).map_err(|_| SetupError::PlanStoreUnavailable)?;
        let observed: SetupPlan =
            serde_json::from_slice(&bytes).map_err(|_| SetupError::PlanDigestInvalid)?;
        if observed != self.plan {
            return Err(SetupError::PlanDigestInvalid);
        }
        Ok(committed_exists)
    }
}

fn regular_plan_path_exists(path: &Path) -> Result<bool, SetupError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(SetupError::PlanStoreUnavailable),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(SetupError::PlanStoreUnavailable),
    }
}

fn claim_plan(store: &Path, digest_hex: &str) -> Result<PlanClaim, SetupError> {
    let source_path = store.join(format!("{digest_hex}.json"));
    let claimed_path = store.join(format!("{digest_hex}.claimed"));
    let committed_path = store.join(format!("{digest_hex}.committed"));
    if regular_plan_path_exists(&committed_path)? {
        if regular_plan_path_exists(&source_path)? || regular_plan_path_exists(&claimed_path)? {
            return Err(SetupError::PlanStoreUnavailable);
        }
        let bytes = read_bounded(&committed_path, MAX_PLAN_BYTES).map_err(|error| match error {
            ReadError::Missing | ReadError::Unsafe | ReadError::Io => {
                SetupError::PlanStoreUnavailable
            }
            ReadError::TooLarge => SetupError::PlanDigestInvalid,
        })?;
        let plan = serde_json::from_slice(&bytes).map_err(|_| SetupError::PlanDigestInvalid)?;
        return Ok(PlanClaim {
            plan,
            source_path,
            claimed_path,
            committed_path,
            owned: false,
            committed: true,
            claim_lock: None,
        });
    }
    let renamed = match fs::rename(&source_path, &claimed_path) {
        Ok(()) => {
            sync_directory(store).map_err(|_| SetupError::PlanStoreUnavailable)?;
            true
        }
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && fs::symlink_metadata(&claimed_path).is_ok_and(|metadata| {
                    metadata.is_file() && !metadata.file_type().is_symlink()
                }) =>
        {
            false
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(SetupError::PlanNotFound);
        }
        Err(_) => return Err(SetupError::PlanStoreUnavailable),
    };
    let claim_lock = acquire_claim_lock(&claimed_path, renamed)?;
    let bytes = read_bounded(&claimed_path, MAX_PLAN_BYTES).map_err(|error| match error {
        ReadError::Missing | ReadError::Unsafe | ReadError::Io => SetupError::PlanStoreUnavailable,
        ReadError::TooLarge => SetupError::PlanDigestInvalid,
    })?;
    let plan = serde_json::from_slice(&bytes).map_err(|_| SetupError::PlanDigestInvalid)?;
    Ok(PlanClaim {
        plan,
        source_path,
        claimed_path,
        committed_path,
        owned: true,
        committed: false,
        claim_lock: Some(claim_lock),
    })
}

#[cfg(unix)]
fn acquire_claim_lock(path: &Path, _renamed: bool) -> Result<File, SetupError> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| SetupError::PlanStoreUnavailable)?;
    let file = File::from(descriptor);
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| SetupError::OperationInProgress)?;
    Ok(file)
}

#[cfg(not(unix))]
fn acquire_claim_lock(path: &Path, renamed: bool) -> Result<File, SetupError> {
    if !renamed {
        return Err(SetupError::OperationInProgress);
    }
    OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|_| SetupError::PlanStoreUnavailable)
}

struct OperationLock {
    #[allow(dead_code)]
    file: File,
}

impl OperationLock {
    fn acquire(root: &Path) -> Result<Self, SetupError> {
        let path = root.join(OPERATION_LOCK_PATH);
        ensure_safe_parent(root, &path, true)?;
        #[cfg(unix)]
        let file = {
            use rustix::fs::{AtFlags, Mode, OFlags};

            let (parent, file_name) = open_parent_directory(root, &path)?;
            let existed = match rustix::fs::statat(&parent, &file_name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => {
                    if !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_file()
                        || u32::from(Mode::from_raw_mode(stat.st_mode).as_raw_mode())
                            != private_file_mode()
                    {
                        return Err(SetupError::PathUnsafe);
                    }
                    true
                }
                Err(rustix::io::Errno::NOENT) => false,
                Err(_) => return Err(SetupError::PathUnsafe),
            };
            let descriptor = rustix::fs::openat(
                &parent,
                &file_name,
                OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(private_file_mode() as _),
            )
            .map_err(|_| SetupError::AtomicWriteFailed)?;
            let file = File::from(descriptor);
            if !existed {
                rustix::fs::fchmod(&file, Mode::from_raw_mode(private_file_mode() as _))
                    .map_err(|_| SetupError::AtomicWriteFailed)?;
                parent
                    .sync_all()
                    .map_err(|_| SetupError::AtomicWriteFailed)?;
            }
            file
        };
        #[cfg(not(unix))]
        let existed = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || file_mode(&metadata) != private_file_mode()
                {
                    return Err(SetupError::PathUnsafe);
                }
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(_) => return Err(SetupError::PathUnsafe),
        };
        #[cfg(not(unix))]
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|_| SetupError::AtomicWriteFailed)?;
        #[cfg(not(unix))]
        if !existed {
            set_file_mode(&path, true).map_err(|_| SetupError::AtomicWriteFailed)?;
        }
        #[cfg(unix)]
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| SetupError::OperationInProgress)?;
        Ok(Self { file })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryOutcome {
    None,
    RolledBack,
    Committed,
}

fn setup_report(plan: &SetupPlan, digest: &str, status: &str) -> SetupReport {
    let mut changed_paths = plan
        .changes
        .iter()
        .map(|change| change.path.clone())
        .collect::<Vec<_>>();
    changed_paths.push(RECEIPT_PATH.to_owned());
    SetupReport {
        schema_version: "godot-codex-setup-result/1.1".to_owned(),
        mode: plan.mode,
        plan_digest: digest.to_owned(),
        status: status.to_owned(),
        profile: plan.profile,
        guidance: plan.guidance,
        changed_paths,
        restart_required: true,
    }
}

const fn applied_status(mode: SetupMode) -> &'static str {
    match mode {
        SetupMode::Configure => "applied",
        SetupMode::Repair => "repaired",
        SetupMode::Remove => "removed",
    }
}

const fn already_applied_status(mode: SetupMode) -> &'static str {
    match mode {
        SetupMode::Configure => "already_applied",
        SetupMode::Repair => "already_repaired",
        SetupMode::Remove => "already_removed",
    }
}

fn build_apply_journal(plan: &SetupPlan, digest: &str) -> Result<SetupJournal, SetupError> {
    let mut files = Vec::with_capacity(plan.changes.len() + 1);
    for change in &plan.changes {
        let target = project_path(&plan.project_root, &change.path)?;
        let max = if change.path == CONFIG_PATH {
            MAX_CONFIG_BYTES
        } else {
            MAX_GUIDANCE_BYTES
        };
        let (before, before_mode) = read_optional_text(&target, max)?;
        files.push(JournalFile {
            path: change.path.clone(),
            before,
            before_mode,
            after: change.desired.clone(),
            after_mode: change.desired.as_ref().map(|_| change.desired_mode),
            commit_marker: false,
        });
    }
    let receipt_path = plan.project_root.join(RECEIPT_PATH);
    let (receipt_before, receipt_before_mode) =
        read_optional_text(&receipt_path, MAX_RECEIPT_BYTES)?;
    let receipt_after = if plan.mode == SetupMode::Remove {
        None
    } else {
        let receipt = SetupReceipt::from_template(&plan.receipt_template, digest);
        Some(serde_json::to_string_pretty(&receipt).map_err(|_| SetupError::AtomicWriteFailed)?)
    };
    files.push(JournalFile {
        path: RECEIPT_PATH.to_owned(),
        before: receipt_before,
        before_mode: receipt_before_mode,
        after_mode: receipt_after.as_ref().map(|_| private_file_mode()),
        after: receipt_after,
        commit_marker: true,
    });
    Ok(SetupJournal {
        schema_version: JOURNAL_SCHEMA.to_owned(),
        operation: if plan.mode == SetupMode::Remove {
            JournalOperation::Remove
        } else {
            JournalOperation::Apply
        },
        project_id: plan.project_id.clone(),
        plan_digest: Some(digest.to_owned()),
        files,
    })
}

fn execute_transaction(
    root: &Path,
    journal: &SetupJournal,
    plan: &SetupPlan,
    digest: &str,
) -> Result<(), SetupError> {
    if root != plan.project_root {
        return Err(SetupError::TransactionRecoveryRequired);
    }
    validate_journal_for_plan(journal, plan, digest)?;
    let bytes = serde_json::to_vec(journal).map_err(|_| SetupError::AtomicWriteFailed)?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(SetupError::InputTooLarge);
    }
    let journal_path = root.join(JOURNAL_PATH);
    if journal_path.exists() {
        return Err(SetupError::TransactionRecoveryRequired);
    }
    atomic_write(root, &journal_path, &bytes, true, private_file_mode())?;

    for (applied, file) in journal.files.iter().enumerate() {
        if let Err(error) = apply_journal_state(root, file, true) {
            let after_visible = journal_state_matches(root, file, true)
                .map_err(|_| SetupError::TransactionRecoveryRequired)?;
            if file.commit_marker && after_visible {
                // The commit marker rename may have succeeded even when its
                // directory fsync reported an error. Keep the durable journal
                // for exact reconciliation; never roll back earlier files
                // while the receipt visibly says committed.
                return Err(SetupError::TransactionRecoveryRequired);
            }
            let rollback_end = applied + usize::from(after_visible);
            let rollback_ok = journal.files[..rollback_end]
                .iter()
                .rev()
                .all(|applied_file| apply_journal_state(root, applied_file, false).is_ok());
            if rollback_ok {
                if remove_plain_file(root, &journal_path).is_err() {
                    return Err(SetupError::TransactionRecoveryRequired);
                }
                return Err(error);
            }
            return Err(SetupError::TransactionRecoveryRequired);
        }
    }
    // The final receipt write/delete is the commit point. Cleanup must never
    // turn a committed setup transaction into a reported failure.
    let _ = remove_plain_file(root, &journal_path);
    Ok(())
}

fn reject_pending_transaction_during_prepare(root: &Path) -> Result<(), SetupError> {
    match fs::symlink_metadata(root.join(JOURNAL_PATH)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) | Err(_) => Err(SetupError::TransactionRecoveryRequired),
    }
}

fn inspect_pending_transaction(
    root: &Path,
    plan: &SetupPlan,
    digest: &str,
) -> Result<Option<SetupJournal>, SetupError> {
    let path = root.join(JOURNAL_PATH);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(SetupError::TransactionRecoveryRequired),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || file_mode(&metadata) != private_file_mode()
    {
        return Err(SetupError::TransactionRecoveryRequired);
    }
    let bytes = read_bounded(&path, MAX_JOURNAL_BYTES)
        .map_err(|_| SetupError::TransactionRecoveryRequired)?;
    let journal: SetupJournal =
        serde_json::from_slice(&bytes).map_err(|_| SetupError::TransactionRecoveryRequired)?;
    validate_journal_for_plan(&journal, plan, digest)?;
    Ok(Some(journal))
}

fn recover_pending_transaction(
    root: &Path,
    plan: &SetupPlan,
    digest: &str,
) -> Result<RecoveryOutcome, SetupError> {
    let Some(journal) = inspect_pending_transaction(root, plan, digest)? else {
        return Ok(RecoveryOutcome::None);
    };
    let path = root.join(JOURNAL_PATH);
    let commit = journal
        .files
        .last()
        .ok_or(SetupError::TransactionRecoveryRequired)?;
    let committed = journal_state_matches(root, commit, true)?;
    let uncommitted = journal_state_matches(root, commit, false)?;
    let use_after = match (committed, uncommitted) {
        (true, false) => true,
        (false, true) => false,
        _ => return Err(SetupError::TransactionRecoveryRequired),
    };

    // Validate the complete observed crash state before changing any file.
    // This keeps a corrupted or externally modified partial transaction from
    // causing another partial mutation during recovery.
    for file in &journal.files {
        if !journal_state_matches(root, file, use_after)?
            && !journal_state_matches(root, file, !use_after)?
        {
            return Err(SetupError::TransactionRecoveryRequired);
        }
    }
    for file in if use_after {
        EitherFiles::Forward(journal.files.iter())
    } else {
        EitherFiles::Reverse(journal.files.iter().rev())
    } {
        apply_journal_state(root, file, use_after)
            .map_err(|_| SetupError::TransactionRecoveryRequired)?;
    }
    if remove_plain_file(root, &path).is_err() {
        return Err(SetupError::TransactionRecoveryRequired);
    }
    Ok(if use_after {
        RecoveryOutcome::Committed
    } else {
        RecoveryOutcome::RolledBack
    })
}

enum EitherFiles<'a> {
    Forward(std::slice::Iter<'a, JournalFile>),
    Reverse(std::iter::Rev<std::slice::Iter<'a, JournalFile>>),
}

impl<'a> Iterator for EitherFiles<'a> {
    type Item = &'a JournalFile;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Forward(files) => files.next(),
            Self::Reverse(files) => files.next(),
        }
    }
}

fn validate_journal(journal: &SetupJournal) -> Result<(), SetupError> {
    if journal.schema_version != JOURNAL_SCHEMA
        || journal.files.is_empty()
        || journal.files.len() > 5
        || journal
            .plan_digest
            .as_deref()
            .is_none_or(|digest| parse_digest(digest).is_err())
        || journal
            .files
            .last()
            .is_none_or(|file| !file.commit_marker || file.path != RECEIPT_PATH)
        || journal.files[..journal.files.len() - 1]
            .iter()
            .any(|file| file.commit_marker)
        || journal.files.iter().any(|file| {
            !matches!(
                file.path.as_str(),
                CONFIG_PATH | AGENTS_PATH | SKILL_PATH | RECEIPT_PATH
            ) || file.before.is_some() != file.before_mode.is_some()
                || file.after.is_some() != file.after_mode.is_some()
                || file.before_mode.is_some_and(|mode| !valid_file_mode(mode))
                || file.after_mode.is_some_and(|mode| !valid_file_mode(mode))
        })
    {
        return Err(SetupError::TransactionRecoveryRequired);
    }
    let mut paths = std::collections::BTreeSet::new();
    if journal.files.iter().any(|file| !paths.insert(&file.path)) {
        return Err(SetupError::TransactionRecoveryRequired);
    }
    Ok(())
}

fn validate_journal_for_plan(
    journal: &SetupJournal,
    plan: &SetupPlan,
    digest: &str,
) -> Result<(), SetupError> {
    validate_journal(journal)?;
    let expected_operation = if plan.mode == SetupMode::Remove {
        JournalOperation::Remove
    } else {
        JournalOperation::Apply
    };
    if journal.operation != expected_operation
        || journal.project_id != plan.project_id
        || journal.plan_digest.as_deref() != Some(digest)
        || journal.files.len() != plan.changes.len() + 1
    {
        return Err(SetupError::TransactionRecoveryRequired);
    }

    for (file, change) in journal.files[..journal.files.len() - 1]
        .iter()
        .zip(&plan.changes)
    {
        if file.path != change.path
            || !journal_before_matches(file, &change.before)
            || file.after != change.desired
            || file.after_mode != change.desired.as_ref().map(|_| change.desired_mode)
        {
            return Err(SetupError::TransactionRecoveryRequired);
        }
    }

    let commit = journal
        .files
        .last()
        .ok_or(SetupError::TransactionRecoveryRequired)?;
    let expected_receipt = if plan.mode == SetupMode::Remove {
        None
    } else {
        Some(
            serde_json::to_string_pretty(&SetupReceipt::from_template(
                &plan.receipt_template,
                digest,
            ))
            .map_err(|_| SetupError::TransactionRecoveryRequired)?,
        )
    };
    if !journal_before_matches(commit, &plan.receipt_before)
        || commit.after != expected_receipt
        || commit.after_mode != commit.after.as_ref().map(|_| private_file_mode())
    {
        return Err(SetupError::TransactionRecoveryRequired);
    }
    Ok(())
}

fn journal_before_matches(file: &JournalFile, expected: &FileState) -> bool {
    let observed = match (&file.before, file.before_mode) {
        (Some(content), Some(mode)) => FileState {
            exists: true,
            digest: digest_text(content),
            mode: Some(mode),
        },
        (None, None) => FileState {
            exists: false,
            digest: MISSING_DIGEST.to_owned(),
            mode: None,
        },
        _ => return false,
    };
    observed == *expected
}

fn read_optional_text(path: &Path, max: u64) -> Result<(Option<String>, Option<u32>), SetupError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(SetupError::PathUnsafe);
            }
            let text = read_utf8(path, max)?;
            Ok((Some(text), Some(file_mode(&metadata))))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok((None, None)),
        Err(_) => Err(SetupError::PathUnsafe),
    }
}

fn journal_state_matches(root: &Path, file: &JournalFile, after: bool) -> Result<bool, SetupError> {
    let target = project_path(root, &file.path)?;
    let (expected, mode) = if after {
        (&file.after, file.after_mode)
    } else {
        (&file.before, file.before_mode)
    };
    match (expected, fs::symlink_metadata(&target)) {
        (None, Err(error)) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        (None, _) => Ok(false),
        (Some(expected), Ok(metadata))
            if !metadata.file_type().is_symlink() && metadata.is_file() =>
        {
            let max = managed_file_limit(&file.path);
            let observed = read_utf8(&target, max)?;
            Ok(&observed == expected && mode == Some(file_mode(&metadata)))
        }
        (Some(_), _) => Ok(false),
    }
}

fn apply_journal_state(root: &Path, file: &JournalFile, after: bool) -> Result<(), SetupError> {
    let target = project_path(root, &file.path)?;
    let (content, mode) = if after {
        (&file.after, file.after_mode)
    } else {
        (&file.before, file.before_mode)
    };
    match content {
        Some(content) => atomic_write(
            root,
            &target,
            content.as_bytes(),
            file.path == RECEIPT_PATH || file.path == JOURNAL_PATH,
            mode.ok_or(SetupError::TransactionRecoveryRequired)?,
        ),
        None => {
            if target.exists() || target.is_symlink() {
                remove_plain_file(root, &target)
            } else {
                Ok(())
            }
        }
    }
}

fn managed_file_limit(path: &str) -> u64 {
    match path {
        CONFIG_PATH => MAX_CONFIG_BYTES,
        RECEIPT_PATH => MAX_RECEIPT_BYTES,
        JOURNAL_PATH => MAX_JOURNAL_BYTES,
        _ => MAX_GUIDANCE_BYTES,
    }
}

fn plan_remove_changes(
    root: &Path,
    receipt: &SetupReceipt,
) -> Result<Vec<PlannedChange>, SetupError> {
    let config_path = root.join(CONFIG_PATH);
    let config_before = file_state(&config_path, MAX_CONFIG_BYTES)?;
    let config_text = read_utf8(&config_path, MAX_CONFIG_BYTES)?;
    let config_stanza = config_table_stanza(&config_text)?.ok_or(SetupError::ReceiptInvalid)?;
    let mut config_document = config_text
        .parse::<DocumentMut>()
        .map_err(|_| SetupError::ConfigInvalid)?;
    remove_config_table(&mut config_document)?;
    let next_config = config_document.to_string();
    ensure_output_bound(&next_config, MAX_CONFIG_BYTES)?;
    let keep_config = !(receipt.config_created && next_config.trim().is_empty());
    let mut changes = vec![PlannedChange {
        path: CONFIG_PATH.to_owned(),
        before: config_before,
        desired: keep_config.then_some(next_config),
        display_before: redact_launcher_from_stanza(config_stanza),
        display_after: "<absent>\n".to_owned(),
        private: false,
        desired_mode: receipt.config_file_mode,
    }];

    if receipt.agents_block_digest.is_some() {
        let path = root.join(AGENTS_PATH);
        let before = file_state(&path, MAX_GUIDANCE_BYTES)?;
        let current = read_utf8(&path, MAX_GUIDANCE_BYTES)?;
        let owned_block = extract_agents_block(&current)?
            .ok_or(SetupError::OwnershipConflict)?
            .to_owned();
        let desired = remove_agents_block(
            &current,
            receipt.agents_separator.as_deref().unwrap_or_default(),
        )?;
        ensure_output_bound(&desired, MAX_GUIDANCE_BYTES)?;
        changes.push(PlannedChange {
            path: AGENTS_PATH.to_owned(),
            before,
            desired: (!desired.is_empty()).then_some(desired),
            display_before: owned_block,
            display_after: "<absent>\n".to_owned(),
            private: false,
            desired_mode: receipt.agents_file_mode.ok_or(SetupError::ReceiptInvalid)?,
        });
    }

    if let Some(skill_digest) = receipt.skill_file_digest.as_deref() {
        let path = root.join(SKILL_PATH);
        let before = file_state(&path, MAX_GUIDANCE_BYTES)?;
        changes.push(PlannedChange {
            path: SKILL_PATH.to_owned(),
            before,
            desired: None,
            display_before: format!("<receipt-owned file digest={skill_digest}>\n"),
            display_after: "<absent>\n".to_owned(),
            private: false,
            desired_mode: receipt.skill_file_mode.ok_or(SetupError::ReceiptInvalid)?,
        });
    }

    changes.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(changes)
}

fn plan_config(
    root: &Path,
    profile: SetupProfile,
    receipt: Option<&SetupReceipt>,
    launcher: &LauncherResolution,
) -> Result<(PlannedChange, String, String, bool), SetupError> {
    let path = root.join(CONFIG_PATH);
    let before = file_state(&path, MAX_CONFIG_BYTES)?;
    let original = if before.exists {
        read_utf8(&path, MAX_CONFIG_BYTES)?
    } else {
        String::new()
    };
    let mut document = original
        .parse::<DocumentMut>()
        .map_err(|_| SetupError::ConfigInvalid)?;
    let existing_stanza = config_table_stanza(&original)?;
    let existing_digest = existing_stanza.map(config_stanza_digest);
    match (existing_digest.as_deref(), receipt) {
        (Some(digest), Some(receipt)) if digest == receipt.config_table_digest => {}
        (Some(_), _) => return Err(SetupError::OwnershipConflict),
        (None, Some(_)) => return Err(SetupError::OwnershipConflict),
        (None, None) => {}
    }

    let ownership_marker = receipt.map_or_else(new_config_ownership_marker, |receipt| {
        Ok(receipt.config_ownership_marker.clone())
    })?;
    let desired_item = desired_config_item(
        profile,
        launcher
            .command_text()
            .map_err(|_| SetupError::PackageInvalid)?,
        launcher.installed_data_root(),
        &ownership_marker,
        root,
    )?;
    install_config_table(&mut document, desired_item)?;
    let desired = document.to_string();
    ensure_output_bound(&desired, MAX_CONFIG_BYTES)?;
    let desired_stanza = config_table_stanza(&desired)?.ok_or(SetupError::ConfigInvalid)?;
    let table_digest = config_stanza_digest(desired_stanza);
    let display_before =
        existing_stanza.map_or_else(|| "<absent>\n".to_owned(), redact_launcher_from_stanza);
    let display_after = redact_launcher_from_stanza(desired_stanza);
    Ok((
        PlannedChange {
            path: CONFIG_PATH.to_owned(),
            desired_mode: desired_mode(&before, false),
            before,
            desired: Some(desired),
            display_before,
            display_after,
            private: false,
        },
        ownership_marker,
        table_digest,
        receipt.map_or(!path.exists(), |value| value.config_created),
    ))
}

fn plan_repair_config(
    root: &Path,
    receipt: &SetupReceipt,
    launcher: &LauncherResolution,
    package_upgrade: bool,
) -> Result<(PlannedChange, String), SetupError> {
    let path = root.join(CONFIG_PATH);
    validate_safe_parent(root, &path, false)?;
    let before = file_state(&path, MAX_CONFIG_BYTES)?;
    if !before.exists {
        return Err(SetupError::ConfigInvalid);
    }
    let desired_mode = before
        .mode
        .filter(|mode| valid_file_mode(*mode))
        .ok_or(SetupError::ConfigInvalid)?;
    let original = read_utf8(&path, MAX_CONFIG_BYTES)?;
    let mut document = original
        .parse::<DocumentMut>()
        .map_err(|_| SetupError::ConfigInvalid)?;
    let stanza_present = config_table_item(&document).is_some();

    let existing_stanza = config_table_stanza(&original)?;
    let exact_owned_stanza = existing_stanza.map(config_stanza_digest).as_deref()
        == Some(receipt.config_table_digest.as_str());
    if exact_owned_stanza {
        if before.mode != Some(receipt.config_file_mode)
            || existing_stanza.and_then(config_ownership_marker).as_deref()
                != Some(receipt.config_ownership_marker.as_str())
        {
            return Err(SetupError::OwnershipConflict);
        }
        if !package_upgrade {
            return Err(SetupError::RepairNotNeeded);
        }
    }
    let launcher_command = launcher
        .command_text()
        .map_err(|_| SetupError::PackageInvalid)?;
    if stanza_present && !exact_owned_stanza {
        let stanza = existing_stanza.ok_or(SetupError::OwnershipConflict)?;
        let item = config_table_item(&document).ok_or(SetupError::OwnershipConflict)?;
        if config_ownership_marker(stanza).as_deref()
            != Some(receipt.config_ownership_marker.as_str())
            || !repairable_config_shape(
                item,
                receipt,
                launcher_command,
                launcher.installed_data_root(),
                root,
            )
        {
            return Err(SetupError::OwnershipConflict);
        }
    }

    let desired_item = desired_config_item(
        receipt.profile,
        launcher_command,
        launcher.installed_data_root(),
        &receipt.config_ownership_marker,
        root,
    )?;
    install_config_table(&mut document, desired_item)?;
    let desired = document.to_string();
    ensure_output_bound(&desired, MAX_CONFIG_BYTES)?;
    let desired_stanza = config_table_stanza(&desired)?.ok_or(SetupError::ConfigInvalid)?;
    let desired_stanza_digest = config_stanza_digest(desired_stanza);
    if !exact_owned_stanza && desired_stanza_digest != receipt.config_table_digest {
        return Err(SetupError::ReceiptInvalid);
    }
    let display_after = redact_launcher_from_stanza(desired_stanza);

    let display_before = if exact_owned_stanza {
        "<exact receipt-owned stanza; package binding refresh>\n".to_owned()
    } else if stanza_present {
        format!(
            "<drifted receipt-owned stanza; current-file-digest={}>\n",
            before.digest
        )
    } else {
        "<missing receipt-owned stanza>\n".to_owned()
    };
    Ok((
        PlannedChange {
            path: CONFIG_PATH.to_owned(),
            before,
            desired: Some(desired),
            display_before,
            display_after,
            private: false,
            // Repair preserves the mode observed in the digest-bound input.
            desired_mode,
        },
        desired_stanza_digest,
    ))
}

fn repairable_config_shape(
    item: &Item,
    receipt: &SetupReceipt,
    expected_command: &str,
    expected_data_root: Option<&Path>,
    expected_root: &Path,
) -> bool {
    let Some(table) = item.as_table() else {
        return false;
    };
    let base_keys = [
        "command",
        "args",
        "cwd",
        "required",
        "startup_timeout_sec",
        "tool_timeout_sec",
        "enabled_tools",
    ];
    let expected_key_count = match receipt.profile {
        SetupProfile::ReadOnly => base_keys.len(),
        SetupProfile::FullBeta => base_keys.len() + 1,
    } + usize::from(expected_data_root.is_some());
    if table.len() != expected_key_count || base_keys.iter().any(|key| !table.contains_key(key)) {
        return false;
    }
    let command = table.get("command").and_then(Item::as_str);
    if command != Some(expected_command)
        || command.is_none_or(|value| !Path::new(value).is_absolute())
    {
        return false;
    }
    let args_are_exact = table
        .get("args")
        .and_then(Item::as_array)
        .is_some_and(|args| {
            args.len() == 2
                && args.get(0).and_then(toml_edit::Value::as_str) == Some("--project-root")
                && args.get(1).and_then(toml_edit::Value::as_str) == Some(".")
        });
    let enabled_tools_have_string_shape = table
        .get("enabled_tools")
        .and_then(Item::as_array)
        .is_some_and(|tools| tools.iter().all(|value| value.as_str().is_some()));
    if !args_are_exact
        || table.get("cwd").and_then(Item::as_str) != expected_root.to_str()
        || config_data_root(table) != expected_data_root.and_then(Path::to_str)
        || table.get("required").and_then(Item::as_bool).is_none()
        || table
            .get("startup_timeout_sec")
            .and_then(Item::as_integer)
            .is_none()
        || table
            .get("tool_timeout_sec")
            .and_then(Item::as_integer)
            .is_none()
        || !enabled_tools_have_string_shape
    {
        return false;
    }
    match receipt.profile {
        SetupProfile::ReadOnly => !table.contains_key("default_tools_approval_mode"),
        SetupProfile::FullBeta => table
            .get("default_tools_approval_mode")
            .and_then(Item::as_str)
            .is_some(),
    }
}

fn plan_agents(
    root: &Path,
    requested: bool,
    receipt: Option<&SetupReceipt>,
) -> Result<PlannedAgents, SetupError> {
    let path = root.join(AGENTS_PATH);
    let before = file_state(&path, MAX_GUIDANCE_BYTES)?;
    let current = if before.exists {
        read_utf8(&path, MAX_GUIDANCE_BYTES)?
    } else {
        String::new()
    };
    let existing_block = extract_agents_block(&current)?;
    let previously_owned = receipt.and_then(|value| value.agents_block_digest.as_deref());
    if let Some(expected) = previously_owned {
        let Some(block) = existing_block else {
            return Err(SetupError::OwnershipConflict);
        };
        if digest_text(block) != expected {
            return Err(SetupError::OwnershipConflict);
        }
    } else if existing_block.is_some() {
        return Err(SetupError::GuidanceConflict);
    }

    if requested {
        let block = canonical_agents_block();
        let (desired, separator) = if existing_block.is_some() {
            (
                replace_agents_block(&current, &block)?,
                receipt
                    .and_then(|value| value.agents_separator.clone())
                    .unwrap_or_default(),
            )
        } else {
            append_agents_block(&current, &block)
        };
        ensure_output_bound(&desired, MAX_GUIDANCE_BYTES)?;
        let block_digest = digest_text(&block);
        return Ok((
            vec![PlannedChange {
                path: AGENTS_PATH.to_owned(),
                desired_mode: desired_mode(&before, false),
                before,
                desired: Some(desired),
                display_before: existing_block.unwrap_or("<absent>\n").to_owned(),
                display_after: block,
                private: false,
            }],
            Some(block_digest),
            Some(separator),
        ));
    }

    if let Some(block) = existing_block {
        let desired = remove_agents_block(
            &current,
            receipt
                .and_then(|value| value.agents_separator.as_deref())
                .unwrap_or_default(),
        )?;
        return Ok((
            vec![PlannedChange {
                path: AGENTS_PATH.to_owned(),
                desired_mode: desired_mode(&before, false),
                before,
                desired: (!desired.is_empty()).then_some(desired),
                display_before: block.to_owned(),
                display_after: "<absent>\n".to_owned(),
                private: false,
            }],
            None,
            None,
        ));
    }
    Ok((Vec::new(), None, None))
}

fn plan_skill(
    root: &Path,
    requested: bool,
    receipt: Option<&SetupReceipt>,
) -> Result<(Vec<PlannedChange>, Option<String>), SetupError> {
    let path = root.join(SKILL_PATH);
    let before = file_state(&path, MAX_GUIDANCE_BYTES)?;
    let current = if before.exists {
        Some(read_utf8(&path, MAX_GUIDANCE_BYTES)?)
    } else {
        None
    };
    let previously_owned = receipt.and_then(|value| value.skill_file_digest.as_deref());
    if let Some(expected) = previously_owned
        && current.as_deref().map(digest_text).as_deref() != Some(expected)
    {
        return Err(SetupError::OwnershipConflict);
    }

    if requested {
        if let Some(existing) = &current {
            if existing != CANONICAL_SKILL {
                return Err(SetupError::GuidanceConflict);
            }
            if previously_owned.is_none() {
                return Ok((Vec::new(), None));
            }
        }
        let digest = digest_text(CANONICAL_SKILL);
        return Ok((
            vec![PlannedChange {
                path: SKILL_PATH.to_owned(),
                desired_mode: desired_mode(&before, false),
                before,
                desired: Some(CANONICAL_SKILL.to_owned()),
                display_before: current.unwrap_or_else(|| "<absent>\n".to_owned()),
                display_after: CANONICAL_SKILL.to_owned(),
                private: false,
            }],
            Some(digest),
        ));
    }

    if let Some(existing) = current
        && previously_owned.is_some()
    {
        return Ok((
            vec![PlannedChange {
                path: SKILL_PATH.to_owned(),
                desired_mode: desired_mode(&before, false),
                before,
                desired: None,
                display_before: existing,
                display_after: "<absent>\n".to_owned(),
                private: false,
            }],
            None,
        ));
    }
    Ok((Vec::new(), None))
}

fn preview(plan: &SetupPlan, digest: &str) -> Result<SetupPreview, SetupError> {
    let mut changes = plan
        .changes
        .iter()
        .map(preview_change)
        .collect::<Result<Vec<_>, _>>()?;
    if plan.mode == SetupMode::Remove {
        changes.push(SetupChange {
            path: RECEIPT_PATH.to_owned(),
            action: SetupAction::Delete,
            before_digest: plan.receipt_before.digest.clone(),
            after_digest: MISSING_DIGEST.to_owned(),
            diff: safe_diff(RECEIPT_PATH, "<existing owned receipt>\n", "<absent>\n")?,
        });
    } else {
        let receipt = SetupReceipt::from_template(&plan.receipt_template, digest);
        let receipt_text =
            serde_json::to_string_pretty(&receipt).map_err(|_| SetupError::PlanDigestInvalid)?;
        changes.push(SetupChange {
            path: RECEIPT_PATH.to_owned(),
            action: if plan.receipt_before.exists {
                SetupAction::Update
            } else {
                SetupAction::Create
            },
            before_digest: plan.receipt_before.digest.clone(),
            after_digest: digest_text(&receipt_text),
            diff: safe_diff(
                RECEIPT_PATH,
                if plan.receipt_before.exists {
                    "<existing owned receipt>\n"
                } else {
                    "<absent>\n"
                },
                &receipt_text,
            )?,
        });
    }
    changes.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(SetupPreview {
        schema_version: PLAN_SCHEMA.to_owned(),
        mode: plan.mode,
        plan_digest: digest.to_owned(),
        expires_in_seconds: PLAN_LIFETIME_SECONDS,
        package_version: PRODUCT_VERSION.to_owned(),
        profile: plan.profile,
        guidance: plan.guidance,
        restart_required: true,
        project_trust_unchanged: true,
        changes,
    })
}

fn preview_change(change: &PlannedChange) -> Result<SetupChange, SetupError> {
    let action = match (change.before.exists, change.desired.is_some()) {
        (false, true) => SetupAction::Create,
        (true, false) => SetupAction::Delete,
        (true, true) => SetupAction::Update,
        (false, false) => return Err(SetupError::PlanDigestInvalid),
    };
    let after_digest = change
        .desired
        .as_deref()
        .map(digest_text)
        .unwrap_or_else(|| MISSING_DIGEST.to_owned());
    Ok(SetupChange {
        path: change.path.clone(),
        action,
        before_digest: change.before.digest.clone(),
        after_digest,
        diff: safe_diff(&change.path, &change.display_before, &change.display_after)?,
    })
}

fn desired_config_item(
    profile: SetupProfile,
    launcher: &str,
    installed_data_root: Option<&Path>,
    ownership_marker: &str,
    project_root: &Path,
) -> Result<Item, SetupError> {
    parse_digest(ownership_marker).map_err(|_| SetupError::ReceiptInvalid)?;
    let project_root = project_root.to_str().ok_or(SetupError::PathUnsafe)?;
    if !Path::new(project_root).is_absolute() {
        return Err(SetupError::PathUnsafe);
    }
    let tools = match profile {
        SetupProfile::ReadOnly => READ_ONLY_TOOLS,
        SetupProfile::FullBeta => FULL_BETA_TOOLS,
    };
    let mut source = format!(
        "[mcp_servers.godot_editor]\n{CONFIG_OWNERSHIP_PREFIX}{ownership_marker}\ncommand = \"/package/launcher\"\nargs = [\"--project-root\", \".\"]\ncwd = \"/project/root\"\nrequired = true\nstartup_timeout_sec = 10\ntool_timeout_sec = 60\n"
    );
    if let Some(data_root) = installed_data_root {
        let data_root = data_root.to_str().ok_or(SetupError::PathUnsafe)?;
        if !Path::new(data_root).is_absolute() {
            return Err(SetupError::PathUnsafe);
        }
        source.push_str("env = { GODOT_CODEX_DATA_ROOT = ");
        source.push_str(&toml_edit::Value::from(data_root).to_string());
        source.push_str(" }\n");
    }
    if profile == SetupProfile::FullBeta {
        source.push_str("default_tools_approval_mode = \"writes\"\n");
    }
    source.push_str("enabled_tools = [\n");
    for tool in tools {
        source.push_str("  \"");
        source.push_str(tool);
        source.push_str("\",\n");
    }
    source.push_str("]\n");
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(|_| SetupError::ConfigInvalid)?;
    document["mcp_servers"]["godot_editor"]["command"] = toml_edit::value(launcher);
    document["mcp_servers"]["godot_editor"]["cwd"] = toml_edit::value(project_root);
    document
        .get("mcp_servers")
        .and_then(|item| item.get("godot_editor"))
        .cloned()
        .ok_or(SetupError::ConfigInvalid)
}

fn config_data_root(table: &Table) -> Option<&str> {
    table
        .get("env")?
        .as_value()?
        .as_inline_table()?
        .get("GODOT_CODEX_DATA_ROOT")?
        .as_str()
}

fn config_table_item(document: &DocumentMut) -> Option<&Item> {
    document
        .get("mcp_servers")
        .and_then(|item| item.get("godot_editor"))
}

fn install_config_table(document: &mut DocumentMut, item: Item) -> Result<(), SetupError> {
    if document.get("mcp_servers").is_none() {
        let mut table = Table::new();
        table.set_implicit(true);
        document.insert("mcp_servers", Item::Table(table));
    }
    let servers = document
        .get_mut("mcp_servers")
        .and_then(Item::as_table_mut)
        .ok_or(SetupError::ConfigInvalid)?;
    servers.insert("godot_editor", item);
    Ok(())
}

fn remove_config_table(document: &mut DocumentMut) -> Result<(), SetupError> {
    let servers = document
        .get_mut("mcp_servers")
        .and_then(Item::as_table_like_mut)
        .ok_or(SetupError::ReceiptInvalid)?;
    servers
        .remove("godot_editor")
        .ok_or(SetupError::ReceiptInvalid)?;
    Ok(())
}

fn verify_owned_state(
    root: &Path,
    project_id: &str,
    receipt: &SetupReceipt,
) -> Result<(), SetupError> {
    validate_receipt_metadata(project_id, receipt)?;
    let config_path = root.join(CONFIG_PATH);
    validate_safe_parent(root, &config_path, false)?;
    let config = read_utf8(&config_path, MAX_CONFIG_BYTES)?;
    if existing_mode(&config_path)? != receipt.config_file_mode {
        return Err(SetupError::OwnershipConflict);
    }
    let document = config
        .parse::<DocumentMut>()
        .map_err(|_| SetupError::ReceiptInvalid)?;
    if config_table_item(&document).is_none() {
        return Err(SetupError::ReceiptInvalid);
    }
    let command = config_table_item(&document)
        .and_then(|item| item.get("command"))
        .and_then(Item::as_str)
        .ok_or(SetupError::ReceiptInvalid)?;
    if !Path::new(command).is_absolute() {
        return Err(SetupError::ReceiptInvalid);
    }
    let stanza = config_table_stanza(&config)?.ok_or(SetupError::ReceiptInvalid)?;
    if config_stanza_digest(stanza) != receipt.config_table_digest
        || config_ownership_marker(stanza).as_deref()
            != Some(receipt.config_ownership_marker.as_str())
    {
        return Err(SetupError::OwnershipConflict);
    }
    verify_owned_guidance_state(root, receipt)
}

fn validate_receipt_metadata(project_id: &str, receipt: &SetupReceipt) -> Result<(), SetupError> {
    if !ReceiptTemplate::from_receipt(receipt).has_valid_ownership_metadata(project_id)
        || parse_digest(&receipt.plan_digest).is_err()
    {
        return Err(SetupError::ReceiptInvalid);
    }
    Ok(())
}

fn receipt_matches_current_package(
    receipt: &SetupReceipt,
    launcher: &LauncherResolution,
    package_identity_digest: &str,
) -> bool {
    receipt.package_version == PRODUCT_VERSION
        && receipt.package_identity_digest == package_identity_digest
        && receipt.launcher_path_sha256 == launcher.path_digest()
        && receipt.launcher_file_sha256 == launcher.file_digest()
}

fn verify_owned_guidance_state(root: &Path, receipt: &SetupReceipt) -> Result<(), SetupError> {
    if let Some(expected) = receipt.agents_block_digest.as_deref() {
        let path = root.join(AGENTS_PATH);
        validate_safe_parent(root, &path, false)?;
        let state = file_state(&path, MAX_GUIDANCE_BYTES)?;
        if !state.exists || state.mode != receipt.agents_file_mode {
            return Err(SetupError::OwnershipConflict);
        }
        let agents = read_utf8(&path, MAX_GUIDANCE_BYTES)?;
        let block = extract_agents_block(&agents)?.ok_or(SetupError::OwnershipConflict)?;
        if digest_text(block) != expected {
            return Err(SetupError::OwnershipConflict);
        }
    }
    if let Some(expected) = receipt.skill_file_digest.as_deref() {
        let path = root.join(SKILL_PATH);
        validate_safe_parent(root, &path, false)?;
        let state = file_state(&path, MAX_GUIDANCE_BYTES)?;
        if !state.exists || state.mode != receipt.skill_file_mode {
            return Err(SetupError::OwnershipConflict);
        }
        let skill = read_utf8(&path, MAX_GUIDANCE_BYTES)?;
        if digest_text(&skill) != expected {
            return Err(SetupError::OwnershipConflict);
        }
    }
    Ok(())
}

fn owned_guidance_guards(
    root: &Path,
    receipt: &SetupReceipt,
) -> Result<Vec<GuardedInput>, SetupError> {
    let mut guards = Vec::with_capacity(2);
    for path in [
        receipt.agents_block_digest.is_some().then_some(AGENTS_PATH),
        receipt.skill_file_digest.is_some().then_some(SKILL_PATH),
    ]
    .into_iter()
    .flatten()
    {
        let target = root.join(path);
        validate_safe_parent(root, &target, false)?;
        let state = file_state(&target, MAX_GUIDANCE_BYTES)?;
        if !state.exists {
            return Err(SetupError::OwnershipConflict);
        }
        guards.push(GuardedInput {
            path: path.to_owned(),
            state,
            private: false,
        });
    }
    Ok(guards)
}

fn read_receipt(path: &Path) -> Result<SetupReceipt, SetupError> {
    if existing_mode(path).map_err(|_| SetupError::ReceiptInvalid)? != private_file_mode() {
        return Err(SetupError::ReceiptInvalid);
    }
    let bytes = read_bounded(path, MAX_RECEIPT_BYTES).map_err(|_| SetupError::ReceiptInvalid)?;
    serde_json::from_slice(&bytes).map_err(|_| SetupError::ReceiptInvalid)
}

pub(crate) struct SurfaceCaptureSetupState {
    pub(crate) config: Vec<u8>,
    pub(crate) receipt: Vec<u8>,
}

/// Reuses the setup ownership contract for surface capture without planning
/// or changing any project file.
pub(crate) fn verify_surface_capture_setup_state(
    project_root: &Path,
    project_id: &str,
    launcher: &LauncherResolution,
    package_directory: Option<&Path>,
) -> Result<SurfaceCaptureSetupState, SetupError> {
    let config_path = project_root.join(CONFIG_PATH);
    let receipt_path = project_root.join(RECEIPT_PATH);
    validate_safe_parent(project_root, &config_path, false)?;
    validate_safe_parent(project_root, &receipt_path, true)?;

    let config =
        read_bounded(&config_path, MAX_CONFIG_BYTES).map_err(|_| SetupError::ConfigInvalid)?;
    let receipt_bytes =
        read_bounded(&receipt_path, MAX_RECEIPT_BYTES).map_err(|_| SetupError::ReceiptInvalid)?;
    let receipt = read_receipt(&receipt_path)?;
    verify_owned_state(project_root, project_id, &receipt)?;
    let package_identity_digest = match package_directory {
        Some(package_directory) => current_package_identity_digest(Some(package_directory))?,
        None => installed_package_identity_digest(launcher.package_root())?,
    };
    if !receipt_matches_current_package(&receipt, launcher, &package_identity_digest) {
        return Err(SetupError::ReceiptInvalid);
    }

    if read_bounded(&config_path, MAX_CONFIG_BYTES).map_err(|_| SetupError::ConfigInvalid)?
        != config
        || read_bounded(&receipt_path, MAX_RECEIPT_BYTES).map_err(|_| SetupError::ReceiptInvalid)?
            != receipt_bytes
    {
        return Err(SetupError::PlanInputsChanged);
    }
    Ok(SurfaceCaptureSetupState {
        config,
        receipt: receipt_bytes,
    })
}

fn committed_plan_state(plan: &SetupPlan, digest: &str) -> Result<bool, SetupError> {
    let receipt_path = plan.project_root.join(RECEIPT_PATH);
    let state = file_state(&receipt_path, MAX_RECEIPT_BYTES)?;
    if plan.mode == SetupMode::Remove {
        if state.exists {
            return Ok(false);
        }
    } else {
        if !state.exists {
            return Ok(false);
        }
        let receipt = read_receipt(&receipt_path).map_err(|_| SetupError::PlanInputsChanged)?;
        if receipt.plan_digest != digest {
            return Ok(false);
        }
        if receipt != SetupReceipt::from_template(&plan.receipt_template, digest) {
            return Err(SetupError::TransactionRecoveryRequired);
        }
    }
    for change in &plan.changes {
        let target = project_path(&plan.project_root, &change.path)?;
        let max = managed_file_limit(&change.path);
        let (content, mode) = read_optional_text(&target, max)?;
        let expected_mode = change.desired.as_ref().map(|_| change.desired_mode);
        if content != change.desired || mode != expected_mode {
            return Err(SetupError::PlanInputsChanged);
        }
    }
    for guard in &plan.guarded_inputs {
        let target = project_path(&plan.project_root, &guard.path)?;
        if file_state(&target, managed_file_limit(&guard.path))? != guard.state {
            return Err(SetupError::PlanInputsChanged);
        }
    }
    Ok(true)
}

fn verify_plan_bindings(plan: &SetupPlan) -> Result<(), SetupError> {
    verify_project_binding(plan)?;
    let launcher = resolve_launcher(plan.package_directory.as_deref())
        .map_err(|_| SetupError::PlanInputsChanged)?;
    if plan.package_version != PRODUCT_VERSION
        || current_package_identity_digest(plan.package_directory.as_deref())?
            != plan.package_identity_digest
        || launcher.path_digest() != plan.launcher_path_sha256
        || launcher.file_digest() != plan.launcher_file_sha256
        || (plan.mode != SetupMode::Remove
            && (plan.receipt_template.package_version != plan.package_version
                || plan.receipt_template.package_identity_digest != plan.package_identity_digest
                || plan.receipt_template.launcher_path_sha256 != plan.launcher_path_sha256
                || plan.receipt_template.launcher_file_sha256 != plan.launcher_file_sha256))
    {
        return Err(SetupError::PlanInputsChanged);
    }
    let matrix = embedded_compatibility_matrix().map_err(|_| SetupError::PlanInputsChanged)?;
    if matrix
        .canonical_digest()
        .map_err(|_| SetupError::PlanInputsChanged)?
        != plan.compatibility_matrix_digest
        || canonical_registry_profile().digest != plan.registry_digest
    {
        return Err(SetupError::PlanInputsChanged);
    }
    Ok(())
}

fn verify_project_binding(plan: &SetupPlan) -> Result<(), SetupError> {
    verify_plan_shape(plan)?;
    let canonical = canonical_project(&plan.project_root)?;
    if canonical != plan.project_root
        || directory_identity(&canonical).map_err(|_| SetupError::PlanInputsChanged)?
            != plan.project_root_identity
        || project_id_for_path(&canonical).map_err(|_| SetupError::PlanInputsChanged)?
            != plan.project_id
    {
        return Err(SetupError::PlanInputsChanged);
    }
    Ok(())
}

fn verify_plan_shape(plan: &SetupPlan) -> Result<(), SetupError> {
    let current_receipt_package = plan.receipt_template.package_version == plan.package_version
        && plan.receipt_template.package_identity_digest == plan.package_identity_digest
        && plan.receipt_template.launcher_path_sha256 == plan.launcher_path_sha256
        && plan.receipt_template.launcher_file_sha256 == plan.launcher_file_sha256;
    if plan.expires_at_epoch_seconds
        != plan
            .created_at_epoch_seconds
            .saturating_add(PLAN_LIFETIME_SECONDS)
        || !plan
            .receipt_template
            .has_valid_ownership_metadata(&plan.project_id)
        || (plan.mode != SetupMode::Remove && !current_receipt_package)
        || plan.receipt_template.profile != plan.profile
        || plan.receipt_template.guidance != plan.guidance
        || plan
            .changes
            .iter()
            .any(|change| !matches!(change.path.as_str(), CONFIG_PATH | AGENTS_PATH | SKILL_PATH))
    {
        return Err(SetupError::PlanDigestInvalid);
    }

    match plan.mode {
        SetupMode::Configure if !plan.guarded_inputs.is_empty() => {
            return Err(SetupError::PlanDigestInvalid);
        }
        SetupMode::Repair => {
            let [change] = plan.changes.as_slice() else {
                return Err(SetupError::PlanDigestInvalid);
            };
            let desired = change
                .desired
                .as_deref()
                .ok_or(SetupError::PlanDigestInvalid)?;
            let desired_stanza = config_table_stanza(desired)
                .map_err(|_| SetupError::PlanDigestInvalid)?
                .ok_or(SetupError::PlanDigestInvalid)?;
            let expected_guards = [
                plan.receipt_template
                    .agents_block_digest
                    .is_some()
                    .then_some(AGENTS_PATH),
                plan.receipt_template
                    .skill_file_digest
                    .is_some()
                    .then_some(SKILL_PATH),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            if change.path != CONFIG_PATH
                || !change.before.exists
                || !plan.receipt_before.exists
                || change.private
                || change.before.mode != Some(change.desired_mode)
                || config_ownership_marker(desired_stanza).as_deref()
                    != Some(plan.receipt_template.config_ownership_marker.as_str())
                || config_stanza_digest(desired_stanza) != plan.receipt_template.config_table_digest
                || plan.guarded_inputs.len() != expected_guards.len()
                || expected_guards.iter().any(|expected| {
                    !plan.guarded_inputs.iter().any(|guard| {
                        guard.path == *expected && guard.state.exists && !guard.private
                    })
                })
            {
                return Err(SetupError::PlanDigestInvalid);
            }
        }
        SetupMode::Remove => {
            let expected_paths = [
                Some(CONFIG_PATH),
                plan.receipt_template
                    .agents_block_digest
                    .is_some()
                    .then_some(AGENTS_PATH),
                plan.receipt_template
                    .skill_file_digest
                    .is_some()
                    .then_some(SKILL_PATH),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            if !plan.receipt_before.exists
                || !plan.guarded_inputs.is_empty()
                || plan.changes.len() != expected_paths.len()
                || expected_paths.iter().any(|expected| {
                    !plan.changes.iter().any(|change| {
                        change.path == *expected && change.before.exists && !change.private
                    })
                })
            {
                return Err(SetupError::PlanDigestInvalid);
            }
            let config = plan
                .changes
                .iter()
                .find(|change| change.path == CONFIG_PATH)
                .ok_or(SetupError::PlanDigestInvalid)?;
            if config.before.mode != Some(plan.receipt_template.config_file_mode)
                || config.desired_mode != plan.receipt_template.config_file_mode
                || config.desired.as_deref().is_some_and(|desired| {
                    config_table_stanza(desired).map_or(true, |stanza| stanza.is_some())
                })
                || (!plan.receipt_template.config_created && config.desired.is_none())
            {
                return Err(SetupError::PlanDigestInvalid);
            }
            if let Some(agents) = plan
                .changes
                .iter()
                .find(|change| change.path == AGENTS_PATH)
                && (agents.before.mode != plan.receipt_template.agents_file_mode
                    || Some(agents.desired_mode) != plan.receipt_template.agents_file_mode
                    || agents.desired.as_deref().is_some_and(|desired| {
                        extract_agents_block(desired).map_or(true, |block| block.is_some())
                    }))
            {
                return Err(SetupError::PlanDigestInvalid);
            }
            if let Some(skill) = plan.changes.iter().find(|change| change.path == SKILL_PATH)
                && (skill.before.mode != plan.receipt_template.skill_file_mode
                    || Some(skill.desired_mode) != plan.receipt_template.skill_file_mode
                    || skill.desired.is_some())
            {
                return Err(SetupError::PlanDigestInvalid);
            }
        }
        SetupMode::Configure => {}
    }
    Ok(())
}

fn preflight_plan_inputs(plan: &SetupPlan) -> Result<(), SetupError> {
    for change in &plan.changes {
        let target = project_path(&plan.project_root, &change.path)?;
        let max = if change.path == CONFIG_PATH {
            MAX_CONFIG_BYTES
        } else {
            MAX_GUIDANCE_BYTES
        };
        if file_state(&target, max)? != change.before {
            return Err(SetupError::PlanInputsChanged);
        }
        validate_safe_parent(&plan.project_root, &target, change.private)?;
    }
    for guard in &plan.guarded_inputs {
        let target = project_path(&plan.project_root, &guard.path)?;
        if file_state(&target, managed_file_limit(&guard.path))? != guard.state {
            return Err(SetupError::PlanInputsChanged);
        }
        validate_safe_parent(&plan.project_root, &target, guard.private)?;
    }
    if file_state(&plan.project_root.join(RECEIPT_PATH), MAX_RECEIPT_BYTES)? != plan.receipt_before
    {
        return Err(SetupError::PlanInputsChanged);
    }
    validate_safe_parent(
        &plan.project_root,
        &plan.project_root.join(RECEIPT_PATH),
        true,
    )?;
    if plan.mode == SetupMode::Remove {
        let receipt = read_receipt(&plan.project_root.join(RECEIPT_PATH))
            .map_err(|_| SetupError::PlanInputsChanged)?;
        if ReceiptTemplate::from_receipt(&receipt) != plan.receipt_template
            || plan_remove_changes(&plan.project_root, &receipt)? != plan.changes
        {
            return Err(SetupError::PlanInputsChanged);
        }
    }
    Ok(())
}

fn persist_plan(store: &Path, digest: &str, plan: &SetupPlan) -> Result<(), SetupError> {
    purge_plans(store)?;
    let hex = parse_digest(digest)?;
    let target = store.join(format!("{hex}.json"));
    let claimed = store.join(format!("{hex}.claimed"));
    let committed = store.join(format!("{hex}.committed"));
    let bytes = serde_json::to_vec(plan).map_err(|_| SetupError::PlanDigestInvalid)?;
    if bytes.len() as u64 > MAX_PLAN_BYTES {
        return Err(SetupError::InputTooLarge);
    }
    if claimed.exists() || committed.exists() {
        return Err(SetupError::PlanStoreUnavailable);
    }
    if target.exists() {
        let existing =
            read_bounded(&target, MAX_PLAN_BYTES).map_err(|_| SetupError::PlanStoreUnavailable)?;
        return if existing == bytes {
            Ok(())
        } else {
            Err(SetupError::PlanStoreUnavailable)
        };
    }
    let count = fs::read_dir(store)
        .map_err(|_| SetupError::PlanStoreUnavailable)?
        .filter_map(Result::ok)
        .filter(|entry| managed_plan_path(&entry.path()))
        .count();
    if count >= MAX_STORED_PLANS {
        return Err(SetupError::PlanLimitReached);
    }
    write_new_private(&target, &bytes).map_err(|_| SetupError::PlanStoreUnavailable)
}

fn purge_plans(store: &Path) -> Result<(), SetupError> {
    let now = now_epoch_seconds()?;
    let mut removed = false;
    for entry in fs::read_dir(store).map_err(|_| SetupError::PlanStoreUnavailable)? {
        let entry = entry.map_err(|_| SetupError::PlanStoreUnavailable)?;
        let path = entry.path();
        let extension = path.extension().and_then(|value| value.to_str());
        let plan = read_bounded(&path, MAX_PLAN_BYTES)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SetupPlan>(&bytes).ok());
        let remove = match extension {
            Some("json" | "committed") => {
                plan.is_none_or(|plan| now > plan.expires_at_epoch_seconds)
            }
            Some("claimed") => abandoned_claim_is_collectable(&path, plan.as_ref(), now),
            _ => false,
        };
        if remove {
            fs::remove_file(path).map_err(|_| SetupError::PlanStoreUnavailable)?;
            removed = true;
        }
    }
    if removed {
        sync_directory(store).map_err(|_| SetupError::PlanStoreUnavailable)?;
    }
    Ok(())
}

fn abandoned_claim_is_collectable(path: &Path, plan: Option<&SetupPlan>, now: u64) -> bool {
    let expired = plan.is_none_or(|plan| {
        now > plan
            .expires_at_epoch_seconds
            .saturating_add(CLAIM_GC_GRACE_SECONDS)
    });
    if !expired {
        return false;
    }
    let Ok(_claim_lock) = acquire_claim_lock(path, false) else {
        return false;
    };
    plan.is_none_or(|plan| {
        matches!(
            fs::symlink_metadata(plan.project_root.join(JOURNAL_PATH)),
            Err(error) if error.kind() == io::ErrorKind::NotFound
        )
    })
}

fn managed_plan_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("json" | "claimed" | "committed")
    )
}

fn prepare_plan_store(store: &Path) -> Result<(), SetupError> {
    if store.exists() {
        return validate_plan_store(store);
    }
    fs::create_dir_all(store).map_err(|_| SetupError::PlanStoreUnavailable)?;
    set_private_directory(store).map_err(|_| SetupError::PlanStoreUnavailable)?;
    validate_plan_store(store)
}

fn validate_plan_store(store: &Path) -> Result<(), SetupError> {
    let metadata = fs::symlink_metadata(store).map_err(|_| SetupError::PlanStoreUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || !owned_by_current_user(&metadata)
    {
        return Err(SetupError::PlanStoreUnavailable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o700 {
            return Err(SetupError::PlanStoreUnavailable);
        }
    }
    Ok(())
}

fn canonical_project(path: &Path) -> Result<PathBuf, SetupError> {
    let canonical = fs::canonicalize(path).map_err(|_| SetupError::ProjectInvalid)?;
    if !canonical.is_dir() || !canonical.join("project.godot").is_file() {
        return Err(SetupError::ProjectInvalid);
    }
    Ok(canonical)
}

#[cfg(unix)]
fn directory_identity(path: &Path) -> Result<DirectoryIdentity, SetupError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path).map_err(|_| SetupError::PathUnsafe)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SetupError::PathUnsafe);
    }
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn directory_identity(path: &Path) -> Result<DirectoryIdentity, SetupError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| SetupError::PathUnsafe)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SetupError::PathUnsafe);
    }
    Ok(DirectoryIdentity {
        device: 0,
        inode: 0,
    })
}

fn verify_supported_target() -> Result<(), SetupError> {
    let matrix = embedded_compatibility_matrix().map_err(|_| SetupError::PlatformIncompatible)?;
    let os = match std::env::consts::OS {
        "macos" => godot_codex_product::OperatingSystem::Macos,
        "windows" => godot_codex_product::OperatingSystem::Windows,
        "linux" => godot_codex_product::OperatingSystem::Linux,
        _ => return Err(SetupError::PlatformIncompatible),
    };
    let architecture = match std::env::consts::ARCH {
        "aarch64" => godot_codex_product::Architecture::Arm64,
        "x86_64" => godot_codex_product::Architecture::X86_64,
        _ => return Err(SetupError::PlatformIncompatible),
    };
    if matrix.package.target.os != os || matrix.package.target.architecture != architecture {
        return Err(SetupError::PlatformIncompatible);
    }
    Ok(())
}

fn current_package_identity_digest(package_override: Option<&Path>) -> Result<String, SetupError> {
    let operations_path =
        fs::canonicalize(std::env::current_exe().map_err(|_| SetupError::PlanInputsChanged)?)
            .map_err(|_| SetupError::PlanInputsChanged)?;
    let inferred_root = operations_path
        .parent()
        .and_then(Path::parent)
        .filter(|root| root.join("package-manifest.json").is_file());
    let package_root = package_override.or(inferred_root);
    let sidecar_path = package_root.map_or_else(
        || {
            operations_path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join("godot-codex-mcp")
        },
        |root| root.join("bin/godot-codex-mcp"),
    );
    let manifest_path = package_root
        .map(|root| root.join("package-manifest.json"))
        .unwrap_or_default();
    package_identity_digest_for_paths(
        &operations_path,
        &sidecar_path,
        package_root.map(|_| manifest_path.as_path()),
        package_override.is_some(),
    )
}

fn installed_package_identity_digest(package_root: &Path) -> Result<String, SetupError> {
    package_identity_digest_for_paths(
        &package_root.join("bin/godot-codex"),
        &package_root.join("bin/godot-codex-mcp"),
        Some(&package_root.join("package-manifest.json")),
        true,
    )
}

fn package_identity_digest_for_paths(
    operations_path: &Path,
    sidecar_path: &Path,
    manifest_path: Option<&Path>,
    require_package_files: bool,
) -> Result<String, SetupError> {
    #[derive(Serialize)]
    struct PackageIdentity {
        operations: FileState,
        sidecar: FileState,
        manifest: FileState,
    }

    let identity = PackageIdentity {
        operations: file_state(operations_path, MAX_OPERATIONS_BINARY_BYTES)
            .map_err(|_| SetupError::PlanInputsChanged)?,
        sidecar: file_state(sidecar_path, MAX_OPERATIONS_BINARY_BYTES)
            .map_err(|_| SetupError::PlanInputsChanged)?,
        manifest: if let Some(manifest_path) = manifest_path {
            file_state(manifest_path, MAX_PLAN_BYTES).map_err(|_| SetupError::PlanInputsChanged)?
        } else {
            FileState {
                exists: false,
                digest: MISSING_DIGEST.to_owned(),
                mode: None,
            }
        },
    };
    if require_package_files
        && (!identity.operations.exists || !identity.sidecar.exists || !identity.manifest.exists)
    {
        return Err(SetupError::PlanInputsChanged);
    }
    let bytes = serde_json::to_vec(&identity).map_err(|_| SetupError::PlanInputsChanged)?;
    Ok(format!("sha256:{}", sha256(&bytes)))
}

fn project_path(root: &Path, relative: &str) -> Result<PathBuf, SetupError> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(SetupError::PathUnsafe);
    }
    Ok(root.join(relative))
}

fn file_state(path: &Path, max: u64) -> Result<FileState, SetupError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(SetupError::PathUnsafe);
            }
            if metadata.len() > max {
                return Err(SetupError::InputTooLarge);
            }
            let bytes = fs::read(path).map_err(|_| SetupError::PathUnsafe)?;
            if bytes.len() as u64 > max {
                return Err(SetupError::InputTooLarge);
            }
            Ok(FileState {
                exists: true,
                digest: format!("sha256:{}", sha256(&bytes)),
                mode: Some(file_mode(&metadata)),
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(FileState {
            exists: false,
            digest: MISSING_DIGEST.to_owned(),
            mode: None,
        }),
        Err(_) => Err(SetupError::PathUnsafe),
    }
}

fn desired_mode(before: &FileState, private: bool) -> u32 {
    before.mode.unwrap_or(if private { 0o600 } else { 0o644 })
}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> u32 {
    0o644
}

fn existing_mode(path: &Path) -> Result<u32, SetupError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| SetupError::PathUnsafe)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SetupError::PathUnsafe);
    }
    Ok(file_mode(&metadata))
}

fn valid_file_mode(mode: u32) -> bool {
    mode <= 0o777
}

const fn private_file_mode() -> u32 {
    if cfg!(unix) { 0o600 } else { 0o644 }
}

fn read_utf8(path: &Path, max: u64) -> Result<String, SetupError> {
    let bytes = read_bounded(path, max).map_err(|error| match error {
        ReadError::TooLarge => SetupError::InputTooLarge,
        ReadError::Missing | ReadError::Unsafe | ReadError::Io => SetupError::PathUnsafe,
    })?;
    String::from_utf8(bytes).map_err(|_| SetupError::ConfigInvalid)
}

enum ReadError {
    Missing,
    Unsafe,
    TooLarge,
    Io,
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, ReadError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ReadError::Missing
        } else {
            ReadError::Io
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ReadError::Unsafe);
    }
    if metadata.len() > max {
        return Err(ReadError::TooLarge);
    }
    let bytes = fs::read(path).map_err(|_| ReadError::Io)?;
    if bytes.len() as u64 > max {
        return Err(ReadError::TooLarge);
    }
    Ok(bytes)
}

fn plan_digest(plan: &SetupPlan) -> Result<String, SetupError> {
    let bytes = serde_json::to_vec(plan).map_err(|_| SetupError::PlanDigestInvalid)?;
    Ok(format!("sha256:{}", sha256(&bytes)))
}

fn parse_digest(value: &str) -> Result<&str, SetupError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(SetupError::PlanDigestInvalid);
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SetupError::PlanDigestInvalid);
    }
    Ok(hex)
}

fn historical_package_version_is_supported(value: &str) -> bool {
    if value.is_empty() || value.len() > 64 || !value.is_ascii() {
        return false;
    }
    let mut build_split = value.split('+');
    let Some(core_and_pre) = build_split.next() else {
        return false;
    };
    let build = build_split.next();
    if build_split.next().is_some()
        || build.is_some_and(|identifiers| !valid_semver_identifiers(identifiers, false))
    {
        return false;
    }
    let (core, pre) = core_and_pre
        .split_once('-')
        .map_or((core_and_pre, None), |(core, pre)| (core, Some(pre)));
    if pre.is_some_and(|identifiers| !valid_semver_identifiers(identifiers, true)) {
        return false;
    }
    let mut numbers = core.split('.');
    let valid_core = (0..3).all(|_| numbers.next().is_some_and(valid_semver_number));
    valid_core && numbers.next().is_none()
}

fn valid_semver_number(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn valid_semver_identifiers(value: &str, reject_leading_zero_numeric: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && (!reject_leading_zero_numeric
                    || !identifier.bytes().all(|byte| byte.is_ascii_digit())
                    || valid_semver_number(identifier))
        })
}

fn config_table_stanza(value: &str) -> Result<Option<&str>, SetupError> {
    let document = value
        .parse::<DocumentMut>()
        .map_err(|_| SetupError::ConfigInvalid)?;
    if config_table_item(&document).is_none() {
        return Ok(None);
    }
    let marker = "[mcp_servers.godot_editor]";
    let mut offset = 0_usize;
    let mut start = None;
    let mut end = value.len();
    for line in value.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if start.is_none() {
            if trimmed == marker {
                start = Some(offset);
            }
        } else if trimmed.starts_with('[') {
            end = offset;
            break;
        }
        offset += line.len();
    }
    let start = start.ok_or(SetupError::ConfigInvalid)?;
    value
        .get(start..end)
        .map(Some)
        .ok_or(SetupError::ConfigInvalid)
}

fn digest_text(value: &str) -> String {
    format!("sha256:{}", sha256(value.as_bytes()))
}

fn config_stanza_digest(value: &str) -> String {
    digest_text(value.trim_end_matches([' ', '\t', '\r', '\n']))
}

fn new_config_ownership_marker() -> Result<String, SetupError> {
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|_| SetupError::PlanStoreUnavailable)?;
    Ok(format!("sha256:{}", sha256(&random)))
}

fn config_ownership_marker(stanza: &str) -> Option<String> {
    let mut markers = stanza.lines().filter_map(|line| {
        line.trim()
            .strip_prefix(CONFIG_OWNERSHIP_PREFIX)
            .map(str::to_owned)
    });
    let marker = markers.next()?;
    if markers.next().is_some() || parse_digest(&marker).is_err() {
        return None;
    }
    Some(marker)
}

fn redact_launcher_from_stanza(value: &str) -> String {
    let mut redacted = String::with_capacity(value.len().min(4096));
    for line in value.lines() {
        if let Some((key, _)) = line.split_once('=') {
            match key.trim() {
                "command" => {
                    redacted.push_str("command = \"<package-launcher>\"\n");
                    continue;
                }
                "cwd" => {
                    redacted.push_str("cwd = \"<project-root>\"\n");
                    continue;
                }
                "env" => {
                    redacted
                        .push_str("env = { GODOT_CODEX_DATA_ROOT = \"<package-data-root>\" }\n");
                    continue;
                }
                _ => {}
            }
        }
        redacted.push_str(line);
        redacted.push('\n');
    }
    redacted
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn now_epoch_seconds() -> Result<u64, SetupError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| SetupError::PlanStoreUnavailable)
}

fn canonical_agents_block() -> String {
    let guidance = CANONICAL_AGENTS_GUIDANCE.trim();
    format!("{AGENTS_BEGIN}\n{guidance}\n{AGENTS_END}\n")
}

fn extract_agents_block(value: &str) -> Result<Option<&str>, SetupError> {
    let begin_count = value.match_indices(AGENTS_BEGIN).count();
    let end_count = value.match_indices(AGENTS_END).count();
    match (begin_count, end_count) {
        (0, 0) => Ok(None),
        (1, 1) => {
            let start = value
                .find(AGENTS_BEGIN)
                .ok_or(SetupError::GuidanceConflict)?;
            let end_marker = value.find(AGENTS_END).ok_or(SetupError::GuidanceConflict)?;
            if end_marker < start {
                return Err(SetupError::GuidanceConflict);
            }
            let end = end_marker + AGENTS_END.len();
            let end = if value.as_bytes().get(end) == Some(&b'\n') {
                end + 1
            } else {
                end
            };
            Ok(Some(&value[start..end]))
        }
        _ => Err(SetupError::GuidanceConflict),
    }
}

fn append_agents_block(current: &str, block: &str) -> (String, String) {
    if current.is_empty() {
        return (block.to_owned(), String::new());
    }
    let separator = if current.ends_with('\n') { "" } else { "\n" };
    (format!("{current}{separator}{block}"), separator.to_owned())
}

fn replace_agents_block(current: &str, block: &str) -> Result<String, SetupError> {
    let old = extract_agents_block(current)?.ok_or(SetupError::GuidanceConflict)?;
    Ok(current.replacen(old, block, 1))
}

fn remove_agents_block(current: &str, separator: &str) -> Result<String, SetupError> {
    if !matches!(separator, "" | "\n") {
        return Err(SetupError::ReceiptInvalid);
    }
    let block = extract_agents_block(current)?.ok_or(SetupError::GuidanceConflict)?;
    let start = current.find(block).ok_or(SetupError::GuidanceConflict)?;
    let separator_start = start
        .checked_sub(separator.len())
        .ok_or(SetupError::OwnershipConflict)?;
    if current.get(separator_start..start) != Some(separator) {
        return Err(SetupError::OwnershipConflict);
    }
    let end = start + block.len();
    Ok(format!(
        "{}{}",
        &current[..separator_start],
        &current[end..]
    ))
}

fn safe_diff(path: &str, before: &str, after: &str) -> Result<String, SetupError> {
    let mut diff = format!("--- a/{path}\n+++ b/{path}\n@@ owned content @@\n");
    for line in before.lines() {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
        if diff.len() > MAX_DIFF_BYTES {
            return Err(SetupError::InputTooLarge);
        }
    }
    for line in after.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
        if diff.len() > MAX_DIFF_BYTES {
            return Err(SetupError::InputTooLarge);
        }
    }
    Ok(diff)
}

fn ensure_output_bound(value: &str, max: u64) -> Result<(), SetupError> {
    (value.len() as u64 <= max)
        .then_some(())
        .ok_or(SetupError::InputTooLarge)
}

#[cfg(test)]
thread_local! {
    static ATOMIC_WRITE_FAULT: std::cell::Cell<Option<usize>> = const {
        std::cell::Cell::new(None)
    };
    static POST_RENAME_SYNC_FAULT: std::cell::Cell<Option<usize>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
fn inject_atomic_write_fault(after_calls: usize) {
    ATOMIC_WRITE_FAULT.with(|fault| fault.set(Some(after_calls)));
}

#[cfg(test)]
fn inject_post_rename_sync_fault(after_renames: usize) {
    POST_RENAME_SYNC_FAULT.with(|fault| fault.set(Some(after_renames)));
}

fn maybe_inject_atomic_write_fault() -> Result<(), SetupError> {
    #[cfg(test)]
    {
        ATOMIC_WRITE_FAULT.with(|fault| match fault.get() {
            Some(0) => {
                fault.set(None);
                Err(SetupError::AtomicWriteFailed)
            }
            Some(remaining) => {
                fault.set(Some(remaining - 1));
                Ok(())
            }
            None => Ok(()),
        })
    }
    #[cfg(not(test))]
    Ok(())
}

fn maybe_inject_post_rename_sync_fault() -> Result<(), SetupError> {
    #[cfg(test)]
    {
        POST_RENAME_SYNC_FAULT.with(|fault| match fault.get() {
            Some(0) => {
                fault.set(None);
                Err(SetupError::AtomicWriteFailed)
            }
            Some(remaining) => {
                fault.set(Some(remaining - 1));
                Ok(())
            }
            None => Ok(()),
        })
    }
    #[cfg(not(test))]
    Ok(())
}

fn atomic_write(
    root: &Path,
    target: &Path,
    bytes: &[u8],
    private: bool,
    mode: u32,
) -> Result<(), SetupError> {
    if !valid_file_mode(mode) {
        return Err(SetupError::PathUnsafe);
    }
    maybe_inject_atomic_write_fault()?;
    ensure_safe_parent(root, target, private)?;
    if let Ok(metadata) = fs::symlink_metadata(target)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(SetupError::PathUnsafe);
    }
    #[cfg(unix)]
    {
        atomic_write_at(root, target, bytes, mode)
    }
    #[cfg(not(unix))]
    {
        atomic_write_path(target, bytes, private)
    }
}

#[cfg(unix)]
fn atomic_write_at(root: &Path, target: &Path, bytes: &[u8], mode: u32) -> Result<(), SetupError> {
    use rustix::fs::{AtFlags, Mode, OFlags};

    let (parent, file_name) = open_parent_directory(root, target)?;
    if let Ok(stat) = rustix::fs::statat(&parent, &file_name, AtFlags::SYMLINK_NOFOLLOW)
        && !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_file()
    {
        return Err(SetupError::PathUnsafe);
    }
    let file_name_text = file_name.to_str().ok_or(SetupError::PathUnsafe)?;
    for nonce in 0_u8..16 {
        let temporary_name = format!(
            ".{file_name_text}.godot-codex-tmp-{}-{nonce}",
            std::process::id()
        );
        let descriptor = match rustix::fs::openat(
            &parent,
            temporary_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(mode as _),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::EXIST) => continue,
            Err(_) => return Err(SetupError::AtomicWriteFailed),
        };
        let mut file = File::from(descriptor);
        if rustix::fs::fchmod(&file, Mode::from_raw_mode(mode as _)).is_err()
            || file.write_all(bytes).is_err()
            || file.sync_all().is_err()
        {
            let _ = rustix::fs::unlinkat(&parent, temporary_name.as_str(), AtFlags::empty());
            return Err(SetupError::AtomicWriteFailed);
        }
        drop(file);
        if let Ok(stat) = rustix::fs::statat(&parent, &file_name, AtFlags::SYMLINK_NOFOLLOW)
            && !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_file()
        {
            let _ = rustix::fs::unlinkat(&parent, temporary_name.as_str(), AtFlags::empty());
            return Err(SetupError::PathUnsafe);
        }
        if rustix::fs::renameat(&parent, temporary_name.as_str(), &parent, &file_name).is_err() {
            let _ = rustix::fs::unlinkat(&parent, temporary_name.as_str(), AtFlags::empty());
            return Err(SetupError::AtomicWriteFailed);
        }
        maybe_inject_post_rename_sync_fault()?;
        parent
            .sync_all()
            .map_err(|_| SetupError::AtomicWriteFailed)?;
        return Ok(());
    }
    Err(SetupError::AtomicWriteFailed)
}

#[cfg(unix)]
fn open_parent_directory(
    root: &Path,
    target: &Path,
) -> Result<(File, std::ffi::OsString), SetupError> {
    use rustix::fs::{Mode, OFlags};
    use std::os::unix::fs::MetadataExt;

    let relative = target
        .strip_prefix(root)
        .map_err(|_| SetupError::PathUnsafe)?;
    let file_name = relative
        .file_name()
        .ok_or(SetupError::PathUnsafe)?
        .to_os_string();
    let parent = relative.parent().ok_or(SetupError::PathUnsafe)?;
    let expected_root = directory_identity(root)?;
    let mut directory = File::open(root).map_err(|_| SetupError::PathUnsafe)?;
    let opened_root = directory.metadata().map_err(|_| SetupError::PathUnsafe)?;
    if opened_root.dev() != expected_root.device || opened_root.ino() != expected_root.inode {
        return Err(SetupError::PathUnsafe);
    }
    for component in parent.components() {
        let Component::Normal(name) = component else {
            return Err(SetupError::PathUnsafe);
        };
        let descriptor = rustix::fs::openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| SetupError::PathUnsafe)?;
        directory = File::from(descriptor);
    }
    Ok((directory, file_name))
}

#[cfg(not(unix))]
fn atomic_write_path(target: &Path, bytes: &[u8], private: bool) -> Result<(), SetupError> {
    let parent = target.parent().ok_or(SetupError::PathUnsafe)?;
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(SetupError::PathUnsafe)?;
    let mut last_error = None;
    for nonce in 0_u8..16 {
        let temporary = parent.join(format!(
            ".{file_name}.godot-codex-tmp-{}-{nonce}",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(mut file) => {
                set_file_mode(&temporary, private).map_err(|_| SetupError::AtomicWriteFailed)?;
                if file.write_all(bytes).is_err() || file.sync_all().is_err() {
                    let _ = fs::remove_file(&temporary);
                    return Err(SetupError::AtomicWriteFailed);
                }
                if fs::rename(&temporary, target).is_err() {
                    let _ = fs::remove_file(&temporary);
                    return Err(SetupError::AtomicWriteFailed);
                }
                maybe_inject_post_rename_sync_fault()?;
                sync_directory(parent).map_err(|_| SetupError::AtomicWriteFailed)?;
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                last_error = Some(error);
            }
            Err(_) => return Err(SetupError::AtomicWriteFailed),
        }
    }
    let _ = last_error;
    Err(SetupError::AtomicWriteFailed)
}

fn ensure_safe_parent(root: &Path, target: &Path, private: bool) -> Result<(), SetupError> {
    if !target.starts_with(root) {
        return Err(SetupError::PathUnsafe);
    }
    let parent = target.parent().ok_or(SetupError::PathUnsafe)?;
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| SetupError::PathUnsafe)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(SetupError::PathUnsafe);
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(SetupError::PathUnsafe);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|_| SetupError::AtomicWriteFailed)?;
                if private || current.ends_with(".godot/codex") {
                    set_private_directory(&current).map_err(|_| SetupError::AtomicWriteFailed)?;
                } else {
                    set_public_directory(&current).map_err(|_| SetupError::AtomicWriteFailed)?;
                }
            }
            Err(_) => return Err(SetupError::PathUnsafe),
        }
    }
    if private && parent.ends_with(".godot/codex") {
        let metadata = fs::symlink_metadata(parent).map_err(|_| SetupError::PathUnsafe)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o777 != 0o700 {
                return Err(SetupError::PathUnsafe);
            }
        }
    }
    Ok(())
}

fn validate_safe_parent(root: &Path, target: &Path, private: bool) -> Result<(), SetupError> {
    if !target.starts_with(root) {
        return Err(SetupError::PathUnsafe);
    }
    let parent = target.parent().ok_or(SetupError::PathUnsafe)?;
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| SetupError::PathUnsafe)?;
    let mut current = root.to_path_buf();
    let mut ancestor_missing = false;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(SetupError::PathUnsafe);
        };
        current.push(name);
        if ancestor_missing {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(SetupError::PathUnsafe);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ancestor_missing = true;
            }
            Err(_) => return Err(SetupError::PathUnsafe),
        }
    }
    if private && !ancestor_missing && parent.ends_with(".godot/codex") {
        let metadata = fs::symlink_metadata(parent).map_err(|_| SetupError::PathUnsafe)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o777 != 0o700 {
                return Err(SetupError::PathUnsafe);
            }
        }
    }
    Ok(())
}

fn remove_plain_file(root: &Path, path: &Path) -> Result<(), SetupError> {
    maybe_inject_atomic_write_fault()?;
    ensure_safe_parent(root, path, false)?;
    #[cfg(unix)]
    {
        use rustix::fs::AtFlags;

        let (parent, file_name) = open_parent_directory(root, path)?;
        let stat = rustix::fs::statat(&parent, &file_name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| SetupError::PathUnsafe)?;
        if !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_file() {
            return Err(SetupError::PathUnsafe);
        }
        rustix::fs::unlinkat(&parent, &file_name, AtFlags::empty())
            .map_err(|_| SetupError::AtomicWriteFailed)?;
        maybe_inject_post_rename_sync_fault()?;
        parent
            .sync_all()
            .map_err(|_| SetupError::AtomicWriteFailed)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let metadata = fs::symlink_metadata(path).map_err(|_| SetupError::PathUnsafe)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SetupError::PathUnsafe);
        }
        fs::remove_file(path).map_err(|_| SetupError::AtomicWriteFailed)?;
        maybe_inject_post_rename_sync_fault()?;
        if let Some(parent) = path.parent() {
            sync_directory(parent).map_err(|_| SetupError::AtomicWriteFailed)?;
        }
        Ok(())
    }
}

fn write_new_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    set_file_mode(path, true)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_public_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_public_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(path: &Path, private: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if private { 0o600 } else { 0o644 }),
    )
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _private: bool) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn owned_by_current_user(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.uid() == rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn owned_by_current_user(_metadata: &fs::Metadata) -> bool {
    true
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn project() -> TempDir {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("project.godot"), "[application]\n").unwrap();
        temp
    }

    fn options(project: &TempDir, store: &TempDir) -> SetupOptions {
        let mut options = SetupOptions::new(project.path(), SetupProfile::ReadOnly);
        options.plan_store = Some(store.path().join("plans"));
        let package = write_test_package(store.path(), "package", b"test-sidecar");
        options.package_directory = Some(package);
        options
    }

    fn write_test_package(root: &Path, name: &str, sidecar_bytes: &[u8]) -> PathBuf {
        let package = root.join(name);
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::write(
            package.join("package-manifest.json"),
            format!(r#"{{"package_version":"{PRODUCT_VERSION}"}}"#),
        )
        .unwrap();
        let sidecar = package.join("bin/godot-codex-mcp");
        fs::write(&sidecar, sidecar_bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o755)).unwrap();
        }
        package
    }

    fn historical_receipt(project: &TempDir, package_version: &str) -> SetupReceipt {
        let path = project.path().join(RECEIPT_PATH);
        let mut receipt: SetupReceipt = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        receipt.package_version = package_version.to_owned();
        fs::write(&path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
        receipt
    }

    fn assert_receipt_uses_current_package(project: &TempDir, options: &SetupOptions) {
        let receipt: SetupReceipt =
            serde_json::from_slice(&fs::read(project.path().join(RECEIPT_PATH)).unwrap()).unwrap();
        let package = options.package_directory.as_deref();
        let launcher = resolve_launcher(package).unwrap();
        assert_eq!(receipt.package_version, PRODUCT_VERSION);
        assert_eq!(
            receipt.package_identity_digest,
            current_package_identity_digest(package).unwrap()
        );
        assert_eq!(receipt.launcher_path_sha256, launcher.path_digest());
        assert_eq!(receipt.launcher_file_sha256, launcher.file_digest());
        let config = fs::read_to_string(project.path().join(CONFIG_PATH)).unwrap();
        let stanza = config_table_stanza(&config).unwrap().unwrap();
        assert_eq!(config_stanza_digest(stanza), receipt.config_table_digest);
        assert_eq!(
            config_ownership_marker(stanza).as_deref(),
            Some(receipt.config_ownership_marker.as_str())
        );
    }

    #[test]
    fn surface_capture_verifier_binds_exact_setup_owned_files_and_package() {
        let project = project();
        let store = TempDir::new().unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();

        let canonical = fs::canonicalize(project.path()).unwrap();
        let project_id = project_id_for_path(&canonical).unwrap();
        let launcher = resolve_launcher(options.package_directory.as_deref()).unwrap();
        let verified = verify_surface_capture_setup_state(
            &canonical,
            &project_id,
            &launcher,
            options.package_directory.as_deref(),
        )
        .unwrap();
        assert_eq!(
            verified.config,
            fs::read(canonical.join(CONFIG_PATH)).unwrap()
        );
        assert_eq!(
            verified.receipt,
            fs::read(canonical.join(RECEIPT_PATH)).unwrap()
        );

        let config_path = canonical.join(CONFIG_PATH);
        let original = fs::read_to_string(&config_path).unwrap();
        fs::write(
            &config_path,
            original.replace("tool_timeout_sec = 60", "tool_timeout_sec = 61"),
        )
        .unwrap();
        assert!(
            verify_surface_capture_setup_state(
                &canonical,
                &project_id,
                &launcher,
                options.package_directory.as_deref(),
            )
            .is_err()
        );
    }

    fn repair_options(options: &SetupOptions) -> RepairOptions {
        RepairOptions {
            project_root: options.project_root.clone(),
            plan_store: options.plan_store.clone(),
            package_directory: options.package_directory.clone(),
        }
    }

    fn remove_options(options: &SetupOptions) -> RemoveOptions {
        RemoveOptions {
            project_root: options.project_root.clone(),
            plan_store: options.plan_store.clone(),
            package_directory: options.package_directory.clone(),
        }
    }

    fn persisted_plan(options: &SetupOptions, digest: &str) -> SetupPlan {
        let hex = digest.strip_prefix("sha256:").unwrap();
        let store = options.plan_store.as_ref().unwrap();
        let path = [".json", ".claimed", ".committed"]
            .into_iter()
            .map(|suffix| store.join(format!("{hex}{suffix}")))
            .find(|path| path.is_file())
            .unwrap();
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn write_pending_journal(project: &TempDir, journal: &SetupJournal) -> Vec<u8> {
        write_pending_journal_bytes(project, serde_json::to_vec(journal).unwrap())
    }

    fn write_pending_journal_bytes(project: &TempDir, bytes: Vec<u8>) -> Vec<u8> {
        let path = project.path().join(JOURNAL_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        set_private_directory(path.parent().unwrap()).unwrap();
        fs::write(&path, &bytes).unwrap();
        set_file_mode(&path, true).unwrap();
        bytes
    }

    fn project_tree_snapshot(root: &Path) -> Vec<(String, String, u32)> {
        fn visit(root: &Path, directory: &Path, output: &mut Vec<(String, String, u32)>) {
            let mut entries = fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap())
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let metadata = fs::symlink_metadata(&path).unwrap();
                if metadata.file_type().is_symlink() {
                    output.push((
                        relative,
                        format!(
                            "symlink:sha256:{}",
                            sha256(fs::read_link(&path).unwrap().as_os_str().as_encoded_bytes())
                        ),
                        file_mode(&metadata),
                    ));
                } else if metadata.is_dir() {
                    output.push((relative, "directory".to_owned(), file_mode(&metadata)));
                    visit(root, &path, output);
                } else {
                    output.push((
                        relative,
                        format!("file:sha256:{}", sha256(&fs::read(&path).unwrap())),
                        file_mode(&metadata),
                    ));
                }
            }
        }

        let mut output = Vec::new();
        visit(root, root, &mut output);
        output
    }

    fn assert_forged_journal_rejected(
        mutate: impl FnOnce(&mut SetupJournal),
    ) -> (SetupPlan, SetupJournal) {
        let project = project();
        let store = TempDir::new().unwrap();
        let mut options = options(&project, &store);
        options.guidance = GuidanceMode::All;
        let preview = prepare_setup(&options).unwrap();
        let plan = persisted_plan(&options, &preview.plan_digest);
        let mut journal = build_apply_journal(&plan, &preview.plan_digest).unwrap();
        mutate(&mut journal);
        let journal_bytes = write_pending_journal(&project, &journal);
        let before = plan
            .changes
            .iter()
            .map(|change| {
                let path = project.path().join(&change.path);
                (
                    change.path.clone(),
                    file_state(&path, managed_file_limit(&change.path)).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let receipt_before =
            file_state(&project.path().join(RECEIPT_PATH), MAX_RECEIPT_BYTES).unwrap();

        assert_eq!(
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()),
            Err(SetupError::TransactionRecoveryRequired)
        );
        assert_eq!(
            fs::read(project.path().join(JOURNAL_PATH)).unwrap(),
            journal_bytes
        );
        assert!(!project.path().join(OPERATION_LOCK_PATH).exists());
        for (path, expected) in before {
            assert_eq!(
                file_state(&project.path().join(&path), managed_file_limit(&path)).unwrap(),
                expected
            );
        }
        assert_eq!(
            file_state(&project.path().join(RECEIPT_PATH), MAX_RECEIPT_BYTES).unwrap(),
            receipt_before
        );
        (plan, journal)
    }

    fn apply_remove(options: &SetupOptions) -> SetupReport {
        let remove = remove_options(options);
        let preview = prepare_setup_remove(&remove).unwrap();
        apply_setup_plan(&preview.plan_digest, remove.plan_store.as_deref()).unwrap()
    }

    fn installed(
        profile: SetupProfile,
        guidance: GuidanceMode,
    ) -> (TempDir, TempDir, SetupOptions) {
        let project = project();
        let store = TempDir::new().unwrap();
        let mut options = options(&project, &store);
        options.profile = profile;
        options.guidance = guidance;
        let preview = prepare_setup(&options).unwrap();
        apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
        (project, store, options)
    }

    fn drift_timeout(project: &TempDir) {
        let path = project.path().join(CONFIG_PATH);
        let current = fs::read_to_string(&path).unwrap();
        let drifted = current.replacen("startup_timeout_sec = 10", "startup_timeout_sec = 99", 1);
        assert_ne!(drifted, current);
        fs::write(path, drifted).unwrap();
    }

    fn assert_repair_ownership_conflict(project: &TempDir, setup: &SetupOptions) {
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        let config_before = fs::read(&config_path).unwrap();
        let receipt_before = fs::read(&receipt_path).unwrap();
        let receipt: SetupReceipt = serde_json::from_slice(&receipt_before).unwrap();
        let config_text = std::str::from_utf8(&config_before).unwrap();
        let stanza = config_table_stanza(config_text).unwrap().unwrap();
        assert_eq!(
            config_ownership_marker(stanza).as_deref(),
            Some(receipt.config_ownership_marker.as_str())
        );

        assert_eq!(
            prepare_setup_repair(&repair_options(setup)),
            Err(SetupError::OwnershipConflict)
        );
        assert_eq!(fs::read(&config_path).unwrap(), config_before);
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt_before);
        assert!(!project.path().join(JOURNAL_PATH).exists());
    }

    #[test]
    fn prepare_is_project_read_only_and_apply_is_digest_bound() {
        let project = project();
        let store = TempDir::new().unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        assert!(!project.path().join(CONFIG_PATH).exists());
        assert!(!project.path().join(RECEIPT_PATH).exists());
        assert_eq!(preview.plan_digest.len(), 71);
        let result = apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
        assert_eq!(result.status, "applied");
        assert!(project.path().join(CONFIG_PATH).is_file());
        assert!(project.path().join(RECEIPT_PATH).is_file());
        let config_after = fs::read(project.path().join(CONFIG_PATH)).unwrap();
        let receipt_after = fs::read(project.path().join(RECEIPT_PATH)).unwrap();
        let receipt_text = fs::read_to_string(project.path().join(RECEIPT_PATH)).unwrap();
        let receipt: SetupReceipt = serde_json::from_str(&receipt_text).unwrap();
        assert!(receipt.launcher_path_sha256.starts_with("sha256:"));
        assert!(receipt.launcher_file_sha256.starts_with("sha256:"));
        assert!(
            !receipt_text.contains(
                options
                    .package_directory
                    .as_ref()
                    .unwrap()
                    .to_str()
                    .unwrap()
            )
        );
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref())
                .unwrap()
                .status,
            "already_applied"
        );
        assert_eq!(
            fs::read(project.path().join(CONFIG_PATH)).unwrap(),
            config_after
        );
        assert_eq!(
            fs::read(project.path().join(RECEIPT_PATH)).unwrap(),
            receipt_after
        );
    }

    #[test]
    fn repair_closes_project_config_invalid_without_manual_toml() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let project = project();
        let store = TempDir::new().unwrap();
        fs::create_dir(project.path().join(".codex")).unwrap();
        let config_path = project.path().join(CONFIG_PATH);
        fs::write(
            &config_path,
            "# retained owner comment\nmodel = \"private-provider-marker\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640)).unwrap();

        let mut setup = options(&project, &store);
        setup.profile = SetupProfile::FullBeta;
        setup.guidance = GuidanceMode::All;
        let setup_preview = prepare_setup(&setup).unwrap();
        apply_setup_plan(&setup_preview.plan_digest, setup.plan_store.as_deref()).unwrap();
        drift_timeout(&project);
        fs::OpenOptions::new()
            .append(true)
            .open(&config_path)
            .unwrap()
            .write_all(b"\n[user]\nkeep = true\n")
            .unwrap();

        let mut doctor_options = crate::doctor::DoctorOptions::new(project.path());
        doctor_options.package_directory = setup.package_directory.clone();
        let invalid = crate::doctor::run_doctor(&doctor_options);
        let config_check = invalid
            .checks
            .iter()
            .find(|check| check.check_id == "project.config")
            .unwrap();
        assert_eq!(
            config_check.code,
            godot_codex_product::DiagnosticCode::ProjectConfigInvalid
        );
        assert_eq!(
            config_check.remediation_id,
            Some(godot_codex_product::RemediationId::RepairProjectConfig)
        );

        let before_config = fs::read(&config_path).unwrap();
        let receipt_path = project.path().join(RECEIPT_PATH);
        let before_receipt = fs::read(&receipt_path).unwrap();
        let before_mode = existing_mode(&config_path).unwrap();
        let repair = repair_options(&setup);
        let preview = prepare_setup_repair(&repair).unwrap();
        assert_eq!(preview.mode, SetupMode::Repair);
        assert_eq!(preview.profile, SetupProfile::FullBeta);
        assert_eq!(preview.guidance, GuidanceMode::All);
        assert_eq!(
            preview
                .changes
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            [CONFIG_PATH, RECEIPT_PATH]
        );
        let serialized = serde_json::to_string(&preview).unwrap();
        assert!(!serialized.contains("private-provider-marker"));
        assert!(!serialized.contains("startup_timeout_sec = 99"));
        assert!(!serialized.contains(project.path().to_str().unwrap()));
        assert!(!serialized.contains(setup.package_directory.as_ref().unwrap().to_str().unwrap()));
        assert_eq!(fs::read(&config_path).unwrap(), before_config);
        assert_eq!(fs::read(&receipt_path).unwrap(), before_receipt);

        let report = apply_setup_plan(&preview.plan_digest, repair.plan_store.as_deref()).unwrap();
        assert_eq!(report.mode, SetupMode::Repair);
        assert_eq!(report.status, "repaired");
        assert_eq!(report.changed_paths, [CONFIG_PATH, RECEIPT_PATH]);
        let repaired = fs::read_to_string(&config_path).unwrap();
        assert!(repaired.contains("# retained owner comment"));
        assert!(repaired.contains("private-provider-marker"));
        assert!(repaired.contains("[user]"));
        assert!(repaired.contains("keep = true"));
        assert!(repaired.contains("startup_timeout_sec = 10"));
        assert!(!repaired.contains("startup_timeout_sec = 99"));
        assert_eq!(existing_mode(&config_path).unwrap(), before_mode);
        let refreshed: SetupReceipt =
            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        assert_eq!(refreshed.plan_digest, preview.plan_digest);
        assert_ne!(fs::read(&receipt_path).unwrap(), before_receipt);

        let ready = crate::doctor::run_doctor(&doctor_options);
        let config_check = ready
            .checks
            .iter()
            .find(|check| check.check_id == "project.config")
            .unwrap();
        assert_eq!(
            config_check.code,
            godot_codex_product::DiagnosticCode::Ready
        );
    }

    #[test]
    fn repair_restores_a_missing_owned_stanza_and_rejects_healthy_state() {
        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let repair = repair_options(&setup);
        assert_eq!(
            prepare_setup_repair(&repair),
            Err(SetupError::RepairNotNeeded)
        );

        let config_path = project.path().join(CONFIG_PATH);
        let original = fs::read_to_string(&config_path).unwrap();
        let mut document = original.parse::<DocumentMut>().unwrap();
        remove_config_table(&mut document).unwrap();
        fs::write(&config_path, document.to_string()).unwrap();
        let preview = prepare_setup_repair(&repair).unwrap();
        assert!(
            preview
                .changes
                .iter()
                .find(|change| change.path == CONFIG_PATH)
                .unwrap()
                .diff
                .contains("<missing receipt-owned stanza>")
        );
        apply_setup_plan(&preview.plan_digest, repair.plan_store.as_deref()).unwrap();
        let repaired = fs::read_to_string(config_path).unwrap();
        assert!(config_table_stanza(&repaired).unwrap().is_some());
    }

    #[test]
    fn both_profiles_have_exact_tool_membership_and_root_semantics() {
        for profile in [SetupProfile::ReadOnly, SetupProfile::FullBeta] {
            let project = project();
            let store = TempDir::new().unwrap();
            let mut options = options(&project, &store);
            options.profile = profile;
            let preview = prepare_setup(&options).unwrap();
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
            let text = fs::read_to_string(project.path().join(CONFIG_PATH)).unwrap();
            let document = text.parse::<DocumentMut>().unwrap();
            let table = config_table_item(&document).unwrap();
            let expected_launcher = fs::canonicalize(
                options
                    .package_directory
                    .as_ref()
                    .unwrap()
                    .join("bin/godot-codex-mcp"),
            )
            .unwrap();
            assert_eq!(
                table["command"].as_str().map(Path::new),
                Some(expected_launcher.as_path())
            );
            let canonical_root = fs::canonicalize(project.path()).unwrap();
            assert_eq!(table["cwd"].as_str(), canonical_root.to_str());
            let args = table["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(args, ["--project-root", "."]);
            let tools = table["enabled_tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<Vec<_>>();
            let expected = match profile {
                SetupProfile::ReadOnly => READ_ONLY_TOOLS,
                SetupProfile::FullBeta => FULL_BETA_TOOLS,
            };
            assert_eq!(tools, expected);
            assert_eq!(
                table
                    .get("default_tools_approval_mode")
                    .and_then(Item::as_str),
                (profile == SetupProfile::FullBeta).then_some("writes")
            );
        }
    }

    #[test]
    fn installed_config_carries_and_redacts_the_exact_data_root() {
        let item = desired_config_item(
            SetupProfile::ReadOnly,
            "/private/package/current/bin/godot-codex-mcp",
            Some(Path::new("/private/package")),
            &format!("sha256:{}", "a".repeat(64)),
            Path::new("/private/project"),
        )
        .unwrap();
        let table = item.as_table().unwrap();
        assert_eq!(config_data_root(table), Some("/private/package"));
        let mut document = DocumentMut::new();
        install_config_table(&mut document, item).unwrap();
        let stanza = config_table_stanza(&document.to_string())
            .unwrap()
            .unwrap()
            .to_owned();
        let redacted = redact_launcher_from_stanza(&stanza);
        assert!(redacted.contains("GODOT_CODEX_DATA_ROOT = \"<package-data-root>\""));
        assert!(!redacted.contains("/private/package"));
        assert!(!redacted.contains("/private/project"));
    }

    #[test]
    fn unrelated_commented_toml_is_preserved_without_preview_leak() {
        let project = project();
        let store = TempDir::new().unwrap();
        fs::create_dir(project.path().join(".codex")).unwrap();
        fs::write(
            project.path().join(CONFIG_PATH),
            "# owner comment\nmodel = \"private-provider-marker\"\n",
        )
        .unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        let json = serde_json::to_string(&preview).unwrap();
        assert!(!json.contains("private-provider-marker"));
        assert!(!json.contains(project.path().to_str().unwrap()));
        assert!(
            !json.contains(
                options
                    .package_directory
                    .as_ref()
                    .unwrap()
                    .to_str()
                    .unwrap()
            )
        );
        assert!(json.contains("<package-launcher>"));
        apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
        let result = fs::read_to_string(project.path().join(CONFIG_PATH)).unwrap();
        assert!(result.contains("# owner comment"));
        assert!(result.contains("private-provider-marker"));
    }

    #[test]
    fn stale_digest_rejects_changed_inputs_before_any_write() {
        let project = project();
        let store = TempDir::new().unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        fs::create_dir(project.path().join(".codex")).unwrap();
        fs::write(project.path().join(CONFIG_PATH), "model = \"changed\"\n").unwrap();
        let before = project_tree_snapshot(project.path());
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()),
            Err(SetupError::PlanInputsChanged)
        );
        assert_eq!(project_tree_snapshot(project.path()), before);
        assert!(!project.path().join(OPERATION_LOCK_PATH).exists());
        assert!(!project.path().join(RECEIPT_PATH).exists());
    }

    #[test]
    fn expired_plan_is_rejected_without_project_mutation() {
        let project = project();
        let store = TempDir::new().unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        let plan_store = options.plan_store.as_ref().unwrap();
        let original_path = plan_store.join(format!(
            "{}.json",
            preview.plan_digest.strip_prefix("sha256:").unwrap()
        ));
        let mut plan: SetupPlan =
            serde_json::from_slice(&fs::read(original_path).unwrap()).unwrap();
        plan.created_at_epoch_seconds = 0;
        plan.expires_at_epoch_seconds = PLAN_LIFETIME_SECONDS;
        let expired_digest = plan_digest(&plan).unwrap();
        persist_plan(plan_store, &expired_digest, &plan).unwrap();
        let before = project_tree_snapshot(project.path());
        assert_eq!(
            apply_setup_plan(&expired_digest, Some(plan_store)),
            Err(SetupError::PlanExpired)
        );
        assert_eq!(project_tree_snapshot(project.path()), before);
        assert!(!project.path().join(OPERATION_LOCK_PATH).exists());
        assert!(!project.path().join(CONFIG_PATH).exists());
        assert!(!project.path().join(RECEIPT_PATH).exists());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_receipt_parent_fails_preflight_before_config_write() {
        use std::os::unix::fs::PermissionsExt;

        let project = project();
        let store = TempDir::new().unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        fs::create_dir_all(project.path().join(".godot/codex")).unwrap();
        fs::set_permissions(
            project.path().join(".godot/codex"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()),
            Err(SetupError::PathUnsafe)
        );
        assert!(!project.path().join(CONFIG_PATH).exists());
        assert!(!project.path().join(RECEIPT_PATH).exists());
    }

    #[test]
    fn destination_parent_swap_fails_before_any_write() {
        let project = project();
        let store = TempDir::new().unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        fs::write(project.path().join(".codex"), "not a directory\n").unwrap();
        assert!(matches!(
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()),
            Err(SetupError::PathUnsafe | SetupError::PlanInputsChanged)
        ));
        assert!(!project.path().join(RECEIPT_PATH).exists());
    }

    #[test]
    fn wrong_or_truncated_digest_is_rejected() {
        assert_eq!(
            apply_setup_plan("sha256:abcd", None),
            Err(SetupError::PlanDigestInvalid)
        );
        assert_eq!(
            apply_setup_plan(&format!("sha256:{}", "A".repeat(64)), None),
            Err(SetupError::PlanDigestInvalid)
        );
    }

    #[test]
    fn every_guidance_mode_has_exact_ownership_behavior() {
        for guidance in [
            GuidanceMode::None,
            GuidanceMode::Agents,
            GuidanceMode::Skill,
            GuidanceMode::All,
        ] {
            let project = project();
            let store = TempDir::new().unwrap();
            let mut options = options(&project, &store);
            options.guidance = guidance;
            let preview = prepare_setup(&options).unwrap();
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
            assert_eq!(project.path().join(AGENTS_PATH).exists(), guidance.agents());
            assert_eq!(project.path().join(SKILL_PATH).exists(), guidance.skill());
            let report = apply_remove(&options);
            assert_eq!(report.status, "removed");
            assert!(!project.path().join(RECEIPT_PATH).exists());
        }
    }

    #[test]
    fn removal_plan_is_expiring_and_rejects_changed_config_receipt_and_package() {
        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let remove = remove_options(&setup);
        let preview = prepare_setup_remove(&remove).unwrap();
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        let receipt_before = fs::read(&receipt_path).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&config_path)
            .unwrap()
            .write_all(b"\n[user]\nkeep = true\n")
            .unwrap();
        let changed_config = fs::read(&config_path).unwrap();
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, remove.plan_store.as_deref()),
            Err(SetupError::PlanInputsChanged)
        );
        assert_eq!(fs::read(&config_path).unwrap(), changed_config);
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt_before);

        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let remove = remove_options(&setup);
        let preview = prepare_setup_remove(&remove).unwrap();
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        let config_before = fs::read(&config_path).unwrap();
        let mut receipt: SetupReceipt =
            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        receipt.plan_digest = format!("sha256:{}", "a".repeat(64));
        fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
        let changed_receipt = fs::read(&receipt_path).unwrap();
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, remove.plan_store.as_deref()),
            Err(SetupError::PlanInputsChanged)
        );
        assert_eq!(fs::read(&config_path).unwrap(), config_before);
        assert_eq!(fs::read(&receipt_path).unwrap(), changed_receipt);

        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let remove = remove_options(&setup);
        let preview = prepare_setup_remove(&remove).unwrap();
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        let config_before = fs::read(&config_path).unwrap();
        let receipt_before = fs::read(&receipt_path).unwrap();
        fs::write(
            remove
                .package_directory
                .as_ref()
                .unwrap()
                .join("bin/godot-codex-mcp"),
            b"changed-sidecar",
        )
        .unwrap();
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, remove.plan_store.as_deref()),
            Err(SetupError::PlanInputsChanged)
        );
        assert_eq!(fs::read(&config_path).unwrap(), config_before);
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt_before);

        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let remove = remove_options(&setup);
        let preview = prepare_setup_remove(&remove).unwrap();
        let plan_store = remove.plan_store.as_ref().unwrap();
        let original_path = plan_store.join(format!(
            "{}.json",
            preview.plan_digest.strip_prefix("sha256:").unwrap()
        ));
        let mut plan: SetupPlan =
            serde_json::from_slice(&fs::read(original_path).unwrap()).unwrap();
        plan.created_at_epoch_seconds = 0;
        plan.expires_at_epoch_seconds = PLAN_LIFETIME_SECONDS;
        let expired_digest = plan_digest(&plan).unwrap();
        persist_plan(plan_store, &expired_digest, &plan).unwrap();
        let config_before = fs::read(project.path().join(CONFIG_PATH)).unwrap();
        let receipt_before = fs::read(project.path().join(RECEIPT_PATH)).unwrap();
        assert_eq!(
            apply_setup_plan(&expired_digest, Some(plan_store)),
            Err(SetupError::PlanExpired)
        );
        assert_eq!(
            fs::read(project.path().join(CONFIG_PATH)).unwrap(),
            config_before
        );
        assert_eq!(
            fs::read(project.path().join(RECEIPT_PATH)).unwrap(),
            receipt_before
        );
    }

    #[test]
    fn remove_preserves_unrelated_config_and_agents_content() {
        let project = project();
        let store = TempDir::new().unwrap();
        fs::write(project.path().join(AGENTS_PATH), "# User policy\n").unwrap();
        let mut options = options(&project, &store);
        options.guidance = GuidanceMode::Agents;
        let preview = prepare_setup(&options).unwrap();
        apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(project.path().join(CONFIG_PATH))
            .unwrap()
            .write_all(b"\n[user]\nkeep = true\n")
            .unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(project.path().join(AGENTS_PATH))
            .unwrap()
            .write_all(b"\n# Later user policy\n")
            .unwrap();
        apply_remove(&options);
        let config = fs::read_to_string(project.path().join(CONFIG_PATH)).unwrap();
        assert!(config.contains("[user]"));
        assert!(!config.contains("godot_editor"));
        let agents = fs::read_to_string(project.path().join(AGENTS_PATH)).unwrap();
        assert!(agents.contains("# User policy"));
        assert!(agents.contains("# Later user policy"));
        assert!(!agents.contains(AGENTS_BEGIN));
    }

    #[test]
    fn exact_remove_plan_mutates_only_receipt_owned_artifacts_and_is_idempotent() {
        let project = project();
        let store = TempDir::new().unwrap();
        fs::write(project.path().join(AGENTS_PATH), "# User policy\n").unwrap();
        fs::write(project.path().join("keep.txt"), "never owned\n").unwrap();
        fs::create_dir(project.path().join(".codex")).unwrap();
        fs::write(
            project.path().join(CONFIG_PATH),
            "# user config\nmodel = \"keep\"\n",
        )
        .unwrap();
        let mut setup = options(&project, &store);
        setup.guidance = GuidanceMode::All;
        let setup_preview = prepare_setup(&setup).unwrap();
        apply_setup_plan(&setup_preview.plan_digest, setup.plan_store.as_deref()).unwrap();

        let remove = remove_options(&setup);
        let preview = prepare_setup_remove(&remove).unwrap();
        assert_eq!(preview.mode, SetupMode::Remove);
        assert_eq!(
            preview
                .changes
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            [SKILL_PATH, CONFIG_PATH, RECEIPT_PATH, AGENTS_PATH]
        );
        let report = apply_setup_plan(&preview.plan_digest, remove.plan_store.as_deref()).unwrap();
        assert_eq!(report.status, "removed");
        assert_eq!(
            fs::read_to_string(project.path().join("keep.txt")).unwrap(),
            "never owned\n"
        );
        assert_eq!(
            fs::read_to_string(project.path().join(CONFIG_PATH)).unwrap(),
            "# user config\nmodel = \"keep\"\n"
        );
        assert_eq!(
            fs::read_to_string(project.path().join(AGENTS_PATH)).unwrap(),
            "# User policy\n"
        );
        assert!(!project.path().join(SKILL_PATH).exists());
        assert!(!project.path().join(RECEIPT_PATH).exists());
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, remove.plan_store.as_deref())
                .unwrap()
                .status,
            "already_removed"
        );
    }

    #[test]
    fn remove_preserves_preexisting_empty_mcp_parent_and_comments() {
        let project = project();
        let store = TempDir::new().unwrap();
        fs::create_dir(project.path().join(".codex")).unwrap();
        fs::write(
            project.path().join(CONFIG_PATH),
            "# user server policy\n[mcp_servers]\n# keep this parent\n",
        )
        .unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
        apply_remove(&options);
        let config = fs::read_to_string(project.path().join(CONFIG_PATH)).unwrap();
        assert!(config.contains("# user server policy"));
        assert!(config.contains("[mcp_servers]"));
        assert!(config.contains("# keep this parent"));
        assert!(!config.contains("godot_editor"));
    }

    #[test]
    fn remove_fails_closed_after_owned_table_or_skill_edit() {
        for guidance in [GuidanceMode::None, GuidanceMode::Skill] {
            let project = project();
            let store = TempDir::new().unwrap();
            let mut options = options(&project, &store);
            options.guidance = guidance;
            let preview = prepare_setup(&options).unwrap();
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
            let path = if guidance == GuidanceMode::Skill {
                project.path().join(SKILL_PATH)
            } else {
                project.path().join(CONFIG_PATH)
            };
            fs::OpenOptions::new()
                .append(true)
                .open(path)
                .unwrap()
                .write_all(b"# user edit\n")
                .unwrap();
            assert_eq!(
                prepare_setup_remove(&remove_options(&options)),
                Err(SetupError::OwnershipConflict)
            );
            assert!(project.path().join(RECEIPT_PATH).exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_targets_fail_closed() {
        use std::os::unix::fs::symlink;

        let project = project();
        let store = TempDir::new().unwrap();
        let outside = store.path().join("outside.toml");
        fs::write(&outside, "outside = true\n").unwrap();
        fs::create_dir(project.path().join(".codex")).unwrap();
        symlink(&outside, project.path().join(CONFIG_PATH)).unwrap();
        let options = options(&project, &store);
        assert_eq!(prepare_setup(&options), Err(SetupError::PathUnsafe));
        assert_eq!(fs::read_to_string(outside).unwrap(), "outside = true\n");
    }

    #[test]
    fn existing_unowned_table_is_a_conflict_even_when_plausible() {
        let project = project();
        let store = TempDir::new().unwrap();
        fs::create_dir(project.path().join(".codex")).unwrap();
        let item = desired_config_item(
            SetupProfile::ReadOnly,
            "/package/launcher",
            None,
            &format!("sha256:{}", "a".repeat(64)),
            project.path(),
        )
        .unwrap();
        let mut document = DocumentMut::new();
        install_config_table(&mut document, item).unwrap();
        fs::write(project.path().join(CONFIG_PATH), document.to_string()).unwrap();
        let options = options(&project, &store);
        assert_eq!(prepare_setup(&options), Err(SetupError::OwnershipConflict));
    }

    #[test]
    fn repair_rejects_an_unowned_replacement_even_with_a_valid_old_receipt() {
        let (project, _store, setup) = installed(SetupProfile::FullBeta, GuidanceMode::None);
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        let receipt_before = fs::read(&receipt_path).unwrap();
        fs::write(
            &config_path,
            "# unrelated user config\n[mcp_servers.godot_editor]\ncommand = \"/tmp/unowned\"\n",
        )
        .unwrap();
        let replacement = fs::read(&config_path).unwrap();

        assert_eq!(
            prepare_setup_repair(&repair_options(&setup)),
            Err(SetupError::OwnershipConflict)
        );
        assert_eq!(fs::read(&config_path).unwrap(), replacement);
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt_before);
        assert!(!project.path().join(JOURNAL_PATH).exists());
    }

    #[test]
    fn repair_rejects_a_copied_marker_with_a_different_command() {
        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let config_path = project.path().join(CONFIG_PATH);
        let current = fs::read_to_string(&config_path).unwrap();
        let command_line = current
            .lines()
            .find(|line| line.trim_start().starts_with("command = "))
            .unwrap();
        let replacement = current.replacen(command_line, "command = \"/tmp/copied-owner\"", 1);
        assert_ne!(replacement, current);
        fs::write(config_path, replacement).unwrap();

        assert_repair_ownership_conflict(&project, &setup);
    }

    #[test]
    fn repair_rejects_a_copied_marker_with_an_unknown_key() {
        let (project, _store, setup) = installed(SetupProfile::FullBeta, GuidanceMode::None);
        let config_path = project.path().join(CONFIG_PATH);
        let mut replacement = fs::read_to_string(&config_path).unwrap();
        replacement.push_str("unexpected_owner_claim = true\n");
        fs::write(config_path, replacement).unwrap();

        assert_repair_ownership_conflict(&project, &setup);
    }

    #[test]
    fn repair_rejects_a_copied_marker_with_wrong_types_or_root_shape() {
        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let config_path = project.path().join(CONFIG_PATH);
        let current = fs::read_to_string(&config_path).unwrap();
        let replacement = current.replacen(
            "startup_timeout_sec = 10",
            "startup_timeout_sec = \"10\"",
            1,
        );
        assert_ne!(replacement, current);
        fs::write(config_path, replacement).unwrap();
        assert_repair_ownership_conflict(&project, &setup);

        let (project, _store, setup) = installed(SetupProfile::FullBeta, GuidanceMode::None);
        let config_path = project.path().join(CONFIG_PATH);
        let current = fs::read_to_string(&config_path).unwrap();
        let replacement = current.replacen(
            "args = [\"--project-root\", \".\"]",
            "args = [\"--project-root\"]",
            1,
        );
        assert_ne!(replacement, current);
        fs::write(config_path, replacement).unwrap();
        assert_repair_ownership_conflict(&project, &setup);
    }

    #[test]
    fn repair_rejects_missing_invalid_and_foreign_receipts_without_mutation() {
        let project = project();
        let store = TempDir::new().unwrap();
        fs::create_dir(project.path().join(".codex")).unwrap();
        let config_path = project.path().join(CONFIG_PATH);
        fs::write(&config_path, "[mcp_servers]\nother = \"owned-by-user\"\n").unwrap();
        let setup = options(&project, &store);
        let repair = repair_options(&setup);
        let unowned_before = fs::read(&config_path).unwrap();
        assert_eq!(
            prepare_setup_repair(&repair),
            Err(SetupError::ReceiptInvalid)
        );
        assert_eq!(fs::read(&config_path).unwrap(), unowned_before);

        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        drift_timeout(&project);
        let repair = repair_options(&setup);
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        fs::write(&receipt_path, b"{invalid receipt").unwrap();
        let invalid_config = fs::read(&config_path).unwrap();
        let invalid_receipt = fs::read(&receipt_path).unwrap();
        assert_eq!(
            prepare_setup_repair(&repair),
            Err(SetupError::ReceiptInvalid)
        );
        assert_eq!(fs::read(&config_path).unwrap(), invalid_config);
        assert_eq!(fs::read(&receipt_path).unwrap(), invalid_receipt);

        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        drift_timeout(&project);
        let repair = repair_options(&setup);
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        let mut foreign: SetupReceipt =
            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        foreign.project_id = format!("sha256:{}", "f".repeat(64));
        fs::write(&receipt_path, serde_json::to_vec_pretty(&foreign).unwrap()).unwrap();
        let foreign_config = fs::read(&config_path).unwrap();
        let foreign_receipt = fs::read(&receipt_path).unwrap();
        assert_eq!(
            prepare_setup_repair(&repair),
            Err(SetupError::ReceiptInvalid)
        );
        assert_eq!(fs::read(&config_path).unwrap(), foreign_config);
        assert_eq!(fs::read(&receipt_path).unwrap(), foreign_receipt);
    }

    #[test]
    fn repair_rejects_malformed_missing_oversized_and_guidance_drift() {
        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let repair = repair_options(&setup);
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        fs::write(&config_path, "[[malformed").unwrap();
        let malformed_config = fs::read(&config_path).unwrap();
        let receipt = fs::read(&receipt_path).unwrap();
        assert_eq!(
            prepare_setup_repair(&repair),
            Err(SetupError::ConfigInvalid)
        );
        assert_eq!(fs::read(&config_path).unwrap(), malformed_config);
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt);

        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let repair = repair_options(&setup);
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        fs::remove_file(&config_path).unwrap();
        let receipt = fs::read(&receipt_path).unwrap();
        assert_eq!(
            prepare_setup_repair(&repair),
            Err(SetupError::ConfigInvalid)
        );
        assert!(!config_path.exists());
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt);

        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let repair = repair_options(&setup);
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        fs::write(&config_path, vec![b'x'; MAX_CONFIG_BYTES as usize + 1]).unwrap();
        let receipt = fs::read(&receipt_path).unwrap();
        assert_eq!(
            prepare_setup_repair(&repair),
            Err(SetupError::InputTooLarge)
        );
        assert_eq!(
            fs::metadata(&config_path).unwrap().len(),
            MAX_CONFIG_BYTES + 1
        );
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt);

        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::All);
        drift_timeout(&project);
        let agents_path = project.path().join(AGENTS_PATH);
        let agents = fs::read_to_string(&agents_path).unwrap();
        let drifted_agents = agents.replacen("Godot", "Drifted Godot", 1);
        assert_ne!(agents, drifted_agents);
        fs::write(agents_path, drifted_agents).unwrap();
        let repair = repair_options(&setup);
        let config_before = fs::read(project.path().join(CONFIG_PATH)).unwrap();
        let receipt_before = fs::read(project.path().join(RECEIPT_PATH)).unwrap();
        assert_eq!(
            prepare_setup_repair(&repair),
            Err(SetupError::OwnershipConflict)
        );
        assert_eq!(
            fs::read(project.path().join(CONFIG_PATH)).unwrap(),
            config_before
        );
        assert_eq!(
            fs::read(project.path().join(RECEIPT_PATH)).unwrap(),
            receipt_before
        );
    }

    #[cfg(unix)]
    #[test]
    fn repair_rejects_symlink_targets_and_ancestors_without_touching_outside() {
        use std::os::unix::fs::symlink;

        let (project, store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let config_path = project.path().join(CONFIG_PATH);
        let outside = store.path().join("outside.toml");
        fs::write(&outside, "outside = true\n").unwrap();
        fs::remove_file(&config_path).unwrap();
        symlink(&outside, &config_path).unwrap();
        let repair = repair_options(&setup);
        assert_eq!(prepare_setup_repair(&repair), Err(SetupError::PathUnsafe));
        assert_eq!(fs::read_to_string(&outside).unwrap(), "outside = true\n");

        let (project, store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let outside = store.path().join("outside-dir");
        fs::create_dir(&outside).unwrap();
        fs::remove_file(project.path().join(CONFIG_PATH)).unwrap();
        fs::remove_dir(project.path().join(".codex")).unwrap();
        symlink(&outside, project.path().join(".codex")).unwrap();
        let repair = repair_options(&setup);
        assert_eq!(prepare_setup_repair(&repair), Err(SetupError::PathUnsafe));
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn repair_requires_a_private_receipt_and_preserves_config_mode() {
        use std::os::unix::fs::PermissionsExt;

        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        drift_timeout(&project);
        let repair = repair_options(&setup);
        let receipt_path = project.path().join(RECEIPT_PATH);
        fs::set_permissions(&receipt_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            prepare_setup_repair(&repair),
            Err(SetupError::ReceiptInvalid)
        );

        fs::set_permissions(&receipt_path, fs::Permissions::from_mode(0o600)).unwrap();
        let config_path = project.path().join(CONFIG_PATH);
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640)).unwrap();
        let preview = prepare_setup_repair(&repair).unwrap();
        apply_setup_plan(&preview.plan_digest, repair.plan_store.as_deref()).unwrap();
        assert_eq!(
            fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let refreshed: SetupReceipt =
            serde_json::from_slice(&fs::read(project.path().join(RECEIPT_PATH)).unwrap()).unwrap();
        assert_eq!(refreshed.config_file_mode, 0o640);
    }

    #[test]
    fn plan_store_is_bounded_and_private() {
        let project = project();
        let store = TempDir::new().unwrap();
        let options = options(&project, &store);
        let _ = prepare_setup(&options).unwrap();
        let plan_store = options.plan_store.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(plan_store).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn expired_abandoned_claim_without_a_journal_is_garbage_collected() {
        let project = project();
        let store = TempDir::new().unwrap();
        let setup = options(&project, &store);
        let preview = prepare_setup(&setup).unwrap();
        let plan_store = setup.plan_store.as_ref().unwrap();
        let hex = preview.plan_digest.strip_prefix("sha256:").unwrap();
        let source = plan_store.join(format!("{hex}.json"));
        let claimed = plan_store.join(format!("{hex}.claimed"));
        let mut plan: SetupPlan = serde_json::from_slice(&fs::read(&source).unwrap()).unwrap();
        plan.created_at_epoch_seconds = 0;
        plan.expires_at_epoch_seconds = 0;
        fs::rename(source, &claimed).unwrap();
        fs::write(&claimed, serde_json::to_vec(&plan).unwrap()).unwrap();

        purge_plans(plan_store).unwrap();
        assert!(!claimed.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_live_claim_cannot_be_collected_or_replayed_after_invalidation() {
        let project = project();
        let store = TempDir::new().unwrap();
        let setup = options(&project, &store);
        let preview = prepare_setup(&setup).unwrap();
        let plan_store = setup.plan_store.as_ref().unwrap();
        let hex = preview.plan_digest.strip_prefix("sha256:").unwrap();
        let claim = claim_plan(plan_store, hex).unwrap();

        let mut expired = claim.plan.clone();
        expired.created_at_epoch_seconds = 0;
        expired.expires_at_epoch_seconds = 0;
        fs::write(&claim.claimed_path, serde_json::to_vec(&expired).unwrap()).unwrap();

        purge_plans(plan_store).unwrap();
        assert!(claim.claimed_path.is_file());
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, Some(plan_store)),
            Err(SetupError::OperationInProgress)
        );

        claim.invalidate_nonfatal();
        drop(claim);
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, Some(plan_store)),
            Err(SetupError::PlanNotFound)
        );
        assert!(!project.path().join(CONFIG_PATH).exists());
        assert!(!project.path().join(RECEIPT_PATH).exists());
    }

    #[test]
    fn nested_root_binding_is_stable_but_other_project_is_not() {
        let project = project();
        let store = TempDir::new().unwrap();
        fs::create_dir_all(project.path().join("nested/task")).unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        let path = options.plan_store.as_ref().unwrap().join(format!(
            "{}.json",
            preview.plan_digest.strip_prefix("sha256:").unwrap()
        ));
        let plan: SetupPlan = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(plan.project_root, fs::canonicalize(project.path()).unwrap());
        assert_ne!(
            plan.project_id,
            project_id_for_path(store.path()).unwrap_or_default()
        );
    }

    #[test]
    fn a_plan_is_claimed_once_under_concurrent_apply() {
        use std::sync::{Arc, Barrier};

        let project = project();
        let store = TempDir::new().unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let barrier = Arc::clone(&barrier);
            let digest = preview.plan_digest.clone();
            let plan_store = options.plan_store.clone().unwrap();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                apply_setup_plan(&digest, Some(&plan_store))
            }));
        }
        barrier.wait();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        assert!(project.path().join(RECEIPT_PATH).is_file());
    }

    #[test]
    fn an_apply_fault_rolls_back_every_managed_file_and_restores_the_claim() {
        let project = project();
        let store = TempDir::new().unwrap();
        let mut options = options(&project, &store);
        options.guidance = GuidanceMode::All;
        let preview = prepare_setup(&options).unwrap();
        inject_atomic_write_fault(2);
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()),
            Err(SetupError::AtomicWriteFailed)
        );
        for path in [
            CONFIG_PATH,
            AGENTS_PATH,
            SKILL_PATH,
            RECEIPT_PATH,
            JOURNAL_PATH,
        ] {
            assert!(!project.path().join(path).exists(), "{path} leaked");
        }
        let plan_path = options.plan_store.as_ref().unwrap().join(format!(
            "{}.json",
            preview.plan_digest.strip_prefix("sha256:").unwrap()
        ));
        assert!(plan_path.is_file());
        apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
    }

    #[test]
    fn a_foreign_plan_journal_is_rejected_before_any_project_mutation() {
        let project = project();
        let store = TempDir::new().unwrap();
        let options_a = options(&project, &store);
        let preview_a = prepare_setup(&options_a).unwrap();
        let plan_a = persisted_plan(&options_a, &preview_a.plan_digest);

        let mut options_b = options_a.clone();
        options_b.profile = SetupProfile::FullBeta;
        let preview_b = prepare_setup(&options_b).unwrap();
        assert_ne!(preview_a.plan_digest, preview_b.plan_digest);

        let journal_a = build_apply_journal(&plan_a, &preview_a.plan_digest).unwrap();
        write_pending_journal(&project, &journal_a);
        let before = project_tree_snapshot(project.path());
        assert_eq!(
            apply_setup_plan(&preview_b.plan_digest, options_b.plan_store.as_deref()),
            Err(SetupError::TransactionRecoveryRequired)
        );
        assert_eq!(project_tree_snapshot(project.path()), before);
        assert!(!project.path().join(OPERATION_LOCK_PATH).exists());

        let recovered =
            apply_setup_plan(&preview_a.plan_digest, options_a.plan_store.as_deref()).unwrap();
        assert_eq!(recovered.status, "applied");
        assert!(!project.path().join(JOURNAL_PATH).exists());
    }

    #[test]
    fn pending_journal_blocks_every_prepare_entrypoint_without_mutation() {
        let project = project();
        let store = TempDir::new().unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        let plan = persisted_plan(&options, &preview.plan_digest);
        let journal = build_apply_journal(&plan, &preview.plan_digest).unwrap();
        write_pending_journal(&project, &journal);
        let before = project_tree_snapshot(project.path());

        assert!(matches!(
            prepare_setup_for_consent(&options),
            Err(SetupError::TransactionRecoveryRequired)
        ));
        assert_eq!(
            prepare_setup(&options),
            Err(SetupError::TransactionRecoveryRequired)
        );
        assert!(matches!(
            prepare_setup_repair_for_consent(&repair_options(&options)),
            Err(SetupError::TransactionRecoveryRequired)
        ));
        assert_eq!(
            prepare_setup_repair(&repair_options(&options)),
            Err(SetupError::TransactionRecoveryRequired)
        );
        assert!(matches!(
            prepare_setup_remove_for_consent(&remove_options(&options)),
            Err(SetupError::TransactionRecoveryRequired)
        ));
        assert_eq!(
            prepare_setup_remove(&remove_options(&options)),
            Err(SetupError::TransactionRecoveryRequired)
        );
        assert_eq!(project_tree_snapshot(project.path()), before);
    }

    #[test]
    fn journal_digest_paths_contents_modes_operation_and_receipt_are_plan_bound() {
        assert_forged_journal_rejected(|journal| journal.plan_digest = None);
        assert_forged_journal_rejected(|journal| {
            journal.plan_digest = Some(format!("sha256:{}", "f".repeat(64)));
        });
        assert_forged_journal_rejected(|journal| journal.files.swap(0, 1));
        assert_forged_journal_rejected(|journal| {
            journal.files[0].before = Some("forged-before\n".to_owned());
            journal.files[0].before_mode = Some(0o644);
        });
        assert_forged_journal_rejected(|journal| {
            journal.files[0].after.as_mut().unwrap().push_str("forged");
        });
        assert_forged_journal_rejected(|journal| {
            let current = journal.files[0].after_mode.unwrap();
            journal.files[0].after_mode = Some(if current == 0o600 { 0o644 } else { 0o600 });
        });
        assert_forged_journal_rejected(|journal| {
            journal
                .files
                .last_mut()
                .unwrap()
                .after
                .as_mut()
                .unwrap()
                .push('\n');
        });
        assert_forged_journal_rejected(|journal| {
            journal.operation = JournalOperation::Remove;
        });
        assert_forged_journal_rejected(|journal| {
            journal.project_id = format!("sha256:{}", "f".repeat(64));
        });
    }

    #[test]
    fn journal_with_an_absent_plan_digest_is_rejected_without_mutation() {
        let project = project();
        let store = TempDir::new().unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        let plan = persisted_plan(&options, &preview.plan_digest);
        let journal = build_apply_journal(&plan, &preview.plan_digest).unwrap();
        let mut value = serde_json::to_value(journal).unwrap();
        value.as_object_mut().unwrap().remove("plan_digest");
        write_pending_journal_bytes(&project, serde_json::to_vec(&value).unwrap());
        let before = project_tree_snapshot(project.path());

        assert_eq!(
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()),
            Err(SetupError::TransactionRecoveryRequired)
        );
        assert_eq!(project_tree_snapshot(project.path()), before);
        assert!(!project.path().join(OPERATION_LOCK_PATH).exists());
    }

    #[cfg(unix)]
    #[test]
    fn exact_partial_journal_rolls_back_content_and_mode_then_applies_once() {
        use std::os::unix::fs::PermissionsExt;

        let project = project();
        let store = TempDir::new().unwrap();
        fs::create_dir(project.path().join(".codex")).unwrap();
        let config = project.path().join(CONFIG_PATH);
        fs::write(&config, "# exact original\n").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o640)).unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        let plan = persisted_plan(&options, &preview.plan_digest);
        let journal = build_apply_journal(&plan, &preview.plan_digest).unwrap();
        write_pending_journal(&project, &journal);

        let config_entry = journal
            .files
            .iter()
            .find(|file| file.path == CONFIG_PATH)
            .unwrap();
        apply_journal_state(project.path(), config_entry, true).unwrap();
        assert_ne!(fs::read_to_string(&config).unwrap(), "# exact original\n");

        assert_eq!(
            recover_pending_transaction(project.path(), &plan, &preview.plan_digest).unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), "# exact original\n");
        assert_eq!(
            fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert!(!project.path().join(RECEIPT_PATH).exists());
        assert!(!project.path().join(JOURNAL_PATH).exists());

        let report = apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
        assert_eq!(report.status, "applied");
        assert!(
            fs::read_to_string(config)
                .unwrap()
                .contains("# exact original")
        );
    }

    #[test]
    fn repair_plan_rejects_stale_config_receipt_and_package_bindings() {
        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        drift_timeout(&project);
        let repair = repair_options(&setup);
        let preview = prepare_setup_repair(&repair).unwrap();
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        fs::OpenOptions::new()
            .append(true)
            .open(&config_path)
            .unwrap()
            .write_all(b"# intervening config edit\n")
            .unwrap();
        let stale_config = fs::read(&config_path).unwrap();
        let receipt_before = fs::read(&receipt_path).unwrap();
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, repair.plan_store.as_deref()),
            Err(SetupError::PlanInputsChanged)
        );
        assert_eq!(fs::read(&config_path).unwrap(), stale_config);
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt_before);

        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        drift_timeout(&project);
        let repair = repair_options(&setup);
        let preview = prepare_setup_repair(&repair).unwrap();
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        let mut receipt: SetupReceipt =
            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        receipt.plan_digest = format!("sha256:{}", "a".repeat(64));
        fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
        let config_before = fs::read(&config_path).unwrap();
        let stale_receipt = fs::read(&receipt_path).unwrap();
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, repair.plan_store.as_deref()),
            Err(SetupError::PlanInputsChanged)
        );
        assert_eq!(fs::read(&config_path).unwrap(), config_before);
        assert_eq!(fs::read(&receipt_path).unwrap(), stale_receipt);

        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::All);
        drift_timeout(&project);
        let repair = repair_options(&setup);
        let preview = prepare_setup_repair(&repair).unwrap();
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        let agents_path = project.path().join(AGENTS_PATH);
        fs::OpenOptions::new()
            .append(true)
            .open(&agents_path)
            .unwrap()
            .write_all(b"# unrelated intervening edit\n")
            .unwrap();
        let config_before = fs::read(&config_path).unwrap();
        let receipt_before = fs::read(&receipt_path).unwrap();
        let agents_before = fs::read(&agents_path).unwrap();
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, repair.plan_store.as_deref()),
            Err(SetupError::PlanInputsChanged)
        );
        assert_eq!(fs::read(&config_path).unwrap(), config_before);
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt_before);
        assert_eq!(fs::read(&agents_path).unwrap(), agents_before);

        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        drift_timeout(&project);
        let repair = repair_options(&setup);
        let preview = prepare_setup_repair(&repair).unwrap();
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        let config_before = fs::read(&config_path).unwrap();
        let receipt_before = fs::read(&receipt_path).unwrap();
        fs::write(
            repair
                .package_directory
                .as_ref()
                .unwrap()
                .join("bin/godot-codex-mcp"),
            b"changed-sidecar",
        )
        .unwrap();
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, repair.plan_store.as_deref()),
            Err(SetupError::PlanInputsChanged)
        );
        assert_eq!(fs::read(&config_path).unwrap(), config_before);
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt_before);
    }

    #[test]
    fn repair_fault_rolls_back_exact_config_and_receipt_then_retries() {
        let (project, _store, setup) = installed(SetupProfile::FullBeta, GuidanceMode::None);
        drift_timeout(&project);
        let repair = repair_options(&setup);
        let preview = prepare_setup_repair(&repair).unwrap();
        let config_path = project.path().join(CONFIG_PATH);
        let receipt_path = project.path().join(RECEIPT_PATH);
        let config_before = fs::read(&config_path).unwrap();
        let receipt_before = fs::read(&receipt_path).unwrap();
        let config_mode = existing_mode(&config_path).unwrap();
        let receipt_mode = existing_mode(&receipt_path).unwrap();

        inject_atomic_write_fault(2);
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, repair.plan_store.as_deref()),
            Err(SetupError::AtomicWriteFailed)
        );
        assert_eq!(fs::read(&config_path).unwrap(), config_before);
        assert_eq!(fs::read(&receipt_path).unwrap(), receipt_before);
        assert_eq!(existing_mode(&config_path).unwrap(), config_mode);
        assert_eq!(existing_mode(&receipt_path).unwrap(), receipt_mode);
        assert!(!project.path().join(JOURNAL_PATH).exists());

        let report = apply_setup_plan(&preview.plan_digest, repair.plan_store.as_deref()).unwrap();
        assert_eq!(report.status, "repaired");
    }

    #[test]
    fn post_rename_commit_fault_recovers_as_idempotent_configure_and_repair() {
        let project = project();
        let store = TempDir::new().unwrap();
        let setup = options(&project, &store);
        let preview = prepare_setup(&setup).unwrap();
        inject_post_rename_sync_fault(2);
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, setup.plan_store.as_deref()),
            Err(SetupError::TransactionRecoveryRequired)
        );
        assert!(project.path().join(JOURNAL_PATH).is_file());
        let recovered =
            apply_setup_plan(&preview.plan_digest, setup.plan_store.as_deref()).unwrap();
        assert_eq!(recovered.status, "already_applied");
        assert!(!project.path().join(JOURNAL_PATH).exists());

        drift_timeout(&project);
        let repair = repair_options(&setup);
        let repair_preview = prepare_setup_repair(&repair).unwrap();
        inject_post_rename_sync_fault(2);
        assert_eq!(
            apply_setup_plan(&repair_preview.plan_digest, repair.plan_store.as_deref()),
            Err(SetupError::TransactionRecoveryRequired)
        );
        assert!(project.path().join(JOURNAL_PATH).is_file());
        let recovered =
            apply_setup_plan(&repair_preview.plan_digest, repair.plan_store.as_deref()).unwrap();
        assert_eq!(recovered.status, "already_repaired");
        assert!(!project.path().join(JOURNAL_PATH).exists());
        assert_eq!(
            apply_setup_plan(&repair_preview.plan_digest, repair.plan_store.as_deref())
                .unwrap()
                .status,
            "already_repaired"
        );
    }

    #[test]
    fn post_receipt_delete_response_loss_recovers_as_idempotent_remove() {
        let (project, _store, setup) = installed(SetupProfile::ReadOnly, GuidanceMode::None);
        let remove = remove_options(&setup);
        let preview = prepare_setup_remove(&remove).unwrap();
        inject_post_rename_sync_fault(2);
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, remove.plan_store.as_deref()),
            Err(SetupError::TransactionRecoveryRequired)
        );
        assert!(!project.path().join(RECEIPT_PATH).exists());
        assert!(project.path().join(JOURNAL_PATH).is_file());

        let recovered =
            apply_setup_plan(&preview.plan_digest, remove.plan_store.as_deref()).unwrap();
        assert_eq!(recovered.status, "already_removed");
        assert!(!project.path().join(JOURNAL_PATH).exists());
        assert!(!project.path().join(CONFIG_PATH).exists());
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, remove.plan_store.as_deref())
                .unwrap()
                .status,
            "already_removed"
        );
    }

    #[test]
    fn post_rename_noncommit_fault_rolls_back_before_retry() {
        let project = project();
        let store = TempDir::new().unwrap();
        let setup = options(&project, &store);
        let preview = prepare_setup(&setup).unwrap();
        inject_post_rename_sync_fault(1);
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, setup.plan_store.as_deref()),
            Err(SetupError::AtomicWriteFailed)
        );
        assert!(!project.path().join(CONFIG_PATH).exists());
        assert!(!project.path().join(RECEIPT_PATH).exists());
        assert!(!project.path().join(JOURNAL_PATH).exists());
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, setup.plan_store.as_deref())
                .unwrap()
                .status,
            "applied"
        );
    }

    #[test]
    fn a_remove_fault_restores_the_exact_pre_remove_state() {
        let project = project();
        let store = TempDir::new().unwrap();
        let mut options = options(&project, &store);
        options.guidance = GuidanceMode::All;
        let preview = prepare_setup(&options).unwrap();
        apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
        let paths = [CONFIG_PATH, AGENTS_PATH, SKILL_PATH, RECEIPT_PATH];
        let before = paths
            .iter()
            .map(|path| (path, fs::read(project.path().join(path)).unwrap()))
            .collect::<Vec<_>>();
        inject_atomic_write_fault(2);
        let remove = remove_options(&options);
        let remove_preview = prepare_setup_remove(&remove).unwrap();
        assert_eq!(
            apply_setup_plan(&remove_preview.plan_digest, remove.plan_store.as_deref()),
            Err(SetupError::AtomicWriteFailed)
        );
        for (path, expected) in before {
            assert_eq!(fs::read(project.path().join(path)).unwrap(), expected);
        }
        assert!(!project.path().join(JOURNAL_PATH).exists());
        apply_setup_plan(&remove_preview.plan_digest, remove.plan_store.as_deref()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn existing_modes_are_preserved_and_chmod_invalidates_a_plan_and_receipt() {
        use std::os::unix::fs::PermissionsExt;

        let project = project();
        let store = TempDir::new().unwrap();
        fs::create_dir(project.path().join(".codex")).unwrap();
        let config = project.path().join(CONFIG_PATH);
        fs::write(&config, "# retained\n").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o640)).unwrap();
        let options = options(&project, &store);
        let preview = prepare_setup(&options).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()),
            Err(SetupError::PlanInputsChanged)
        );
        fs::set_permissions(&config, fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()),
            Err(SetupError::PlanNotFound)
        );
        let refreshed = prepare_setup(&options).unwrap();
        apply_setup_plan(&refreshed.plan_digest, options.plan_store.as_deref()).unwrap();
        assert_eq!(
            fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            prepare_setup_remove(&remove_options(&options)),
            Err(SetupError::OwnershipConflict)
        );
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_symlinks_are_rejected_before_plan_persistence() {
        use std::os::unix::fs::symlink;

        let project = project();
        let store = TempDir::new().unwrap();
        let outside = store.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, project.path().join(".agents")).unwrap();
        let mut options = options(&project, &store);
        options.guidance = GuidanceMode::Skill;
        assert_eq!(prepare_setup(&options), Err(SetupError::PathUnsafe));
        assert!(!outside.join("skills/godot-editor/SKILL.md").exists());
    }

    #[test]
    fn configure_migrates_two_exact_owned_projects_from_different_package_versions() {
        for (suffix, historical_version) in [("upgrade", "0.0.9"), ("rollback", "0.1.1")] {
            let project = project();
            let store = TempDir::new().unwrap();
            let mut options = options(&project, &store);
            options.guidance = GuidanceMode::All;
            let initial = prepare_setup(&options).unwrap();
            apply_setup_plan(&initial.plan_digest, options.plan_store.as_deref()).unwrap();
            let previous = historical_receipt(&project, historical_version);
            let old_package = options.package_directory.clone().unwrap();
            let current_package = write_test_package(
                store.path(),
                &format!("current-{suffix}"),
                format!("current-sidecar-{suffix}").as_bytes(),
            );
            options.package_directory = Some(current_package.clone());
            fs::remove_dir_all(old_package).unwrap();

            let preview = prepare_setup(&options).unwrap();
            assert_eq!(preview.mode, SetupMode::Configure);
            assert_eq!(preview.package_version, PRODUCT_VERSION);
            let serialized = serde_json::to_string(&preview).unwrap();
            assert!(!serialized.contains(current_package.to_str().unwrap()));
            let report =
                apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
            assert_eq!(report.status, "applied");
            assert_receipt_uses_current_package(&project, &options);
            let migrated: SetupReceipt =
                serde_json::from_slice(&fs::read(project.path().join(RECEIPT_PATH)).unwrap())
                    .unwrap();
            assert_eq!(
                migrated.config_ownership_marker,
                previous.config_ownership_marker
            );
            assert_ne!(
                migrated.package_identity_digest,
                previous.package_identity_digest
            );
            assert_ne!(migrated.launcher_path_sha256, previous.launcher_path_sha256);
        }
    }

    #[test]
    fn repair_migrates_an_exact_owned_receipt_across_package_rollback() {
        let project = project();
        let store = TempDir::new().unwrap();
        let mut options = options(&project, &store);
        let initial = prepare_setup(&options).unwrap();
        apply_setup_plan(&initial.plan_digest, options.plan_store.as_deref()).unwrap();
        let previous = historical_receipt(&project, "0.1.1");
        let old_config = fs::read(project.path().join(CONFIG_PATH)).unwrap();
        let old_package = options.package_directory.clone().unwrap();
        options.package_directory = Some(write_test_package(
            store.path(),
            "rollback-current",
            b"rollback-current-sidecar",
        ));
        fs::remove_dir_all(old_package).unwrap();

        let repair = repair_options(&options);
        let preview = prepare_setup_repair(&repair).unwrap();
        assert_eq!(preview.mode, SetupMode::Repair);
        assert_eq!(
            preview
                .changes
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            [CONFIG_PATH, RECEIPT_PATH]
        );
        let report = apply_setup_plan(&preview.plan_digest, repair.plan_store.as_deref()).unwrap();
        assert_eq!(report.status, "repaired");
        assert_ne!(
            fs::read(project.path().join(CONFIG_PATH)).unwrap(),
            old_config
        );
        assert_receipt_uses_current_package(&project, &options);
        let migrated: SetupReceipt =
            serde_json::from_slice(&fs::read(project.path().join(RECEIPT_PATH)).unwrap()).unwrap();
        assert_eq!(
            migrated.config_ownership_marker,
            previous.config_ownership_marker
        );
    }

    #[test]
    fn repair_refreshes_receipt_when_stable_launcher_path_changes_bytes() {
        let project = project();
        let store = TempDir::new().unwrap();
        let options = options(&project, &store);
        let initial = prepare_setup(&options).unwrap();
        apply_setup_plan(&initial.plan_digest, options.plan_store.as_deref()).unwrap();
        let previous = historical_receipt(&project, "0.0.9");
        let config_before = fs::read(project.path().join(CONFIG_PATH)).unwrap();
        let sidecar = options
            .package_directory
            .as_ref()
            .unwrap()
            .join("bin/godot-codex-mcp");
        fs::write(&sidecar, b"upgraded-at-stable-current-path").unwrap();

        let repair = repair_options(&options);
        let preview = prepare_setup_repair(&repair).unwrap();
        let config_change = preview
            .changes
            .iter()
            .find(|change| change.path == CONFIG_PATH)
            .unwrap();
        assert_eq!(config_change.before_digest, config_change.after_digest);
        apply_setup_plan(&preview.plan_digest, repair.plan_store.as_deref()).unwrap();

        assert_eq!(
            fs::read(project.path().join(CONFIG_PATH)).unwrap(),
            config_before
        );
        assert_receipt_uses_current_package(&project, &options);
        let migrated: SetupReceipt =
            serde_json::from_slice(&fs::read(project.path().join(RECEIPT_PATH)).unwrap()).unwrap();
        assert_ne!(migrated.launcher_file_sha256, previous.launcher_file_sha256);
        assert_eq!(
            migrated.config_ownership_marker,
            previous.config_ownership_marker
        );
    }

    #[test]
    fn remove_uses_exact_ownership_but_only_current_package_authority_after_upgrade() {
        let project = project();
        let store = TempDir::new().unwrap();
        let mut options = options(&project, &store);
        options.guidance = GuidanceMode::All;
        let initial = prepare_setup(&options).unwrap();
        apply_setup_plan(&initial.plan_digest, options.plan_store.as_deref()).unwrap();
        let historical = historical_receipt(&project, "0.0.9");
        let old_package = options.package_directory.clone().unwrap();
        options.package_directory = Some(write_test_package(
            store.path(),
            "remove-current",
            b"remove-current-sidecar",
        ));
        fs::remove_dir_all(old_package).unwrap();

        let remove = remove_options(&options);
        let preview = prepare_setup_remove(&remove).unwrap();
        let plan = persisted_plan(&options, &preview.plan_digest);
        assert_eq!(plan.package_version, PRODUCT_VERSION);
        assert_ne!(
            plan.package_identity_digest,
            historical.package_identity_digest
        );
        assert_eq!(
            plan.receipt_template.package_version,
            historical.package_version
        );
        let report = apply_setup_plan(&preview.plan_digest, remove.plan_store.as_deref()).unwrap();
        assert_eq!(report.status, "removed");
        for path in [CONFIG_PATH, AGENTS_PATH, SKILL_PATH, RECEIPT_PATH] {
            assert!(!project.path().join(path).exists(), "{path} leaked");
        }
    }

    #[test]
    fn package_target_change_after_upgrade_preview_invalidates_without_project_mutation() {
        let project = project();
        let store = TempDir::new().unwrap();
        let mut options = options(&project, &store);
        let initial = prepare_setup(&options).unwrap();
        apply_setup_plan(&initial.plan_digest, options.plan_store.as_deref()).unwrap();
        historical_receipt(&project, "0.0.9");
        options.package_directory = Some(write_test_package(
            store.path(),
            "preview-current",
            b"preview-current-sidecar",
        ));
        let preview = prepare_setup(&options).unwrap();
        let before = project_tree_snapshot(project.path());
        let bound_package = options.package_directory.as_ref().unwrap();
        fs::rename(bound_package, store.path().join("retired-preview-target")).unwrap();
        let replacement = write_test_package(
            store.path(),
            "replacement-preview-target",
            b"switched-current-sidecar",
        );
        fs::rename(replacement, bound_package).unwrap();

        assert_eq!(
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()),
            Err(SetupError::PlanInputsChanged)
        );
        assert_eq!(project_tree_snapshot(project.path()), before);
        let receipt: SetupReceipt =
            serde_json::from_slice(&fs::read(project.path().join(RECEIPT_PATH)).unwrap()).unwrap();
        assert_eq!(receipt.package_version, "0.0.9");
    }

    #[test]
    fn malformed_historical_version_and_tampered_ownership_still_fail_closed() {
        let project = project();
        let store = TempDir::new().unwrap();
        let options = options(&project, &store);
        let initial = prepare_setup(&options).unwrap();
        apply_setup_plan(&initial.plan_digest, options.plan_store.as_deref()).unwrap();
        historical_receipt(&project, "01.0.0");
        let before = project_tree_snapshot(project.path());
        assert_eq!(prepare_setup(&options), Err(SetupError::ReceiptInvalid));
        assert_eq!(
            prepare_setup_repair(&repair_options(&options)),
            Err(SetupError::ReceiptInvalid)
        );
        assert_eq!(
            prepare_setup_remove(&remove_options(&options)),
            Err(SetupError::ReceiptInvalid)
        );
        assert_eq!(project_tree_snapshot(project.path()), before);

        let mut receipt: SetupReceipt =
            serde_json::from_slice(&fs::read(project.path().join(RECEIPT_PATH)).unwrap()).unwrap();
        receipt.package_version = "0.0.9".to_owned();
        receipt.config_ownership_marker = format!("sha256:{}", "f".repeat(64));
        fs::write(
            project.path().join(RECEIPT_PATH),
            serde_json::to_vec_pretty(&receipt).unwrap(),
        )
        .unwrap();
        let tampered = project_tree_snapshot(project.path());
        assert_eq!(prepare_setup(&options), Err(SetupError::OwnershipConflict));
        assert_eq!(
            prepare_setup_remove(&remove_options(&options)),
            Err(SetupError::OwnershipConflict)
        );
        assert_eq!(project_tree_snapshot(project.path()), tampered);
    }

    #[test]
    fn historical_package_version_is_bounded_semver_without_version_ordering() {
        for version in [
            "0.0.9",
            PRODUCT_VERSION,
            "0.1.1",
            "2.0.0-rc.1",
            "2.0.0-rollback-safe+build.7",
        ] {
            assert!(
                historical_package_version_is_supported(version),
                "{version}"
            );
        }
        for version in ["", "01.0.0", "1.0", "1.0.0-01", "1.0.0+", "1.0.0+bad!"] {
            assert!(
                !historical_package_version_is_supported(version),
                "{version}"
            );
        }
        assert!(!historical_package_version_is_supported(&"1".repeat(65)));
    }

    #[test]
    fn sidecar_and_detached_manifest_are_part_of_the_plan_binding() {
        let project = project();
        let store = TempDir::new().unwrap();
        let package = TempDir::new().unwrap();
        fs::create_dir(package.path().join("bin")).unwrap();
        let sidecar = package.path().join("bin/godot-codex-mcp");
        fs::write(&sidecar, b"sidecar-v1").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(
            package.path().join("package-manifest.json"),
            format!(r#"{{"package_version":"{PRODUCT_VERSION}"}}"#),
        )
        .unwrap();
        let mut options = options(&project, &store);
        options.package_directory = Some(package.path().to_path_buf());
        let preview = prepare_setup(&options).unwrap();
        fs::write(package.path().join("bin/godot-codex-mcp"), b"sidecar-v2").unwrap();
        assert_eq!(
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()),
            Err(SetupError::PlanInputsChanged)
        );
        assert!(!project.path().join(CONFIG_PATH).exists());
    }
}
