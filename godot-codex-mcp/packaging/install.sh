#!/bin/sh
set -eu

PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
umask 077

fail() {
  printf '%s\n' "godot-codex installer: $1" >&2
  exit 1
}

package_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
version_file="$package_root/VERSION"
checksums_file="$package_root/checksums.sha256"

[ -f "$version_file" ] || fail "VERSION is missing"
[ -f "$checksums_file" ] || fail "checksums.sha256 is missing"
version=$(sed -n '1p' "$version_file")
case "$version" in
  ''|*[!0-9A-Za-z.+-]*) fail "VERSION is invalid" ;;
esac

data_root=${GODOT_CODEX_DATA_ROOT:-"${HOME:?}/Library/Application Support/GodotCodex"}
bin_dir=${GODOT_CODEX_BIN_DIR:-"${HOME:?}/.local/bin"}
operation_lock=
stage=

cleanup() {
  if [ -n "$stage" ] && [ -d "$stage" ] && [ ! -L "$stage" ]; then
    find "$stage" -depth -delete 2>/dev/null || true
  fi
  if [ -n "$operation_lock" ] && [ -d "$operation_lock" ] &&
    [ ! -L "$operation_lock" ]; then
    rmdir -- "$operation_lock" 2>/dev/null || true
  fi
}

trap cleanup EXIT
trap 'cleanup; exit 1' HUP INT TERM

validate_root() {
  candidate=$1
  case "$candidate" in
    ''|'/'|'.'|'..'|*//*|*/|*'
'*) fail "unsafe managed root" ;;
  esac
  case "$candidate" in
    /*) ;;
    *) fail "managed roots must be absolute" ;;
  esac
  printf '%s\n' "$candidate" | LC_ALL=C awk '
    NR != 1 { exit 1 }
    {
      if ($0 ~ /[[:cntrl:]]/) exit 1
      component_count = split(substr($0, 2), components, "/")
      for (component_index = 1;
           component_index <= component_count;
           component_index++) {
        if (components[component_index] == "" ||
            components[component_index] == "." ||
            components[component_index] == "..") exit 1
      }
    }
    END {
      if (NR != 1) exit 1
    }
  ' || fail "unsafe managed root"

  probe=$candidate
  missing=
  while [ ! -e "$probe" ] && [ ! -L "$probe" ]; do
    component=${probe##*/}
    [ -n "$component" ] || fail "unsafe managed root"
    if [ -n "$missing" ]; then
      missing="$component/$missing"
    else
      missing=$component
    fi
    probe=${probe%/*}
    [ -n "$probe" ] || probe=/
  done
  [ -d "$probe" ] || fail "managed root ancestor is not a directory"

  ancestry=$probe
  while [ "$ancestry" != "/" ]; do
    if [ -L "$ancestry" ]; then
      # macOS exposes trusted system aliases such as /var -> /private/var.
      # Only root-owned ancestor links are accepted; a user-owned swap inside
      # the managed path still fails closed.
      [ "$(stat -f '%u' "$ancestry" 2>/dev/null || printf '%s' unsafe)" = "0" ] ||
        fail "managed root ancestry may not contain a user-owned symlink"
    fi
    ancestry=$(dirname -- "$ancestry")
  done

  canonical_parent=$(CDPATH= cd -- "$probe" && pwd -P) ||
    fail "managed root ancestor cannot be resolved"
  if [ -n "$missing" ]; then
    validated_root="${canonical_parent%/}/$missing"
  else
    validated_root=$canonical_parent
  fi
  canonical_home=$(CDPATH= cd -- "${HOME:?}" && pwd -P) ||
    fail "HOME cannot be resolved"
  case "$validated_root" in
    '/'|"$canonical_home") fail "unsafe managed root" ;;
  esac
}

validate_root "$data_root"
data_root=$validated_root
validate_root "$bin_dir"
bin_dir=$validated_root

versions_root="$data_root/versions"
target="$versions_root/$version"
current_link="$data_root/current"
previous_link="$data_root/previous"

require_direct_managed_version_target() {
  candidate=$1
  label=$2
  case "$candidate" in
    "$versions_root"/*) ;;
    *) fail "$label is not package-owned" ;;
  esac
  leaf=${candidate#"$versions_root"/}
  case "$leaf" in
    ''|'.'|'..'|*/*|*[!0-9A-Za-z.+-]*) fail "$label is not a direct managed version" ;;
  esac
  [ "$candidate" = "$versions_root/$leaf" ] ||
    fail "$label is not a direct managed version"
}

verify_existing_target_matches_source() {
  installed_tree=$1
  cmp -s "$package_root/package-manifest.json" \
    "$installed_tree/package-manifest.json" &&
    cmp -s "$package_root/checksums.sha256" \
      "$installed_tree/checksums.sha256" ||
    fail "existing version differs from source package"
}

verify_tree() {
  tree=$1
  ownership=${2:-source}
  [ -d "$tree" ] && [ ! -L "$tree" ] || fail "package tree is missing or unsafe"
  [ -f "$tree/checksums.sha256" ] && [ ! -L "$tree/checksums.sha256" ] ||
    fail "package checksum manifest is missing or unsafe"
  [ -z "$(find "$tree" -type l -print -quit)" ] ||
    fail "package tree contains a symlink"
  [ -z "$(find "$tree" ! -type d ! -type f -print -quit)" ] ||
    fail "package tree contains a non-regular entry"
  awk '
    {
      if (length($0) < 67 || substr($0, 65, 2) != "  ") exit 1
      digest = substr($0, 1, 64)
      path = substr($0, 67)
      if (digest ~ /[^0-9a-f]/ || path == "" || substr(path, 1, 1) == "/") exit 1
      count = split(path, parts, "/")
      for (part_index = 1; part_index <= count; part_index++) {
        if (parts[part_index] == "" || parts[part_index] == "." || parts[part_index] == "..") exit 1
      }
      if (seen[path]++) exit 1
      if (path == "VERSION") version = 1
      if (path == "package-manifest.json") manifest = 1
      if (path == "bin/godot-codex") operations = 1
      if (path == "bin/godot-codex-mcp") sidecar = 1
      if (path == "share/godot-codex/licenses/Godot-LICENSE.txt") godot_license = 1
      if (path == "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt") third_party_licenses = 1
    }
    END {
      if (!version || !manifest || !operations || !sidecar ||
          !godot_license || !third_party_licenses) exit 1
    }
  ' "$tree/checksums.sha256" ||
    fail "package checksum manifest is invalid"
  (
    CDPATH= cd -- "$tree"
    shasum -a 256 -c checksums.sha256 >/dev/null
  ) || fail "package checksum verification failed"
  expected_files=$(
    {
      awk '{ print substr($0, 67) }' "$tree/checksums.sha256"
      printf '%s\n' "checksums.sha256"
      [ "$ownership" != "installed" ] ||
        printf '%s\n' ".godot-codex-owned"
    } | LC_ALL=C sort
  )
  actual_files=$(
    (
      CDPATH= cd -- "$tree"
      find . -type f -print
    ) | sed 's#^\./##' | LC_ALL=C sort
  )
  [ "$actual_files" = "$expected_files" ] ||
    fail "package tree contains an unlisted or missing file"
  [ -x "$tree/bin/godot-codex" ] && [ -x "$tree/bin/godot-codex-mcp" ] ||
    fail "package executable mode is invalid"
  if [ "$ownership" = "installed" ]; then
    [ -f "$tree/.godot-codex-owned" ] && [ ! -L "$tree/.godot-codex-owned" ] ||
      fail "installed ownership marker is missing"
    expected_owner=$(shasum -a 256 "$tree/package-manifest.json" | awk '{print $1}')
    [ "$(sed -n '1p' "$tree/.godot-codex-owned")" = "$expected_owner" ] ||
      fail "installed ownership marker differs"
  fi
}

prepare_managed_roots() {
  mkdir -p -- "$data_root" "$versions_root" "$bin_dir"
  [ -d "$data_root" ] && [ ! -L "$data_root" ] ||
    fail "data root is unsafe"
  [ -d "$versions_root" ] && [ ! -L "$versions_root" ] ||
    fail "versions root is unsafe"
  [ -d "$bin_dir" ] && [ ! -L "$bin_dir" ] ||
    fail "binary root is unsafe"
  chmod 700 "$data_root" "$versions_root"
}

require_managed_roots() {
  [ -d "$data_root" ] && [ ! -L "$data_root" ] ||
    fail "no managed install root"
  [ -d "$versions_root" ] && [ ! -L "$versions_root" ] ||
    fail "no managed versions root"
  [ -d "$bin_dir" ] && [ ! -L "$bin_dir" ] ||
    fail "managed binary root is missing or unsafe"
}

acquire_operation_lock() {
  [ -z "$operation_lock" ] || fail "installer lock is already held"
  lock_candidate="$data_root/.installer-lock"
  mkdir -- "$lock_candidate" 2>/dev/null ||
    fail "another installer operation is in progress"
  operation_lock=$lock_candidate
}

require_install_space() {
  required_kb=$(du -sk "$package_root" | awk '{print $1}')
  available_kb=$(df -Pk "$versions_root" | awk 'NR == 2 {print $4}')
  case "$required_kb:$available_kb" in
    *[!0-9:]*|:|*:) fail "free-space probe failed" ;;
  esac
  [ "$available_kb" -ge "$required_kb" ] ||
    fail "insufficient free space for package install"
}

managed_link_or_absent() {
  link=$1
  expected=$2
  if [ -e "$link" ] || [ -L "$link" ]; then
    [ -L "$link" ] || fail "refusing to replace a foreign executable"
    [ "$(readlink "$link")" = "$expected" ] ||
      fail "refusing to replace a foreign symlink"
  fi
}

managed_link_required() {
  link=$1
  expected=$2
  [ -L "$link" ] || fail "managed executable link is missing"
  [ "$(readlink "$link")" = "$expected" ] ||
    fail "managed executable link differs"
  [ -x "$link" ] || fail "managed executable target is not executable"
}

link_atomically() {
  target_value=$1
  link_path=$2
  temporary="$link_path.tmp.$$"
  [ ! -e "$temporary" ] && [ ! -L "$temporary" ] ||
    fail "temporary link already exists"
  ln -s -- "$target_value" "$temporary"
  # BSD/macOS mv needs -h so a destination symlink to a directory is replaced
  # rather than treated as that directory.
  mv -fh -- "$temporary" "$link_path"
}

install_package() {
  verify_tree "$package_root" source
  prepare_managed_roots
  acquire_operation_lock
  require_install_space

  old=
  if [ -L "$current_link" ]; then
    old=$(readlink "$current_link")
    require_direct_managed_version_target "$old" "current link"
    verify_tree "$old" installed
  elif [ -e "$current_link" ]; then
    fail "current path is not a symlink"
  fi
  if [ -L "$previous_link" ]; then
    previous=$(readlink "$previous_link")
    require_direct_managed_version_target "$previous" "previous link"
    verify_tree "$previous" installed
  elif [ -e "$previous_link" ]; then
    fail "previous path is not a symlink"
  fi
  managed_link_or_absent "$bin_dir/godot-codex" "$current_link/bin/godot-codex"
  managed_link_or_absent "$bin_dir/godot-codex-mcp" "$current_link/bin/godot-codex-mcp"

  if [ -e "$target" ] || [ -L "$target" ]; then
    [ -d "$target" ] && [ ! -L "$target" ] ||
      fail "version target is not a managed directory"
    verify_tree "$target" installed
    verify_existing_target_matches_source "$target"
  else
    stage="$versions_root/.install-$version-$$"
    [ ! -e "$stage" ] && [ ! -L "$stage" ] ||
      fail "staging path already exists"
    mkdir -- "$stage"
    (
      CDPATH= cd -- "$package_root"
      tar -cpf - .
    ) | (
      CDPATH= cd -- "$stage"
      tar -xpf -
    )
    verify_tree "$stage" source
    printf '%s\n' "$(shasum -a 256 "$stage/package-manifest.json" | awk '{print $1}')" \
      > "$stage/.godot-codex-owned"
    chmod 600 "$stage/.godot-codex-owned"
    verify_tree "$stage" installed
    mv -- "$stage" "$target"
    stage=
  fi

  if [ -n "$old" ] && [ "$old" != "$target" ]; then
    link_atomically "$old" "$previous_link"
  fi
  link_atomically "$target" "$current_link"
  link_atomically "$current_link/bin/godot-codex" "$bin_dir/godot-codex"
  link_atomically "$current_link/bin/godot-codex-mcp" "$bin_dir/godot-codex-mcp"
  printf '%s\n' "installed godot-codex $version"
}

rollback_package() {
  require_managed_roots
  acquire_operation_lock
  [ -L "$current_link" ] || fail "no current managed version"
  [ -L "$previous_link" ] || fail "no previous managed version"
  current=$(readlink "$current_link")
  previous=$(readlink "$previous_link")
  require_direct_managed_version_target "$current" "current link"
  require_direct_managed_version_target "$previous" "previous link"
  verify_tree "$current" installed
  verify_tree "$previous" installed
  link_atomically "$previous" "$current_link"
  link_atomically "$current" "$previous_link"
  printf '%s\n' "rolled back godot-codex"
}

verify_install() {
  require_managed_roots
  acquire_operation_lock
  [ -L "$current_link" ] || fail "no current managed version"
  current=$(readlink "$current_link")
  require_direct_managed_version_target "$current" "current link"
  verify_tree "$current" installed
  managed_link_required "$bin_dir/godot-codex" "$current_link/bin/godot-codex"
  managed_link_required "$bin_dir/godot-codex-mcp" "$current_link/bin/godot-codex-mcp"
  [ -x "$current_link/bin/godot-codex" ] &&
    [ -x "$current_link/bin/godot-codex-mcp" ] ||
    fail "stable current launcher path is not executable"
  printf '%s\n' "verified godot-codex"
}

uninstall_package() {
  require_managed_roots
  acquire_operation_lock
  if [ -d "$versions_root" ] && [ ! -L "$versions_root" ]; then
    for entry in "$versions_root"/*; do
      [ -e "$entry" ] || continue
      [ -d "$entry" ] && [ ! -L "$entry" ] &&
        [ -f "$entry/.godot-codex-owned" ] ||
        fail "refusing to remove a foreign version entry"
      verify_tree "$entry" installed
    done
  fi
  # Complete every ownership/type check before removing the first path.
  for name in godot-codex godot-codex-mcp; do
    link="$bin_dir/$name"
    if [ -L "$link" ] && [ "$(readlink "$link")" = "$current_link/bin/$name" ]; then
      :
    elif [ -e "$link" ] || [ -L "$link" ]; then
      fail "refusing to remove a foreign executable"
    fi
  done
  [ ! -e "$current_link" ] || [ -L "$current_link" ] ||
    fail "current path is not package-owned"
  [ ! -e "$previous_link" ] || [ -L "$previous_link" ] ||
    fail "previous path is not package-owned"
  if [ -L "$current_link" ]; then
    current=$(readlink "$current_link")
    require_direct_managed_version_target "$current" "current link"
  fi
  if [ -L "$previous_link" ]; then
    previous=$(readlink "$previous_link")
    require_direct_managed_version_target "$previous" "previous link"
  fi

  for name in godot-codex godot-codex-mcp; do
    link="$bin_dir/$name"
    [ ! -L "$link" ] || rm -- "$link"
  done
  [ ! -L "$current_link" ] || rm -- "$current_link"
  [ ! -L "$previous_link" ] || rm -- "$previous_link"

  if [ -d "$versions_root" ] && [ ! -L "$versions_root" ]; then
    for entry in "$versions_root"/*; do
      [ -e "$entry" ] || continue
      verify_tree "$entry" installed
      case "$entry" in
        "$versions_root"/*) find "$entry" -depth -delete ;;
        *) fail "unsafe version entry" ;;
      esac
    done
    rmdir -- "$versions_root" 2>/dev/null || true
  fi
  rmdir -- "$operation_lock" 2>/dev/null ||
    fail "installer lock could not be released"
  operation_lock=
  rmdir -- "$data_root" 2>/dev/null || true
  rmdir -- "$bin_dir" 2>/dev/null || true
  printf '%s\n' "uninstalled godot-codex"
}

case ${1:-} in
  install) install_package ;;
  rollback) rollback_package ;;
  verify) verify_install ;;
  uninstall) uninstall_package ;;
  *) fail "usage: install.sh install|verify|rollback|uninstall" ;;
esac
