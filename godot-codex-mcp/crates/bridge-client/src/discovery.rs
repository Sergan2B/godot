use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::protocol::BridgeError;

pub fn project_id_for_root(root: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"godot-codex-project-id/v1\0");
    hasher.update(root);
    format!("project:sha256:{:x}", hasher.finalize())
}

#[derive(Clone, Debug)]
pub enum BridgeEndpoint {
    Unix(PathBuf),
    Tcp(SocketAddr),
    Unsupported,
}

#[derive(Clone, Debug)]
pub struct Discovery {
    pub canonical_root: std::path::PathBuf,
    pub project_id: String,
    pub editor_session_id: String,
    pub endpoint: BridgeEndpoint,
    pub token: [u8; 32],
    pub(crate) raw_record: Vec<u8>,
}

#[cfg(unix)]
mod unix {
    use std::fs::{self, FileType, Metadata};
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::path::{Component, Path, PathBuf};

    use serde::Deserialize;

    use super::{BridgeError, Discovery, project_id_for_root};

    const DISCOVERY_RELATIVE_PATH: &str = ".godot/codex/bridge.json";

    #[derive(Deserialize)]
    struct DiscoveryRecord {
        discovery_schema: u32,
        transport: String,
        endpoint: String,
        token_file: String,
        project_id: String,
        editor_session_id: String,
        protocol_versions: Vec<String>,
    }

    fn require_private_metadata(
        path: &Path,
        expected_mode: u32,
        expected_kind: fn(FileType) -> bool,
    ) -> Result<Metadata, BridgeError> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink()
            || !expected_kind(metadata.file_type())
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & 0o777 != expected_mode
        {
            return Err(BridgeError::Invalid(format!(
                "private runtime metadata is invalid: {}",
                path.display()
            )));
        }
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

    fn safe_relative(value: &str, expected_prefix: &Path) -> Result<PathBuf, BridgeError> {
        let path = PathBuf::from(value);
        if path.is_absolute()
            || !path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            || !path.starts_with(expected_prefix)
        {
            return Err(BridgeError::Invalid("unsafe discovery path".to_owned()));
        }
        Ok(path)
    }

    fn canonical_descendant(path: &Path, parent: &Path) -> Result<PathBuf, BridgeError> {
        let canonical = fs::canonicalize(path)?;
        if !canonical.starts_with(parent) {
            return Err(BridgeError::Invalid(format!(
                "runtime path escapes project: {}",
                path.display()
            )));
        }
        Ok(canonical)
    }

    fn valid_editor_session_id(value: &str) -> bool {
        value.len() == 39
            && value.starts_with("editor:")
            && value[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }

    pub(super) fn load(project_root: &Path) -> Result<Discovery, BridgeError> {
        let canonical_root = fs::canonicalize(project_root)?;
        if !canonical_root.is_dir() || !canonical_root.join("project.godot").is_file() {
            return Err(BridgeError::Invalid(
                "project root must contain project.godot".to_owned(),
            ));
        }
        let codex_dir = canonical_root.join(".godot/codex");
        let run_dir = codex_dir.join("run");
        require_private_metadata(&codex_dir, 0o700, is_directory)?;
        require_private_metadata(&run_dir, 0o700, is_directory)?;
        let canonical_codex = fs::canonicalize(&codex_dir)?;

        let discovery_path = canonical_root.join(DISCOVERY_RELATIVE_PATH);
        require_private_metadata(&discovery_path, 0o600, is_regular_file)?;
        let raw_record = fs::read(&discovery_path)?;
        let record: DiscoveryRecord = serde_json::from_slice(&raw_record)?;
        if record.discovery_schema != 1
            || record.transport != "uds"
            || !record
                .protocol_versions
                .iter()
                .any(|version| matches!(version.as_str(), "1.0" | "1.1" | "1.2"))
            || !valid_editor_session_id(&record.editor_session_id)
        {
            return Err(BridgeError::Invalid(
                "discovery record has no supported Bridge RPC major-one version".to_owned(),
            ));
        }

        let root = canonical_root
            .to_str()
            .ok_or_else(|| BridgeError::Invalid("project root is not UTF-8".to_owned()))?;
        let expected_project_id = project_id_for_root(root.as_bytes());
        if record.project_id != expected_project_id {
            return Err(BridgeError::Invalid(
                "discovery project binding mismatch".to_owned(),
            ));
        }

        let token_relative = safe_relative(&record.token_file, Path::new(".godot/codex"))?;
        if token_relative != Path::new(".godot/codex/session.token") {
            return Err(BridgeError::Invalid("unexpected token path".to_owned()));
        }
        let token_path = canonical_root.join(token_relative);
        require_private_metadata(&token_path, 0o600, is_regular_file)?;
        canonical_descendant(&token_path, &canonical_codex)?;
        let token: [u8; 32] = fs::read(&token_path)?
            .try_into()
            .map_err(|_| BridgeError::Invalid("session token is not 32 bytes".to_owned()))?;

        let endpoint_relative = safe_relative(&record.endpoint, Path::new(".godot/codex/run"))?;
        let endpoint = canonical_root.join(endpoint_relative);
        require_private_metadata(&endpoint, 0o600, is_socket)?;
        canonical_descendant(&endpoint, &canonical_codex)?;
        let lock_path = codex_dir.join("bridge.lock");
        require_private_metadata(&lock_path, 0o600, is_regular_file)?;
        canonical_descendant(&lock_path, &canonical_codex)?;

        Ok(Discovery {
            canonical_root,
            project_id: record.project_id,
            editor_session_id: record.editor_session_id,
            endpoint: super::BridgeEndpoint::Unix(endpoint),
            token,
            raw_record,
        })
    }
}

#[cfg(windows)]
mod windows {
    use std::fs::{self, FileType, Metadata};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::os::windows::fs::MetadataExt;
    use std::path::{Component, Path, PathBuf};

    use serde::Deserialize;

    use super::{BridgeEndpoint, BridgeError, Discovery, project_id_for_root};

    const DISCOVERY_RELATIVE_PATH: &str = ".godot/codex/bridge.json";
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    #[derive(Deserialize)]
    struct DiscoveryRecord {
        discovery_schema: u32,
        transport: String,
        endpoint: String,
        token_file: String,
        project_id: String,
        editor_session_id: String,
        protocol_versions: Vec<String>,
    }

    fn require_plain_metadata(
        path: &Path,
        expected_kind: fn(FileType) -> bool,
    ) -> Result<Metadata, BridgeError> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || !expected_kind(metadata.file_type())
        {
            return Err(BridgeError::Invalid(format!(
                "private runtime metadata is invalid: {}",
                path.display()
            )));
        }
        Ok(metadata)
    }

    fn is_directory(file_type: FileType) -> bool {
        file_type.is_dir()
    }

    fn is_regular_file(file_type: FileType) -> bool {
        file_type.is_file()
    }

    fn safe_relative(value: &str, expected_prefix: &Path) -> Result<PathBuf, BridgeError> {
        let path = PathBuf::from(value);
        if path.is_absolute()
            || !path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            || !path.starts_with(expected_prefix)
        {
            return Err(BridgeError::Invalid("unsafe discovery path".to_owned()));
        }
        Ok(path)
    }

    fn canonical_descendant(path: &Path, parent: &Path) -> Result<PathBuf, BridgeError> {
        let canonical = fs::canonicalize(path)?;
        if !canonical.starts_with(parent) {
            return Err(BridgeError::Invalid(format!(
                "runtime path escapes project: {}",
                path.display()
            )));
        }
        Ok(canonical)
    }

    fn canonical_identity(path: &Path) -> Result<String, BridgeError> {
        let raw = path
            .to_str()
            .ok_or_else(|| BridgeError::Invalid("project root is not UTF-8".to_owned()))?
            .replace('\\', "/");
        if let Some(rest) = raw.strip_prefix("//?/UNC/") {
            Ok(format!("//{rest}").trim_end_matches('/').to_owned())
        } else {
            Ok(raw
                .strip_prefix("//?/")
                .unwrap_or(&raw)
                .trim_end_matches('/')
                .to_owned())
        }
    }

    fn valid_editor_session_id(value: &str) -> bool {
        value.len() == 39
            && value.starts_with("editor:")
            && value[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }

    fn parse_loopback_endpoint(value: &str) -> Result<SocketAddr, BridgeError> {
        let endpoint: SocketAddr = value
            .parse()
            .map_err(|_| BridgeError::Invalid("invalid loopback endpoint".to_owned()))?;
        if endpoint.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) || endpoint.port() == 0 {
            return Err(BridgeError::Invalid(
                "endpoint is not IPv4 loopback".to_owned(),
            ));
        }
        if endpoint.to_string() != value {
            return Err(BridgeError::Invalid(
                "loopback endpoint is not canonical".to_owned(),
            ));
        }
        Ok(endpoint)
    }

    pub(super) fn load(project_root: &Path) -> Result<Discovery, BridgeError> {
        let canonical_root = fs::canonicalize(project_root)?;
        require_plain_metadata(&canonical_root, is_directory)?;
        if !canonical_root.join("project.godot").is_file() {
            return Err(BridgeError::Invalid(
                "project root must contain project.godot".to_owned(),
            ));
        }
        let codex_dir = canonical_root.join(".godot/codex");
        let run_dir = codex_dir.join("run");
        require_plain_metadata(&codex_dir, is_directory)?;
        require_plain_metadata(&run_dir, is_directory)?;
        let canonical_codex = fs::canonicalize(&codex_dir)?;

        let discovery_path = canonical_root.join(DISCOVERY_RELATIVE_PATH);
        require_plain_metadata(&discovery_path, is_regular_file)?;
        let raw_record = fs::read(&discovery_path)?;
        let record: DiscoveryRecord = serde_json::from_slice(&raw_record)?;
        if record.discovery_schema != 1
            || record.transport != "tcp_loopback"
            || !record
                .protocol_versions
                .iter()
                .any(|version| matches!(version.as_str(), "1.0" | "1.1" | "1.2"))
            || !valid_editor_session_id(&record.editor_session_id)
        {
            return Err(BridgeError::Invalid(
                "discovery record has no supported Bridge RPC major-one version".to_owned(),
            ));
        }

        let identity = canonical_identity(&canonical_root)?;
        if record.project_id != project_id_for_root(identity.as_bytes()) {
            return Err(BridgeError::Invalid(
                "discovery project binding mismatch".to_owned(),
            ));
        }

        let token_relative = safe_relative(&record.token_file, Path::new(".godot/codex"))?;
        if token_relative != Path::new(".godot/codex/session.token") {
            return Err(BridgeError::Invalid("unexpected token path".to_owned()));
        }
        let token_path = canonical_root.join(token_relative);
        require_plain_metadata(&token_path, is_regular_file)?;
        canonical_descendant(&token_path, &canonical_codex)?;
        let token: [u8; 32] = fs::read(&token_path)?
            .try_into()
            .map_err(|_| BridgeError::Invalid("session token is not 32 bytes".to_owned()))?;

        let lock_path = codex_dir.join("bridge.lock");
        require_plain_metadata(&lock_path, is_regular_file)?;
        canonical_descendant(&lock_path, &canonical_codex)?;

        Ok(Discovery {
            canonical_root,
            project_id: record.project_id,
            editor_session_id: record.editor_session_id,
            endpoint: BridgeEndpoint::Tcp(parse_loopback_endpoint(&record.endpoint)?),
            token,
            raw_record,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn only_canonical_ipv4_loopback_endpoints_are_accepted() {
            assert_eq!(
                parse_loopback_endpoint("127.0.0.1:49152").unwrap(),
                SocketAddr::from((Ipv4Addr::LOCALHOST, 49152))
            );
            for invalid in [
                "0.0.0.0:49152",
                "192.168.1.2:49152",
                "[::1]:49152",
                "127.0.0.1:0",
                "127.0.0.1:049152",
                "127.0.0.1:65536",
            ] {
                assert!(parse_loopback_endpoint(invalid).is_err(), "{invalid}");
            }
        }
    }
}

impl Discovery {
    #[cfg(unix)]
    pub fn load(project_root: &Path) -> Result<Self, BridgeError> {
        unix::load(project_root)
    }

    #[cfg(windows)]
    pub fn load(project_root: &Path) -> Result<Self, BridgeError> {
        windows::load(project_root)
    }

    #[cfg(not(any(unix, windows)))]
    pub fn load(_project_root: &Path) -> Result<Self, BridgeError> {
        Err(BridgeError::Unsupported)
    }

    pub fn transport_capability(&self) -> &'static str {
        match self.endpoint {
            BridgeEndpoint::Unix(_) => "transport.uds",
            BridgeEndpoint::Tcp(_) => "transport.tcp_loopback",
            BridgeEndpoint::Unsupported => "transport.unsupported",
        }
    }

    pub fn assert_unchanged(&self) -> Result<(), BridgeError> {
        let current = std::fs::read(self.canonical_root.join(".godot/codex/bridge.json"))?;
        if current != self.raw_record {
            return Err(BridgeError::Invalid(
                "discovery record changed while connecting".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_matches_the_canonical_vector() {
        assert_eq!(
            project_id_for_root(b"/fixtures/codex-smoke"),
            "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd"
        );
    }
}
