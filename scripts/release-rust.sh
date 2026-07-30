#!/usr/bin/env bash
# release-rust.sh — the shared pipeline for a Rust-workspace repo, optionally
# with a Tauri launcher alongside it.
#
# A repo's own scripts/release-local.sh sets a handful of variables and sources
# this; nothing repo-specific belongs in here. See srt-router for the reference
# caller.
#
# Required before sourcing:
#   RR_NAME       "SRT Router"                human-readable product name
#   RR_SLUG       srt-router                  artefact filename stem
#   RR_IDENT      com.stoatworks.srt-router   pkg/bundle identifier
#
# Optional:
#   RR_EXTRA_FILES  ("README.md" "LICENSE")   copied into every archive
#   RR_EXTRA_DIRS   ("config" "docs")         copied into every archive
#   RR_PREBUILD     shell snippet run once before any target is compiled —
#                   use it to build a web console the server binary serves
#   RR_LAUNCHER     launcher                  Tauri subdir; empty = no launcher
#   RR_APP_NAME     "SRT Router.app"          bundle name the launcher produces
#   RR_SERVER_BIN   srtrouter                 bin the launcher embeds
#   RR_VERSION_FILES  extra files to rewrite the version in
#
# Flags handled here: --upload --no-vm --version X.Y.Z --skip <label>
set -euo pipefail

: "${RR_NAME:?set RR_NAME}"; : "${RR_SLUG:?set RR_SLUG}"; : "${RR_IDENT:?set RR_IDENT}"
RR_LAUNCHER="${RR_LAUNCHER:-}"
RR_APP_NAME="${RR_APP_NAME:-}"
RR_SERVER_BIN="${RR_SERVER_BIN:-}"
declare -a RR_EXTRA_FILES=("${RR_EXTRA_FILES[@]:-}")
declare -a RR_EXTRA_DIRS=("${RR_EXTRA_DIRS[@]:-}")

repo="$(cd "$(dirname "${BASH_SOURCE[1]}")/.." && pwd)"
cd "$repo"
source "$repo/scripts/release-lib.sh"

out="$repo/dist-release"
upload=0; use_vm=1; version=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --upload)  upload=1 ;;
    --no-vm)   use_vm=0 ;;
    --version) version="$2"; shift ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
  shift
done

# ------------------------------------------------------------- versioning ---

# Workspace repos keep the version under [workspace.package]; single-crate ones
# under [package]. Try the former, fall back to the latter.
current="$(awk '/^\[workspace.package\]/{f=1} f&&/^version = /{gsub(/[",]/,"",$3); print $3; exit}' Cargo.toml)"
[[ -z "$current" ]] && current="$(awk '/^\[package\]/{f=1} f&&/^version = /{gsub(/[",]/,"",$3); print $3; exit}' Cargo.toml)"
[[ -z "$current" ]] && { echo "cannot determine current version" >&2; exit 1; }

if [[ -z "$version" ]]; then
  version="$(awk -F. '{printf "%d.%d.%d", $1, $2, $3+1}' <<<"$current")"
fi
tag="v${version}"
echo "==> ${RR_NAME} ${current} -> ${version}"

# Replace only the FIRST line matching a pattern.
#
# The obvious `sed -i '' '0,/re/s//new/'` is a GNU extension: BSD sed does not
# accept a 0 address and silently leaves the file untouched — no error, no exit
# code. That shipped artefacts named 0.1.1 containing a binary reporting 0.1.0,
# so this uses awk and then verifies the file actually changed.
rr_bump() { # rr_bump <file> <line-regex> <replacement-line>
  local file="$1" pat="$2" new="$3" tmp
  [[ -f "$file" ]] || return 0
  tmp="$(mktemp)"
  awk -v pat="$pat" -v new="$new" '
    !done && $0 ~ pat { print new; done = 1; next }
    { print }
  ' "$file" >"$tmp"
  if cmp -s "$file" "$tmp"; then
    rm -f "$tmp"
    echo "    warning: no version line matched in ${file}" >&2
    return 0
  fi
  mv "$tmp" "$file"
  echo "    bumped ${file}"
}

rr_bump Cargo.toml "^version = \"${current}\"" "version = \"${version}\""
if [[ -n "$RR_LAUNCHER" ]]; then
  rr_bump "$RR_LAUNCHER/package.json"              "^  \"version\": \"${current}\"," "  \"version\": \"${version}\","
  rr_bump "$RR_LAUNCHER/src-tauri/tauri.conf.json" "^  \"version\": \"${current}\"," "  \"version\": \"${version}\","
  rr_bump "$RR_LAUNCHER/src-tauri/Cargo.toml"      "^version = \"${current}\""       "version = \"${version}\""
fi
for f in "${RR_VERSION_FILES[@]:-}"; do
  [[ -n "$f" ]] && rr_bump "$f" "${current}" "${version}"
done
cargo update -w >/dev/null 2>&1 || true

# The version is what users see in About boxes and bug reports; a mismatch
# between it and the artefact name is invisible until someone files a bug
# against the wrong release. Fail loudly instead.
check="$(awk '/^\[workspace.package\]/{f=1} f&&/^version = /{gsub(/[",]/,"",$3); print $3; exit}' Cargo.toml)"
[[ -z "$check" ]] && check="$(awk '/^\[package\]/{f=1} f&&/^version = /{gsub(/[",]/,"",$3); print $3; exit}' Cargo.toml)"
[[ "$check" == "$version" ]] \
  || { echo "version bump failed: Cargo.toml says ${check}, expected ${version}" >&2; exit 1; }

rl_init "$RR_NAME" "$RR_SLUG" "$version" "$RR_IDENT" "$out"
rm -rf "$out"; mkdir -p "$out"

# ------------------------------------------------------------- prebuild ----

if [[ -n "${RR_PREBUILD:-}" ]]; then
  echo "==> prebuild"
  eval "$RR_PREBUILD"
fi

# ------------------------------------------------------- server binaries ----

rr_stage() { # rr_stage <stagedir> <target> <ext>
  local stage="$1" target="$2" ext="${3:-}" f d b
  rm -rf "$stage"; mkdir -p "$stage"
  # Every bin the workspace produces, not a hand-maintained list.
  while IFS= read -r b; do
    [[ -f "target/$target/release/${b}${ext}" ]] && cp "target/$target/release/${b}${ext}" "$stage/"
  done < <(cargo metadata --no-deps --format-version 1 2>/dev/null \
           | python3 -c "import json,sys
for p in json.load(sys.stdin)['packages']:
    for t in p['targets']:
        if 'bin' in t['kind']: print(t['name'])")
  for f in "${RR_EXTRA_FILES[@]}"; do [[ -n "$f" && -f "$f" ]] && cp "$f" "$stage/"; done
  for d in "${RR_EXTRA_DIRS[@]}"; do [[ -n "$d" && -d "$d" ]] && cp -R "$d" "$stage/"; done
  return 0
}

rr_build_target() { # rr_build_target <label> <target> <builder> <ext>
  local label="$1" target="$2" builder="$3" ext="${4:-}"
  echo "==> server ${label} (${target})"
  rustup target add "$target" >/dev/null 2>&1 || true
  case "$builder" in
    cargo)    cargo build --release --target "$target" --bins ;;
    zigbuild) cargo zigbuild --release --target "$target" --bins ;;
    xwin)     cargo xwin build --release --target "$target" --bins ;;
  esac
  local stage="$out/.stage-$label"
  rr_stage "$stage" "$target" "$ext"
  if [[ "$ext" == ".exe" ]]; then
    rl_zip  "$label" "$stage"
    rl_nsis "$label" "$stage" --cli
  else
    rl_targz "$label" "$stage"
  fi
  rm -rf "$stage"
}

rr_build_target macos-aarch64 aarch64-apple-darwin cargo
rr_build_target macos-x86_64  x86_64-apple-darwin  cargo

if command -v cargo-zigbuild >/dev/null 2>&1 && command -v zig >/dev/null 2>&1; then
  rr_build_target linux-x86_64  x86_64-unknown-linux-gnu  zigbuild
  rr_build_target linux-aarch64 aarch64-unknown-linux-gnu zigbuild
else
  rl_skip "Linux server builds (install zig + cargo-zigbuild)"
fi

if command -v cargo-xwin >/dev/null 2>&1; then
  rr_build_target windows-x86_64  x86_64-pc-windows-msvc  xwin .exe
  rr_build_target windows-aarch64 aarch64-pc-windows-msvc xwin .exe
else
  rl_skip "Windows server builds (cargo install cargo-xwin)"
fi

# macOS gets both shapes for the command-line payload: a .pkg that puts the
# binaries on PATH, and a .dmg for people who would rather mount and copy.
# There is no .app here — these are console tools — so the .dmg holds the plain
# payload rather than a bundle.
rr_stage "$out/.stage-srv-mac" aarch64-apple-darwin
rl_pkg macos-aarch64-cli "$out/.stage-srv-mac" --cli
rl_dmg macos-aarch64-cli "$out/.stage-srv-mac"
rm -rf "$out/.stage-srv-mac"
rr_stage "$out/.stage-srv-mac-x64" x86_64-apple-darwin
rl_pkg macos-x86_64-cli "$out/.stage-srv-mac-x64" --cli
rl_dmg macos-x86_64-cli "$out/.stage-srv-mac-x64"
rm -rf "$out/.stage-srv-mac-x64"

# ----------------------------------------------------------- Tauri launcher --

if [[ -n "$RR_LAUNCHER" ]]; then
  echo "==> launcher npm deps"
  npm --prefix "$RR_LAUNCHER" install --silent --no-audit --no-fund

  rr_launcher_mac() { # rr_launcher_mac <label> <target>
    local label="$1" target="$2"
    echo "==> launcher ${label}"
    if [[ -n "$RR_SERVER_BIN" ]]; then
      cargo build --release --target "$target" -p "$RR_SERVER_BIN" 2>/dev/null \
        || cargo build --release --target "$target" --bins
      mkdir -p "$RR_LAUNCHER/src-tauri/bin"
      cp "target/$target/release/$RR_SERVER_BIN" "$RR_LAUNCHER/src-tauri/bin/$RR_SERVER_BIN"
      chmod +x "$RR_LAUNCHER/src-tauri/bin/$RR_SERVER_BIN"
      [[ -f config/example.toml ]] && cp config/example.toml "$RR_LAUNCHER/src-tauri/bin/server-config.toml"
    fi
    # Only the .app: Tauri's own dmg step drives Finder over AppleScript and
    # fails without a logged-in GUI session. The image is built here instead.
    ( cd "$RR_LAUNCHER" && npx --no-install tauri build --target "$target" --bundles app ) || {
      rl_skip "launcher ${label} (tauri build failed)"; return 0; }

    local appdir="$RR_LAUNCHER/src-tauri/target/$target/release/bundle/macos"
    [[ -d "$appdir/$RR_APP_NAME" ]] || { rl_skip "launcher ${label} (no .app)"; return 0; }
    local stage="$out/.stage-app-$label"
    rm -rf "$stage"; mkdir -p "$stage"
    cp -R "$appdir/$RR_APP_NAME" "$stage/"
    rl_adhoc_sign "$stage/$RR_APP_NAME"
    rl_dmg "$label" "$stage" --app "$RR_APP_NAME"
    rl_pkg "$label" "$stage" --app "$RR_APP_NAME"
    rm -rf "$stage"
  }

  rr_launcher_mac macos-aarch64-app aarch64-apple-darwin
  rr_launcher_mac macos-x86_64-app  x86_64-apple-darwin

  # Windows: the guest produces the .exe, we build the installer here — Tauri's
  # own makensis is a 32-bit x86 binary that will not run in the ARM64 guest,
  # and packaging on the Mac keeps every project's installer identical anyway.
  if (( use_vm )) && command -v prlctl >/dev/null 2>&1 \
     && prlctl list -a 2>/dev/null | grep -q "running.*Windows 11"; then
    echo "==> launcher windows (Parallels VM)"
    product="${RR_APP_NAME%.app}"
    if bash "$repo/scripts/release-windows-vm.sh" \
         "$repo" "$RR_SLUG" "$version" "$out" "$RR_LAUNCHER" "$product"; then
      for label in aarch64 x86_64; do
        src="$out/win-${label}"
        if [[ -f "$src/${product}.exe" ]]; then
          wstage="$out/.stage-win-app-${label}"
          rm -rf "$wstage"; mkdir -p "$wstage"
          cp -R "$src"/* "$wstage/"
          for f in "${RR_EXTRA_FILES[@]}"; do [[ -n "$f" && -f "$f" ]] && cp "$f" "$wstage/"; done
          rl_zip  "windows-${label}-app" "$wstage"
          rl_nsis "windows-${label}-app" "$wstage" --gui "${product}.exe"
          rm -rf "$wstage"
        else
          rl_skip "Windows ${label} launcher (${product}.exe not produced)"
        fi
        rm -rf "$src"
      done
    else
      rl_skip "Windows launcher bundles (VM build failed)"
    fi
  else
    rl_skip "Windows launcher bundles (VM not running or --no-vm)"
  fi

  rl_skip "Linux launcher bundles (Tauri cannot cross-bundle; needs a Linux host)"
fi

# ------------------------------------------------------------------ done ----

rl_summary

if [[ -n "$RR_APP_NAME" ]]; then
  cat <<NOTE

    Nothing here is code-signed. On macOS users must run
      xattr -dr com.apple.quarantine "/Applications/${RR_APP_NAME}"
    after installing — approving the outer app does not unquarantine nested
    helper binaries, and Gatekeeper SIGKILLs those silently.
NOTE
else
  cat <<NOTE

    Nothing here is code-signed. These are command-line tools: the .pkg
    installs them under /usr/local/${RR_SLUG} and links them into
    /usr/local/bin. If macOS refuses to run one, clear the quarantine flag:
      xattr -dr com.apple.quarantine /usr/local/${RR_SLUG}
NOTE
fi

if (( upload )); then
  echo "==> tagging ${tag}"
  git add -A
  git commit -m "release: ${tag}" || true
  git tag -a "$tag" -m "${RR_NAME} ${version}" || true
  git push origin HEAD --tags
  gh release create "$tag" --title "${RR_NAME} ${version}" \
     --notes "Local build — GitHub Actions minutes are exhausted, so these artefacts were cut on a Mac. Unsigned: see the README for the macOS quarantine step." \
     "$out"/* \
    || gh release upload "$tag" "$out"/* --clobber
fi
