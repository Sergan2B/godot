use std::fs::{self, FileType, Metadata};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::bundle::validate_instance;
use crate::error::{ConformanceError, Result, require};
use crate::json::parse_strict_object;
use crate::protocol::project_id_for_root;

const DISCOVERY_RELATIVE_PATH: &str = ".godot/codex/bridge.json";

#[derive(Clone)]
pub struct Discovery {
    pub canonical_root: PathBuf,
    pub project_id: String,
    pub editor_session_id: String,
    pub endpoint: PathBuf,
    pub token: [u8; 32],
    raw_record: Vec<u8>,
}

#[allow(unsafe_code)]
fn effective_uid() -> u32 {
    // SAFETY: geteuid has no arguments, no preconditions, and no memory effects.
    unsafe { libc::geteuid() }
}

fn require_private_metadata(
    path: &Path,
    expected_mode: u32,
    expected_kind: fn(FileType) -> bool,
) -> Result<Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ConformanceError(format!(
            "cannot inspect private path {}: {error}",
            path.display()
        ))
    })?;
    require(
        !metadata.file_type().is_symlink(),
        format!("private path must not be a symlink: {}", path.display()),
    )?;
    require(
        expected_kind(metadata.file_type()),
        format!("private path has an unexpected type: {}", path.display()),
    )?;
    require(
        metadata.uid() == effective_uid(),
        format!("private path has an unexpected owner: {}", path.display()),
    )?;
    require(
        metadata.permissions().mode() & 0o777 == expected_mode,
        format!(
            "private path mode is not {expected_mode:04o}: {}",
            path.display()
        ),
    )?;
    Ok(metadata)
}

fn is_directory(file_type: FileType) -> bool {
    file_type.is_dir()
}

fn is_regular_file(file_type: FileType) -> bool {
    file_type.is_file()
}

fn is_socket(file_type: FileType) -> bool {
    file_type.is_socket()
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ConformanceError(format!("discovery field {key} must be a string")))
}

fn require_safe_relative_path(value: &str, expected_prefix: &Path) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    require(!path.is_absolute(), "discovery path must be relative")?;
    require(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "discovery path contains traversal or a non-normal component",
    )?;
    require(
        path.starts_with(expected_prefix),
        "discovery path escapes the private runtime directory",
    )?;
    Ok(path)
}

fn canonical_descendant(path: &Path, parent: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path).map_err(|error| {
        ConformanceError(format!("cannot canonicalize {}: {error}", path.display()))
    })?;
    require(
        canonical.starts_with(parent),
        format!("path escapes private runtime: {}", path.display()),
    )?;
    Ok(canonical)
}

impl Discovery {
    pub fn load(project_root: &Path) -> Result<Self> {
        let canonical_root = fs::canonicalize(project_root).map_err(|error| {
            ConformanceError(format!(
                "cannot canonicalize project root {}: {error}",
                project_root.display()
            ))
        })?;
        require(
            fs::metadata(&canonical_root)?.is_dir(),
            "project root must be a directory",
        )?;
        require(
            fs::metadata(canonical_root.join("project.godot"))?.is_file(),
            "project.godot is missing",
        )?;

        let codex_dir = canonical_root.join(".godot/codex");
        let run_dir = codex_dir.join("run");
        require_private_metadata(&codex_dir, 0o700, is_directory)?;
        require_private_metadata(&run_dir, 0o700, is_directory)?;
        let canonical_codex = fs::canonicalize(&codex_dir)?;

        let discovery_path = canonical_root.join(DISCOVERY_RELATIVE_PATH);
        require_private_metadata(&discovery_path, 0o600, is_regular_file)?;
        let discovery_bytes = fs::read(&discovery_path)?;
        let record = parse_strict_object(&discovery_bytes)?;
        validate_instance("discovery.schema.json", &record)?;

        let root_bytes = canonical_root
            .to_str()
            .ok_or_else(|| ConformanceError("canonical project root is not UTF-8".to_owned()))?
            .as_bytes();
        let expected_project_id = project_id_for_root(root_bytes);
        let project_id = required_string(&record, "project_id")?.to_owned();
        require(
            project_id == expected_project_id,
            "discovery project_id does not match the canonical project root",
        )?;
        let editor_session_id = required_string(&record, "editor_session_id")?.to_owned();
        require(
            record.get("transport").and_then(Value::as_str) == Some("uds"),
            "discovery transport is not uds",
        )?;
        require(
            record
                .get("protocol_versions")
                .and_then(Value::as_array)
                .is_some_and(|versions| versions.iter().any(|value| value == "1.0")),
            "discovery does not advertise Bridge RPC 1.0",
        )?;

        let token_relative = require_safe_relative_path(
            required_string(&record, "token_file")?,
            Path::new(".godot/codex"),
        )?;
        require(
            token_relative == Path::new(".godot/codex/session.token"),
            "unexpected token path",
        )?;
        let token_path = canonical_root.join(token_relative);
        require_private_metadata(&token_path, 0o600, is_regular_file)?;
        canonical_descendant(&token_path, &canonical_codex)?;
        let token_bytes = fs::read(&token_path)?;
        let token: [u8; 32] = token_bytes
            .try_into()
            .map_err(|_| ConformanceError("session token must be exactly 32 bytes".to_owned()))?;

        let endpoint_relative = require_safe_relative_path(
            required_string(&record, "endpoint")?,
            Path::new(".godot/codex/run"),
        )?;
        let endpoint = canonical_root.join(endpoint_relative);
        require_private_metadata(&endpoint, 0o600, is_socket)?;
        canonical_descendant(&endpoint, &canonical_codex)?;

        let lock_path = codex_dir.join("bridge.lock");
        require_private_metadata(&lock_path, 0o600, is_regular_file)?;
        canonical_descendant(&lock_path, &canonical_codex)?;

        Ok(Self {
            canonical_root,
            project_id,
            editor_session_id,
            endpoint,
            token,
            raw_record: discovery_bytes,
        })
    }

    pub fn assert_unchanged(&self) -> Result<()> {
        let current = fs::read(self.canonical_root.join(DISCOVERY_RELATIVE_PATH))?;
        require(
            current == self.raw_record,
            "discovery record changed during connection establishment",
        )
    }

    pub fn secret_fingerprints(&self) -> Vec<String> {
        let mut values = Vec::new();
        values.push(base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            self.token,
        ));
        let digest = Sha256::digest(self.token);
        values.push(format!("{digest:x}"));
        values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_uid_matches_the_test_process_files() {
        let metadata = fs::metadata(env!("CARGO_MANIFEST_DIR")).unwrap();
        assert_eq!(metadata.uid(), effective_uid());
    }

    #[test]
    fn traversal_is_not_a_safe_discovery_path() {
        assert!(
            require_safe_relative_path(".godot/codex/../session.token", Path::new(".godot/codex"))
                .is_err()
        );
    }
}
