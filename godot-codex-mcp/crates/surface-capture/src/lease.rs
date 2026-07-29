use std::fmt;
#[cfg(test)]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::journal::{CaptureRecorder, JournalBinding, sha256_bytes, validate_capture_journal};
use crate::{CaptureJournal, JOURNAL_SCHEMA_VERSION, LEASE_SCHEMA_VERSION, TappedTransport};

const CAPTURE_DIRECTORY: &str = "surface-capture-v1";
const RUNS_DIRECTORY: &str = "runs";
const METADATA_FILE: &str = "metadata.json";
const ARMED_FILE: &str = "armed.lease.json";
const CLAIMED_FILE: &str = "claimed.lease.json";
const JOURNAL_FILE: &str = "journal.json";
const JOURNAL_TEMP_FILE: &str = ".journal.pending";
const STORE_LOCK_FILE: &str = ".store.lock";
const STAGING_DIRECTORY: &str = ".staging";
const RETIRED_DIRECTORY: &str = ".retired";
const MAX_METADATA_BYTES: usize = 64 * 1024;
const MAX_LEASE_BYTES: usize = 32 * 1024;
const MAX_FINAL_ARTIFACT_BYTES: usize = 768 * 1024;
const MAX_RUNS: usize = 128;
const MAX_LEASE_LIFETIME_SECONDS: u64 = 60 * 60;

/// Official host surface participating in Sprint 11 acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    /// Codex desktop app.
    App,
    /// Official Codex CLI.
    Cli,
    /// Official IDE extension.
    Ide,
}

impl Surface {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Cli => "cli",
            Self::Ide => "ide",
        }
    }
}

/// Exact digests binding a capture to installed package state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingDigests {
    /// Installed `godot-codex-mcp` launcher digest.
    pub package_launcher_sha256: String,
    /// Exact `.codex/config.toml` digest.
    pub project_config_sha256: String,
    /// Exact setup receipt digest.
    pub setup_receipt_sha256: String,
}

/// Bounded in-store metadata document and its externally computed digest.
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataBinding {
    /// Canonical JSON document saved as the run's private `metadata.json`.
    pub document: Value,
    /// SHA-256 of the compact canonical JSON bytes.
    pub sha256: String,
}

impl MetadataBinding {
    /// Canonicalize a bounded JSON document and compute its exact digest.
    pub fn from_document(document: Value) -> Result<Self, LeaseError> {
        validate_metadata_value(&document, 0)?;
        let mut bytes = serde_json::to_vec(&document).map_err(|_| LeaseError::InvalidMetadata)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(LeaseError::InvalidMetadata);
        }
        Ok(Self {
            document,
            sha256: sha256_bytes(&bytes),
        })
    }
}

/// Request to arm one one-shot capture.
#[derive(Debug, Clone, PartialEq)]
pub struct ArmRequest {
    /// Canonicalizable Godot project directory.
    pub project_root: PathBuf,
    /// Official surface expected to own the stdio session.
    pub surface: Surface,
    /// Bounded metadata document and digest.
    pub metadata: MetadataBinding,
    /// Exact package/config/receipt bindings.
    pub bindings: BindingDigests,
    /// Absolute Unix expiry, bounded by the calling control plane.
    pub expires_at_unix: u64,
}

/// Context presented by a sidecar attempting to claim a lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimContext {
    /// Canonicalizable Godot project directory.
    pub project_root: PathBuf,
    /// Exact package/config/receipt bindings observed at startup.
    pub bindings: BindingDigests,
}

/// Summary returned after arming.
#[derive(Clone, PartialEq, Eq)]
pub struct ArmedLease {
    /// Random opaque 256-bit run id.
    pub run_id: String,
    /// Hashed canonical project identity.
    pub project_identity: String,
    /// Surface.
    pub surface: Surface,
    /// Metadata digest.
    pub metadata_sha256: String,
    /// Expiry.
    pub expires_at_unix: u64,
    /// Actual state of the newly created or recovered project lease.
    pub state: LeaseState,
    /// Whether this call created, recovered, or conflicted with the lease.
    pub disposition: ArmDisposition,
    /// Exact package/config/receipt bindings stored by the returned lease.
    pub bindings: BindingDigests,
    /// Canonical stored metadata used to keep recovered/conflict receipts coherent.
    metadata_document: Value,
}

impl fmt::Debug for ArmedLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArmedLease")
            .field("run_id", &self.run_id)
            .field("project_identity", &self.project_identity)
            .field("surface", &self.surface)
            .field("metadata_sha256", &self.metadata_sha256)
            .field("expires_at_unix", &self.expires_at_unix)
            .field("state", &self.state)
            .field("disposition", &self.disposition)
            .field("bindings", &self.bindings)
            .finish_non_exhaustive()
    }
}

impl ArmedLease {
    /// Canonical metadata stored by the returned existing or newly created run.
    #[must_use]
    pub const fn metadata_document(&self) -> &Value {
        &self.metadata_document
    }
}

/// Outcome of an arm attempt under the one-Armed-run-per-project invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmDisposition {
    /// A new complete Armed run was atomically published.
    Created,
    /// An exact prior arm was recovered after response loss.
    Recovered,
    /// A different Armed run already owns this project.
    Conflict,
}

/// State of a specific run id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    /// No such run.
    Absent,
    /// Lease can be claimed.
    Armed,
    /// Lease exists but is expired and cannot be claimed.
    Expired,
    /// Lease has already been claimed.
    Claimed,
    /// Journal was finalized.
    Finalized,
}

/// Read-only status of a specific run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseStatus {
    /// Queried run id.
    pub run_id: String,
    /// Current state.
    pub state: LeaseState,
    /// Surface when a run exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<Surface>,
    /// Hashed project identity when a run exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_identity: Option<String>,
    /// Expiry when a run exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at_unix: Option<u64>,
}

/// Content-minimized capture outcome written with the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalizeOutcome {
    /// Surface session completed normally.
    Completed,
    /// Surface session was cancelled by its operator.
    Cancelled,
    /// Surface session ended with an error.
    Failed,
}

/// Root-scoped lease store.
#[derive(Debug, Clone)]
pub struct LeaseStore {
    data_root: PathBuf,
    #[cfg(unix)]
    data_root_identity: FileIdentity,
}

impl LeaseStore {
    /// Open an owner-private canonical data root.
    ///
    /// This does not create a capture directory, preserving no-op behavior when
    /// no lease has ever been armed.
    pub fn open(data_root: impl AsRef<Path>) -> Result<Self, LeaseError> {
        #[cfg(not(unix))]
        {
            let _ = data_root;
            Err(LeaseError::Unsupported)
        }
        #[cfg(unix)]
        {
            let supplied = data_root.as_ref();
            validate_private_dir(supplied)?;
            let canonical = fs::canonicalize(supplied).map_err(LeaseError::Io)?;
            validate_private_dir(&canonical)?;
            let data_root_identity = {
                let directory = open_private_directory(&canonical)?;
                file_identity(&validate_private_directory_descriptor(&directory)?)?
            };
            Ok(Self {
                data_root: canonical,
                data_root_identity,
            })
        }
    }

    /// Canonical data root containing all store state.
    #[must_use]
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// Return whether at least one armed or claimed run exists.
    ///
    /// The preflight is bounded and read-only. A never-armed store returns
    /// `false` without creating capture directories.
    pub fn has_capture_state(&self) -> Result<bool, LeaseError> {
        let Some(runs) = self.existing_runs_directory()? else {
            return Ok(false);
        };
        for (index, entry) in fs::read_dir(runs).map_err(LeaseError::Io)?.enumerate() {
            if index >= MAX_RUNS {
                return Err(LeaseError::StoreBoundExceeded);
            }
            let entry = entry.map_err(LeaseError::Io)?;
            let Some(run_id) = entry.file_name().to_str().map(str::to_owned) else {
                return Err(LeaseError::UnsafeMetadata);
            };
            validate_run_id(&run_id)?;
            validate_private_dir(&entry.path())?;
            for lease_file in [ARMED_FILE, CLAIMED_FILE] {
                let path = entry.path().join(lease_file);
                if path_metadata_exists(&path)? {
                    let lease = read_lease(&path)?;
                    validate_lease(&lease)?;
                    if lease.run_id != run_id {
                        return Err(LeaseError::InvalidLease);
                    }
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Arm a one-shot capture.
    pub fn arm(&self, request: ArmRequest) -> Result<ArmedLease, LeaseError> {
        self.arm_at(request, unix_seconds())
    }

    /// Read the state of one opaque run id.
    pub fn status(&self, run_id: &str) -> Result<LeaseStatus, LeaseError> {
        self.status_at(run_id, unix_seconds())
    }

    /// Read and validate the fixed private metadata document for one run.
    pub fn metadata(&self, run_id: &str) -> Result<Option<MetadataBinding>, LeaseError> {
        validate_run_id(run_id)?;
        let Some(run_directory) = self.existing_run_directory(run_id)? else {
            return Ok(None);
        };
        for lease_file in [ARMED_FILE, CLAIMED_FILE] {
            let path = run_directory.join(lease_file);
            if path_metadata_exists(&path)? {
                let lease = read_lease(&path)?;
                validate_lease(&lease)?;
                if lease.run_id != run_id {
                    return Err(LeaseError::InvalidLease);
                }
                let document = read_metadata(&run_directory, &lease)?;
                return Ok(Some(MetadataBinding {
                    document,
                    sha256: lease.metadata_sha256,
                }));
            }
        }
        if path_metadata_exists(&run_directory.join(JOURNAL_FILE))? {
            let artifact = read_finalized_document(&run_directory)?;
            if artifact.journal.run_id != run_id
                || artifact.metadata_sha256 != artifact.journal.metadata_sha256
            {
                return Err(LeaseError::InvalidJournal);
            }
            let metadata = read_metadata_unbound(&run_directory)?;
            if metadata.sha256 != artifact.metadata_sha256 {
                return Err(LeaseError::InvalidMetadata);
            }
            return Ok(Some(metadata));
        }
        Err(LeaseError::InvalidLease)
    }

    /// Cancel an unclaimed run. Claimed/finalized runs cannot be cancelled.
    pub fn cancel(&self, run_id: &str) -> Result<(), LeaseError> {
        validate_run_id(run_id)?;
        let Some(lock) = StoreMutationLock::acquire_existing(self)? else {
            return Err(LeaseError::NotFound);
        };
        #[cfg(unix)]
        {
            let handles =
                OpenRunDirectory::open_locked(self, &lock, run_id)?.ok_or(LeaseError::NotFound)?;
            if private_file_exists_at(&handles.run, CLAIMED_FILE)?
                || private_file_exists_at(&handles.run, JOURNAL_FILE)?
            {
                return Err(LeaseError::AlreadyClaimed);
            }
            validate_exact_run_entries(&handles.run, &[ARMED_FILE, METADATA_FILE])?;
            let armed = read_private_file_at(&handles.run, ARMED_FILE, MAX_LEASE_BYTES)?;
            let metadata = read_private_file_at(&handles.run, METADATA_FILE, MAX_METADATA_BYTES)?;
            let lease: LeaseDocument =
                serde_json::from_slice(&armed.bytes).map_err(|_| LeaseError::InvalidLease)?;
            validate_lease(&lease)?;
            if lease.run_id != run_id {
                return Err(LeaseError::InvalidLease);
            }
            let metadata_binding = metadata_binding_from_bytes(&metadata.bytes)?;
            if metadata_binding.sha256 != lease.metadata_sha256 {
                return Err(LeaseError::InvalidMetadata);
            }
            remove_bound_run(
                &handles,
                run_id,
                &[ARMED_FILE, METADATA_FILE],
                &[(ARMED_FILE, &armed), (METADATA_FILE, &metadata)],
            )?;
            lock.validate_tree(self)
        }
        #[cfg(not(unix))]
        {
            let _ = lock;
            Err(LeaseError::Unsupported)
        }
    }

    /// Consume one exact finalized artifact after verifying its compact wrapper
    /// digest and complete private run layout.
    ///
    /// Armed, claimed, absent, malformed, unsafe, or digest-mismatched runs are
    /// rejected before any filesystem entry is removed.
    pub fn consume_finalized(&self, run_id: &str, capture_sha256: &str) -> Result<(), LeaseError> {
        validate_run_id(run_id)?;
        if !valid_sha256(capture_sha256) {
            return Err(LeaseError::DigestMismatch);
        }
        let Some(lock) = StoreMutationLock::acquire_existing(self)? else {
            return Err(LeaseError::NotFound);
        };
        #[cfg(unix)]
        {
            self.consume_finalized_unix(&lock, run_id, capture_sha256, || {})?;
            lock.validate_tree(self)
        }
        #[cfg(not(unix))]
        {
            let _ = lock;
            Err(LeaseError::Unsupported)
        }
    }

    /// Explicitly abandon one exact inactive Claimed run using the metadata
    /// digest returned when it was armed.
    ///
    /// Bare Claimed evidence and structurally corrupt allowlisted partial
    /// finalizations can be removed. Live claims, valid recoverable partials,
    /// Armed/Finalized runs, unsafe layouts, and digest mismatch are rejected
    /// before any removal.
    pub fn abandon_claimed(&self, run_id: &str, metadata_sha256: &str) -> Result<(), LeaseError> {
        validate_run_id(run_id)?;
        if !valid_sha256(metadata_sha256) {
            return Err(LeaseError::MetadataDigestMismatch);
        }
        let Some(lock) = StoreMutationLock::acquire_existing(self)? else {
            return Err(LeaseError::NotFound);
        };
        #[cfg(unix)]
        {
            self.abandon_claimed_unix(&lock, run_id, metadata_sha256, || {})?;
            lock.validate_tree(self)
        }
        #[cfg(not(unix))]
        {
            let _ = lock;
            Err(LeaseError::Unsupported)
        }
    }

    #[cfg(unix)]
    fn abandon_claimed_unix(
        &self,
        lock: &StoreMutationLock,
        run_id: &str,
        metadata_sha256: &str,
        before_remove: impl FnOnce(),
    ) -> Result<(), LeaseError> {
        let handles =
            OpenRunDirectory::open_locked(self, lock, run_id)?.ok_or(LeaseError::NotFound)?;
        match explicit_abandon_layout(&handles.run)? {
            ExplicitAbandonLayout::Armed => {
                let armed = read_private_file_at(&handles.run, ARMED_FILE, MAX_LEASE_BYTES)?;
                let metadata =
                    read_private_file_at(&handles.run, METADATA_FILE, MAX_METADATA_BYTES)?;
                validate_lease_and_metadata(
                    run_id,
                    metadata_sha256,
                    &armed.bytes,
                    &metadata.bytes,
                )?;
                Err(LeaseError::NotFinalized)
            }
            ExplicitAbandonLayout::Finalized => {
                let journal =
                    read_private_file_at(&handles.run, JOURNAL_FILE, MAX_FINAL_ARTIFACT_BYTES)?;
                let metadata =
                    read_private_file_at(&handles.run, METADATA_FILE, MAX_METADATA_BYTES)?;
                let metadata_binding = metadata_binding_from_bytes(&metadata.bytes)?;
                if metadata_binding.sha256 != metadata_sha256 {
                    return Err(LeaseError::MetadataDigestMismatch);
                }
                let (artifact, _) = compact_finalized_from_bytes(journal.bytes)?;
                validate_finalized_binding(&artifact, run_id)?;
                if artifact.metadata_sha256 != metadata_binding.sha256 {
                    return Err(LeaseError::InvalidMetadata);
                }
                Err(LeaseError::ConsumeRequired)
            }
            layout @ (ExplicitAbandonLayout::BareClaimed
            | ExplicitAbandonLayout::Partial { .. }) => {
                let claimed = read_private_file_at(&handles.run, CLAIMED_FILE, MAX_LEASE_BYTES)?;
                let metadata =
                    read_private_file_at(&handles.run, METADATA_FILE, MAX_METADATA_BYTES)?;
                let (lease, metadata_binding) = validate_lease_and_metadata(
                    run_id,
                    metadata_sha256,
                    &claimed.bytes,
                    &metadata.bytes,
                )?;
                acquire_claim_guard(&handles.run)?;

                let pending =
                    if matches!(layout, ExplicitAbandonLayout::Partial { pending: true, .. }) {
                        Some(read_private_file_at(
                            &handles.run,
                            JOURNAL_TEMP_FILE,
                            MAX_FINAL_ARTIFACT_BYTES,
                        )?)
                    } else {
                        None
                    };
                let journal =
                    if matches!(layout, ExplicitAbandonLayout::Partial { journal: true, .. }) {
                        Some(read_private_file_at(
                            &handles.run,
                            JOURNAL_FILE,
                            MAX_FINAL_ARTIFACT_BYTES,
                        )?)
                    } else {
                        None
                    };
                if matches!(layout, ExplicitAbandonLayout::Partial { .. })
                    && classify_partial_finalization(
                        run_id,
                        &lease,
                        &metadata_binding,
                        pending.as_ref(),
                        journal.as_ref(),
                    )? == PartialFinalization::Recoverable
                {
                    return Err(LeaseError::RecoveryRequired);
                }

                let mut expected = vec![CLAIMED_FILE, METADATA_FILE];
                let mut files = Vec::with_capacity(4);
                if let Some(pending) = pending.as_ref() {
                    expected.push(JOURNAL_TEMP_FILE);
                    files.push((JOURNAL_TEMP_FILE, pending));
                }
                if let Some(journal) = journal.as_ref() {
                    expected.push(JOURNAL_FILE);
                    files.push((JOURNAL_FILE, journal));
                }
                files.push((CLAIMED_FILE, &claimed));
                files.push((METADATA_FILE, &metadata));
                before_remove();
                remove_bound_run(&handles, run_id, &expected, &files)
            }
        }
    }

    #[cfg(unix)]
    fn consume_finalized_unix(
        &self,
        lock: &StoreMutationLock,
        run_id: &str,
        capture_sha256: &str,
        before_unlink: impl FnOnce(),
    ) -> Result<(), LeaseError> {
        let handles =
            OpenRunDirectory::open_locked(self, lock, run_id)?.ok_or(LeaseError::NotFound)?;
        if !private_file_exists_at(&handles.run, JOURNAL_FILE)? {
            for lease_file in [ARMED_FILE, CLAIMED_FILE] {
                if private_file_exists_at(&handles.run, lease_file)? {
                    let snapshot = read_private_file_at(&handles.run, lease_file, MAX_LEASE_BYTES)?;
                    let lease: LeaseDocument = serde_json::from_slice(&snapshot.bytes)
                        .map_err(|_| LeaseError::InvalidLease)?;
                    validate_lease(&lease)?;
                    if lease.run_id != run_id {
                        return Err(LeaseError::InvalidLease);
                    }
                    return Err(LeaseError::NotFinalized);
                }
            }
            return Err(LeaseError::InvalidLease);
        }
        let journal = read_private_file_at(&handles.run, JOURNAL_FILE, MAX_FINAL_ARTIFACT_BYTES)?;
        let metadata = read_private_file_at(&handles.run, METADATA_FILE, MAX_METADATA_BYTES)?;
        validate_exact_run_entries(&handles.run, &[JOURNAL_FILE, METADATA_FILE])?;

        let (artifact, compact) = compact_finalized_from_bytes(journal.bytes.clone())?;
        validate_finalized_binding(&artifact, run_id)?;
        let metadata_binding = metadata_binding_from_bytes(&metadata.bytes)?;
        if metadata_binding.sha256 != artifact.metadata_sha256 {
            return Err(LeaseError::InvalidMetadata);
        }
        if sha256_bytes(&compact) != capture_sha256 {
            return Err(LeaseError::DigestMismatch);
        }
        acquire_claim_guard(&handles.run)?;

        before_unlink();

        // Retire the exact validated run from the bounded scanner atomically
        // before purging its files. A crash can leave only an authorized
        // private tombstone, never a partially deleted live run.
        remove_bound_run(
            &handles,
            run_id,
            &[JOURNAL_FILE, METADATA_FILE],
            &[(JOURNAL_FILE, &journal), (METADATA_FILE, &metadata)],
        )
    }

    /// Claim exactly one armed lease matching the canonical project and exact
    /// package/config/receipt digests.
    ///
    /// With no capture directory or no matching lease this returns `Ok(None)`
    /// and performs no writes.
    pub fn claim(&self, context: &ClaimContext) -> Result<Option<ClaimedLease>, LeaseError> {
        self.claim_at(context, unix_seconds())
    }

    fn arm_at(&self, request: ArmRequest, now: u64) -> Result<ArmedLease, LeaseError> {
        validate_expiry(request.expires_at_unix, now)?;
        validate_bindings(&request.bindings)?;
        let project_identity = project_identity_for_path(&request.project_root)?;
        let metadata = canonical_metadata(&request.metadata)?;
        #[cfg(unix)]
        {
            let lock = StoreMutationLock::acquire_or_create(self)?;
            self.recover_retired_runs(&lock)?;
            self.recover_staged_runs(&lock)?;
            if let Some(existing) =
                find_retryable_armed(self, &lock, &request, &project_identity, &metadata, now)?
            {
                lock.validate_tree(self)?;
                return Ok(existing);
            }
            validate_run_capacity_at(&lock.runs)?;

            let (run_id, staged) = create_unique_staging_directory_at(&lock)?;
            let lease = LeaseDocument {
                schema_version: LEASE_SCHEMA_VERSION.to_owned(),
                run_id: run_id.clone(),
                project_identity: project_identity.clone(),
                surface: request.surface,
                metadata_file: METADATA_FILE.to_owned(),
                metadata_sha256: request.metadata.sha256.clone(),
                bindings: request.bindings,
                created_at_unix: now,
                expires_at_unix: request.expires_at_unix,
            };
            let encoded = serde_json::to_vec(&lease).map_err(|_| LeaseError::InvalidLease)?;
            if let Err(error) = (|| {
                write_new_private_at(&staged.run, METADATA_FILE, &metadata)?;
                write_new_private_at(&staged.run, ARMED_FILE, &encoded)?;
                staged.run.sync_all().map_err(LeaseError::Io)
            })() {
                let _ = cleanup_failed_staging(&staged, &run_id);
                return Err(error);
            }
            publish_staged_run(self, &lock, &staged, &run_id)?;
            Ok(ArmedLease {
                run_id,
                project_identity,
                surface: lease.surface,
                metadata_sha256: lease.metadata_sha256,
                expires_at_unix: lease.expires_at_unix,
                state: LeaseState::Armed,
                disposition: ArmDisposition::Created,
                bindings: lease.bindings,
                metadata_document: request.metadata.document,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (request, now, project_identity, metadata);
            Err(LeaseError::Unsupported)
        }
    }

    fn status_at(&self, run_id: &str, now: u64) -> Result<LeaseStatus, LeaseError> {
        validate_run_id(run_id)?;
        let Some(run_directory) = self.existing_run_directory(run_id)? else {
            return Ok(absent_status(run_id));
        };
        if path_metadata_exists(&run_directory.join(JOURNAL_FILE))? {
            let artifact = read_finalized_document(&run_directory)?;
            if artifact.journal.run_id != run_id {
                return Err(LeaseError::InvalidJournal);
            }
            if path_metadata_exists(&run_directory.join(ARMED_FILE))? {
                return Err(LeaseError::InvalidJournal);
            }
            let lease = read_optional_lease(&run_directory.join(CLAIMED_FILE))?;
            if let Some(lease) = lease.as_ref() {
                validate_lease(lease)?;
                validate_finalized_lease_binding(&artifact, lease)?;
                if path_metadata_exists(&run_directory.join(JOURNAL_TEMP_FILE))? {
                    let pending = read_bounded_private(
                        &run_directory.join(JOURNAL_TEMP_FILE),
                        MAX_FINAL_ARTIFACT_BYTES,
                    )?;
                    let finalized = read_bounded_private(
                        &run_directory.join(JOURNAL_FILE),
                        MAX_FINAL_ARTIFACT_BYTES,
                    )?;
                    if pending != finalized {
                        return Err(LeaseError::InvalidJournal);
                    }
                }
                return Ok(status_from_optional(
                    run_id,
                    LeaseState::Claimed,
                    Some(lease),
                ));
            }
            if path_metadata_exists(&run_directory.join(JOURNAL_TEMP_FILE))? {
                return Err(LeaseError::InvalidJournal);
            }
            validate_finalized_run_layout(&run_directory)?;
            return Ok(status_from_optional(run_id, LeaseState::Finalized, None));
        }
        if path_metadata_exists(&run_directory.join(CLAIMED_FILE))? {
            let lease = read_lease(&run_directory.join(CLAIMED_FILE))?;
            validate_lease(&lease)?;
            if lease.run_id != run_id {
                return Err(LeaseError::InvalidLease);
            }
            return Ok(status_from_optional(
                run_id,
                LeaseState::Claimed,
                Some(&lease),
            ));
        }
        if path_metadata_exists(&run_directory.join(ARMED_FILE))? {
            let lease = read_lease(&run_directory.join(ARMED_FILE))?;
            validate_lease(&lease)?;
            if lease.run_id != run_id {
                return Err(LeaseError::InvalidLease);
            }
            let state = if lease.expires_at_unix <= now {
                LeaseState::Expired
            } else {
                LeaseState::Armed
            };
            return Ok(status_from_optional(run_id, state, Some(&lease)));
        }
        Err(LeaseError::InvalidLease)
    }

    fn claim_at(
        &self,
        context: &ClaimContext,
        now: u64,
    ) -> Result<Option<ClaimedLease>, LeaseError> {
        validate_bindings(&context.bindings)?;
        let expected_project = project_identity_for_path(&context.project_root)?;
        #[cfg(unix)]
        {
            let Some(lock) = StoreMutationLock::acquire_existing(self)? else {
                return Ok(None);
            };
            self.recover_retired_runs(&lock)?;
            self.recover_staged_runs(&lock)?;
            self.recover_interrupted_finalizations(&lock)?;
            let mut matching = Vec::new();
            for run_id in run_ids_at(&lock.runs)? {
                let handles = OpenRunDirectory::open_locked(self, &lock, &run_id)?
                    .ok_or(LeaseError::NotFound)?;
                if !private_file_exists_at(&handles.run, ARMED_FILE)? {
                    continue;
                }
                let armed = read_private_file_at(&handles.run, ARMED_FILE, MAX_LEASE_BYTES)?;
                let lease: LeaseDocument =
                    serde_json::from_slice(&armed.bytes).map_err(|_| LeaseError::InvalidLease)?;
                validate_lease(&lease)?;
                if lease.run_id != run_id || lease.project_identity != expected_project {
                    continue;
                }
                if lease.bindings != context.bindings {
                    return Err(LeaseError::BindingMismatch);
                }
                if lease.expires_at_unix <= now {
                    return Err(LeaseError::Expired);
                }
                matching.push(lease);
            }
            let result = match matching.len() {
                0 => Ok(None),
                1 => {
                    let lease = matching.pop().ok_or(LeaseError::Ambiguous)?;
                    self.claim_run_unix(&lock, lease).map(Some)
                }
                _ => Err(LeaseError::Ambiguous),
            }?;
            lock.validate_tree(self)?;
            Ok(result)
        }
        #[cfg(not(unix))]
        {
            let _ = (context, now, expected_project);
            Err(LeaseError::Unsupported)
        }
    }

    #[cfg(unix)]
    fn claim_run_unix(
        &self,
        lock: &StoreMutationLock,
        lease: LeaseDocument,
    ) -> Result<ClaimedLease, LeaseError> {
        let (handles, claimed, metadata, claim_guard) = {
            let handles = OpenRunDirectory::open_locked(self, lock, &lease.run_id)?
                .ok_or(LeaseError::NotFound)?;
            handles.validate_named_run(&lease.run_id)?;
            validate_exact_run_entries(&handles.run, &[ARMED_FILE, METADATA_FILE])?;
            let armed = read_private_file_at(&handles.run, ARMED_FILE, MAX_LEASE_BYTES)?;
            let stored_lease: LeaseDocument =
                serde_json::from_slice(&armed.bytes).map_err(|_| LeaseError::InvalidLease)?;
            validate_lease(&stored_lease)?;
            if stored_lease != lease || stored_lease.run_id != lease.run_id {
                return Err(LeaseError::InvalidLease);
            }
            let metadata = read_private_file_at(&handles.run, METADATA_FILE, MAX_METADATA_BYTES)?;
            let metadata_binding = metadata_binding_from_bytes(&metadata.bytes)?;
            if metadata_binding.sha256 != lease.metadata_sha256 {
                return Err(LeaseError::InvalidMetadata);
            }
            validate_private_file_snapshot(&handles.run, ARMED_FILE, &armed)?;
            validate_private_file_snapshot(&handles.run, METADATA_FILE, &metadata)?;
            acquire_claim_guard(&handles.run)?;
            let claim_guard = Arc::new(handles.run.try_clone().map_err(LeaseError::Io)?);
            rustix::fs::renameat_with(
                &handles.run,
                ARMED_FILE,
                &handles.run,
                CLAIMED_FILE,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|error| {
                if error == rustix::io::Errno::EXIST {
                    LeaseError::AlreadyClaimed
                } else {
                    LeaseError::Io(std::io::Error::from_raw_os_error(error.raw_os_error()))
                }
            })?;
            handles.run.sync_all().map_err(LeaseError::Io)?;
            let claimed = read_private_file_at(&handles.run, CLAIMED_FILE, MAX_LEASE_BYTES)?;
            if claimed.bytes != armed.bytes || !same_file_identity(&claimed.stat, &armed.stat) {
                return Err(LeaseError::UnsafeMetadata);
            }
            validate_exact_run_entries(&handles.run, &[CLAIMED_FILE, METADATA_FILE])?;
            handles.validate_named_run(&lease.run_id)?;
            lock.validate_tree(self)?;
            (handles, claimed, metadata_binding.document, claim_guard)
        };
        let recorder = CaptureRecorder::new(JournalBinding {
            run_id: lease.run_id.clone(),
            surface: lease.surface.as_str().to_owned(),
            project_identity: lease.project_identity.clone(),
            metadata_sha256: lease.metadata_sha256.clone(),
            package_launcher_sha256: lease.bindings.package_launcher_sha256.clone(),
            project_config_sha256: lease.bindings.project_config_sha256.clone(),
            setup_receipt_sha256: lease.bindings.setup_receipt_sha256.clone(),
        });
        Ok(ClaimedLease {
            lease,
            metadata,
            recorder,
            handles,
            claimed_snapshot: claimed,
            claim_guard,
        })
    }

    #[cfg(unix)]
    fn recover_retired_runs(&self, lock: &StoreMutationLock) -> Result<(), LeaseError> {
        lock.validate_tree(self)?;
        match rustix::fs::statat(
            &lock.capture,
            RETIRED_DIRECTORY,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Ok(stat) if rustix::fs::FileType::from_raw_mode(stat.st_mode).is_dir() => {}
            Ok(_) | Err(_) => return Err(LeaseError::UnsafeMetadata),
        }
        let retired = open_private_directory_at(&lock.capture, RETIRED_DIRECTORY)?;
        let retired_stat = validate_private_directory_descriptor(&retired)?;
        validate_named_directory_identity(&lock.capture, RETIRED_DIRECTORY, &retired_stat)?;
        let entries =
            rustix::fs::Dir::read_from(&retired).map_err(|_| LeaseError::UnsafeMetadata)?;
        let mut processed = 0_usize;
        for entry in entries {
            let entry = entry.map_err(|_| LeaseError::UnsafeMetadata)?;
            let bytes = entry.file_name().to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            if processed >= MAX_RUNS {
                return Err(LeaseError::StoreBoundExceeded);
            }
            processed += 1;
            let run_id = std::str::from_utf8(bytes).map_err(|_| LeaseError::UnsafeMetadata)?;
            validate_run_id(run_id)?;
            let run = open_private_directory_at(&retired, run_id)?;
            acquire_claim_guard(&run)?;
            let run_stat = validate_private_directory_descriptor(&run)?;
            validate_named_directory_identity(&retired, run_id, &run_stat)?;
            let names = run_entry_names(&run, 4)?;
            let mut snapshots = Vec::with_capacity(names.len());
            for name in &names {
                let maximum = match name.as_str() {
                    METADATA_FILE => MAX_METADATA_BYTES,
                    ARMED_FILE | CLAIMED_FILE => MAX_LEASE_BYTES,
                    JOURNAL_FILE | JOURNAL_TEMP_FILE => MAX_FINAL_ARTIFACT_BYTES,
                    _ => return Err(LeaseError::InvalidJournal),
                };
                snapshots.push((name.clone(), read_private_file_at(&run, name, maximum)?));
            }
            for (name, snapshot) in &snapshots {
                validate_private_file_snapshot(&run, name, snapshot)?;
            }
            for (name, snapshot) in &snapshots {
                unlink_private_file_snapshot(&run, name, snapshot)?;
            }
            validate_exact_run_entries(&run, &[])?;
            validate_named_directory_identity(&retired, run_id, &run_stat)?;
            rustix::fs::unlinkat(&retired, run_id, rustix::fs::AtFlags::REMOVEDIR)
                .map_err(|_| LeaseError::UnsafeMetadata)?;
            retired.sync_all().map_err(LeaseError::Io)?;
        }
        validate_named_directory_identity(&lock.capture, RETIRED_DIRECTORY, &retired_stat)?;
        lock.validate_tree(self)
    }

    #[cfg(unix)]
    fn recover_staged_runs(&self, lock: &StoreMutationLock) -> Result<(), LeaseError> {
        lock.validate_tree(self)?;
        match rustix::fs::statat(
            &lock.capture,
            STAGING_DIRECTORY,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Ok(stat) if rustix::fs::FileType::from_raw_mode(stat.st_mode).is_dir() => {}
            Ok(_) | Err(_) => return Err(LeaseError::UnsafeMetadata),
        }
        let staging = open_private_directory_at(&lock.capture, STAGING_DIRECTORY)?;
        let staging_stat = validate_private_directory_descriptor(&staging)?;
        validate_named_directory_identity(&lock.capture, STAGING_DIRECTORY, &staging_stat)?;
        let entries =
            rustix::fs::Dir::read_from(&staging).map_err(|_| LeaseError::UnsafeMetadata)?;
        let mut processed = 0_usize;
        for entry in entries {
            let entry = entry.map_err(|_| LeaseError::UnsafeMetadata)?;
            let bytes = entry.file_name().to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            if processed >= MAX_RUNS {
                return Err(LeaseError::StoreBoundExceeded);
            }
            processed += 1;
            let run_id = std::str::from_utf8(bytes).map_err(|_| LeaseError::UnsafeMetadata)?;
            validate_run_id(run_id)?;
            let run = open_private_directory_at(&staging, run_id)?;
            acquire_claim_guard(&run)?;
            let run_stat = validate_private_directory_descriptor(&run)?;
            validate_named_directory_identity(&staging, run_id, &run_stat)?;
            let names = run_entry_names(&run, 2)?;
            let mut snapshots = Vec::with_capacity(names.len());
            for name in &names {
                let maximum = match name.as_str() {
                    METADATA_FILE => MAX_METADATA_BYTES,
                    ARMED_FILE => MAX_LEASE_BYTES,
                    _ => return Err(LeaseError::UnsafeMetadata),
                };
                snapshots.push((name.clone(), read_private_file_at(&run, name, maximum)?));
            }
            for (name, snapshot) in &snapshots {
                unlink_private_file_snapshot(&run, name, snapshot)?;
            }
            validate_exact_run_entries(&run, &[])?;
            validate_named_directory_identity(&staging, run_id, &run_stat)?;
            rustix::fs::unlinkat(&staging, run_id, rustix::fs::AtFlags::REMOVEDIR)
                .map_err(|_| LeaseError::UnsafeMetadata)?;
            staging.sync_all().map_err(LeaseError::Io)?;
        }
        validate_named_directory_identity(&lock.capture, STAGING_DIRECTORY, &staging_stat)?;
        lock.validate_tree(self)
    }

    #[cfg(unix)]
    fn recover_interrupted_finalizations(
        &self,
        lock: &StoreMutationLock,
    ) -> Result<(), LeaseError> {
        for run_id in run_ids_at(&lock.runs)? {
            let handles =
                OpenRunDirectory::open_locked(self, lock, &run_id)?.ok_or(LeaseError::NotFound)?;
            let ExplicitAbandonLayout::Partial { pending, journal } =
                explicit_abandon_layout(&handles.run)?
            else {
                continue;
            };
            let claimed = read_private_file_at(&handles.run, CLAIMED_FILE, MAX_LEASE_BYTES)?;
            acquire_claim_guard(&handles.run)?;
            let metadata_snapshot =
                read_private_file_at(&handles.run, METADATA_FILE, MAX_METADATA_BYTES)?;
            let lease: LeaseDocument =
                serde_json::from_slice(&claimed.bytes).map_err(|_| LeaseError::InvalidLease)?;
            validate_lease(&lease)?;
            if lease.run_id != run_id {
                return Err(LeaseError::InvalidLease);
            }
            let metadata = metadata_binding_from_bytes(&metadata_snapshot.bytes)?;
            if metadata.sha256 != lease.metadata_sha256 {
                return Err(LeaseError::InvalidMetadata);
            }
            let pending_snapshot = if pending {
                Some(read_private_file_at(
                    &handles.run,
                    JOURNAL_TEMP_FILE,
                    MAX_FINAL_ARTIFACT_BYTES,
                )?)
            } else {
                None
            };
            let journal_snapshot = if journal {
                Some(read_private_file_at(
                    &handles.run,
                    JOURNAL_FILE,
                    MAX_FINAL_ARTIFACT_BYTES,
                )?)
            } else {
                None
            };
            if classify_partial_finalization(
                &run_id,
                &lease,
                &metadata,
                pending_snapshot.as_ref(),
                journal_snapshot.as_ref(),
            )? == PartialFinalization::Unrecoverable
            {
                // Corrupt partial evidence is never collected automatically.
                // The explicit digest-bound abandon command is its only
                // destructive recovery path.
                continue;
            }

            let mut expected = vec![CLAIMED_FILE, METADATA_FILE];
            if pending {
                expected.push(JOURNAL_TEMP_FILE);
            }
            if journal {
                expected.push(JOURNAL_FILE);
            }
            handles.validate_named_run(&run_id)?;
            validate_exact_run_entries(&handles.run, &expected)?;
            validate_private_file_snapshot(&handles.run, CLAIMED_FILE, &claimed)?;
            validate_private_file_snapshot(&handles.run, METADATA_FILE, &metadata_snapshot)?;
            if let Some(snapshot) = pending_snapshot.as_ref() {
                validate_private_file_snapshot(&handles.run, JOURNAL_TEMP_FILE, snapshot)?;
            }
            if let Some(snapshot) = journal_snapshot.as_ref() {
                validate_private_file_snapshot(&handles.run, JOURNAL_FILE, snapshot)?;
            }

            if journal {
                if let Some(snapshot) = pending_snapshot.as_ref() {
                    unlink_private_file_snapshot(&handles.run, JOURNAL_TEMP_FILE, snapshot)?;
                }
            } else {
                rustix::fs::renameat_with(
                    &handles.run,
                    JOURNAL_TEMP_FILE,
                    &handles.run,
                    JOURNAL_FILE,
                    rustix::fs::RenameFlags::NOREPLACE,
                )
                .map_err(|error| {
                    LeaseError::Io(std::io::Error::from_raw_os_error(error.raw_os_error()))
                })?;
            }
            handles.run.sync_all().map_err(LeaseError::Io)?;
            unlink_private_file_snapshot(&handles.run, CLAIMED_FILE, &claimed)?;
            handles.run.sync_all().map_err(LeaseError::Io)?;
            validate_exact_run_entries(&handles.run, &[JOURNAL_FILE, METADATA_FILE])?;
            handles.validate_named_run(&run_id)?;
        }
        lock.validate_tree(self)
    }

    fn existing_runs_directory(&self) -> Result<Option<PathBuf>, LeaseError> {
        let capture = self.capture_directory();
        match fs::symlink_metadata(&capture) {
            Ok(_) => validate_private_dir(&capture)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(LeaseError::Io(error)),
        }
        let runs = capture.join(RUNS_DIRECTORY);
        match fs::symlink_metadata(&runs) {
            Ok(_) => {
                validate_private_dir(&runs)?;
                Ok(Some(runs))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(LeaseError::Io(error)),
        }
    }

    fn existing_run_directory(&self, run_id: &str) -> Result<Option<PathBuf>, LeaseError> {
        let Some(runs) = self.existing_runs_directory()? else {
            return Ok(None);
        };
        let run = runs.join(run_id);
        match fs::symlink_metadata(&run) {
            Ok(_) => {
                validate_private_dir(&run)?;
                Ok(Some(run))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(LeaseError::Io(error)),
        }
    }

    fn capture_directory(&self) -> PathBuf {
        self.data_root.join(CAPTURE_DIRECTORY)
    }

    #[cfg(all(test, unix))]
    fn runs_directory(&self) -> PathBuf {
        self.capture_directory().join(RUNS_DIRECTORY)
    }
}

/// Successfully claimed lease plus its recorder and immutable metadata.
#[derive(Debug)]
pub struct ClaimedLease {
    lease: LeaseDocument,
    metadata: Value,
    recorder: CaptureRecorder,
    #[cfg(unix)]
    handles: OpenRunDirectory,
    #[cfg(unix)]
    claimed_snapshot: PrivateFileSnapshot,
    #[cfg(unix)]
    claim_guard: Arc<File>,
}

impl ClaimedLease {
    /// Opaque run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.lease.run_id
    }

    /// Surface fixed by the lease.
    #[must_use]
    pub const fn surface(&self) -> Surface {
        self.lease.surface
    }

    /// Validated immutable metadata document.
    #[must_use]
    pub const fn metadata_document(&self) -> &Value {
        &self.metadata
    }

    /// Digest of `metadata_document`.
    #[must_use]
    pub fn metadata_sha256(&self) -> &str {
        &self.lease.metadata_sha256
    }

    /// Shared bounded recorder.
    #[must_use]
    pub fn recorder(&self) -> CaptureRecorder {
        self.recorder.clone()
    }

    /// Wrap a typed rmcp transport without modifying protocol messages.
    pub fn wrap_transport<R, T>(&self, transport: T) -> TappedTransport<R, T>
    where
        R: rmcp::service::ServiceRole,
        T: rmcp::transport::Transport<R>,
    {
        let tapped = TappedTransport::new(transport, self.recorder());
        #[cfg(unix)]
        {
            tapped.with_claim_guard(self.claim_guard.clone())
        }
        #[cfg(not(unix))]
        {
            tapped
        }
    }

    /// Atomically finalize the fixed in-store `journal.json` artifact.
    pub fn finalize(self, outcome: FinalizeOutcome) -> Result<CaptureJournal, LeaseError> {
        let journal = self.recorder.snapshot();
        let artifact = FinalizedDocument {
            schema_version: "sprint11-surface-capture-artifact/1.1".to_owned(),
            outcome,
            metadata_file: METADATA_FILE.to_owned(),
            metadata_sha256: self.lease.metadata_sha256.clone(),
            journal: journal.clone(),
        };
        let bytes = serde_json::to_vec(&artifact).map_err(|_| LeaseError::InvalidJournal)?;
        if bytes.len() > MAX_FINAL_ARTIFACT_BYTES {
            return Err(LeaseError::InvalidJournal);
        }
        #[cfg(unix)]
        {
            acquire_claim_guard(&self.handles.run)?;
            self.handles.validate_named_run(&self.lease.run_id)?;
            validate_exact_run_entries(&self.handles.run, &[CLAIMED_FILE, METADATA_FILE])?;
            validate_private_file_snapshot(
                &self.handles.run,
                CLAIMED_FILE,
                &self.claimed_snapshot,
            )?;
            let metadata =
                read_private_file_at(&self.handles.run, METADATA_FILE, MAX_METADATA_BYTES)?;
            let metadata_binding = metadata_binding_from_bytes(&metadata.bytes)?;
            if metadata_binding.sha256 != self.lease.metadata_sha256 {
                return Err(LeaseError::InvalidMetadata);
            }
            write_new_private_at(&self.handles.run, JOURNAL_TEMP_FILE, &bytes)?;
            rustix::fs::renameat_with(
                &self.handles.run,
                JOURNAL_TEMP_FILE,
                &self.handles.run,
                JOURNAL_FILE,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|error| {
                if error == rustix::io::Errno::EXIST {
                    LeaseError::AlreadyFinalized
                } else {
                    LeaseError::Io(std::io::Error::from_raw_os_error(error.raw_os_error()))
                }
            })?;
            self.handles.run.sync_all().map_err(LeaseError::Io)?;
            unlink_private_file_snapshot(&self.handles.run, CLAIMED_FILE, &self.claimed_snapshot)?;
            self.handles.run.sync_all().map_err(LeaseError::Io)?;
            validate_exact_run_entries(&self.handles.run, &[JOURNAL_FILE, METADATA_FILE])?;
            self.handles.validate_named_run(&self.lease.run_id)?;
            Ok(journal)
        }
        #[cfg(not(unix))]
        {
            let _ = (bytes, journal);
            Err(LeaseError::Unsupported)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseDocument {
    schema_version: String,
    run_id: String,
    project_identity: String,
    surface: Surface,
    metadata_file: String,
    metadata_sha256: String,
    bindings: BindingDigests,
    created_at_unix: u64,
    expires_at_unix: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalizedDocument {
    schema_version: String,
    outcome: FinalizeOutcome,
    metadata_file: String,
    metadata_sha256: String,
    journal: CaptureJournal,
}

/// Fail-closed lease/store error.
#[derive(Debug, Error)]
pub enum LeaseError {
    /// Filesystem I/O failure.
    #[error("surface capture store I/O failed")]
    Io(#[source] std::io::Error),
    /// Store metadata is a symlink, wrong type, wrong owner, or non-private.
    #[error("surface capture store metadata is unsafe")]
    UnsafeMetadata,
    /// Lease or run id is malformed.
    #[error("surface capture lease is invalid")]
    InvalidLease,
    /// Metadata is malformed, oversized, or does not match its digest.
    #[error("surface capture metadata is invalid")]
    InvalidMetadata,
    /// Expiry is not in the bounded future.
    #[error("surface capture lease expiry is invalid")]
    InvalidExpiry,
    /// Exact project/package binding did not match.
    #[error("surface capture binding does not match")]
    BindingMismatch,
    /// More than one lease matched a startup context.
    #[error("surface capture lease selection is ambiguous")]
    Ambiguous,
    /// Lease was already claimed.
    #[error("surface capture lease was already claimed")]
    AlreadyClaimed,
    /// Lease has expired.
    #[error("surface capture lease has expired")]
    Expired,
    /// Run was not found.
    #[error("surface capture run was not found")]
    NotFound,
    /// Store has more runs than the bounded scanner permits.
    #[error("surface capture store bound was exceeded")]
    StoreBoundExceeded,
    /// Journal could not be encoded.
    #[error("surface capture journal is invalid")]
    InvalidJournal,
    /// Run already has a finalized journal.
    #[error("surface capture journal was already finalized")]
    AlreadyFinalized,
    /// Run has not produced a finalized journal.
    #[error("surface capture run is not finalized")]
    NotFinalized,
    /// Caller-supplied capture digest did not match the exact compact wrapper.
    #[error("surface capture digest does not match")]
    DigestMismatch,
    /// Caller-supplied metadata digest did not match the exact claimed run.
    #[error("surface capture metadata digest does not match")]
    MetadataDigestMismatch,
    /// OS randomness was unavailable.
    #[error("surface capture random identity is unavailable")]
    RandomUnavailable,
    /// Another capture store mutation is already in progress.
    #[error("surface capture store mutation is already in progress")]
    StoreBusy,
    /// A live claimant still owns the exact Claimed lease.
    #[error("surface capture claim is active")]
    ClaimActive,
    /// A valid interrupted finalization must be recovered, not discarded.
    #[error("surface capture interrupted finalization requires recovery")]
    RecoveryRequired,
    /// A healthy finalized capture must be consumed, not abandoned.
    #[error("surface capture finalized run must be consumed")]
    ConsumeRequired,
    /// Secure capture-store mutation is unsupported on this platform.
    #[error("surface capture is unsupported on this platform")]
    Unsupported,
}

fn canonical_metadata(binding: &MetadataBinding) -> Result<Vec<u8>, LeaseError> {
    validate_metadata_value(&binding.document, 0)?;
    let mut bytes =
        serde_json::to_vec(&binding.document).map_err(|_| LeaseError::InvalidMetadata)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_METADATA_BYTES
        || !valid_sha256(&binding.sha256)
        || sha256_bytes(&bytes) != binding.sha256
    {
        return Err(LeaseError::InvalidMetadata);
    }
    Ok(bytes)
}

fn validate_metadata_value(value: &Value, depth: usize) -> Result<(), LeaseError> {
    if depth > 16 {
        return Err(LeaseError::InvalidMetadata);
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(value) if value.len() <= 4096 => Ok(()),
        Value::Array(values) if values.len() <= 1024 => {
            for value in values {
                validate_metadata_value(value, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(values) if values.len() <= 1024 => {
            for (key, value) in values {
                if key.is_empty() || key.len() > 128 {
                    return Err(LeaseError::InvalidMetadata);
                }
                validate_metadata_value(value, depth + 1)?;
            }
            Ok(())
        }
        _ => Err(LeaseError::InvalidMetadata),
    }
}

fn read_metadata(run_directory: &Path, lease: &LeaseDocument) -> Result<Value, LeaseError> {
    if lease.metadata_file != METADATA_FILE {
        return Err(LeaseError::InvalidLease);
    }
    let bytes = read_bounded_private(&run_directory.join(METADATA_FILE), MAX_METADATA_BYTES)?;
    if sha256_bytes(&bytes) != lease.metadata_sha256 {
        return Err(LeaseError::InvalidMetadata);
    }
    let metadata: Value =
        serde_json::from_slice(&bytes).map_err(|_| LeaseError::InvalidMetadata)?;
    validate_metadata_value(&metadata, 0)?;
    let mut canonical = serde_json::to_vec(&metadata).map_err(|_| LeaseError::InvalidMetadata)?;
    canonical.push(b'\n');
    if canonical != bytes {
        return Err(LeaseError::InvalidMetadata);
    }
    Ok(metadata)
}

fn read_metadata_unbound(run_directory: &Path) -> Result<MetadataBinding, LeaseError> {
    let bytes = read_bounded_private(&run_directory.join(METADATA_FILE), MAX_METADATA_BYTES)?;
    metadata_binding_from_bytes(&bytes)
}

fn metadata_binding_from_bytes(bytes: &[u8]) -> Result<MetadataBinding, LeaseError> {
    let document: Value = serde_json::from_slice(bytes).map_err(|_| LeaseError::InvalidMetadata)?;
    let binding = MetadataBinding::from_document(document)?;
    if canonical_metadata(&binding)? != bytes {
        return Err(LeaseError::InvalidMetadata);
    }
    Ok(binding)
}

fn read_finalized_document(run_directory: &Path) -> Result<FinalizedDocument, LeaseError> {
    read_compact_finalized_document(run_directory).map(|(artifact, _)| artifact)
}

fn read_compact_finalized_document(
    run_directory: &Path,
) -> Result<(FinalizedDocument, Vec<u8>), LeaseError> {
    read_compact_finalized_file(&run_directory.join(JOURNAL_FILE))
}

fn read_compact_finalized_file(path: &Path) -> Result<(FinalizedDocument, Vec<u8>), LeaseError> {
    let bytes = read_bounded_private(path, MAX_FINAL_ARTIFACT_BYTES)?;
    compact_finalized_from_bytes(bytes)
}

fn compact_finalized_from_bytes(
    bytes: Vec<u8>,
) -> Result<(FinalizedDocument, Vec<u8>), LeaseError> {
    let artifact: FinalizedDocument =
        serde_json::from_slice(&bytes).map_err(|_| LeaseError::InvalidJournal)?;
    let compact = serde_json::to_vec(&artifact).map_err(|_| LeaseError::InvalidJournal)?;
    if compact != bytes {
        return Err(LeaseError::InvalidJournal);
    }
    validate_finalized_binding(&artifact, &artifact.journal.run_id)?;
    Ok((artifact, compact))
}

fn validate_finalized_binding(
    artifact: &FinalizedDocument,
    run_id: &str,
) -> Result<(), LeaseError> {
    let journal = &artifact.journal;
    if artifact.schema_version != "sprint11-surface-capture-artifact/1.1"
        || artifact.metadata_file != METADATA_FILE
        || !valid_sha256(&artifact.metadata_sha256)
        || !valid_run_id(run_id)
        || journal.schema_version != JOURNAL_SCHEMA_VERSION
        || journal.run_id != run_id
        || journal.metadata_sha256 != artifact.metadata_sha256
        || !valid_sha256(&journal.project_identity)
        || !valid_sha256(&journal.metadata_sha256)
        || !valid_sha256(&journal.package_launcher_sha256)
        || !valid_sha256(&journal.project_config_sha256)
        || !valid_sha256(&journal.setup_receipt_sha256)
        || !validate_capture_journal(journal)
    {
        return Err(LeaseError::InvalidJournal);
    }
    Ok(())
}

fn validate_finalized_lease_binding(
    artifact: &FinalizedDocument,
    lease: &LeaseDocument,
) -> Result<(), LeaseError> {
    let journal = &artifact.journal;
    if journal.run_id != lease.run_id
        || journal.surface != lease.surface.as_str()
        || journal.project_identity != lease.project_identity
        || journal.metadata_sha256 != lease.metadata_sha256
        || journal.package_launcher_sha256 != lease.bindings.package_launcher_sha256
        || journal.project_config_sha256 != lease.bindings.project_config_sha256
        || journal.setup_receipt_sha256 != lease.bindings.setup_receipt_sha256
    {
        return Err(LeaseError::InvalidJournal);
    }
    Ok(())
}

fn validate_finalized_run_layout(run_directory: &Path) -> Result<(), LeaseError> {
    validate_run_layout(run_directory, &[JOURNAL_FILE, METADATA_FILE])
}

fn validate_run_layout(run_directory: &Path, expected: &[&str]) -> Result<(), LeaseError> {
    validate_private_dir(run_directory)?;
    let mut names = Vec::with_capacity(expected.len());
    for entry in fs::read_dir(run_directory).map_err(LeaseError::Io)? {
        let entry = entry.map_err(LeaseError::Io)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| LeaseError::UnsafeMetadata)?;
        names.push(name);
        if names.len() > expected.len() {
            return Err(LeaseError::InvalidJournal);
        }
    }
    names.sort_unstable();
    let mut expected = expected.iter().map(ToString::to_string).collect::<Vec<_>>();
    expected.sort_unstable();
    if names != expected {
        return Err(LeaseError::InvalidJournal);
    }
    for name in expected {
        validate_private_file(&run_directory.join(name))?;
    }
    Ok(())
}

fn validate_lease(lease: &LeaseDocument) -> Result<(), LeaseError> {
    if lease.schema_version != LEASE_SCHEMA_VERSION
        || lease.metadata_file != METADATA_FILE
        || !valid_run_id(&lease.run_id)
        || !valid_sha256(&lease.project_identity)
        || !valid_sha256(&lease.metadata_sha256)
        || validate_bindings(&lease.bindings).is_err()
        || lease.created_at_unix >= lease.expires_at_unix
        || lease.expires_at_unix - lease.created_at_unix > MAX_LEASE_LIFETIME_SECONDS
    {
        return Err(LeaseError::InvalidLease);
    }
    Ok(())
}

fn validate_bindings(bindings: &BindingDigests) -> Result<(), LeaseError> {
    if [
        &bindings.package_launcher_sha256,
        &bindings.project_config_sha256,
        &bindings.setup_receipt_sha256,
    ]
    .into_iter()
    .all(|digest| valid_sha256(digest))
    {
        Ok(())
    } else {
        Err(LeaseError::BindingMismatch)
    }
}

fn validate_expiry(expiry: u64, now: u64) -> Result<(), LeaseError> {
    if expiry <= now || expiry.saturating_sub(now) > MAX_LEASE_LIFETIME_SECONDS {
        Err(LeaseError::InvalidExpiry)
    } else {
        Ok(())
    }
}

fn read_lease(path: &Path) -> Result<LeaseDocument, LeaseError> {
    let bytes = read_bounded_private(path, MAX_LEASE_BYTES)?;
    serde_json::from_slice(&bytes).map_err(|_| LeaseError::InvalidLease)
}

fn read_optional_lease(path: &Path) -> Result<Option<LeaseDocument>, LeaseError> {
    match fs::symlink_metadata(path) {
        Ok(_) => read_lease(path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(LeaseError::Io(error)),
    }
}

#[cfg(unix)]
fn read_bounded_private(path: &Path, maximum: usize) -> Result<Vec<u8>, LeaseError> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| LeaseError::UnsafeMetadata)?;
    let file = File::from(descriptor);
    let stat = validate_private_file_descriptor(&file)?;
    let size = usize::try_from(stat.st_size).map_err(|_| LeaseError::UnsafeMetadata)?;
    if size > maximum {
        return Err(LeaseError::UnsafeMetadata);
    }
    let mut bytes = Vec::with_capacity(size);
    (&file)
        .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(LeaseError::Io)?;
    if bytes.len() > maximum {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_bounded_private(path: &Path, maximum: usize) -> Result<Vec<u8>, LeaseError> {
    validate_private_file(path)?;
    let metadata = fs::metadata(path).map_err(LeaseError::Io)?;
    let size = usize::try_from(metadata.len()).map_err(|_| LeaseError::UnsafeMetadata)?;
    if size > maximum {
        return Err(LeaseError::UnsafeMetadata);
    }
    let file = File::open(path).map_err(LeaseError::Io)?;
    let mut bytes = Vec::with_capacity(size);
    file.take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(LeaseError::Io)?;
    if bytes.len() > maximum {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(bytes)
}

/// Hash the canonical project root with the lease binding domain.
pub fn project_identity_for_path(project_root: &Path) -> Result<String, LeaseError> {
    let canonical = fs::canonicalize(project_root).map_err(LeaseError::Io)?;
    let metadata = fs::metadata(&canonical).map_err(LeaseError::Io)?;
    if !metadata.is_dir() {
        return Err(LeaseError::BindingMismatch);
    }
    let mut digest = Sha256::new();
    digest.update(b"sprint11-canonical-project-root-v1\0");
    digest.update(path_bytes(&canonical));
    Ok(format_digest(digest.finalize().as_slice()))
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> &[u8] {
    use std::os::unix::ffi::OsStrExt as _;
    path.as_os_str().as_bytes()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> &[u8] {
    path.to_str().unwrap_or_default().as_bytes()
}

#[cfg(unix)]
fn run_ids_at(runs: &File) -> Result<Vec<String>, LeaseError> {
    let entries = rustix::fs::Dir::read_from(runs).map_err(|_| LeaseError::UnsafeMetadata)?;
    let mut run_ids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| LeaseError::UnsafeMetadata)?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        if run_ids.len() >= MAX_RUNS {
            return Err(LeaseError::StoreBoundExceeded);
        }
        let run_id = std::str::from_utf8(bytes)
            .map_err(|_| LeaseError::UnsafeMetadata)?
            .to_owned();
        validate_run_id(&run_id)?;
        let run = open_private_directory_at(runs, &run_id)?;
        let run_stat = validate_private_directory_descriptor(&run)?;
        validate_named_directory_identity(runs, &run_id, &run_stat)?;
        run_ids.push(run_id);
    }
    Ok(run_ids)
}

#[cfg(unix)]
fn validate_run_capacity_at(runs: &File) -> Result<(), LeaseError> {
    if run_ids_at(runs)?.len() >= MAX_RUNS {
        Err(LeaseError::StoreBoundExceeded)
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn find_retryable_armed(
    store: &LeaseStore,
    lock: &StoreMutationLock,
    request: &ArmRequest,
    project_identity: &str,
    canonical_metadata: &[u8],
    now: u64,
) -> Result<Option<ArmedLease>, LeaseError> {
    let mut matching = Vec::new();
    for run_id in run_ids_at(&lock.runs)? {
        let handles =
            OpenRunDirectory::open_locked(store, lock, &run_id)?.ok_or(LeaseError::NotFound)?;
        let has_armed = private_file_exists_at(&handles.run, ARMED_FILE)?;
        let has_claimed = private_file_exists_at(&handles.run, CLAIMED_FILE)?;
        if has_armed && has_claimed {
            return Err(LeaseError::InvalidLease);
        }
        if !has_armed && !has_claimed {
            continue;
        }
        let mut expected = vec![METADATA_FILE];
        let lease_file = if has_armed {
            expected.push(ARMED_FILE);
            ARMED_FILE
        } else {
            expected.push(CLAIMED_FILE);
            match explicit_abandon_layout(&handles.run)? {
                ExplicitAbandonLayout::BareClaimed => {}
                ExplicitAbandonLayout::Partial { pending, journal } => {
                    if pending {
                        expected.push(JOURNAL_TEMP_FILE);
                    }
                    if journal {
                        expected.push(JOURNAL_FILE);
                    }
                }
                ExplicitAbandonLayout::Armed | ExplicitAbandonLayout::Finalized => {
                    return Err(LeaseError::InvalidLease);
                }
            }
            CLAIMED_FILE
        };
        validate_exact_run_entries(&handles.run, &expected)?;
        let lease_snapshot = read_private_file_at(&handles.run, lease_file, MAX_LEASE_BYTES)?;
        let lease: LeaseDocument =
            serde_json::from_slice(&lease_snapshot.bytes).map_err(|_| LeaseError::InvalidLease)?;
        validate_lease(&lease)?;
        if lease.run_id != run_id {
            return Err(LeaseError::InvalidLease);
        }
        if lease.project_identity != project_identity {
            continue;
        }
        let metadata = read_private_file_at(&handles.run, METADATA_FILE, MAX_METADATA_BYTES)?;
        let metadata_binding = metadata_binding_from_bytes(&metadata.bytes)?;
        if metadata_binding.sha256 != lease.metadata_sha256 {
            return Err(LeaseError::InvalidMetadata);
        }
        let exact = lease.surface == request.surface
            && lease.metadata_sha256 == request.metadata.sha256
            && lease.bindings == request.bindings
            && metadata.bytes == canonical_metadata;
        let mut snapshots = vec![(lease_file, lease_snapshot), (METADATA_FILE, metadata)];
        if expected.contains(&JOURNAL_TEMP_FILE) {
            snapshots.push((
                JOURNAL_TEMP_FILE,
                read_private_file_at(&handles.run, JOURNAL_TEMP_FILE, MAX_FINAL_ARTIFACT_BYTES)?,
            ));
        }
        if expected.contains(&JOURNAL_FILE) {
            snapshots.push((
                JOURNAL_FILE,
                read_private_file_at(&handles.run, JOURNAL_FILE, MAX_FINAL_ARTIFACT_BYTES)?,
            ));
        }
        matching.push((
            lease,
            metadata_binding.document,
            exact && has_armed,
            has_armed,
            handles,
            expected,
            snapshots,
        ));
    }
    match matching.len() {
        0 => Ok(None),
        1 => {
            let (lease, metadata_document, exact, was_armed, handles, expected, snapshots) =
                matching.pop().ok_or(LeaseError::Ambiguous)?;
            handles.validate_named_run(&lease.run_id)?;
            validate_exact_run_entries(&handles.run, &expected)?;
            for (name, snapshot) in &snapshots {
                validate_private_file_snapshot(&handles.run, name, snapshot)?;
            }
            let state = if !was_armed {
                LeaseState::Claimed
            } else if lease.expires_at_unix <= now {
                LeaseState::Expired
            } else {
                LeaseState::Armed
            };
            Ok(Some(ArmedLease {
                run_id: lease.run_id,
                project_identity: lease.project_identity,
                surface: lease.surface,
                metadata_sha256: lease.metadata_sha256,
                expires_at_unix: lease.expires_at_unix,
                state,
                disposition: if exact {
                    ArmDisposition::Recovered
                } else {
                    ArmDisposition::Conflict
                },
                bindings: lease.bindings,
                metadata_document,
            }))
        }
        _ => Err(LeaseError::Ambiguous),
    }
}

#[cfg(unix)]
struct StagedRun {
    parent: File,
    run: File,
    parent_stat: rustix::fs::Stat,
    run_stat: rustix::fs::Stat,
}

#[cfg(unix)]
impl StagedRun {
    fn validate_named(&self, lock: &StoreMutationLock, run_id: &str) -> Result<(), LeaseError> {
        validate_named_directory_identity(&lock.capture, STAGING_DIRECTORY, &self.parent_stat)?;
        validate_named_directory_identity(&self.parent, run_id, &self.run_stat)
    }
}

#[cfg(unix)]
fn create_unique_staging_directory_at(
    lock: &StoreMutationLock,
) -> Result<(String, StagedRun), LeaseError> {
    use rustix::fs::Mode;

    let parent = open_or_create_private_directory_at(&lock.capture, STAGING_DIRECTORY)?;
    let parent_stat = validate_private_directory_descriptor(&parent)?;
    validate_named_directory_identity(&lock.capture, STAGING_DIRECTORY, &parent_stat)?;
    for _ in 0..16 {
        let run_id = random_run_id()?;
        match rustix::fs::mkdirat(&parent, run_id.as_str(), Mode::from_raw_mode(0o700)) {
            Ok(()) => {
                parent.sync_all().map_err(LeaseError::Io)?;
                let run = open_private_directory_at(&parent, &run_id)?;
                let run_stat = validate_private_directory_descriptor(&run)?;
                let staged = StagedRun {
                    parent: reopen_private_directory(&parent)?,
                    run,
                    parent_stat,
                    run_stat,
                };
                staged.validate_named(lock, &run_id)?;
                return Ok((run_id, staged));
            }
            Err(rustix::io::Errno::EXIST) => {}
            Err(error) => {
                return Err(LeaseError::Io(std::io::Error::from_raw_os_error(
                    error.raw_os_error(),
                )));
            }
        }
    }
    Err(LeaseError::RandomUnavailable)
}

#[cfg(unix)]
fn publish_staged_run(
    store: &LeaseStore,
    lock: &StoreMutationLock,
    staged: &StagedRun,
    run_id: &str,
) -> Result<(), LeaseError> {
    staged.validate_named(lock, run_id)?;
    lock.validate_tree(store)?;
    validate_exact_run_entries(&staged.run, &[ARMED_FILE, METADATA_FILE])?;
    let armed = read_private_file_at(&staged.run, ARMED_FILE, MAX_LEASE_BYTES)?;
    let metadata = read_private_file_at(&staged.run, METADATA_FILE, MAX_METADATA_BYTES)?;
    let lease: LeaseDocument =
        serde_json::from_slice(&armed.bytes).map_err(|_| LeaseError::InvalidLease)?;
    validate_lease(&lease)?;
    if lease.run_id != run_id {
        return Err(LeaseError::InvalidLease);
    }
    let metadata_binding = metadata_binding_from_bytes(&metadata.bytes)?;
    if metadata_binding.sha256 != lease.metadata_sha256 {
        return Err(LeaseError::InvalidMetadata);
    }
    validate_private_file_snapshot(&staged.run, ARMED_FILE, &armed)?;
    validate_private_file_snapshot(&staged.run, METADATA_FILE, &metadata)?;
    rustix::fs::renameat_with(
        &staged.parent,
        run_id,
        &lock.runs,
        run_id,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|error| LeaseError::Io(std::io::Error::from_raw_os_error(error.raw_os_error())))?;
    // Persist destination publication before source removal. Any crash-visible
    // copy is therefore a complete, fully synced Armed run.
    lock.runs.sync_all().map_err(LeaseError::Io)?;
    staged.parent.sync_all().map_err(LeaseError::Io)?;
    validate_named_directory_identity(&lock.runs, run_id, &staged.run_stat)?;
    if !matches!(
        rustix::fs::statat(
            &staged.parent,
            run_id,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW
        ),
        Err(rustix::io::Errno::NOENT)
    ) {
        return Err(LeaseError::UnsafeMetadata);
    }
    let published =
        OpenRunDirectory::open_locked(store, lock, run_id)?.ok_or(LeaseError::UnsafeMetadata)?;
    if !same_file_identity(&published.run_stat, &staged.run_stat) {
        return Err(LeaseError::UnsafeMetadata);
    }
    validate_exact_run_entries(&published.run, &[ARMED_FILE, METADATA_FILE])?;
    lock.validate_tree(store)
}

#[cfg(unix)]
fn cleanup_failed_staging(staged: &StagedRun, run_id: &str) -> Result<(), LeaseError> {
    validate_named_directory_identity(&staged.parent, run_id, &staged.run_stat)?;
    let names = run_entry_names(&staged.run, 2)?;
    let mut snapshots = Vec::with_capacity(names.len());
    for name in &names {
        let maximum = match name.as_str() {
            ARMED_FILE => MAX_LEASE_BYTES,
            METADATA_FILE => MAX_METADATA_BYTES,
            _ => return Err(LeaseError::UnsafeMetadata),
        };
        snapshots.push((
            name.clone(),
            read_private_file_at(&staged.run, name, maximum)?,
        ));
    }
    for (name, snapshot) in &snapshots {
        unlink_private_file_snapshot(&staged.run, name, snapshot)?;
    }
    validate_exact_run_entries(&staged.run, &[])?;
    validate_named_directory_identity(&staged.parent, run_id, &staged.run_stat)?;
    rustix::fs::unlinkat(&staged.parent, run_id, rustix::fs::AtFlags::REMOVEDIR)
        .map_err(|_| LeaseError::UnsafeMetadata)?;
    staged.parent.sync_all().map_err(LeaseError::Io)
}

fn random_run_id() -> Result<String, LeaseError> {
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|_| LeaseError::RandomUnavailable)?;
    let mut output = String::with_capacity(64);
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    Ok(output)
}

struct StoreMutationLock {
    #[cfg(unix)]
    data_root: File,
    #[cfg(unix)]
    capture: File,
    #[cfg(unix)]
    runs: File,
    #[cfg(unix)]
    capture_stat: rustix::fs::Stat,
    #[cfg(unix)]
    runs_stat: rustix::fs::Stat,
}

impl StoreMutationLock {
    #[cfg(test)]
    fn acquire(store: &LeaseStore) -> Result<Self, LeaseError> {
        Self::acquire_inner(store, false)?.ok_or(LeaseError::UnsafeMetadata)
    }

    fn acquire_or_create(store: &LeaseStore) -> Result<Self, LeaseError> {
        Self::acquire_inner(store, true)?.ok_or(LeaseError::UnsafeMetadata)
    }

    fn acquire_existing(store: &LeaseStore) -> Result<Option<Self>, LeaseError> {
        Self::acquire_inner(store, false)
    }

    fn acquire_inner(store: &LeaseStore, create: bool) -> Result<Option<Self>, LeaseError> {
        #[cfg(unix)]
        {
            let data_root = open_private_directory(&store.data_root)?;
            let opened_data_root =
                file_identity(&validate_private_directory_descriptor(&data_root)?)?;
            if opened_data_root != store.data_root_identity {
                return Err(LeaseError::UnsafeMetadata);
            }
            rustix::fs::flock(
                &data_root,
                rustix::fs::FlockOperation::NonBlockingLockExclusive,
            )
            .map_err(|error| {
                if error == rustix::io::Errno::AGAIN {
                    LeaseError::StoreBusy
                } else {
                    LeaseError::UnsafeMetadata
                }
            })?;
            validate_data_root_path(store, &data_root)?;

            let capture = if create {
                open_or_create_private_directory_at(&data_root, CAPTURE_DIRECTORY)?
            } else {
                match open_optional_private_directory_at(&data_root, CAPTURE_DIRECTORY)? {
                    Some(capture) => capture,
                    None => return Ok(None),
                }
            };
            let capture_stat = validate_private_directory_descriptor(&capture)?;
            validate_named_directory_identity(&data_root, CAPTURE_DIRECTORY, &capture_stat)?;
            open_or_create_legacy_store_marker(&capture, create)?;
            let runs = if create {
                open_or_create_private_directory_at(&capture, RUNS_DIRECTORY)?
            } else {
                open_private_directory_at(&capture, RUNS_DIRECTORY)?
            };
            let runs_stat = validate_private_directory_descriptor(&runs)?;
            let lock = Self {
                data_root,
                capture,
                runs,
                capture_stat,
                runs_stat,
            };
            lock.validate_tree(store)?;
            Ok(Some(lock))
        }
        #[cfg(not(unix))]
        {
            let _ = (store, create);
            Err(LeaseError::Unsupported)
        }
    }

    #[cfg(unix)]
    fn validate_tree(&self, store: &LeaseStore) -> Result<(), LeaseError> {
        validate_data_root_path(store, &self.data_root)?;
        validate_named_directory_identity(&self.data_root, CAPTURE_DIRECTORY, &self.capture_stat)?;
        validate_named_directory_identity(&self.capture, RUNS_DIRECTORY, &self.runs_stat)
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
#[derive(Debug)]
struct OpenRunDirectory {
    store: LeaseStore,
    data_root: File,
    capture: File,
    runs: File,
    run: File,
    capture_stat: rustix::fs::Stat,
    runs_stat: rustix::fs::Stat,
    run_stat: rustix::fs::Stat,
}

#[cfg(unix)]
impl OpenRunDirectory {
    #[cfg(test)]
    fn open(store: &LeaseStore, run_id: &str) -> Result<Option<Self>, LeaseError> {
        let data_root = open_private_directory(&store.data_root)?;
        let opened_data_root = file_identity(&validate_private_directory_descriptor(&data_root)?)?;
        if opened_data_root != store.data_root_identity {
            return Err(LeaseError::UnsafeMetadata);
        }
        let capture = open_private_directory_at(&data_root, CAPTURE_DIRECTORY)?;
        let runs = open_private_directory_at(&capture, RUNS_DIRECTORY)?;
        Self::open_from_descriptors(store, data_root, capture, runs, run_id)
    }

    fn open_locked(
        store: &LeaseStore,
        lock: &StoreMutationLock,
        run_id: &str,
    ) -> Result<Option<Self>, LeaseError> {
        lock.validate_tree(store)?;
        let data_root = reopen_private_directory(&lock.data_root)?;
        let capture = reopen_private_directory(&lock.capture)?;
        let runs = reopen_private_directory(&lock.runs)?;
        let opened = Self::open_from_descriptors(store, data_root, capture, runs, run_id)?;
        if let Some(opened) = opened.as_ref()
            && (!same_file_identity(&opened.capture_stat, &lock.capture_stat)
                || !same_file_identity(&opened.runs_stat, &lock.runs_stat))
        {
            return Err(LeaseError::UnsafeMetadata);
        }
        Ok(opened)
    }

    fn open_from_descriptors(
        store: &LeaseStore,
        data_root: File,
        capture: File,
        runs: File,
        run_id: &str,
    ) -> Result<Option<Self>, LeaseError> {
        use rustix::fs::{Mode, OFlags};

        let capture_stat = validate_private_directory_descriptor(&capture)?;
        let runs_stat = validate_private_directory_descriptor(&runs)?;
        let descriptor = match rustix::fs::openat(
            &runs,
            run_id,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(_) => return Err(LeaseError::UnsafeMetadata),
        };
        let run = File::from(descriptor);
        let run_stat = validate_private_directory_descriptor(&run)?;
        let opened = Self {
            store: store.clone(),
            data_root,
            capture,
            runs,
            run,
            capture_stat,
            runs_stat,
            run_stat,
        };
        opened.validate_named_run(run_id)?;
        Ok(Some(opened))
    }

    fn validate_named_run(&self, run_id: &str) -> Result<(), LeaseError> {
        validate_data_root_path(&self.store, &self.data_root)?;
        validate_named_directory_identity(&self.data_root, CAPTURE_DIRECTORY, &self.capture_stat)?;
        validate_named_directory_identity(&self.capture, RUNS_DIRECTORY, &self.runs_stat)?;
        let named = rustix::fs::statat(&self.runs, run_id, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| LeaseError::UnsafeMetadata)?;
        if !same_file_identity(&named, &self.run_stat)
            || !rustix::fs::FileType::from_raw_mode(named.st_mode).is_dir()
        {
            return Err(LeaseError::UnsafeMetadata);
        }
        Ok(())
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct PrivateFileSnapshot {
    _file: File,
    stat: rustix::fs::Stat,
    bytes: Vec<u8>,
}

#[cfg(unix)]
fn open_private_directory(path: &Path) -> Result<File, LeaseError> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| LeaseError::UnsafeMetadata)?;
    let directory = File::from(descriptor);
    validate_private_directory_descriptor(&directory)?;
    Ok(directory)
}

#[cfg(unix)]
fn open_private_directory_at(parent: &File, name: &str) -> Result<File, LeaseError> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| LeaseError::UnsafeMetadata)?;
    let directory = File::from(descriptor);
    validate_private_directory_descriptor(&directory)?;
    Ok(directory)
}

#[cfg(unix)]
fn reopen_private_directory(directory: &File) -> Result<File, LeaseError> {
    open_private_directory_at(directory, ".")
}

#[cfg(unix)]
fn open_optional_private_directory_at(
    parent: &File,
    name: &str,
) -> Result<Option<File>, LeaseError> {
    match rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Ok(_) => open_private_directory_at(parent, name).map(Some),
        Err(_) => Err(LeaseError::UnsafeMetadata),
    }
}

#[cfg(unix)]
fn open_or_create_private_directory_at(parent: &File, name: &str) -> Result<File, LeaseError> {
    use rustix::fs::Mode;

    match rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
        Ok(()) => parent.sync_all().map_err(LeaseError::Io)?,
        Err(rustix::io::Errno::EXIST) => {}
        Err(error) => {
            return Err(LeaseError::Io(std::io::Error::from_raw_os_error(
                error.raw_os_error(),
            )));
        }
    }
    open_private_directory_at(parent, name)
}

#[cfg(unix)]
fn open_or_create_legacy_store_marker(capture: &File, create: bool) -> Result<(), LeaseError> {
    use rustix::fs::{Mode, OFlags};

    let mut created = false;
    let descriptor = if create {
        match rustix::fs::openat(
            capture,
            STORE_LOCK_FILE,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(descriptor) => {
                created = true;
                descriptor
            }
            Err(rustix::io::Errno::EXIST) => rustix::fs::openat(
                capture,
                STORE_LOCK_FILE,
                OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| LeaseError::UnsafeMetadata)?,
            Err(error) => {
                return Err(LeaseError::Io(std::io::Error::from_raw_os_error(
                    error.raw_os_error(),
                )));
            }
        }
    } else {
        rustix::fs::openat(
            capture,
            STORE_LOCK_FILE,
            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| LeaseError::UnsafeMetadata)?
    };
    let marker = File::from(descriptor);
    let marker_stat = validate_private_file_descriptor(&marker)?;
    if marker_stat.st_size != 0 {
        return Err(LeaseError::UnsafeMetadata);
    }
    validate_named_file_identity(capture, STORE_LOCK_FILE, &marker_stat)?;
    if created {
        marker.sync_all().map_err(LeaseError::Io)?;
        capture.sync_all().map_err(LeaseError::Io)?;
    }
    Ok(())
}

#[cfg(unix)]
fn validate_data_root_path(store: &LeaseStore, locked: &File) -> Result<(), LeaseError> {
    let locked_identity = file_identity(&validate_private_directory_descriptor(locked)?)?;
    if locked_identity != store.data_root_identity {
        return Err(LeaseError::UnsafeMetadata);
    }
    let named = open_private_directory(&store.data_root)?;
    let named_identity = file_identity(&validate_private_directory_descriptor(&named)?)?;
    if named_identity != locked_identity {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(())
}

#[cfg(unix)]
fn read_private_file_at(
    directory: &File,
    name: &str,
    maximum: usize,
) -> Result<PrivateFileSnapshot, LeaseError> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| LeaseError::UnsafeMetadata)?;
    let file = File::from(descriptor);
    let stat = validate_private_file_descriptor(&file)?;
    let size = usize::try_from(stat.st_size).map_err(|_| LeaseError::UnsafeMetadata)?;
    if size > maximum {
        return Err(LeaseError::UnsafeMetadata);
    }
    let mut bytes = Vec::with_capacity(size);
    (&file)
        .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(LeaseError::Io)?;
    if bytes.len() > maximum {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(PrivateFileSnapshot {
        _file: file,
        stat,
        bytes,
    })
}

#[cfg(unix)]
fn acquire_claim_guard(run_directory: &File) -> Result<(), LeaseError> {
    rustix::fs::flock(
        run_directory,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .map_err(|error| {
        if error == rustix::io::Errno::AGAIN {
            LeaseError::ClaimActive
        } else {
            LeaseError::UnsafeMetadata
        }
    })
}

#[cfg(unix)]
fn private_file_exists_at(directory: &File, name: &str) -> Result<bool, LeaseError> {
    match rustix::fs::statat(directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => {
            if !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_file() {
                return Err(LeaseError::UnsafeMetadata);
            }
            Ok(true)
        }
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(_) => Err(LeaseError::UnsafeMetadata),
    }
}

#[cfg(unix)]
fn write_new_private_at(directory: &File, name: &str, bytes: &[u8]) -> Result<(), LeaseError> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::openat(
        directory,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|error| {
        if error == rustix::io::Errno::EXIST {
            LeaseError::AlreadyFinalized
        } else {
            LeaseError::Io(std::io::Error::from_raw_os_error(error.raw_os_error()))
        }
    })?;
    let mut file = File::from(descriptor);
    rustix::fs::fchmod(&file, Mode::from_raw_mode(0o600))
        .map_err(|_| LeaseError::UnsafeMetadata)?;
    validate_private_file_descriptor(&file)?;
    file.write_all(bytes).map_err(LeaseError::Io)?;
    file.sync_all().map_err(LeaseError::Io)?;
    Ok(())
}

#[cfg(unix)]
fn validate_private_file_snapshot(
    directory: &File,
    name: &str,
    expected: &PrivateFileSnapshot,
) -> Result<(), LeaseError> {
    let current = read_private_file_at(directory, name, expected.bytes.len())?;
    if !same_file_identity(&current.stat, &expected.stat) || current.bytes != expected.bytes {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(())
}

#[cfg(unix)]
fn unlink_private_file_snapshot(
    directory: &File,
    name: &str,
    expected: &PrivateFileSnapshot,
) -> Result<(), LeaseError> {
    validate_private_file_snapshot(directory, name, expected)?;
    let named = rustix::fs::statat(directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| LeaseError::UnsafeMetadata)?;
    if !same_file_identity(&named, &expected.stat)
        || !rustix::fs::FileType::from_raw_mode(named.st_mode).is_file()
    {
        return Err(LeaseError::UnsafeMetadata);
    }
    rustix::fs::unlinkat(directory, name, rustix::fs::AtFlags::empty())
        .map_err(|_| LeaseError::UnsafeMetadata)
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExplicitAbandonLayout {
    Armed,
    BareClaimed,
    Partial { pending: bool, journal: bool },
    Finalized,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartialFinalization {
    Recoverable,
    Unrecoverable,
}

#[cfg(unix)]
fn explicit_abandon_layout(directory: &File) -> Result<ExplicitAbandonLayout, LeaseError> {
    let names = run_entry_names(directory, 4)?;
    let has = |name: &str| names.iter().any(|entry| entry == name);
    if names.len() == 2 && has(ARMED_FILE) && has(METADATA_FILE) {
        return Ok(ExplicitAbandonLayout::Armed);
    }
    if names.len() == 2 && has(CLAIMED_FILE) && has(METADATA_FILE) {
        return Ok(ExplicitAbandonLayout::BareClaimed);
    }
    if names.len() == 2 && has(JOURNAL_FILE) && has(METADATA_FILE) {
        return Ok(ExplicitAbandonLayout::Finalized);
    }
    let pending = has(JOURNAL_TEMP_FILE);
    let journal = has(JOURNAL_FILE);
    if has(CLAIMED_FILE)
        && has(METADATA_FILE)
        && (pending || journal)
        && names.len() == 2 + usize::from(pending) + usize::from(journal)
    {
        return Ok(ExplicitAbandonLayout::Partial { pending, journal });
    }
    Err(LeaseError::InvalidJournal)
}

#[cfg(unix)]
fn validate_lease_and_metadata(
    run_id: &str,
    expected_metadata_sha256: &str,
    lease_bytes: &[u8],
    metadata_bytes: &[u8],
) -> Result<(LeaseDocument, MetadataBinding), LeaseError> {
    let lease: LeaseDocument =
        serde_json::from_slice(lease_bytes).map_err(|_| LeaseError::InvalidLease)?;
    validate_lease(&lease)?;
    if lease.run_id != run_id {
        return Err(LeaseError::InvalidLease);
    }
    let metadata = metadata_binding_from_bytes(metadata_bytes)?;
    if metadata.sha256 != lease.metadata_sha256 {
        return Err(LeaseError::InvalidMetadata);
    }
    if metadata.sha256 != expected_metadata_sha256 {
        return Err(LeaseError::MetadataDigestMismatch);
    }
    Ok((lease, metadata))
}

#[cfg(unix)]
fn classify_partial_finalization(
    run_id: &str,
    lease: &LeaseDocument,
    metadata: &MetadataBinding,
    pending: Option<&PrivateFileSnapshot>,
    journal: Option<&PrivateFileSnapshot>,
) -> Result<PartialFinalization, LeaseError> {
    let validate = |snapshot: &PrivateFileSnapshot| {
        let parsed = match compact_finalized_from_bytes(snapshot.bytes.clone()) {
            Ok(parsed) => parsed,
            Err(LeaseError::InvalidJournal) => return Ok(None),
            Err(error) => return Err(error),
        };
        if validate_finalized_binding(&parsed.0, run_id).is_err()
            || validate_finalized_lease_binding(&parsed.0, lease).is_err()
            || parsed.0.metadata_sha256 != metadata.sha256
        {
            return Ok(None);
        }
        Ok(Some(parsed.1))
    };

    let pending = pending.map(validate).transpose()?;
    let journal = journal.map(validate).transpose()?;
    if pending.as_ref().is_some_and(Option::is_none)
        || journal.as_ref().is_some_and(Option::is_none)
    {
        return Ok(PartialFinalization::Unrecoverable);
    }
    let pending = pending.flatten();
    let journal = journal.flatten();
    if let (Some(pending), Some(journal)) = (pending.as_ref(), journal.as_ref())
        && pending != journal
    {
        return Ok(PartialFinalization::Unrecoverable);
    }
    if pending.is_some() || journal.is_some() {
        Ok(PartialFinalization::Recoverable)
    } else {
        Err(LeaseError::InvalidJournal)
    }
}

#[cfg(unix)]
fn remove_bound_run(
    handles: &OpenRunDirectory,
    run_id: &str,
    expected_names: &[&str],
    files: &[(&str, &PrivateFileSnapshot)],
) -> Result<(), LeaseError> {
    let retired = retire_bound_run(handles, run_id, expected_names, files)?;
    purge_retired_run(handles, &retired, run_id, expected_names, files)
}

#[cfg(unix)]
fn retire_bound_run(
    handles: &OpenRunDirectory,
    run_id: &str,
    expected_names: &[&str],
    files: &[(&str, &PrivateFileSnapshot)],
) -> Result<File, LeaseError> {
    handles.validate_named_run(run_id)?;
    validate_exact_run_entries(&handles.run, expected_names)?;
    for (name, snapshot) in files {
        validate_private_file_snapshot(&handles.run, name, snapshot)?;
    }

    let retired = open_or_create_private_directory_at(&handles.capture, RETIRED_DIRECTORY)?;
    match rustix::fs::statat(&retired, run_id, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => {}
        Ok(_) | Err(_) => return Err(LeaseError::UnsafeMetadata),
    }
    handles.validate_named_run(run_id)?;
    rustix::fs::renameat_with(
        &handles.runs,
        run_id,
        &retired,
        run_id,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|error| LeaseError::Io(std::io::Error::from_raw_os_error(error.raw_os_error())))?;
    // Persist the destination before persisting removal from the source. If power
    // fails between these fsyncs, evidence may still appear in both directories,
    // but it cannot disappear from both.
    retired.sync_all().map_err(LeaseError::Io)?;
    handles.runs.sync_all().map_err(LeaseError::Io)?;
    validate_named_directory_identity(&retired, run_id, &handles.run_stat)?;
    if !matches!(
        rustix::fs::statat(&handles.runs, run_id, rustix::fs::AtFlags::SYMLINK_NOFOLLOW),
        Err(rustix::io::Errno::NOENT)
    ) {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(retired)
}

#[cfg(unix)]
fn purge_retired_run(
    handles: &OpenRunDirectory,
    retired: &File,
    run_id: &str,
    expected_names: &[&str],
    files: &[(&str, &PrivateFileSnapshot)],
) -> Result<(), LeaseError> {
    validate_exact_run_entries(&handles.run, expected_names)?;
    for (name, snapshot) in files {
        validate_private_file_snapshot(&handles.run, name, snapshot)?;
    }
    for (name, snapshot) in files {
        unlink_private_file_snapshot(&handles.run, name, snapshot)?;
    }
    validate_exact_run_entries(&handles.run, &[])?;
    validate_named_directory_identity(retired, run_id, &handles.run_stat)?;
    rustix::fs::unlinkat(retired, run_id, rustix::fs::AtFlags::REMOVEDIR)
        .map_err(|_| LeaseError::UnsafeMetadata)?;
    retired.sync_all().map_err(LeaseError::Io)
}

#[cfg(unix)]
fn validate_named_directory_identity(
    parent: &File,
    name: &str,
    expected: &rustix::fs::Stat,
) -> Result<(), LeaseError> {
    let named = rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| LeaseError::UnsafeMetadata)?;
    if !same_file_identity(&named, expected)
        || !rustix::fs::FileType::from_raw_mode(named.st_mode).is_dir()
    {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_named_file_identity(
    parent: &File,
    name: &str,
    expected: &rustix::fs::Stat,
) -> Result<(), LeaseError> {
    let named = rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| LeaseError::UnsafeMetadata)?;
    if !same_file_identity(&named, expected)
        || !rustix::fs::FileType::from_raw_mode(named.st_mode).is_file()
    {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(())
}

#[cfg(unix)]
fn run_entry_names(directory: &File, maximum: usize) -> Result<Vec<String>, LeaseError> {
    let mut names = Vec::with_capacity(maximum);
    let entries = rustix::fs::Dir::read_from(directory).map_err(|_| LeaseError::UnsafeMetadata)?;
    for entry in entries {
        let entry = entry.map_err(|_| LeaseError::UnsafeMetadata)?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let name = std::str::from_utf8(bytes).map_err(|_| LeaseError::UnsafeMetadata)?;
        names.push(name.to_owned());
        if names.len() > maximum {
            return Err(LeaseError::InvalidJournal);
        }
    }
    names.sort_unstable();
    Ok(names)
}

#[cfg(unix)]
fn validate_exact_run_entries(directory: &File, expected: &[&str]) -> Result<(), LeaseError> {
    let names = run_entry_names(directory, expected.len())?;
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    if names.iter().map(String::as_str).eq(expected) {
        Ok(())
    } else {
        Err(LeaseError::InvalidJournal)
    }
}

#[cfg(unix)]
fn same_file_identity(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

#[cfg(unix)]
fn file_identity(stat: &rustix::fs::Stat) -> Result<FileIdentity, LeaseError> {
    Ok(FileIdentity {
        device: stat_field_to_u64(stat.st_dev)?,
        inode: stat_field_to_u64(stat.st_ino)?,
    })
}

#[cfg(unix)]
fn stat_field_to_u64<T>(value: T) -> Result<u64, LeaseError>
where
    T: TryInto<u64>,
{
    value.try_into().map_err(|_| LeaseError::UnsafeMetadata)
}

#[cfg(test)]
fn write_new_private(path: &Path, bytes: &[u8]) -> Result<(), LeaseError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_private_file_creation_mode(&mut options);
    let mut file = options.open(path).map_err(LeaseError::Io)?;
    set_private_file_permissions(path)?;
    validate_private_file(path)?;
    file.write_all(bytes).map_err(LeaseError::Io)?;
    file.sync_all().map_err(LeaseError::Io)?;
    Ok(())
}

fn path_metadata_exists(path: &Path) -> Result<bool, LeaseError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(LeaseError::Io(error)),
    }
}

fn validate_run_id(run_id: &str) -> Result<(), LeaseError> {
    if valid_run_id(run_id) {
        Ok(())
    } else {
        Err(LeaseError::InvalidLease)
    }
}

fn valid_run_id(run_id: &str) -> bool {
    run_id.len() == 64
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn absent_status(run_id: &str) -> LeaseStatus {
    LeaseStatus {
        run_id: run_id.to_owned(),
        state: LeaseState::Absent,
        surface: None,
        project_identity: None,
        expires_at_unix: None,
    }
}

fn status_from_optional(
    run_id: &str,
    state: LeaseState,
    lease: Option<&LeaseDocument>,
) -> LeaseStatus {
    LeaseStatus {
        run_id: run_id.to_owned(),
        state,
        surface: lease.map(|value| value.surface),
        project_identity: lease.map(|value| value.project_identity.clone()),
        expires_at_unix: lease.map(|value| value.expires_at_unix),
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn format_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(71);
    output.push_str("sha256:");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(all(test, unix))]
fn set_private_file_creation_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
}

#[cfg(all(test, not(unix)))]
fn set_private_file_creation_mode(_options: &mut OpenOptions) {}

#[cfg(all(test, unix))]
fn set_private_file_permissions(path: &Path) -> Result<(), LeaseError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(LeaseError::Io)
}

#[cfg(all(test, not(unix)))]
fn set_private_file_permissions(_path: &Path) -> Result<(), LeaseError> {
    Ok(())
}

#[cfg(unix)]
fn validate_private_dir(path: &Path) -> Result<(), LeaseError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let metadata = fs::symlink_metadata(path).map_err(LeaseError::Io)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_dir(path: &Path) -> Result<(), LeaseError> {
    let metadata = fs::symlink_metadata(path).map_err(LeaseError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_file(path: &Path) -> Result<(), LeaseError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let metadata = fs::symlink_metadata(path).map_err(LeaseError::Io)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_file_descriptor(file: &File) -> Result<rustix::fs::Stat, LeaseError> {
    use rustix::fs::{FileType, Mode};

    let stat = rustix::fs::fstat(file).map_err(|_| LeaseError::UnsafeMetadata)?;
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || Mode::from_raw_mode(stat.st_mode).as_raw_mode() & 0o777 != 0o600
    {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(stat)
}

#[cfg(unix)]
fn validate_private_directory_descriptor(file: &File) -> Result<rustix::fs::Stat, LeaseError> {
    use rustix::fs::{FileType, Mode};

    let stat = rustix::fs::fstat(file).map_err(|_| LeaseError::UnsafeMetadata)?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || Mode::from_raw_mode(stat.st_mode).as_raw_mode() & 0o777 != 0o700
    {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(stat)
}

#[cfg(not(unix))]
fn validate_private_file(path: &Path) -> Result<(), LeaseError> {
    let metadata = fs::symlink_metadata(path).map_err(LeaseError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(LeaseError::UnsafeMetadata);
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    struct IdleServerTransport;

    impl rmcp::transport::Transport<rmcp::RoleServer> for IdleServerTransport {
        type Error = std::io::Error;

        fn send(
            &mut self,
            _item: rmcp::service::TxJsonRpcMessage<rmcp::RoleServer>,
        ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
            std::future::ready(Ok(()))
        }

        async fn receive(&mut self) -> Option<rmcp::service::RxJsonRpcMessage<rmcp::RoleServer>> {
            None
        }

        fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
            std::future::ready(Ok(()))
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        data_root: PathBuf,
        project: PathBuf,
        other_project: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let data_root = temp.path().join("data");
            let project = temp.path().join("project");
            let other_project = temp.path().join("other-project");
            fs::create_dir(&data_root).unwrap();
            fs::create_dir(&project).unwrap();
            fs::create_dir(&other_project).unwrap();
            #[cfg(unix)]
            fs::set_permissions(&data_root, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                _temp: temp,
                data_root,
                project,
                other_project,
            }
        }

        fn store(&self) -> LeaseStore {
            LeaseStore::open(&self.data_root).unwrap()
        }

        fn request(&self, now: u64) -> ArmRequest {
            let document = serde_json::json!({
                "schema_version": "test/1.0",
                "surface": "cli",
                "host_controls": {"official_host": true}
            });
            ArmRequest {
                project_root: self.project.clone(),
                surface: Surface::Cli,
                metadata: MetadataBinding::from_document(document).unwrap(),
                bindings: bindings(),
                expires_at_unix: now + 60,
            }
        }
    }

    fn bindings() -> BindingDigests {
        BindingDigests {
            package_launcher_sha256: format!("sha256:{}", "11".repeat(32)),
            project_config_sha256: format!("sha256:{}", "22".repeat(32)),
            setup_receipt_sha256: format!("sha256:{}", "33".repeat(32)),
        }
    }

    fn finalize_one(fixture: &Fixture, store: &LeaseStore, now: u64) -> (String, String) {
        let armed = store.arm_at(fixture.request(now), now).unwrap();
        let claimed = store
            .claim_at(
                &ClaimContext {
                    project_root: fixture.project.clone(),
                    bindings: bindings(),
                },
                now + 1,
            )
            .unwrap()
            .unwrap();
        claimed.finalize(FinalizeOutcome::Completed).unwrap();
        let wrapper = fs::read(
            store
                .runs_directory()
                .join(&armed.run_id)
                .join(JOURNAL_FILE),
        )
        .unwrap();
        (armed.run_id, sha256_bytes(&wrapper))
    }

    fn write_pending_artifact(claimed: &ClaimedLease) -> Vec<u8> {
        let artifact = FinalizedDocument {
            schema_version: "sprint11-surface-capture-artifact/1.1".to_owned(),
            outcome: FinalizeOutcome::Completed,
            metadata_file: METADATA_FILE.to_owned(),
            metadata_sha256: claimed.lease.metadata_sha256.clone(),
            journal: claimed.recorder.snapshot(),
        };
        let bytes = serde_json::to_vec(&artifact).unwrap();
        write_new_private_at(&claimed.handles.run, JOURNAL_TEMP_FILE, &bytes).unwrap();
        bytes
    }

    #[test]
    fn no_lease_is_a_read_only_noop() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };
        assert!(!store.has_capture_state().unwrap());
        assert!(store.claim_at(&context, 10).unwrap().is_none());
        assert!(!store.capture_directory().exists());
        assert_eq!(
            store.status_at(&"01".repeat(32), 10).unwrap().state,
            LeaseState::Absent
        );
        assert!(!store.capture_directory().exists());
    }

    #[test]
    fn arm_uses_private_modes_and_cancel_removes_only_armed_run() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let armed = store.arm_at(fixture.request(10), 10).unwrap();
        assert!(store.has_capture_state().unwrap());
        let run = store.runs_directory().join(&armed.run_id);
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(store.capture_directory())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&run).unwrap().permissions().mode() & 0o777,
                0o700
            );
            for file in [METADATA_FILE, ARMED_FILE] {
                assert_eq!(
                    fs::metadata(run.join(file)).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
        assert_eq!(
            store.status_at(&armed.run_id, 10).unwrap().state,
            LeaseState::Armed
        );
        store.cancel(&armed.run_id).unwrap();
        assert_eq!(
            store.status_at(&armed.run_id, 10).unwrap().state,
            LeaseState::Absent
        );
    }

    #[test]
    fn arm_response_loss_retry_recovers_the_same_unique_run() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let first = store.arm_at(fixture.request(10), 10).unwrap();
        let recovered = store.arm_at(fixture.request(11), 11).unwrap();

        assert_eq!(recovered.run_id, first.run_id);
        assert_eq!(recovered.expires_at_unix, first.expires_at_unix);
        assert_eq!(recovered.state, LeaseState::Armed);
        assert_eq!(recovered.disposition, ArmDisposition::Recovered);
        assert_eq!(fs::read_dir(store.runs_directory()).unwrap().count(), 1);
        let claimed = store
            .claim_at(
                &ClaimContext {
                    project_root: fixture.project.clone(),
                    bindings: bindings(),
                },
                12,
            )
            .unwrap()
            .unwrap();
        assert_eq!(claimed.lease.run_id, first.run_id);
    }

    #[test]
    fn conflicting_arm_returns_existing_receipt_without_publishing_a_second_run() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut first_request = fixture.request(10);
        first_request.metadata = MetadataBinding::from_document(serde_json::json!({
            "schema_version": "test/1.0",
            "surface": "cli",
            "package": {"internal_manifest_sha256": format!("sha256:{}", "11".repeat(32))}
        }))
        .unwrap();
        let first = store.arm_at(first_request, 10).unwrap();
        let first_metadata = first.metadata_document().clone();
        let mut conflicting = fixture.request(11);
        conflicting.surface = Surface::App;
        conflicting.metadata = MetadataBinding::from_document(serde_json::json!({
            "schema_version": "test/1.0",
            "surface": "app",
            "package": {"internal_manifest_sha256": format!("sha256:{}", "22".repeat(32))}
        }))
        .unwrap();
        let conflict = store.arm_at(conflicting, 11).unwrap();

        assert_eq!(conflict.run_id, first.run_id);
        assert_eq!(conflict.state, LeaseState::Armed);
        assert_eq!(conflict.disposition, ArmDisposition::Conflict);
        assert_eq!(conflict.surface, first.surface);
        assert_eq!(conflict.metadata_document(), &first_metadata);
        assert_eq!(fs::read_dir(store.runs_directory()).unwrap().count(), 1);
    }

    #[test]
    fn armed_lease_debug_does_not_disclose_stored_metadata() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let canary = "metadata-debug-canary-7d1b38ec";
        let private_path = "/Users/operator/private/project.godot";
        let mut request = fixture.request(10);
        request.metadata = MetadataBinding::from_document(serde_json::json!({
            "schema_version": "test/1.0",
            "surface": "cli",
            "private": {
                "canary": canary,
                "path": private_path
            }
        }))
        .unwrap();
        let armed = store.arm_at(request, 10).unwrap();

        let rendered = format!("{armed:?}");
        assert!(rendered.contains(&armed.run_id));
        assert!(rendered.contains(&armed.metadata_sha256));
        assert!(!rendered.contains(canary));
        assert!(!rendered.contains(private_path));
    }

    #[test]
    fn expired_arm_retry_returns_the_run_id_for_explicit_cancel_and_rearm() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let first = store.arm_at(fixture.request(10), 10).unwrap();
        let recovered = store.arm_at(fixture.request(71), 71).unwrap();

        assert_eq!(recovered.run_id, first.run_id);
        assert_eq!(recovered.state, LeaseState::Expired);
        assert_eq!(recovered.disposition, ArmDisposition::Recovered);
        assert_eq!(fs::read_dir(store.runs_directory()).unwrap().count(), 1);

        store.cancel(&recovered.run_id).unwrap();
        let fresh = store.arm_at(fixture.request(72), 72).unwrap();
        assert_ne!(fresh.run_id, first.run_id);
        assert_eq!(fresh.state, LeaseState::Armed);
        assert_eq!(fresh.disposition, ArmDisposition::Created);
    }

    #[test]
    fn claimed_project_blocks_rearm_until_the_capture_is_finalized() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let first = store.arm_at(fixture.request(10), 10).unwrap();
        let claimed = store
            .claim_at(
                &ClaimContext {
                    project_root: fixture.project.clone(),
                    bindings: bindings(),
                },
                11,
            )
            .unwrap()
            .unwrap();

        let conflict = store.arm_at(fixture.request(12), 12).unwrap();
        assert_eq!(conflict.run_id, first.run_id);
        assert_eq!(conflict.state, LeaseState::Claimed);
        assert_eq!(conflict.disposition, ArmDisposition::Conflict);
        assert_eq!(fs::read_dir(store.runs_directory()).unwrap().count(), 1);

        claimed.finalize(FinalizeOutcome::Completed).unwrap();
        let fresh = store.arm_at(fixture.request(13), 13).unwrap();
        assert_ne!(fresh.run_id, first.run_id);
        assert_eq!(fresh.disposition, ArmDisposition::Created);
    }

    #[test]
    fn interrupted_claimed_partial_blocks_rearm_without_being_deleted() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let first = store.arm_at(fixture.request(10), 10).unwrap();
        let claimed = store
            .claim_at(
                &ClaimContext {
                    project_root: fixture.project.clone(),
                    bindings: bindings(),
                },
                11,
            )
            .unwrap()
            .unwrap();
        let pending = write_pending_artifact(&claimed);
        drop(claimed);

        let conflict = store.arm_at(fixture.request(12), 12).unwrap();
        assert_eq!(conflict.run_id, first.run_id);
        assert_eq!(conflict.state, LeaseState::Claimed);
        assert_eq!(conflict.disposition, ArmDisposition::Conflict);
        assert_eq!(
            fs::read(
                store
                    .runs_directory()
                    .join(&first.run_id)
                    .join(JOURNAL_TEMP_FILE)
            )
            .unwrap(),
            pending
        );
        assert_eq!(fs::read_dir(store.runs_directory()).unwrap().count(), 1);
    }

    #[test]
    fn cancel_rejects_a_lease_whose_embedded_run_id_changed() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let armed = store.arm_at(fixture.request(10), 10).unwrap();
        let run = store.runs_directory().join(&armed.run_id);
        let lease_path = run.join(ARMED_FILE);
        let mut lease: Value = serde_json::from_slice(&fs::read(&lease_path).unwrap()).unwrap();
        lease["run_id"] = Value::String("cd".repeat(32));
        let changed = serde_json::to_vec(&lease).unwrap();
        fs::write(&lease_path, &changed).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(matches!(
            store.status_at(&armed.run_id, 11),
            Err(LeaseError::InvalidLease)
        ));
        assert!(matches!(
            store.metadata(&armed.run_id),
            Err(LeaseError::InvalidLease)
        ));
        assert!(matches!(
            store.cancel(&armed.run_id),
            Err(LeaseError::InvalidLease)
        ));
        assert_eq!(fs::read(&lease_path).unwrap(), changed);
        assert!(run.join(METADATA_FILE).is_file());
    }

    #[test]
    fn claim_is_one_shot_and_finalize_is_atomic() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let armed = store.arm_at(fixture.request(10), 10).unwrap();
        let stored_metadata = store.metadata(&armed.run_id).unwrap().unwrap();
        assert_eq!(stored_metadata.sha256, armed.metadata_sha256);
        let metadata_bytes = fs::read(
            store
                .runs_directory()
                .join(&armed.run_id)
                .join(METADATA_FILE),
        )
        .unwrap();
        assert_eq!(metadata_bytes.last(), Some(&b'\n'));
        assert_eq!(sha256_bytes(&metadata_bytes), stored_metadata.sha256);
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };
        let claimed = store.claim_at(&context, 11).unwrap().unwrap();
        assert_eq!(claimed.run_id(), armed.run_id);
        assert_eq!(
            claimed.metadata_document()["surface"],
            Value::String("cli".to_owned())
        );
        assert!(store.claim_at(&context, 11).unwrap().is_none());
        assert_eq!(
            store.status_at(&armed.run_id, 11).unwrap().state,
            LeaseState::Claimed
        );
        let journal = claimed.finalize(FinalizeOutcome::Completed).unwrap();
        assert_eq!(journal.run_id, armed.run_id);
        assert_eq!(
            store.status_at(&armed.run_id, 11).unwrap().state,
            LeaseState::Finalized
        );
        assert!(!store.has_capture_state().unwrap());
        let run = store.runs_directory().join(&armed.run_id);
        assert!(run.join(JOURNAL_FILE).is_file());
        assert!(!run.join(JOURNAL_TEMP_FILE).exists());
        assert!(!run.join(CLAIMED_FILE).exists());
    }

    #[test]
    fn interrupted_finalize_is_never_reported_finalized_and_recovers_from_exact_authority() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };

        for (index, layout) in ["pending", "journal", "both"].into_iter().enumerate() {
            let now = 10 + u64::try_from(index).unwrap() * 100;
            let armed = store.arm_at(fixture.request(now), now).unwrap();
            let claimed = store.claim_at(&context, now + 1).unwrap().unwrap();
            let pending_bytes = write_pending_artifact(&claimed);
            let run = store.runs_directory().join(&armed.run_id);
            if layout == "journal" {
                fs::rename(run.join(JOURNAL_TEMP_FILE), run.join(JOURNAL_FILE)).unwrap();
            } else if layout == "both" {
                write_new_private(&run.join(JOURNAL_FILE), &pending_bytes).unwrap();
            }
            drop(claimed);

            assert_eq!(
                store.status_at(&armed.run_id, now + 1).unwrap().state,
                LeaseState::Claimed
            );
            assert!(store.claim_at(&context, now + 2).unwrap().is_none());
            assert_eq!(
                store.status_at(&armed.run_id, now + 2).unwrap().state,
                LeaseState::Finalized
            );
            assert_eq!(fs::read(run.join(JOURNAL_FILE)).unwrap(), pending_bytes);
            assert!(!run.join(JOURNAL_TEMP_FILE).exists());
            assert!(!run.join(CLAIMED_FILE).exists());
        }
    }

    #[test]
    fn finalized_status_rejects_extra_pending_without_claim_authority() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (run_id, _) = finalize_one(&fixture, &store, 10);
        let run = store.runs_directory().join(&run_id);
        let journal = fs::read(run.join(JOURNAL_FILE)).unwrap();
        write_new_private(&run.join(JOURNAL_TEMP_FILE), &journal).unwrap();

        assert!(matches!(
            store.status_at(&run_id, 12),
            Err(LeaseError::InvalidJournal)
        ));
        assert_eq!(fs::read(run.join(JOURNAL_FILE)).unwrap(), journal);
        assert!(run.join(METADATA_FILE).is_file());
        assert!(run.join(JOURNAL_TEMP_FILE).is_file());
    }

    #[test]
    fn consume_finalized_is_digest_bound_and_removes_only_the_exact_run() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (run_id, digest) = finalize_one(&fixture, &store, 10);
        let run = store.runs_directory().join(&run_id);

        assert!(matches!(
            store.consume_finalized(&run_id, &format!("sha256:{}", "aa".repeat(32))),
            Err(LeaseError::DigestMismatch)
        ));
        assert!(run.join(JOURNAL_FILE).is_file());
        assert!(run.join(METADATA_FILE).is_file());

        store.consume_finalized(&run_id, &digest).unwrap();
        assert!(!run.exists());
        assert_eq!(
            store.status_at(&run_id, 12).unwrap().state,
            LeaseState::Absent
        );
    }

    #[test]
    fn crash_after_atomic_retire_is_reaped_by_the_next_mutation() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (run_id, _) = finalize_one(&fixture, &store, 10);
        let handles = OpenRunDirectory::open(&store, &run_id).unwrap().unwrap();
        let journal =
            read_private_file_at(&handles.run, JOURNAL_FILE, MAX_FINAL_ARTIFACT_BYTES).unwrap();
        let metadata =
            read_private_file_at(&handles.run, METADATA_FILE, MAX_METADATA_BYTES).unwrap();
        let retired = retire_bound_run(
            &handles,
            &run_id,
            &[JOURNAL_FILE, METADATA_FILE],
            &[(JOURNAL_FILE, &journal), (METADATA_FILE, &metadata)],
        )
        .unwrap();
        unlink_private_file_snapshot(&handles.run, JOURNAL_FILE, &journal).unwrap();
        retired.sync_all().unwrap();
        let retired_run = store
            .capture_directory()
            .join(RETIRED_DIRECTORY)
            .join(&run_id);
        assert!(!store.runs_directory().join(&run_id).exists());
        assert!(retired_run.join(METADATA_FILE).is_file());
        assert!(!retired_run.join(JOURNAL_FILE).exists());
        drop((retired, handles, journal, metadata));

        let fresh = store.arm_at(fixture.request(100), 100).unwrap();
        assert!(!retired_run.exists());
        assert_eq!(
            store.status_at(&fresh.run_id, 101).unwrap().state,
            LeaseState::Armed
        );
    }

    #[test]
    fn retired_recovery_bound_counts_only_real_directory_entries() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let seed = store.arm_at(fixture.request(1), 1).unwrap();
        store.cancel(&seed.run_id).unwrap();
        let retired = store.capture_directory().join(RETIRED_DIRECTORY);
        for index in 0..MAX_RUNS {
            let tombstone = retired.join(format!("{index:064x}"));
            fs::create_dir(&tombstone).unwrap();
            fs::set_permissions(&tombstone, fs::Permissions::from_mode(0o700)).unwrap();
        }

        let fresh = store.arm_at(fixture.request(100), 100).unwrap();

        assert_eq!(
            fs::read_dir(&retired).unwrap().count(),
            0,
            "all MAX_RUNS real tombstones must fit in one bounded recovery pass"
        );
        assert_eq!(
            store.status_at(&fresh.run_id, 101).unwrap().state,
            LeaseState::Armed
        );
    }

    #[test]
    fn empty_and_metadata_only_staging_crashes_are_bounded_and_reaped_before_publish() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let seed = store.arm_at(fixture.request(1), 1).unwrap();
        store.cancel(&seed.run_id).unwrap();
        let staging = store.capture_directory().join(STAGING_DIRECTORY);
        for index in 0..MAX_RUNS {
            let staged = staging.join(format!("{:064x}", index + 1));
            fs::create_dir(&staged).unwrap();
            fs::set_permissions(&staged, fs::Permissions::from_mode(0o700)).unwrap();
            if index % 2 == 1 {
                write_new_private(&staged.join(METADATA_FILE), b"{}\n").unwrap();
            }
        }

        let fresh = store.arm_at(fixture.request(100), 100).unwrap();

        assert_eq!(
            fs::read_dir(&staging).unwrap().count(),
            0,
            "empty and metadata-only pre-publication crashes must be reaped"
        );
        assert_eq!(
            fs::read_dir(store.runs_directory()).unwrap().count(),
            1,
            "staging crashes must never consume active run capacity"
        );
        assert_eq!(
            store.status_at(&fresh.run_id, 101).unwrap().state,
            LeaseState::Armed
        );
    }

    #[test]
    fn replacing_legacy_lock_file_cannot_create_a_second_store_mutex() {
        let fixture = Fixture::new();
        let first_store = fixture.store();
        let seed = first_store.arm_at(fixture.request(1), 1).unwrap();
        let second_store = fixture.store();
        let first_lock = StoreMutationLock::acquire(&first_store).unwrap();
        let capture = first_store.capture_directory();
        let marker = capture.join(STORE_LOCK_FILE);
        let displaced = capture.join(".store.lock.displaced");
        fs::rename(&marker, &displaced).unwrap();
        write_new_private(&marker, b"").unwrap();

        assert!(matches!(
            StoreMutationLock::acquire(&second_store),
            Err(LeaseError::StoreBusy)
        ));

        drop(first_lock);
        fs::remove_file(&displaced).unwrap();
        let second_lock = StoreMutationLock::acquire(&second_store).unwrap();
        drop(second_lock);
        first_store.cancel(&seed.run_id).unwrap();
    }

    #[test]
    fn consume_rejects_capture_tree_swap_without_touching_replacement() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (run_id, digest) = finalize_one(&fixture, &store, 10);
        let capture = store.capture_directory();
        let displaced = fixture.data_root.join("validated-capture");
        let replacement_run = capture.join(RUNS_DIRECTORY).join(&run_id);
        let lock = StoreMutationLock::acquire(&store).unwrap();

        let result = store.consume_finalized_unix(&lock, &run_id, &digest, || {
            fs::rename(&capture, &displaced).unwrap();
            fs::create_dir(&capture).unwrap();
            fs::set_permissions(&capture, fs::Permissions::from_mode(0o700)).unwrap();
            write_new_private(&capture.join(STORE_LOCK_FILE), b"").unwrap();
            fs::create_dir(capture.join(RUNS_DIRECTORY)).unwrap();
            fs::set_permissions(
                capture.join(RUNS_DIRECTORY),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            fs::create_dir(&replacement_run).unwrap();
            fs::set_permissions(&replacement_run, fs::Permissions::from_mode(0o700)).unwrap();
            write_new_private(&replacement_run.join(JOURNAL_FILE), b"foreign-journal").unwrap();
            write_new_private(&replacement_run.join(METADATA_FILE), b"foreign-metadata").unwrap();
        });

        assert!(matches!(result, Err(LeaseError::UnsafeMetadata)));
        assert_eq!(
            fs::read(replacement_run.join(JOURNAL_FILE)).unwrap(),
            b"foreign-journal"
        );
        assert_eq!(
            fs::read(replacement_run.join(METADATA_FILE)).unwrap(),
            b"foreign-metadata"
        );
        assert!(
            displaced
                .join(RUNS_DIRECTORY)
                .join(&run_id)
                .join(JOURNAL_FILE)
                .is_file()
        );
    }

    #[cfg(unix)]
    #[test]
    fn consume_rejects_same_uid_run_directory_swap_before_unlinking_replacement() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (run_id, digest) = finalize_one(&fixture, &store, 10);
        let runs = store.runs_directory();
        let run = runs.join(&run_id);
        let displaced = fixture._temp.path().join("validated-run");

        let lock = StoreMutationLock::acquire(&store).unwrap();
        let result = store.consume_finalized_unix(&lock, &run_id, &digest, || {
            fs::rename(&run, &displaced).unwrap();
            fs::create_dir(&run).unwrap();
            fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
            write_new_private(&run.join(JOURNAL_FILE), b"foreign-journal").unwrap();
            write_new_private(&run.join(METADATA_FILE), b"foreign-metadata").unwrap();
        });

        assert!(matches!(result, Err(LeaseError::UnsafeMetadata)));
        assert_eq!(
            fs::read(run.join(JOURNAL_FILE)).unwrap(),
            b"foreign-journal"
        );
        assert_eq!(
            fs::read(run.join(METADATA_FILE)).unwrap(),
            b"foreign-metadata"
        );
        assert!(displaced.join(JOURNAL_FILE).is_file());
        assert!(displaced.join(METADATA_FILE).is_file());
    }

    #[cfg(unix)]
    #[test]
    fn consume_rejects_same_uid_file_swap_before_unlinking_replacement() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (run_id, digest) = finalize_one(&fixture, &store, 10);
        let run = store.runs_directory().join(&run_id);
        let displaced = fixture._temp.path().join("validated-journal");

        let lock = StoreMutationLock::acquire(&store).unwrap();
        let result = store.consume_finalized_unix(&lock, &run_id, &digest, || {
            fs::rename(run.join(JOURNAL_FILE), &displaced).unwrap();
            write_new_private(&run.join(JOURNAL_FILE), b"foreign-journal").unwrap();
        });

        assert!(matches!(result, Err(LeaseError::UnsafeMetadata)));
        assert_eq!(
            fs::read(run.join(JOURNAL_FILE)).unwrap(),
            b"foreign-journal"
        );
        assert!(run.join(METADATA_FILE).is_file());
        assert!(displaced.is_file());
    }

    #[test]
    fn abandon_rejects_same_uid_run_swap_before_removing_replacement() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };
        let armed = store.arm_at(fixture.request(10), 10).unwrap();
        let claimed = store.claim_at(&context, 11).unwrap().unwrap();
        drop(claimed);
        let run = store.runs_directory().join(&armed.run_id);
        let displaced = fixture._temp.path().join("abandon-validated-run");

        let lock = StoreMutationLock::acquire(&store).unwrap();
        let result =
            store.abandon_claimed_unix(&lock, &armed.run_id, &armed.metadata_sha256, || {
                fs::rename(&run, &displaced).unwrap();
                fs::create_dir(&run).unwrap();
                fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
                write_new_private(&run.join(CLAIMED_FILE), b"replacement-claimed").unwrap();
                write_new_private(&run.join(METADATA_FILE), b"replacement-metadata").unwrap();
            });

        assert!(matches!(result, Err(LeaseError::UnsafeMetadata)));
        assert_eq!(
            fs::read(run.join(CLAIMED_FILE)).unwrap(),
            b"replacement-claimed"
        );
        assert_eq!(
            fs::read(run.join(METADATA_FILE)).unwrap(),
            b"replacement-metadata"
        );
        assert!(displaced.join(CLAIMED_FILE).is_file());
        assert!(displaced.join(METADATA_FILE).is_file());
    }

    #[test]
    fn consume_rejects_absent_armed_and_claimed_runs_without_changes() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let digest = format!("sha256:{}", "aa".repeat(32));
        let absent = "ab".repeat(32);
        assert!(matches!(
            store.consume_finalized(&absent, &digest),
            Err(LeaseError::NotFound)
        ));

        let armed = store.arm_at(fixture.request(10), 10).unwrap();
        let armed_run = store.runs_directory().join(&armed.run_id);
        let armed_before = fs::read(armed_run.join(ARMED_FILE)).unwrap();
        assert!(matches!(
            store.consume_finalized(&armed.run_id, &digest),
            Err(LeaseError::NotFinalized)
        ));
        assert_eq!(fs::read(armed_run.join(ARMED_FILE)).unwrap(), armed_before);

        let claimed = store
            .claim_at(
                &ClaimContext {
                    project_root: fixture.project.clone(),
                    bindings: bindings(),
                },
                11,
            )
            .unwrap()
            .unwrap();
        let claimed_run = store.runs_directory().join(claimed.run_id());
        let claimed_before = fs::read(claimed_run.join(CLAIMED_FILE)).unwrap();
        assert!(matches!(
            store.consume_finalized(claimed.run_id(), &digest),
            Err(LeaseError::NotFinalized)
        ));
        assert_eq!(
            fs::read(claimed_run.join(CLAIMED_FILE)).unwrap(),
            claimed_before
        );
    }

    #[test]
    fn abandon_is_metadata_digest_bound_and_only_accepts_exact_bare_claimed_layout() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };

        let armed_only = store.arm_at(fixture.request(10), 10).unwrap();
        let armed_run = store.runs_directory().join(&armed_only.run_id);
        assert!(matches!(
            store.abandon_claimed(&armed_only.run_id, &armed_only.metadata_sha256),
            Err(LeaseError::NotFinalized)
        ));
        assert!(armed_run.join(ARMED_FILE).is_file());
        store.cancel(&armed_only.run_id).unwrap();

        let claimed = store.arm_at(fixture.request(20), 20).unwrap();
        let claimed_lease = store.claim_at(&context, 21).unwrap().unwrap();
        let claimed_run = store.runs_directory().join(&claimed.run_id);
        let claimed_before = fs::read(claimed_run.join(CLAIMED_FILE)).unwrap();
        assert!(matches!(
            store.abandon_claimed(&claimed.run_id, &format!("sha256:{}", "aa".repeat(32))),
            Err(LeaseError::MetadataDigestMismatch)
        ));
        assert_eq!(
            fs::read(claimed_run.join(CLAIMED_FILE)).unwrap(),
            claimed_before
        );
        assert!(matches!(
            store.abandon_claimed(&claimed.run_id, &claimed.metadata_sha256),
            Err(LeaseError::ClaimActive)
        ));
        assert_eq!(
            fs::read(claimed_run.join(CLAIMED_FILE)).unwrap(),
            claimed_before
        );
        drop(claimed_lease);
        store
            .abandon_claimed(&claimed.run_id, &claimed.metadata_sha256)
            .unwrap();
        assert!(!claimed_run.exists());

        let finalized = store.arm_at(fixture.request(30), 30).unwrap();
        let finalized_lease = store.claim_at(&context, 31).unwrap().unwrap();
        finalized_lease
            .finalize(FinalizeOutcome::Completed)
            .unwrap();
        let finalized_run = store.runs_directory().join(&finalized.run_id);
        assert!(matches!(
            store.abandon_claimed(&finalized.run_id, &finalized.metadata_sha256),
            Err(LeaseError::ConsumeRequired)
        ));
        assert!(finalized_run.join(JOURNAL_FILE).is_file());
        assert!(finalized_run.join(METADATA_FILE).is_file());

        let partial = store.arm_at(fixture.request(40), 40).unwrap();
        let partial_lease = store.claim_at(&context, 41).unwrap().unwrap();
        write_pending_artifact(&partial_lease);
        let partial_run = store.runs_directory().join(&partial.run_id);
        drop(partial_lease);
        assert!(matches!(
            store.abandon_claimed(&partial.run_id, &partial.metadata_sha256),
            Err(LeaseError::RecoveryRequired)
        ));
        assert!(partial_run.join(CLAIMED_FILE).is_file());
        assert!(partial_run.join(METADATA_FILE).is_file());
        assert!(partial_run.join(JOURNAL_TEMP_FILE).is_file());
    }

    #[test]
    fn live_claim_guard_blocks_abandon_and_interrupted_recovery_until_drop() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };
        let armed = store.arm_at(fixture.request(10), 10).unwrap();
        let claimed = store.claim_at(&context, 11).unwrap().unwrap();
        let pending = write_pending_artifact(&claimed);
        let run = store.runs_directory().join(&armed.run_id);

        assert!(matches!(
            store.abandon_claimed(&armed.run_id, &armed.metadata_sha256),
            Err(LeaseError::ClaimActive)
        ));
        assert!(matches!(
            store.claim_at(&context, 12),
            Err(LeaseError::ClaimActive)
        ));
        assert_eq!(
            store.status_at(&armed.run_id, 12).unwrap().state,
            LeaseState::Claimed
        );
        assert_eq!(fs::read(run.join(JOURNAL_TEMP_FILE)).unwrap(), pending);
        assert!(run.join(CLAIMED_FILE).is_file());

        drop(claimed);
        assert!(matches!(
            store.abandon_claimed(&armed.run_id, &armed.metadata_sha256),
            Err(LeaseError::RecoveryRequired)
        ));
        assert!(store.claim_at(&context, 13).unwrap().is_none());
        assert_eq!(
            store.status_at(&armed.run_id, 13).unwrap().state,
            LeaseState::Finalized
        );
        assert_eq!(fs::read(run.join(JOURNAL_FILE)).unwrap(), pending);
        assert!(!run.join(CLAIMED_FILE).exists());
    }

    #[test]
    fn tapped_transport_retains_live_claim_guard_after_lease_owner_drops() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };
        let armed = store.arm_at(fixture.request(10), 10).unwrap();
        let claimed = store.claim_at(&context, 11).unwrap().unwrap();
        let transport = claimed.wrap_transport::<rmcp::RoleServer, _>(IdleServerTransport);

        drop(claimed);
        assert!(matches!(
            store.abandon_claimed(&armed.run_id, &armed.metadata_sha256),
            Err(LeaseError::ClaimActive)
        ));

        drop(transport);
        store
            .abandon_claimed(&armed.run_id, &armed.metadata_sha256)
            .unwrap();
        assert!(!store.runs_directory().join(&armed.run_id).exists());
    }

    #[test]
    fn live_guard_blocks_consume_during_the_final_exact_layout_window() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };
        let armed = store.arm_at(fixture.request(10), 10).unwrap();
        let claimed = store.claim_at(&context, 11).unwrap().unwrap();
        let journal = write_pending_artifact(&claimed);
        rustix::fs::renameat_with(
            &claimed.handles.run,
            JOURNAL_TEMP_FILE,
            &claimed.handles.run,
            JOURNAL_FILE,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .unwrap();
        unlink_private_file_snapshot(
            &claimed.handles.run,
            CLAIMED_FILE,
            &claimed.claimed_snapshot,
        )
        .unwrap();
        claimed.handles.run.sync_all().unwrap();
        let run = store.runs_directory().join(&armed.run_id);
        let digest = sha256_bytes(&journal);

        assert!(matches!(
            store.consume_finalized(&armed.run_id, &digest),
            Err(LeaseError::ClaimActive)
        ));
        assert!(run.join(JOURNAL_FILE).is_file());
        assert!(run.join(METADATA_FILE).is_file());

        drop(claimed);
        store.consume_finalized(&armed.run_id, &digest).unwrap();
        assert!(!run.exists());
    }

    #[test]
    fn explicit_abandon_discards_only_inactive_digest_bound_corrupt_partial() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };
        for (index, (pending, journal)) in [(true, false), (false, true), (true, true)]
            .into_iter()
            .enumerate()
        {
            let now = 10 + u64::try_from(index).unwrap() * 10;
            let armed = store.arm_at(fixture.request(now), now).unwrap();
            let claimed = store.claim_at(&context, now + 1).unwrap().unwrap();
            if pending {
                write_new_private_at(&claimed.handles.run, JOURNAL_TEMP_FILE, b"{").unwrap();
            }
            if journal {
                write_new_private_at(&claimed.handles.run, JOURNAL_FILE, b"{").unwrap();
            }
            let run = store.runs_directory().join(&armed.run_id);
            drop(claimed);

            assert!(store.claim_at(&context, now + 2).unwrap().is_none());
            if journal {
                assert!(matches!(
                    store.status_at(&armed.run_id, now + 2),
                    Err(LeaseError::InvalidJournal)
                ));
            } else {
                assert_eq!(
                    store.status_at(&armed.run_id, now + 2).unwrap().state,
                    LeaseState::Claimed
                );
            }
            assert!(matches!(
                store.abandon_claimed(&armed.run_id, &format!("sha256:{}", "aa".repeat(32))),
                Err(LeaseError::MetadataDigestMismatch)
            ));
            if pending {
                assert_eq!(fs::read(run.join(JOURNAL_TEMP_FILE)).unwrap(), b"{");
            }
            if journal {
                assert_eq!(fs::read(run.join(JOURNAL_FILE)).unwrap(), b"{");
            }
            assert!(run.join(CLAIMED_FILE).is_file());
            assert!(run.join(METADATA_FILE).is_file());

            store
                .abandon_claimed(&armed.run_id, &armed.metadata_sha256)
                .unwrap();
            assert!(!run.exists());
        }
    }

    #[test]
    fn explicit_abandon_discards_wrong_bound_or_conflicting_partial_artifacts() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };

        let wrong_bound = store.arm_at(fixture.request(10), 10).unwrap();
        let claimed = store.claim_at(&context, 11).unwrap().unwrap();
        let bytes = write_pending_artifact(&claimed);
        let mut artifact: FinalizedDocument = serde_json::from_slice(&bytes).unwrap();
        artifact.journal.run_id = "ef".repeat(32);
        fs::write(
            store
                .runs_directory()
                .join(&wrong_bound.run_id)
                .join(JOURNAL_TEMP_FILE),
            serde_json::to_vec(&artifact).unwrap(),
        )
        .unwrap();
        drop(claimed);
        store
            .abandon_claimed(&wrong_bound.run_id, &wrong_bound.metadata_sha256)
            .unwrap();
        assert!(!store.runs_directory().join(&wrong_bound.run_id).exists());

        let conflicting = store.arm_at(fixture.request(20), 20).unwrap();
        let claimed = store.claim_at(&context, 21).unwrap().unwrap();
        let pending = write_pending_artifact(&claimed);
        let mut artifact: FinalizedDocument = serde_json::from_slice(&pending).unwrap();
        artifact.outcome = FinalizeOutcome::Failed;
        write_new_private_at(
            &claimed.handles.run,
            JOURNAL_FILE,
            &serde_json::to_vec(&artifact).unwrap(),
        )
        .unwrap();
        drop(claimed);
        store
            .abandon_claimed(&conflicting.run_id, &conflicting.metadata_sha256)
            .unwrap();
        assert!(!store.runs_directory().join(&conflicting.run_id).exists());
    }

    #[test]
    fn corrupt_partial_with_unknown_extra_is_never_abandoned() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };
        let armed = store.arm_at(fixture.request(10), 10).unwrap();
        let claimed = store.claim_at(&context, 11).unwrap().unwrap();
        write_new_private_at(&claimed.handles.run, JOURNAL_TEMP_FILE, b"{").unwrap();
        write_new_private_at(&claimed.handles.run, "unknown", b"unknown").unwrap();
        let run = store.runs_directory().join(&armed.run_id);
        drop(claimed);

        assert!(matches!(
            store.abandon_claimed(&armed.run_id, &armed.metadata_sha256),
            Err(LeaseError::InvalidJournal)
        ));
        for name in [CLAIMED_FILE, METADATA_FILE, JOURNAL_TEMP_FILE, "unknown"] {
            assert!(run.join(name).is_file());
        }
    }

    #[test]
    fn finalize_rejects_named_run_swap_without_reopening_the_replacement() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };
        let armed = store.arm_at(fixture.request(10), 10).unwrap();
        let claimed = store.claim_at(&context, 11).unwrap().unwrap();
        let run = store.runs_directory().join(&armed.run_id);
        let displaced = fixture._temp.path().join("live-claimed-run");
        fs::rename(&run, &displaced).unwrap();
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        write_new_private(&run.join(CLAIMED_FILE), b"replacement-claimed").unwrap();
        write_new_private(&run.join(METADATA_FILE), b"replacement-metadata").unwrap();

        assert!(matches!(
            claimed.finalize(FinalizeOutcome::Completed),
            Err(LeaseError::UnsafeMetadata)
        ));
        assert_eq!(
            fs::read(run.join(CLAIMED_FILE)).unwrap(),
            b"replacement-claimed"
        );
        assert_eq!(
            fs::read(run.join(METADATA_FILE)).unwrap(),
            b"replacement-metadata"
        );
        assert!(displaced.join(CLAIMED_FILE).is_file());
        assert!(!displaced.join(JOURNAL_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn abandon_rejects_nonprivate_or_symlinked_claimed_files_without_changes() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };

        let nonprivate = store.arm_at(fixture.request(10), 10).unwrap();
        let claimed = store.claim_at(&context, 11).unwrap().unwrap();
        drop(claimed);
        let nonprivate_run = store.runs_directory().join(&nonprivate.run_id);
        let metadata = nonprivate_run.join(METADATA_FILE);
        fs::set_permissions(&metadata, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            store.abandon_claimed(&nonprivate.run_id, &nonprivate.metadata_sha256),
            Err(LeaseError::UnsafeMetadata)
        ));
        assert!(nonprivate_run.join(CLAIMED_FILE).is_file());
        assert!(metadata.is_file());

        let fixture = Fixture::new();
        let store = fixture.store();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };
        let linked = store.arm_at(fixture.request(20), 20).unwrap();
        let claimed = store.claim_at(&context, 21).unwrap().unwrap();
        drop(claimed);
        let linked_run = store.runs_directory().join(&linked.run_id);
        let claimed_path = linked_run.join(CLAIMED_FILE);
        let target = fixture._temp.path().join("claimed-target");
        fs::rename(&claimed_path, &target).unwrap();
        symlink(&target, &claimed_path).unwrap();
        assert!(matches!(
            store.abandon_claimed(&linked.run_id, &linked.metadata_sha256),
            Err(LeaseError::UnsafeMetadata)
        ));
        assert!(
            claimed_path
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(linked_run.join(METADATA_FILE).is_file());
        assert!(target.is_file());
    }

    #[test]
    fn malformed_or_nonexclusive_finalized_layout_is_never_consumed() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (malformed, digest) = finalize_one(&fixture, &store, 10);
        let malformed_run = store.runs_directory().join(&malformed);
        let journal_path = malformed_run.join(JOURNAL_FILE);
        let metadata_path = malformed_run.join(METADATA_FILE);
        let metadata_before = fs::read(&metadata_path).unwrap();
        fs::write(&journal_path, b"{}").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            store.consume_finalized(&malformed, &digest),
            Err(LeaseError::InvalidJournal)
        ));
        assert_eq!(fs::read(&journal_path).unwrap(), b"{}");
        assert_eq!(fs::read(&metadata_path).unwrap(), metadata_before);

        let (noncompact, _) = finalize_one(&fixture, &store, 20);
        let noncompact_run = store.runs_directory().join(&noncompact);
        let noncompact_path = noncompact_run.join(JOURNAL_FILE);
        let document: Value = serde_json::from_slice(&fs::read(&noncompact_path).unwrap()).unwrap();
        let noncompact_bytes = serde_json::to_vec_pretty(&document).unwrap();
        fs::write(&noncompact_path, &noncompact_bytes).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&noncompact_path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            store.consume_finalized(&noncompact, &sha256_bytes(&noncompact_bytes)),
            Err(LeaseError::InvalidJournal)
        ));
        assert_eq!(fs::read(&noncompact_path).unwrap(), noncompact_bytes);
        assert!(noncompact_run.join(METADATA_FILE).is_file());

        let (extra, extra_digest) = finalize_one(&fixture, &store, 30);
        let extra_run = store.runs_directory().join(&extra);
        write_new_private(&extra_run.join("unexpected"), b"unexpected").unwrap();
        let journal_before = fs::read(extra_run.join(JOURNAL_FILE)).unwrap();
        assert!(matches!(
            store.consume_finalized(&extra, &extra_digest),
            Err(LeaseError::InvalidJournal)
        ));
        assert_eq!(
            fs::read(extra_run.join(JOURNAL_FILE)).unwrap(),
            journal_before
        );
        assert!(extra_run.join(METADATA_FILE).is_file());
        assert!(extra_run.join("unexpected").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn consume_rejects_nonprivate_or_symlinked_finalized_files_without_changes() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let (nonprivate, digest) = finalize_one(&fixture, &store, 10);
        let nonprivate_run = store.runs_directory().join(&nonprivate);
        let metadata_path = nonprivate_run.join(METADATA_FILE);
        fs::set_permissions(&metadata_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            store.consume_finalized(&nonprivate, &digest),
            Err(LeaseError::UnsafeMetadata)
        ));
        assert!(nonprivate_run.join(JOURNAL_FILE).is_file());
        assert!(metadata_path.is_file());

        let (linked, linked_digest) = finalize_one(&fixture, &store, 20);
        let linked_run = store.runs_directory().join(&linked);
        let journal = linked_run.join(JOURNAL_FILE);
        let target = fixture._temp.path().join("journal-target");
        fs::rename(&journal, &target).unwrap();
        symlink(&target, &journal).unwrap();
        assert!(matches!(
            store.consume_finalized(&linked, &linked_digest),
            Err(LeaseError::UnsafeMetadata)
        ));
        assert!(journal.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(linked_run.join(METADATA_FILE).is_file());
        assert!(target.is_file());
    }

    #[test]
    fn consuming_full_finalized_capacity_restores_bounded_scan_and_arm() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut finalized = Vec::with_capacity(MAX_RUNS);
        for index in 0..MAX_RUNS {
            let now = 10 + u64::try_from(index).unwrap() * 2;
            finalized.push(finalize_one(&fixture, &store, now));
        }
        assert!(!store.has_capture_state().unwrap());
        let before = fs::read_dir(store.runs_directory())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(matches!(
            store.arm_at(fixture.request(500), 500),
            Err(LeaseError::StoreBoundExceeded)
        ));
        let after = fs::read_dir(store.runs_directory())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(after, before);

        for (run_id, digest) in finalized {
            store.consume_finalized(&run_id, &digest).unwrap();
        }
        assert_eq!(fs::read_dir(store.runs_directory()).unwrap().count(), 0);

        let fresh = store.arm_at(fixture.request(1_000), 1_000).unwrap();
        assert!(store.has_capture_state().unwrap());
        assert_eq!(
            store.status_at(&fresh.run_id, 1_001).unwrap().state,
            LeaseState::Armed
        );
    }

    #[test]
    fn full_store_corrupt_partial_requires_explicit_digest_bound_abandon_to_free_capacity() {
        let fixture = Fixture::new();
        let store = fixture.store();
        for index in 0..(MAX_RUNS - 1) {
            let now = 10 + u64::try_from(index).unwrap() * 2;
            let _ = finalize_one(&fixture, &store, now);
        }
        let now = 1_000;
        let orphan = store.arm_at(fixture.request(now), now).unwrap();
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };
        let claimed = store.claim_at(&context, now + 1).unwrap().unwrap();
        let claimed_path = store
            .runs_directory()
            .join(&orphan.run_id)
            .join(CLAIMED_FILE);
        let claimed_before = fs::read(&claimed_path).unwrap();
        write_new_private_at(&claimed.handles.run, JOURNAL_TEMP_FILE, b"{").unwrap();
        drop(claimed);

        let conflict = store.arm_at(fixture.request(2_000), 2_000).unwrap();
        assert_eq!(conflict.run_id, orphan.run_id);
        assert_eq!(conflict.state, LeaseState::Claimed);
        assert_eq!(conflict.disposition, ArmDisposition::Conflict);
        assert_eq!(fs::read(&claimed_path).unwrap(), claimed_before);
        assert_eq!(
            store.status_at(&orphan.run_id, now + 2).unwrap().state,
            LeaseState::Claimed
        );
        assert!(store.claim_at(&context, now + 2).unwrap().is_none());

        assert!(matches!(
            store.abandon_claimed(&orphan.run_id, &format!("sha256:{}", "aa".repeat(32))),
            Err(LeaseError::MetadataDigestMismatch)
        ));
        assert_eq!(fs::read(&claimed_path).unwrap(), claimed_before);
        store
            .abandon_claimed(&orphan.run_id, &orphan.metadata_sha256)
            .unwrap();
        assert!(!claimed_path.exists());
        let fresh = store.arm_at(fixture.request(2_000), 2_000).unwrap();
        assert_eq!(
            store.status_at(&fresh.run_id, 2_001).unwrap().state,
            LeaseState::Armed
        );
        assert!(store.has_capture_state().unwrap());
    }

    #[test]
    fn wrong_root_does_not_consume_a_lease() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let armed = store.arm_at(fixture.request(10), 10).unwrap();
        let wrong = ClaimContext {
            project_root: fixture.other_project.clone(),
            bindings: bindings(),
        };
        assert!(store.claim_at(&wrong, 11).unwrap().is_none());
        assert_eq!(
            store.status_at(&armed.run_id, 11).unwrap().state,
            LeaseState::Armed
        );
    }

    #[test]
    fn expiry_and_binding_mismatch_fail_closed() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let armed = store.arm_at(fixture.request(100), 100).unwrap();
        assert_eq!(
            store.status_at(&armed.run_id, 161).unwrap().state,
            LeaseState::Expired
        );
        let context = ClaimContext {
            project_root: fixture.project.clone(),
            bindings: bindings(),
        };
        assert!(matches!(
            store.claim_at(&context, 161),
            Err(LeaseError::Expired)
        ));

        store.cancel(&armed.run_id).unwrap();
        let fresh = store.arm_at(fixture.request(200), 200).unwrap();
        let mut wrong_bindings = bindings();
        wrong_bindings.project_config_sha256 = format!("sha256:{}", "44".repeat(32));
        assert!(matches!(
            store.claim_at(
                &ClaimContext {
                    project_root: fixture.project.clone(),
                    bindings: wrong_bindings
                },
                201
            ),
            Err(LeaseError::BindingMismatch)
        ));
        assert_eq!(
            store.status_at(&fresh.run_id, 201).unwrap().state,
            LeaseState::Armed
        );
    }

    #[test]
    fn malformed_and_symlinked_lease_fail_closed() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let malformed = store.arm_at(fixture.request(10), 10).unwrap();
        let malformed_path = store
            .runs_directory()
            .join(&malformed.run_id)
            .join(ARMED_FILE);
        fs::write(&malformed_path, b"{}").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&malformed_path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            store.status_at(&malformed.run_id, 11),
            Err(LeaseError::InvalidLease)
        ));

        let fixture = Fixture::new();
        let store = fixture.store();
        let linked = store.arm_at(fixture.request(20), 20).unwrap();
        let run = store.runs_directory().join(&linked.run_id);
        let armed_path = run.join(ARMED_FILE);
        let target = run.join("target");
        fs::rename(&armed_path, &target).unwrap();
        #[cfg(unix)]
        symlink(&target, &armed_path).unwrap();
        #[cfg(unix)]
        assert!(matches!(
            store.status_at(&linked.run_id, 21),
            Err(LeaseError::UnsafeMetadata)
        ));
    }

    #[test]
    fn metadata_digest_and_expiry_are_validated_before_store_creation() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut wrong_metadata = fixture.request(10);
        wrong_metadata.metadata.sha256 = format!("sha256:{}", "aa".repeat(32));
        assert!(matches!(
            store.arm_at(wrong_metadata, 10),
            Err(LeaseError::InvalidMetadata)
        ));
        let mut too_long = fixture.request(10);
        too_long.expires_at_unix = 10 + MAX_LEASE_LIFETIME_SECONDS + 1;
        assert!(matches!(
            store.arm_at(too_long, 10),
            Err(LeaseError::InvalidExpiry)
        ));
        assert!(!store.capture_directory().exists());
    }

    #[cfg(unix)]
    #[test]
    fn data_root_and_run_symlinks_are_rejected() {
        let fixture = Fixture::new();
        let alias = fixture._temp.path().join("data-alias");
        symlink(&fixture.data_root, &alias).unwrap();
        assert!(matches!(
            LeaseStore::open(alias),
            Err(LeaseError::UnsafeMetadata)
        ));

        let store = fixture.store();
        let armed = store.arm_at(fixture.request(10), 10).unwrap();
        let run = store.runs_directory().join(&armed.run_id);
        let moved = store.runs_directory().join("moved");
        fs::rename(&run, &moved).unwrap();
        symlink(&moved, &run).unwrap();
        assert!(matches!(
            store.status_at(&armed.run_id, 11),
            Err(LeaseError::UnsafeMetadata)
        ));
    }
}
