use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::PRODUCT_VERSION;

const MAX_PATH_BYTES: usize = 4096;
const MAX_CHECKSUMS_BYTES: u64 = 128 * 1024;
const MAX_MANIFEST_BYTES: u64 = 512 * 1024;
const MAX_PACKAGE_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PACKAGE_FILES: usize = 512;
const MANIFEST_NAME: &str = "package-manifest.json";
const CHECKSUMS_NAME: &str = "checksums.sha256";
const OWNERSHIP_MARKER: &str = ".godot-codex-owned";
const SIDECAR_RELATIVE: &str = "bin/godot-codex-mcp";
const OPERATIONS_RELATIVE: &str = "bin/godot-codex";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LauncherError {
    Unavailable,
    Unsafe,
    InvalidPackage,
}

/// Exact package-owned sidecar selected for project configuration.
///
/// `command` can contain an installed `current` symlink on purpose: that is
/// the stable absolute launcher Codex hosts must execute. The other fields are
/// safe digests used to bind a setup plan/receipt without copying the path
/// into reports or evidence.
#[derive(Clone)]
pub(crate) struct LauncherResolution {
    command: PathBuf,
    operations_command: PathBuf,
    package_root: PathBuf,
    installed_data_root: Option<PathBuf>,
    path_digest: String,
    file_digest: String,
}

impl LauncherResolution {
    pub(crate) fn command(&self) -> &Path {
        &self.command
    }

    /// Stable package-owned operations launcher paired with the configured
    /// sidecar. Setup guidance must use this exact sibling so an isolated
    /// `GODOT_CODEX_DATA_ROOT` cannot silently fall back to a global install.
    pub(crate) fn operations_command_text(&self) -> Result<&str, LauncherError> {
        self.operations_command
            .to_str()
            .ok_or(LauncherError::Unsafe)
    }

    pub(crate) fn operations_command(&self) -> &Path {
        &self.operations_command
    }

    pub(crate) fn package_root(&self) -> &Path {
        &self.package_root
    }

    pub(crate) fn installed_data_root(&self) -> Option<&Path> {
        self.installed_data_root.as_deref()
    }

    pub(crate) fn path_digest(&self) -> &str {
        &self.path_digest
    }

    pub(crate) fn file_digest(&self) -> &str {
        &self.file_digest
    }
}

/// Resolves the one launcher authority shared by setup, doctor, and host
/// effective-config probes.
///
/// Explicit package roots are intended for an unpacked package or a package
/// validator fixture and use their exact canonical sibling sidecar. Normal
/// installed operation requires the installer-owned `current` link to point
/// at this exact package version and returns the stable lexical `current`
/// launcher instead of a PATH-dependent basename.
pub(crate) fn resolve_launcher(
    package_directory: Option<&Path>,
) -> Result<LauncherResolution, LauncherError> {
    let executable =
        fs::canonicalize(std::env::current_exe().map_err(|_| LauncherError::Unavailable)?)
            .map_err(|_| LauncherError::Unavailable)?;
    let installed_data_root = if package_directory.is_none() {
        Some(data_root(&executable)?)
    } else {
        None
    };
    resolve_launcher_from(
        package_directory,
        &executable,
        installed_data_root.as_deref(),
    )
}

fn resolve_launcher_from(
    package_directory: Option<&Path>,
    executable: &Path,
    installed_data_root: Option<&Path>,
) -> Result<LauncherResolution, LauncherError> {
    if let Some(package_root) = package_directory {
        return resolve_unpacked_launcher(package_root);
    }
    resolve_installed_launcher_at(
        installed_data_root.ok_or(LauncherError::Unavailable)?,
        executable,
    )
}

pub(crate) fn launcher_identity_for_command(
    command: &Path,
) -> Result<(String, String), LauncherError> {
    if !safe_absolute_path(command) {
        return Err(LauncherError::Unsafe);
    }
    let bytes = read_executable_bounded(command, MAX_PACKAGE_FILE_BYTES)?;
    Ok((digest_path(command)?, digest_bytes(&bytes)))
}

fn resolve_unpacked_launcher(package_root: &Path) -> Result<LauncherResolution, LauncherError> {
    let package_root = fs::canonicalize(package_root).map_err(|_| LauncherError::Unavailable)?;
    if !safe_absolute_path(&package_root) || !plain_directory(&package_root) {
        return Err(LauncherError::Unsafe);
    }
    let manifest_path = package_root.join(MANIFEST_NAME);
    let manifest = read_plain_bounded(&manifest_path, MAX_MANIFEST_BYTES)?;
    let value =
        serde_json::from_slice::<Value>(&manifest).map_err(|_| LauncherError::InvalidPackage)?;
    if value.get("package_version").and_then(Value::as_str) != Some(PRODUCT_VERSION) {
        return Err(LauncherError::InvalidPackage);
    }

    let command = package_root.join(SIDECAR_RELATIVE);
    let operations_command = package_root.join(OPERATIONS_RELATIVE);
    let (path_digest, file_digest) = launcher_identity_for_command(&command)?;
    read_executable_bounded(&operations_command, MAX_PACKAGE_FILE_BYTES)?;
    Ok(LauncherResolution {
        command,
        operations_command,
        package_root,
        installed_data_root: None,
        path_digest,
        file_digest,
    })
}

fn data_root(executable: &Path) -> Result<PathBuf, LauncherError> {
    if let Some(root) = std::env::var_os("GODOT_CODEX_DATA_ROOT").filter(|value| !value.is_empty())
    {
        let root = PathBuf::from(root);
        return safe_absolute_path(&root)
            .then_some(root)
            .ok_or(LauncherError::Unsafe);
    }
    if let Some(root) = installed_data_root_from_executable(executable) {
        return Ok(root);
    }
    if !cfg!(target_os = "macos") {
        return Err(LauncherError::Unavailable);
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(LauncherError::Unavailable)?;
    let root = home.join("Library/Application Support/GodotCodex");
    safe_absolute_path(&root)
        .then_some(root)
        .ok_or(LauncherError::Unsafe)
}

fn installed_data_root_from_executable(executable: &Path) -> Option<PathBuf> {
    let bin = executable.parent()?;
    let version = bin.parent()?;
    let versions = version.parent()?;
    let root = versions.parent()?.to_path_buf();
    (bin.file_name()?.to_str()? == "bin"
        && version.file_name()?.to_str()? == PRODUCT_VERSION
        && versions.file_name()?.to_str()? == "versions"
        && safe_absolute_path(&root))
    .then_some(root)
}

fn resolve_installed_launcher_at(
    data_root: &Path,
    executable: &Path,
) -> Result<LauncherResolution, LauncherError> {
    if !safe_absolute_path(data_root) || !plain_directory(data_root) || data_root == Path::new("/")
    {
        return Err(LauncherError::Unsafe);
    }
    let versions_root = data_root.join("versions");
    if !plain_directory(&versions_root) {
        return Err(LauncherError::Unsafe);
    }
    let exact_target = versions_root.join(PRODUCT_VERSION);
    if !plain_directory(&exact_target) {
        return Err(LauncherError::Unavailable);
    }
    let current = data_root.join("current");
    let current_metadata =
        fs::symlink_metadata(&current).map_err(|_| LauncherError::Unavailable)?;
    if !current_metadata.file_type().is_symlink()
        || fs::read_link(&current).map_err(|_| LauncherError::Unsafe)? != exact_target
        || fs::canonicalize(&current).map_err(|_| LauncherError::Unsafe)?
            != fs::canonicalize(&exact_target).map_err(|_| LauncherError::Unsafe)?
    {
        return Err(LauncherError::InvalidPackage);
    }

    let executable = fs::canonicalize(executable).map_err(|_| LauncherError::Unavailable)?;
    let executable_root = executable
        .parent()
        .and_then(Path::parent)
        .ok_or(LauncherError::Unsafe)?;
    if executable_root
        != fs::canonicalize(&exact_target).map_err(|_| LauncherError::InvalidPackage)?
    {
        return Err(LauncherError::InvalidPackage);
    }

    verify_installed_tree(&exact_target)?;
    let command = current.join(SIDECAR_RELATIVE);
    let resolved_command = fs::canonicalize(&command).map_err(|_| LauncherError::InvalidPackage)?;
    if resolved_command
        != fs::canonicalize(exact_target.join(SIDECAR_RELATIVE))
            .map_err(|_| LauncherError::InvalidPackage)?
    {
        return Err(LauncherError::InvalidPackage);
    }
    let file_digest = digest_executable_file(&resolved_command, MAX_PACKAGE_FILE_BYTES)?;
    let operations_command = current.join(OPERATIONS_RELATIVE);
    let resolved_operations =
        fs::canonicalize(&operations_command).map_err(|_| LauncherError::InvalidPackage)?;
    if resolved_operations
        != fs::canonicalize(exact_target.join(OPERATIONS_RELATIVE))
            .map_err(|_| LauncherError::InvalidPackage)?
    {
        return Err(LauncherError::InvalidPackage);
    }
    Ok(LauncherResolution {
        path_digest: digest_path(&command)?,
        file_digest,
        command,
        operations_command,
        package_root: exact_target,
        installed_data_root: Some(data_root.to_path_buf()),
    })
}

fn verify_installed_tree(package_root: &Path) -> Result<(), LauncherError> {
    let manifest_path = package_root.join(MANIFEST_NAME);
    let manifest = read_plain_bounded(&manifest_path, MAX_MANIFEST_BYTES)?;
    let value =
        serde_json::from_slice::<Value>(&manifest).map_err(|_| LauncherError::InvalidPackage)?;
    if value.get("package_version").and_then(Value::as_str) != Some(PRODUCT_VERSION) {
        return Err(LauncherError::InvalidPackage);
    }
    let manifest_digest = digest_bytes(&manifest);
    let owner = read_plain_bounded(&package_root.join(OWNERSHIP_MARKER), 128)?;
    if owner != format!("{}\n", manifest_digest.trim_start_matches("sha256:")).as_bytes() {
        return Err(LauncherError::InvalidPackage);
    }

    let checksums = read_plain_bounded(&package_root.join(CHECKSUMS_NAME), MAX_CHECKSUMS_BYTES)?;
    let checksums = parse_checksums(&checksums).ok_or(LauncherError::InvalidPackage)?;
    for required in [
        "VERSION",
        MANIFEST_NAME,
        "bin/godot-codex",
        SIDECAR_RELATIVE,
    ] {
        if !checksums.contains_key(required) {
            return Err(LauncherError::InvalidPackage);
        }
    }
    let mut total = 0_u64;
    for (relative, expected) in checksums {
        let path = package_root.join(relative);
        let bytes = if matches!(relative, "bin/godot-codex" | SIDECAR_RELATIVE) {
            read_executable_bounded(&path, MAX_PACKAGE_FILE_BYTES)?
        } else {
            read_plain_bounded(&path, MAX_PACKAGE_FILE_BYTES)?
        };
        total = total.saturating_add(bytes.len() as u64);
        if total > MAX_PACKAGE_BYTES
            || digest_bytes(&bytes).trim_start_matches("sha256:") != expected
        {
            return Err(LauncherError::InvalidPackage);
        }
    }
    let version = read_plain_bounded(&package_root.join("VERSION"), 128)?;
    if version != format!("{PRODUCT_VERSION}\n").as_bytes() {
        return Err(LauncherError::InvalidPackage);
    }
    Ok(())
}

fn parse_checksums(bytes: &[u8]) -> Option<BTreeMap<&str, &str>> {
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let (digest, relative) = line.split_once("  ")?;
        if !valid_raw_sha256(digest)
            || !safe_relative_path(relative)
            || values.insert(relative, digest).is_some()
            || values.len() > MAX_PACKAGE_FILES
        {
            return None;
        }
    }
    Some(values)
}

fn safe_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && value.len() <= MAX_PATH_BYTES
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn safe_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && !path.as_os_str().is_empty()
        && path.as_os_str().as_encoded_bytes().len() <= MAX_PATH_BYTES
        && path.components().all(|component| {
            matches!(
                component,
                Component::RootDir | Component::Prefix(_) | Component::Normal(_)
            )
        })
}

fn plain_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_dir())
}

pub(crate) fn read_plain_bounded(path: &Path, max: u64) -> Result<Vec<u8>, LauncherError> {
    read_opened_bounded(path, max, false)
}

fn read_executable_bounded(path: &Path, max: u64) -> Result<Vec<u8>, LauncherError> {
    read_opened_bounded(path, max, true)
}

#[cfg(unix)]
fn read_opened_bounded(
    path: &Path,
    max: u64,
    require_executable: bool,
) -> Result<Vec<u8>, LauncherError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| LauncherError::Unavailable)?;
    let mut file = File::from(descriptor);
    let before = file.metadata().map_err(|_| LauncherError::Unavailable)?;
    if !before.is_file()
        || before.len() > max
        || (require_executable && before.permissions().mode() & 0o111 == 0)
    {
        return Err(LauncherError::Unsafe);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(before.len()).unwrap_or(0));
    (&mut file)
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| LauncherError::Unavailable)?;
    let after = file.metadata().map_err(|_| LauncherError::Unavailable)?;
    if bytes.len() as u64 > max
        || bytes.len() as u64 != before.len()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mode() != after.mode()
    {
        return Err(LauncherError::InvalidPackage);
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_opened_bounded(
    path: &Path,
    max: u64,
    require_executable: bool,
) -> Result<Vec<u8>, LauncherError> {
    let before = fs::symlink_metadata(path).map_err(|_| LauncherError::Unavailable)?;
    if before.file_type().is_symlink()
        || !before.is_file()
        || before.len() > max
        || (require_executable && !is_executable(path))
    {
        return Err(LauncherError::Unsafe);
    }
    let mut file = File::open(path).map_err(|_| LauncherError::Unavailable)?;
    let opened = file.metadata().map_err(|_| LauncherError::Unavailable)?;
    let mut bytes = Vec::with_capacity(usize::try_from(opened.len()).unwrap_or(0));
    (&mut file)
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| LauncherError::Unavailable)?;
    let after = file.metadata().map_err(|_| LauncherError::Unavailable)?;
    if bytes.len() as u64 > max || bytes.len() as u64 != opened.len() || opened.len() != after.len()
    {
        return Err(LauncherError::InvalidPackage);
    }
    Ok(bytes)
}

fn digest_executable_file(path: &Path, max: u64) -> Result<String, LauncherError> {
    Ok(digest_bytes(&read_executable_bounded(path, max)?))
}

fn digest_path(path: &Path) -> Result<String, LauncherError> {
    let value = path.to_str().ok_or(LauncherError::Unsafe)?;
    Ok(digest_bytes(value.as_bytes()))
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn valid_raw_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_file())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn write_executable(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn unpacked_launcher_is_absolute_exact_and_digest_bound() {
        let package = TempDir::new().unwrap();
        fs::write(
            package.path().join(MANIFEST_NAME),
            format!(r#"{{"package_version":"{PRODUCT_VERSION}"}}"#),
        )
        .unwrap();
        write_executable(
            &package.path().join(SIDECAR_RELATIVE),
            b"exact-unpacked-sidecar",
        );
        write_executable(
            &package.path().join(OPERATIONS_RELATIVE),
            b"exact-unpacked-operations",
        );
        let resolved = resolve_unpacked_launcher(package.path()).unwrap();
        assert!(resolved.command().is_absolute());
        assert_eq!(
            resolved.command(),
            fs::canonicalize(package.path())
                .unwrap()
                .join(SIDECAR_RELATIVE)
        );
        assert!(resolved.path_digest().starts_with("sha256:"));
        assert!(resolved.file_digest().starts_with("sha256:"));
    }

    #[cfg(unix)]
    #[test]
    fn unpacked_launcher_rejects_a_symlink_even_when_its_target_is_executable() {
        use std::os::unix::fs::symlink;

        let package = TempDir::new().unwrap();
        fs::write(
            package.path().join(MANIFEST_NAME),
            format!(r#"{{"package_version":"{PRODUCT_VERSION}"}}"#),
        )
        .unwrap();
        let actual = package.path().join("actual-sidecar");
        write_executable(&actual, b"sidecar");
        write_executable(
            &package.path().join(OPERATIONS_RELATIVE),
            b"exact-unpacked-operations",
        );
        fs::create_dir_all(package.path().join("bin")).unwrap();
        symlink(&actual, package.path().join(SIDECAR_RELATIVE)).unwrap();
        assert!(matches!(
            resolve_unpacked_launcher(package.path()),
            Err(LauncherError::Unavailable | LauncherError::Unsafe)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn installed_launcher_requires_exact_current_target_and_owned_tree() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let data_root = temp.path().join("GodotCodex");
        let target = data_root.join("versions").join(PRODUCT_VERSION);
        fs::create_dir_all(&target).unwrap();
        let manifest = format!(r#"{{"package_version":"{PRODUCT_VERSION}"}}"#).into_bytes();
        let files = [
            ("VERSION", format!("{PRODUCT_VERSION}\n").into_bytes()),
            (MANIFEST_NAME, manifest.clone()),
            ("bin/godot-codex", b"operations".to_vec()),
            (SIDECAR_RELATIVE, b"sidecar".to_vec()),
        ];
        let mut checksums = String::new();
        for (relative, bytes) in files {
            let path = target.join(relative);
            if relative.starts_with("bin/") {
                write_executable(&path, &bytes);
            } else {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, &bytes).unwrap();
            }
            checksums.push_str(&format!(
                "{}  {relative}\n",
                digest_bytes(&bytes).trim_start_matches("sha256:")
            ));
        }
        fs::write(target.join(CHECKSUMS_NAME), checksums).unwrap();
        fs::write(
            target.join(OWNERSHIP_MARKER),
            format!(
                "{}\n",
                digest_bytes(&manifest).trim_start_matches("sha256:")
            ),
        )
        .unwrap();
        symlink(&target, data_root.join("current")).unwrap();

        // Even though current_exe resolves inside a directory containing a
        // manifest, the normal installed path must not downgrade to the
        // weaker explicit-unpacked resolver.
        let resolved =
            resolve_launcher_from(None, &target.join("bin/godot-codex"), Some(&data_root)).unwrap();
        assert_eq!(
            resolved.command(),
            data_root.join("current").join(SIDECAR_RELATIVE)
        );
        assert_eq!(resolved.installed_data_root(), Some(data_root.as_path()));

        fs::remove_file(data_root.join("current")).unwrap();
        let wrong = data_root.join("versions/0.0.0");
        fs::create_dir(&wrong).unwrap();
        symlink(wrong, data_root.join("current")).unwrap();
        assert_eq!(
            resolve_launcher_from(None, &target.join("bin/godot-codex"), Some(&data_root),).err(),
            Some(LauncherError::InvalidPackage)
        );
    }

    #[test]
    fn installed_data_root_is_derived_from_the_exact_versioned_executable() {
        let executable = Path::new("/private/custom/GodotCodex")
            .join("versions")
            .join(PRODUCT_VERSION)
            .join("bin/godot-codex-mcp");
        assert_eq!(
            installed_data_root_from_executable(&executable),
            Some(PathBuf::from("/private/custom/GodotCodex"))
        );
        assert_eq!(
            installed_data_root_from_executable(Path::new(
                "/private/custom/GodotCodex/current/bin/godot-codex-mcp"
            )),
            None
        );
    }
}
