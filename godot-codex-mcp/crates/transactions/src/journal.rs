use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use godot_codex_bridge_client::{
    AffectedEntity, AffectedEntityRole, ApprovalScope, OperationKind, Risk, SafeTransactionError,
    TransactionState,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const JOURNAL_SCHEMA: &str = "transaction-journal/1.0";
pub const MAX_JOURNAL_RECORDS: usize = 1_024;
pub const MAX_JOURNAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionJournalRecord {
    pub transaction_id: String,
    pub idempotency_fingerprint: String,
    pub operation_digest: String,
    pub editor_session_id: String,
    pub scene_id: String,
    pub history_id: String,
    pub state: TransactionState,
    pub transaction_seq: u64,
    pub scene_revision: u64,
    pub operation_seq: u64,
    pub operation_kind: OperationKind,
    pub risk: Risk,
    pub scope: ApprovalScope,
    pub affected_entities: Vec<AffectedEntity>,
    pub preview_digest: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub expires_at_ms: u64,
    pub apply_dispatched: bool,
    pub undo_eligible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe_error: Option<SafeTransactionError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_hash: Option<String>,
}

impl TransactionJournalRecord {
    fn eviction_class(&self) -> Option<u8> {
        match self.state {
            TransactionState::Expired
            | TransactionState::Rejected
            | TransactionState::Failed
            | TransactionState::FailedRolledBack
            | TransactionState::Conflicted => Some(0),
            TransactionState::Undone => Some(1),
            TransactionState::Committed if !self.undo_eligible => Some(2),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalDocument {
    schema_version: String,
    project_id: String,
    journal_revision: u64,
    records: Vec<TransactionJournalRecord>,
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("transaction journal I/O failed")]
    Io(#[source] std::io::Error),
    #[error("transaction journal JSON is invalid")]
    Json(#[source] serde_json::Error),
    #[error("transaction journal project binding is invalid")]
    ProjectMismatch,
    #[error("transaction journal schema is incompatible")]
    IncompatibleSchema,
    #[error("transaction journal protected records exceed the retention limit")]
    ProtectedRecordsExceedLimit,
    #[error("transaction journal private path metadata is invalid")]
    UnsafeMetadata,
    #[error("another transaction coordinator owns this project journal")]
    Locked,
}

impl From<std::io::Error> for JournalError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for JournalError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug)]
pub struct JournalStore {
    project_id: String,
    journal_path: PathBuf,
    transactions_dir: PathBuf,
    codex_dir: PathBuf,
    _lock: File,
    recovered_corruption: bool,
}

impl JournalStore {
    pub fn open(
        project_root: &Path,
        project_id: &str,
    ) -> Result<(Self, JournalState), JournalError> {
        let canonical_root = fs::canonicalize(project_root)?;
        if !canonical_root.join("project.godot").is_file() {
            return Err(JournalError::UnsafeMetadata);
        }
        let godot_dir = canonical_root.join(".godot");
        create_plain_dir(&godot_dir)?;
        let codex_dir = godot_dir.join("codex");
        create_private_dir(&codex_dir)?;
        let transactions_dir = codex_dir.join("transactions");
        create_private_dir(&transactions_dir)?;
        let lock_path = codex_dir.join("transactions.lock");
        let lock = open_private_file(&lock_path)?;
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => JournalError::Locked,
            std::fs::TryLockError::Error(error) => JournalError::Io(error),
        })?;
        cleanup_stale_temps(&transactions_dir)?;
        let journal_path = transactions_dir.join("journal-v1.json");
        let mut store = Self {
            project_id: project_id.to_owned(),
            journal_path,
            transactions_dir,
            codex_dir,
            _lock: lock,
            recovered_corruption: false,
        };
        let state = store.load_or_recover()?;
        Ok((store, state))
    }

    #[must_use]
    pub fn recovered_corruption(&self) -> bool {
        self.recovered_corruption
    }

    fn load_or_recover(&mut self) -> Result<JournalState, JournalError> {
        if !self.journal_path.exists() {
            return Ok(JournalState::default());
        }
        validate_private_file(&self.journal_path)?;
        let bytes = fs::read(&self.journal_path)?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            self.quarantine_corrupt()?;
            self.recovered_corruption = true;
            return Ok(JournalState::default());
        }
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                self.quarantine_corrupt()?;
                self.recovered_corruption = true;
                return Ok(JournalState::default());
            }
        };
        let Some(schema) = value
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
        else {
            self.quarantine_corrupt()?;
            self.recovered_corruption = true;
            return Ok(JournalState::default());
        };
        let migrate_compatible_minor = schema != JOURNAL_SCHEMA;
        if !compatible_journal_schema(schema) {
            return Err(JournalError::IncompatibleSchema);
        }
        let mut value = value;
        if migrate_compatible_minor {
            value["schema_version"] = serde_json::Value::String(JOURNAL_SCHEMA.to_owned());
        }
        let document: JournalDocument = match serde_json::from_value(value) {
            Ok(document) => document,
            Err(_) => {
                self.quarantine_corrupt()?;
                self.recovered_corruption = true;
                return Ok(JournalState::default());
            }
        };
        if document.project_id != self.project_id {
            return Err(JournalError::ProjectMismatch);
        }
        let mut seen = BTreeSet::new();
        if document.journal_revision > MAX_SAFE_INTEGER
            || document
                .records
                .iter()
                .any(|record| !seen.insert(record.transaction_id.clone()) || !valid_record(record))
        {
            self.quarantine_corrupt()?;
            self.recovered_corruption = true;
            return Ok(JournalState::default());
        }
        let mut state = JournalState {
            revision: document.journal_revision,
            records: document
                .records
                .into_iter()
                .map(|record| (record.transaction_id.clone(), record))
                .collect(),
        };
        state.enforce_bounds_for_project(&self.project_id)?;
        if migrate_compatible_minor {
            self.persist(&mut state)?;
        }
        Ok(state)
    }

    pub fn persist(&self, state: &mut JournalState) -> Result<(), JournalError> {
        if state.records.values().any(|record| !valid_record(record)) {
            return Err(JournalError::UnsafeMetadata);
        }
        state.enforce_bounds_for_project(&self.project_id)?;
        let next_revision = state.revision.saturating_add(1);
        let document = JournalDocument {
            schema_version: JOURNAL_SCHEMA.to_owned(),
            project_id: self.project_id.clone(),
            journal_revision: next_revision,
            records: state.records.values().cloned().collect(),
        };
        let payload = serde_json::to_vec(&document)?;
        if payload.len() > MAX_JOURNAL_BYTES {
            return Err(JournalError::ProtectedRecordsExceedLimit);
        }
        let temp_path = self.transactions_dir.join(format!(
            ".journal-v1.{next_revision}.{}.tmp",
            unique_nonce()
        ));
        let mut temp = create_new_private_file(&temp_path)?;
        temp.write_all(&payload)?;
        temp.sync_all()?;
        fs::rename(&temp_path, &self.journal_path)?;
        sync_directory(&self.transactions_dir)?;
        validate_private_file(&self.journal_path)?;
        state.revision = next_revision;
        Ok(())
    }

    fn quarantine_corrupt(&self) -> Result<(), JournalError> {
        let quarantine_dir = self.codex_dir.join("quarantine");
        create_private_dir(&quarantine_dir)?;
        let target = quarantine_dir.join(format!("transactions-{:016x}.json", unique_nonce()));
        fs::rename(&self.journal_path, target)?;
        sync_directory(&quarantine_dir)?;
        sync_directory(&self.transactions_dir)?;
        Ok(())
    }
}

fn compatible_journal_schema(value: &str) -> bool {
    let Some(version) = value.strip_prefix("transaction-journal/") else {
        return false;
    };
    let Some((major, minor)) = version.split_once('.') else {
        return false;
    };
    major == "1" && !minor.is_empty() && minor.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_record(record: &TransactionJournalRecord) -> bool {
    valid_identifier(&record.transaction_id, "transaction:", 32)
        && valid_digest(&record.idempotency_fingerprint)
        && valid_digest(&record.operation_digest)
        && valid_identifier(&record.editor_session_id, "editor:", 32)
        && valid_identifier(&record.scene_id, "scene:", 32)
        && valid_identifier(&record.history_id, "history:", 32)
        && valid_digest(&record.preview_digest)
        && valid_risk(record.operation_kind, record.risk)
        && record.scope == expected_scope(record.operation_kind)
        && record.transaction_seq > 0
        && record.transaction_seq <= MAX_SAFE_INTEGER
        && record.scene_revision <= MAX_SAFE_INTEGER
        && record.operation_seq <= MAX_SAFE_INTEGER
        && record.created_at_ms <= MAX_SAFE_INTEGER
        && record.updated_at_ms <= MAX_SAFE_INTEGER
        && record.expires_at_ms <= MAX_SAFE_INTEGER
        && record.expires_at_ms >= record.created_at_ms
        && record.affected_entities.len() <= 16
        && record.affected_entities.iter().all(|entity| {
            valid_identifier(&entity.node_id, "node:", 32)
                && allowed_role(record.operation_kind, entity.role)
        })
        && record.safe_error.as_ref().is_none_or(valid_safe_error)
        && record
            .receipt_hash
            .as_ref()
            .is_none_or(|hash| valid_digest(hash))
}

fn valid_risk(kind: OperationKind, risk: Risk) -> bool {
    match kind {
        OperationKind::DeleteNode
        | OperationKind::ReparentNode
        | OperationKind::SetProperty
        | OperationKind::DetachScript
        | OperationKind::DisconnectSignal => risk == Risk::Destructive,
        OperationKind::AttachScript => matches!(risk, Risk::Write | Risk::Destructive),
        OperationKind::CreateNode | OperationKind::ConnectSignal => risk == Risk::Write,
    }
}

fn expected_scope(kind: OperationKind) -> ApprovalScope {
    match kind {
        OperationKind::CreateNode => ApprovalScope::SceneNodeCreate,
        OperationKind::DeleteNode => ApprovalScope::SceneNodeDelete,
        OperationKind::ReparentNode => ApprovalScope::SceneNodeReparent,
        OperationKind::SetProperty => ApprovalScope::ScenePropertySet,
        OperationKind::AttachScript => ApprovalScope::SceneScriptAttach,
        OperationKind::DetachScript => ApprovalScope::SceneScriptDetach,
        OperationKind::ConnectSignal => ApprovalScope::SceneSignalConnect,
        OperationKind::DisconnectSignal => ApprovalScope::SceneSignalDisconnect,
    }
}

fn valid_identifier(value: &str, prefix: &str, hexadecimal_length: usize) -> bool {
    value.strip_prefix(prefix).is_some_and(|tail| {
        tail.len() == hexadecimal_length
            && tail
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_digest(value: &str) -> bool {
    valid_identifier(value, "sha256:", 64)
}

fn allowed_role(kind: OperationKind, role: AffectedEntityRole) -> bool {
    match kind {
        OperationKind::CreateNode => {
            matches!(
                role,
                AffectedEntityRole::Parent | AffectedEntityRole::Created
            )
        }
        OperationKind::DeleteNode
        | OperationKind::SetProperty
        | OperationKind::AttachScript
        | OperationKind::DetachScript => role == AffectedEntityRole::Target,
        OperationKind::ReparentNode => {
            matches!(
                role,
                AffectedEntityRole::Target | AffectedEntityRole::NewParent
            )
        }
        OperationKind::ConnectSignal | OperationKind::DisconnectSignal => {
            matches!(
                role,
                AffectedEntityRole::Emitter | AffectedEntityRole::Receiver
            )
        }
    }
}

fn valid_safe_error(error: &SafeTransactionError) -> bool {
    let valid_code = (1..=64).contains(&error.code.len())
        && error
            .code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    let message = error.message.as_str();
    let bytes = message.as_bytes();
    valid_code
        && !message.is_empty()
        && message.len() <= 256
        && !message.chars().any(char::is_control)
        && !message.contains("/Users/")
        && !message.contains("/home/")
        && !message.contains('\\')
        && !message.starts_with('/')
        && !(bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'/')
}

#[derive(Clone, Debug, Default)]
pub struct JournalState {
    pub revision: u64,
    pub records: BTreeMap<String, TransactionJournalRecord>,
}

impl JournalState {
    #[cfg(test)]
    fn enforce_bounds(&mut self) -> Result<(), JournalError> {
        self.enforce_bounds_for_project("")
    }

    fn enforce_bounds_for_project(&mut self, project_id: &str) -> Result<(), JournalError> {
        while self.records.len() > MAX_JOURNAL_RECORDS {
            let transaction_id = self.eviction_candidate()?;
            self.records.remove(&transaction_id);
        }

        let document = JournalDocument {
            schema_version: JOURNAL_SCHEMA.to_owned(),
            project_id: project_id.to_owned(),
            journal_revision: self.revision.saturating_add(1),
            records: self.records.values().cloned().collect(),
        };
        let mut bytes = serde_json::to_vec(&document)?.len();
        while bytes > MAX_JOURNAL_BYTES {
            let transaction_id = self.eviction_candidate()?;
            let record = self
                .records
                .remove(&transaction_id)
                .expect("eviction candidate must exist");
            let record_bytes = serde_json::to_vec(&record)?.len();
            bytes = bytes.saturating_sub(record_bytes);
            if !self.records.is_empty() {
                bytes = bytes.saturating_sub(1);
            }
        }
        Ok(())
    }

    fn eviction_candidate(&self) -> Result<String, JournalError> {
        self.records
            .values()
            .filter_map(|record| {
                record
                    .eviction_class()
                    .map(|class| (class, record.updated_at_ms, record.transaction_id.clone()))
            })
            .min()
            .map(|(_, _, transaction_id)| transaction_id)
            .ok_or(JournalError::ProtectedRecordsExceedLimit)
    }
}

fn unique_nonce() -> u64 {
    let mut random = [0_u8; 8];
    if getrandom::fill(&mut random).is_ok() {
        return u64::from_le_bytes(random);
    }
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn create_private_dir(path: &Path) -> Result<(), JournalError> {
    if path.exists() {
        validate_private_dir(path)?;
        return Ok(());
    }
    fs::create_dir(path)?;
    set_private_dir_permissions(path)?;
    validate_private_dir(path)
}

fn create_plain_dir(path: &Path) -> Result<(), JournalError> {
    if !path.exists() {
        fs::create_dir(path)?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(JournalError::UnsafeMetadata);
    }
    Ok(())
}

fn open_private_file(path: &Path) -> Result<File, JournalError> {
    if path.exists() {
        validate_private_file(path)?;
        return OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(JournalError::Io);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    set_private_file_creation_mode(&mut options);
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    validate_private_file(path)?;
    Ok(file)
}

fn cleanup_stale_temps(transactions_dir: &Path) -> Result<(), JournalError> {
    for entry in fs::read_dir(transactions_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(JournalError::UnsafeMetadata);
        };
        if name.starts_with(".journal-v1.") && name.ends_with(".tmp") {
            validate_private_file(&entry.path())?;
            fs::remove_file(entry.path())?;
        }
    }
    sync_directory(transactions_dir)
}

fn create_new_private_file(path: &Path) -> Result<File, JournalError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_private_file_creation_mode(&mut options);
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    validate_private_file(path)?;
    Ok(file)
}

fn sync_directory(path: &Path) -> Result<(), JournalError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn set_private_file_creation_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_file_creation_mode(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<(), JournalError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<(), JournalError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), JournalError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), JournalError> {
    Ok(())
}

#[cfg(unix)]
fn validate_private_dir(path: &Path) -> Result<(), JournalError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(JournalError::UnsafeMetadata);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_dir(path: &Path) -> Result<(), JournalError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(JournalError::UnsafeMetadata);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_file(path: &Path) -> Result<(), JournalError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(JournalError::UnsafeMetadata);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_file(path: &Path) -> Result<(), JournalError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(JournalError::UnsafeMetadata);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use godot_codex_bridge_client::{
        AffectedEntityRole, ApprovalScope, OperationKind, Risk, TransactionState,
    };

    fn project() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("project.godot"), b"[application]\n").unwrap();
        directory
    }

    fn record(index: usize, state: TransactionState) -> TransactionJournalRecord {
        TransactionJournalRecord {
            transaction_id: format!("transaction:{index:032x}"),
            idempotency_fingerprint: format!("sha256:{}", "1".repeat(64)),
            operation_digest: format!("sha256:{}", "2".repeat(64)),
            editor_session_id: format!("editor:{}", "3".repeat(32)),
            scene_id: format!("scene:{}", "4".repeat(32)),
            history_id: format!("history:{}", "5".repeat(32)),
            state,
            transaction_seq: 1,
            scene_revision: 2,
            operation_seq: 3,
            operation_kind: OperationKind::CreateNode,
            risk: Risk::Write,
            scope: ApprovalScope::SceneNodeCreate,
            affected_entities: vec![AffectedEntity {
                node_id: format!("node:{}", "6".repeat(32)),
                role: AffectedEntityRole::Parent,
            }],
            preview_digest: format!("sha256:{}", "7".repeat(64)),
            created_at_ms: 10,
            updated_at_ms: u64::try_from(index).unwrap(),
            expires_at_ms: 300_010,
            apply_dispatched: false,
            undo_eligible: false,
            safe_error: None,
            receipt_hash: None,
        }
    }

    #[test]
    fn journal_round_trips_atomically_and_privately() {
        let project = project();
        let project_id = format!("project:sha256:{}", "a".repeat(64));
        let (store, mut state) = JournalStore::open(project.path(), &project_id).unwrap();
        state.records.insert(
            "transaction:00000000000000000000000000000001".to_owned(),
            record(1, TransactionState::Previewed),
        );
        store.persist(&mut state).unwrap();
        drop(store);
        let (_store, loaded) = JournalStore::open(project.path(), &project_id).unwrap();
        assert_eq!(loaded.records, state.records);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(
                    project
                        .path()
                        .join(".godot/codex/transactions/journal-v1.json")
                )
                .unwrap()
                .permissions()
                .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn journal_projection_excludes_replay_and_approval_secrets() {
        let project = project();
        let project_id = format!("project:sha256:{}", "a".repeat(64));
        let (store, mut state) = JournalStore::open(project.path(), &project_id).unwrap();
        state.records.insert(
            "transaction:00000000000000000000000000000001".to_owned(),
            record(1, TransactionState::AwaitingApproval),
        );
        store.persist(&mut state).unwrap();
        let value: serde_json::Value = serde_json::from_slice(
            &fs::read(
                project
                    .path()
                    .join(".godot/codex/transactions/journal-v1.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let record = value["records"][0].as_object().unwrap();
        for forbidden in [
            "operation",
            "preview",
            "preview_payload_json",
            "idempotency_key",
            "approval",
            "receipt",
            "nonce",
            "mac",
            "session_token",
            "native_history_id",
        ] {
            assert!(
                !record.contains_key(forbidden),
                "journal leaked forbidden field {forbidden}"
            );
        }
    }

    #[test]
    fn journals_are_isolated_by_project_root() {
        let first_project = project();
        let second_project = project();
        let first_id = format!("project:sha256:{}", "a".repeat(64));
        let second_id = format!("project:sha256:{}", "b".repeat(64));
        let (first_store, mut first_state) =
            JournalStore::open(first_project.path(), &first_id).unwrap();
        first_state.records.insert(
            "transaction:00000000000000000000000000000001".to_owned(),
            record(1, TransactionState::Committed),
        );
        first_store.persist(&mut first_state).unwrap();

        let (_second_store, second_state) =
            JournalStore::open(second_project.path(), &second_id).unwrap();
        assert!(second_state.records.is_empty());
        assert!(
            !second_project
                .path()
                .join(".godot/codex/transactions/journal-v1.json")
                .exists()
        );
    }

    #[test]
    fn corrupt_journal_is_quarantined_without_replay_data() {
        let project = project();
        let project_id = format!("project:sha256:{}", "a".repeat(64));
        let (store, mut state) = JournalStore::open(project.path(), &project_id).unwrap();
        state.records.insert(
            "transaction:00000000000000000000000000000001".to_owned(),
            record(1, TransactionState::Previewed),
        );
        store.persist(&mut state).unwrap();
        let journal = project
            .path()
            .join(".godot/codex/transactions/journal-v1.json");
        fs::write(&journal, b"{truncated").unwrap();
        set_private_file_permissions(&journal).unwrap();
        drop(store);
        let (store, loaded) = JournalStore::open(project.path(), &project_id).unwrap();
        assert!(store.recovered_corruption());
        assert!(loaded.records.is_empty());
        assert!(
            fs::read_dir(project.path().join(".godot/codex/quarantine"))
                .unwrap()
                .next()
                .is_some()
        );
    }

    #[test]
    fn compatible_minor_journal_migrates_atomically_to_current_schema() {
        let project = project();
        let project_id = format!("project:sha256:{}", "a".repeat(64));
        let (store, mut state) = JournalStore::open(project.path(), &project_id).unwrap();
        state.records.insert(
            "transaction:00000000000000000000000000000001".to_owned(),
            record(1, TransactionState::Committed),
        );
        store.persist(&mut state).unwrap();
        let journal = project
            .path()
            .join(".godot/codex/transactions/journal-v1.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
        value["schema_version"] = serde_json::Value::String("transaction-journal/1.1".to_owned());
        fs::write(&journal, serde_json::to_vec(&value).unwrap()).unwrap();
        set_private_file_permissions(&journal).unwrap();
        drop(store);

        let (_store, loaded) = JournalStore::open(project.path(), &project_id).unwrap();
        assert_eq!(loaded.records.len(), 1);
        let migrated: serde_json::Value =
            serde_json::from_slice(&fs::read(journal).unwrap()).unwrap();
        assert_eq!(migrated["schema_version"], JOURNAL_SCHEMA);
        assert!(migrated["journal_revision"].as_u64().unwrap() > state.revision);
    }

    #[test]
    fn stale_atomic_temp_is_removed_while_the_project_lock_is_held() {
        let project = project();
        let project_id = format!("project:sha256:{}", "a".repeat(64));
        let (store, _) = JournalStore::open(project.path(), &project_id).unwrap();
        let stale = project
            .path()
            .join(".godot/codex/transactions/.journal-v1.1.stale.tmp");
        let mut temp = create_new_private_file(&stale).unwrap();
        temp.write_all(b"partial").unwrap();
        temp.sync_all().unwrap();
        drop(temp);
        drop(store);
        let (_store, _state) = JournalStore::open(project.path(), &project_id).unwrap();
        assert!(!stale.exists());
    }

    #[test]
    fn gc_never_evicts_in_doubt_or_active_records() {
        let mut state = JournalState::default();
        for index in 0..MAX_JOURNAL_RECORDS {
            state.records.insert(
                format!("transaction:{index:032x}"),
                record(index, TransactionState::Previewed),
            );
        }
        state.records.insert(
            format!("transaction:{:032x}", MAX_JOURNAL_RECORDS),
            record(MAX_JOURNAL_RECORDS, TransactionState::InDoubt),
        );
        assert!(matches!(
            state.enforce_bounds(),
            Err(JournalError::ProtectedRecordsExceedLimit)
        ));
    }

    #[test]
    fn gc_removes_old_terminal_records_first() {
        let mut state = JournalState::default();
        for index in 0..=MAX_JOURNAL_RECORDS {
            state.records.insert(
                format!("transaction:{index:032x}"),
                record(index, TransactionState::Expired),
            );
        }
        state.enforce_bounds().unwrap();
        assert_eq!(state.records.len(), MAX_JOURNAL_RECORDS);
        assert!(
            !state
                .records
                .contains_key(&format!("transaction:{:032x}", 0))
        );
    }

    #[test]
    fn gc_enforces_the_byte_budget_deterministically() {
        let mut state = JournalState::default();
        for index in 0..MAX_JOURNAL_RECORDS {
            let mut record = record(index, TransactionState::Expired);
            record.safe_error = Some(SafeTransactionError {
                code: "transaction_failed".to_owned(),
                message: "x".repeat(9_000),
                retryable: false,
            });
            state
                .records
                .insert(format!("transaction:{index:032x}"), record);
        }
        state.enforce_bounds().unwrap();
        assert!(state.records.len() < MAX_JOURNAL_RECORDS);
        let serialized = serde_json::to_vec(&JournalDocument {
            schema_version: JOURNAL_SCHEMA.to_owned(),
            project_id: String::new(),
            journal_revision: state.revision,
            records: state.records.values().cloned().collect(),
        })
        .unwrap();
        assert!(serialized.len() <= MAX_JOURNAL_BYTES);
        assert!(
            !state
                .records
                .contains_key("transaction:00000000000000000000000000000000")
        );
    }

    #[test]
    fn project_binding_and_unknown_major_fail_closed_without_overwrite() {
        let project = project();
        let project_id = format!("project:sha256:{}", "a".repeat(64));
        let other_project_id = format!("project:sha256:{}", "b".repeat(64));
        let (store, mut state) = JournalStore::open(project.path(), &project_id).unwrap();
        state.records.insert(
            "transaction:00000000000000000000000000000001".to_owned(),
            record(1, TransactionState::Committed),
        );
        store.persist(&mut state).unwrap();
        drop(store);
        assert!(matches!(
            JournalStore::open(project.path(), &other_project_id),
            Err(JournalError::ProjectMismatch)
        ));

        let journal = project
            .path()
            .join(".godot/codex/transactions/journal-v1.json");
        let incompatible =
            br#"{"schema_version":"transaction-journal/2.0","project_id":"opaque","journal_revision":1,"records":[]}"#;
        fs::write(&journal, incompatible).unwrap();
        set_private_file_permissions(&journal).unwrap();
        assert!(matches!(
            JournalStore::open(project.path(), &project_id),
            Err(JournalError::IncompatibleSchema)
        ));
        assert_eq!(fs::read(journal).unwrap(), incompatible);
    }

    #[test]
    fn duplicate_records_are_quarantined() {
        let project = project();
        let project_id = format!("project:sha256:{}", "a".repeat(64));
        let (store, _) = JournalStore::open(project.path(), &project_id).unwrap();
        drop(store);
        let duplicate = JournalDocument {
            schema_version: JOURNAL_SCHEMA.to_owned(),
            project_id: project_id.clone(),
            journal_revision: 1,
            records: vec![
                record(1, TransactionState::Expired),
                record(1, TransactionState::Expired),
            ],
        };
        let journal = project
            .path()
            .join(".godot/codex/transactions/journal-v1.json");
        fs::write(&journal, serde_json::to_vec(&duplicate).unwrap()).unwrap();
        set_private_file_permissions(&journal).unwrap();
        let (store, state) = JournalStore::open(project.path(), &project_id).unwrap();
        assert!(store.recovered_corruption());
        assert!(state.records.is_empty());
    }

    #[test]
    fn semantically_unsafe_record_is_quarantined_before_it_can_be_served() {
        let project = project();
        let project_id = format!("project:sha256:{}", "a".repeat(64));
        let (store, _) = JournalStore::open(project.path(), &project_id).unwrap();
        drop(store);
        let mut unsafe_record = record(1, TransactionState::Rejected);
        unsafe_record.safe_error = Some(SafeTransactionError {
            code: "transaction_failed".to_owned(),
            message: "Failed at /Users/private/project/main.tscn".to_owned(),
            retryable: false,
        });
        let document = JournalDocument {
            schema_version: JOURNAL_SCHEMA.to_owned(),
            project_id: project_id.clone(),
            journal_revision: 1,
            records: vec![unsafe_record],
        };
        let journal = project
            .path()
            .join(".godot/codex/transactions/journal-v1.json");
        fs::write(&journal, serde_json::to_vec(&document).unwrap()).unwrap();
        set_private_file_permissions(&journal).unwrap();
        let (store, state) = JournalStore::open(project.path(), &project_id).unwrap();
        assert!(store.recovered_corruption());
        assert!(state.records.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn wrong_existing_private_mode_is_rejected_not_repaired() {
        use std::os::unix::fs::PermissionsExt;

        let project = project();
        let project_id = format!("project:sha256:{}", "a".repeat(64));
        let (store, _) = JournalStore::open(project.path(), &project_id).unwrap();
        drop(store);
        let lock = project.path().join(".godot/codex/transactions.lock");
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            JournalStore::open(project.path(), &project_id),
            Err(JournalError::UnsafeMetadata)
        ));
        assert_eq!(
            fs::metadata(lock).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_transaction_directory_is_rejected() {
        use std::os::unix::fs::symlink;

        let project = project();
        let project_id = format!("project:sha256:{}", "a".repeat(64));
        let (store, _) = JournalStore::open(project.path(), &project_id).unwrap();
        drop(store);
        let transactions = project.path().join(".godot/codex/transactions");
        fs::remove_dir(&transactions).unwrap();
        symlink(project.path(), &transactions).unwrap();
        assert!(matches!(
            JournalStore::open(project.path(), &project_id),
            Err(JournalError::UnsafeMetadata)
        ));
    }
}
