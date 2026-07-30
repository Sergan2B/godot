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

fn canonical_project_identity(path: &Path) -> Result<String, BridgeError> {
    let raw = path
        .to_str()
        .ok_or_else(|| BridgeError::Invalid("project root is not UTF-8".to_owned()))?;
    #[cfg(windows)]
    {
        let normalized = raw.replace('\\', "/");
        if let Some(rest) = normalized.strip_prefix("//?/UNC/") {
            return Ok(format!("//{rest}").trim_end_matches('/').to_owned());
        }
        Ok(normalized
            .strip_prefix("//?/")
            .unwrap_or(&normalized)
            .trim_end_matches('/')
            .to_owned())
    }
    #[cfg(not(windows))]
    {
        Ok(raw.trim_end_matches('/').to_owned())
    }
}

pub fn project_id_for_path(path: &Path) -> Result<String, BridgeError> {
    let canonical = std::fs::canonicalize(path)?;
    let identity = canonical_project_identity(&canonical)?;
    Ok(project_id_for_root(identity.as_bytes()))
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

    fn external_runtime_root() -> PathBuf {
        #[cfg(target_os = "macos")]
        let temporary_root = Path::new("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let temporary_root = Path::new("/tmp");
        temporary_root.join(format!("gcx-{}", rustix::process::geteuid().as_raw()))
    }

    fn external_project_directory(project_id: &str) -> Result<PathBuf, BridgeError> {
        let digest = project_id
            .strip_prefix("project:sha256:")
            .filter(|value| {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
            .ok_or_else(|| BridgeError::Invalid("invalid project binding".to_owned()))?;
        Ok(external_runtime_root().join(format!("p-{}", &digest[..32])))
    }

    fn expected_external_endpoint(
        project_id: &str,
        editor_session_id: &str,
    ) -> Result<PathBuf, BridgeError> {
        if !valid_editor_session_id(editor_session_id) {
            return Err(BridgeError::Invalid(
                "invalid editor session binding".to_owned(),
            ));
        }
        Ok(external_project_directory(project_id)?
            .join(format!("b-{}.sock", &editor_session_id[7..])))
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
        if !matches!(record.discovery_schema, 1 | 2)
            || record.transport != "uds"
            || !valid_editor_session_id(&record.editor_session_id)
        {
            return Err(BridgeError::Invalid(
                "discovery record is invalid".to_owned(),
            ));
        }
        if !record.protocol_versions.iter().any(|version| {
            matches!(
                version.as_str(),
                "1.0" | "1.1" | "1.2" | "1.3" | "1.4" | "1.5" | "1.6" | "1.7" | "1.8"
            )
        }) {
            return Err(BridgeError::ProtocolVersionMismatch);
        }

        let root = canonical_root
            .to_str()
            .ok_or_else(|| BridgeError::Invalid("project root is not UTF-8".to_owned()))?;
        let expected_project_id = project_id_for_root(root.as_bytes());
        if record.project_id != expected_project_id {
            return Err(BridgeError::ProjectBindingMismatch);
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

        let endpoint = if record.discovery_schema == 1 {
            let endpoint_relative = safe_relative(&record.endpoint, Path::new(".godot/codex/run"))?;
            let endpoint = canonical_root.join(endpoint_relative);
            require_private_metadata(&endpoint, 0o600, is_socket)?;
            canonical_descendant(&endpoint, &canonical_codex)?
        } else {
            let expected =
                expected_external_endpoint(&record.project_id, &record.editor_session_id)?;
            if Path::new(&record.endpoint) != expected {
                return Err(BridgeError::Invalid(
                    "external endpoint does not match the project/session binding".to_owned(),
                ));
            }
            let external_root = external_runtime_root();
            let external_project = external_project_directory(&record.project_id)?;
            require_private_metadata(&external_root, 0o700, is_directory)?;
            require_private_metadata(&external_project, 0o700, is_directory)?;
            let canonical_external_project = fs::canonicalize(&external_project)?;
            let endpoint = PathBuf::from(&record.endpoint);
            require_private_metadata(&endpoint, 0o600, is_socket)?;
            canonical_descendant(&endpoint, &canonical_external_project)?
        };
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

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;
        use std::time::{SystemTime, UNIX_EPOCH};

        #[test]
        fn external_endpoint_is_exactly_bound_to_project_and_session() {
            let project_id = format!("project:sha256:{}", "a".repeat(64));
            let session_id = format!("editor:{}", "b".repeat(32));
            let endpoint = expected_external_endpoint(&project_id, &session_id).unwrap();
            assert_eq!(
                endpoint,
                external_runtime_root()
                    .join(format!("p-{}", "a".repeat(32)))
                    .join(format!("b-{}.sock", "b".repeat(32)))
            );
            assert!(endpoint.is_absolute());
            assert!(expected_external_endpoint("project:sha256:bad", &session_id).is_err());
            assert!(expected_external_endpoint(&project_id, "editor:bad").is_err());
        }

        #[test]
        fn schema_two_loads_an_exact_private_external_socket() {
            let unique = format!(
                "gcb-client-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let project_root = std::env::temp_dir().join(unique);
            let codex = project_root.join(".godot/codex");
            let run = codex.join("run");
            fs::create_dir_all(&run).unwrap();
            fs::write(project_root.join("project.godot"), b"[application]\n").unwrap();
            fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();

            let canonical_root = fs::canonicalize(&project_root).unwrap();
            let project_id = project_id_for_root(canonical_root.to_str().unwrap().as_bytes());
            let session_id = format!("editor:{}", "b".repeat(32));
            let external_root = external_runtime_root();
            let external_project = external_project_directory(&project_id).unwrap();
            fs::create_dir_all(&external_project).unwrap();
            fs::set_permissions(&external_root, fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&external_project, fs::Permissions::from_mode(0o700)).unwrap();
            let endpoint = expected_external_endpoint(&project_id, &session_id).unwrap();
            let listener = UnixListener::bind(&endpoint).unwrap();
            fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)).unwrap();

            let record = serde_json::json!({
                "discovery_schema": 2,
                "transport": "uds",
                "endpoint": endpoint.to_str().unwrap(),
                "token_file": ".godot/codex/session.token",
                "project_id": project_id.clone(),
                "editor_session_id": session_id,
                "protocol_versions": ["1.8"],
            });
            fs::write(
                codex.join("bridge.json"),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            fs::write(codex.join("session.token"), [7_u8; 32]).unwrap();
            fs::write(codex.join("bridge.lock"), b"{}").unwrap();
            for name in ["bridge.json", "session.token", "bridge.lock"] {
                fs::set_permissions(codex.join(name), fs::Permissions::from_mode(0o600)).unwrap();
            }

            let discovery = Discovery::load(&project_root).unwrap();
            assert_eq!(discovery.project_id, project_id);
            assert!(matches!(
                discovery.endpoint,
                super::super::BridgeEndpoint::Unix(ref value) if value == &endpoint
            ));

            drop(listener);
            fs::remove_file(&endpoint).unwrap();
            fs::remove_dir(&external_project).unwrap();
            let _ = fs::remove_dir(&external_root);
            fs::remove_dir_all(&project_root).unwrap();
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::fs::{self, FileType, Metadata};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::os::windows::fs::MetadataExt;
    use std::path::{Component, Path, PathBuf};

    use serde::Deserialize;

    use super::{BridgeEndpoint, BridgeError, Discovery, project_id_for_path};

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
            || !valid_editor_session_id(&record.editor_session_id)
        {
            return Err(BridgeError::Invalid(
                "discovery record is invalid".to_owned(),
            ));
        }
        if !record.protocol_versions.iter().any(|version| {
            matches!(
                version.as_str(),
                "1.0" | "1.1" | "1.2" | "1.3" | "1.4" | "1.5" | "1.6" | "1.7" | "1.8"
            )
        }) {
            return Err(BridgeError::ProtocolVersionMismatch);
        }

        if record.project_id != project_id_for_path(&canonical_root)? {
            return Err(BridgeError::ProjectBindingMismatch);
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

        #[test]
        fn canonical_windows_identity_matches_godot_path_spelling() {
            assert_eq!(
                crate::discovery::canonical_project_identity(Path::new(
                    r"\\?\C:\Users\Player\Game\",
                ))
                .unwrap(),
                "C:/Users/Player/Game"
            );
            assert_eq!(
                crate::discovery::canonical_project_identity(Path::new(
                    r"\\?\UNC\server\share\Game\",
                ))
                .unwrap(),
                "//server/share/Game"
            );
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
