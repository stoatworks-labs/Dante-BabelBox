#!/usr/bin/env bash
# release-lib.sh — shared local packaging helpers for the stoatworks fleet.
#
# GitHub Actions minutes are exhausted, so every release is cut on this Mac.
# This library turns a *staging directory* (a plain folder holding whatever the
# user should end up with) into the per-platform artefacts we publish:
#
#   Windows   portable .zip           + NSIS .exe installer
#   macOS     .dmg                    + .pkg installer
#   Linux     .tar.gz                 (+ .deb where a GUI app exists)
#
# The Windows installer is built with makensis, which runs natively on macOS —
# no Wine. The macOS installer uses pkgbuild/productbuild and the disk image
# uses create-dmg, both of which are host tools, so mac artefacts must be cut
# on a Mac (they are).
#
#   brew install makensis create-dmg
#
# macOS artefacts are Developer ID-signed AND notarised when the RL_MAC_* /
# RL_NOTARY_PROFILE variables are set (normally via
# ~/.config/stoatworks/release-signing.env, which rl_init sources) — see the
# macOS signing section. Unset, bundles fall back to ad-hoc signing: an
# entirely unsigned bundle produces "is damaged", and approving the outer app
# does NOT unquarantine nested helper binaries — they get SIGKILLed silently.
# Only the notarised path removes the quarantine prompt altogether; the ad-hoc
# fallback still needs the documented `xattr -dr com.apple.quarantine` step.
#
# Windows artefacts ARE Authenticode-signed when the RL_SIGN_* variables are
# set — see the Windows signing section. Unset, they skip rather than fail, so
# an unconfigured host still cuts a valid (unsigned) release.
#
# Usage:
#   source .../release-lib.sh
#   rl_init "SRT Router" srt-router 0.1.1 com.stoatworks.srt-router "$outdir"
#   rl_zip     windows-x86_64 "$stage"
#   rl_nsis    windows-x86_64 "$stage" --cli          # or --gui SRTRouter.exe
#   rl_targz   linux-x86_64   "$stage"
#   rl_pkg     macos-arm64    "$stage" --cli          # or --app "SRT Router.app"
#   rl_dmg     macos-arm64    "$stage"
set -euo pipefail

RL_NAME=""      # human name, e.g. "SRT Router"
RL_SLUG=""      # file-safe name, e.g. srt-router
RL_VERSION=""
RL_IDENT=""     # reverse-DNS bundle/package identifier
RL_OUT=""       # directory artefacts land in
RL_PUBLISHER="${RL_PUBLISHER:-Stoatworks Labs}"
RL_URL="${RL_URL:-https://github.com/stoatworks-labs}"
RL_SKIPPED=()

# Captured at source time: the vendored copy lives in each repo's scripts/, so
# this is where per-repo signing extras (mac-entitlements.plist) are looked up.
RL_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

rl_init() {
  RL_NAME="$1"; RL_SLUG="$2"; RL_VERSION="$3"; RL_IDENT="$4"; RL_OUT="$5"
  mkdir -p "$RL_OUT"

  # Machine-local signing configuration. Deliberately a dotfile and not a
  # repo file: identity names and the notary profile belong to this Mac, not
  # to (mostly public) repos, and CI hosts simply lack the file and skip.
  local cfg="$HOME/.config/stoatworks/release-signing.env"
  [[ -f "$cfg" ]] && source "$cfg"

  # Tauri signs its own bundles when this is exported, which covers the DMGs
  # it builds before any rl_* helper ever sees them.
  if [[ -n "${RL_MAC_SIGN_IDENTITY:-}" && -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
    export APPLE_SIGNING_IDENTITY="$RL_MAC_SIGN_IDENTITY"
  fi

  # A repo that needs hardened-runtime exceptions drops the file next to the
  # vendored lib; nothing to wire in the caller.
  if [[ -z "${RL_MAC_ENTITLEMENTS:-}" && -f "$RL_LIB_DIR/mac-entitlements.plist" ]]; then
    RL_MAC_ENTITLEMENTS="$RL_LIB_DIR/mac-entitlements.plist"
  fi
}

rl_note()  { printf '    %s\n' "$*"; }
rl_step()  { printf '==> %s\n' "$*"; }
rl_skip()  { RL_SKIPPED+=("$1"); printf '    skipped: %s\n' "$1"; }

# NSIS wants a 4-part numeric version (1.2.3 -> 1.2.3.0) for VIProductVersion.
rl_numver() {
  local v="${RL_VERSION%%-*}" n
  n=$(awk -F. '{printf "%d.%d.%d.0", $1, ($2==""?0:$2), ($3==""?0:$3)}' <<<"$v")
  printf '%s' "$n"
}

# --------------------------------------------------------------------- NDI --
#
# Shipping the NDI runtime inside an installer.
#
# NDI's licence permits this — the SDK is royalty-free and Vizrt's Software
# Distribution page explicitly allows both shipping the libraries inside an
# application folder and bundling the redistributable installer. The condition
# is that the licence *you* distribute under must forbid modifying,
# reverse-engineering, disassembling and decompiling the SDK.
#
# That condition is why the source repos cannot carry NDI binaries: MIT grants
# exactly those rights. An installer is different — it has its own EULA, which
# `rl_eula` generates with the required terms, and which `rl_nsis`/`rl_pkg`
# then present. So this is the one place in the fleet where the runtime may
# legitimately be shipped.
#
# Two obligations come with it, and neither is automatic:
#   * the bundled version must be kept current
#   * NDI Tools must NOT be redistributed (link to https://ndi.video/tools)
#
# Opt-in per project, never on by default: a project that calls neither
# function ships exactly as it did before.
#
#   rl_ndi_bundle windows-x86_64 "$stage"                  # beside the .exe
#   rl_ndi_bundle macos-arm64    "$stage" --app "Foo.app"  # Contents/Frameworks
#
# The runtime for a target usually is not on the release host (a Mac cutting
# Linux and Windows builds), so each target's directory is named explicitly:
#
#   RL_NDI_DIR_MACOS_ARM64, RL_NDI_DIR_WINDOWS_X86_64,
#   RL_NDI_DIR_LINUX_X86_64, ...   (uppercased label, '-' -> '_')
#
# macOS falls back to the locally installed SDK. A target with nothing set is
# skipped, not failed — a release without NDI bundled is still a valid release,
# because every app in the fleet loads the runtime dynamically and degrades to
# "NDI unavailable, here is the download" when it is absent.

RL_NDI_BUNDLED=0        # set by rl_ndi_bundle; read by rl_eula
RL_NDI_REDIST_URL="${RL_NDI_REDIST_URL:-https://ndi.video/for-developers/ndi-sdk/}"

# The filename the application's loader looks for. This is what the runtime is
# installed *as*, which is not always what the SDK ships it as — see
# rl_ndi_srcfile.
rl_ndi_libname() { # rl_ndi_libname <label>
  case "$1" in
    macos-*)          printf 'libndi.dylib' ;;
    windows-aarch64)  printf '' ;;   # see below
    windows-*)        printf 'Processing.NDI.Lib.x64.dll' ;;
    *)                printf 'libndi.so.6' ;;
  esac
}

# The file to copy *from*, which may be named differently.
#
# The Linux SDK ships `libndi.so.6.3.2` and no `libndi.so.6` symlink, so match
# the versioned name too and install it under the name the loader wants. This is
# safe because the library is opened by path with dlopen, where the filename
# need not match the SONAME.
rl_ndi_srcfile() { # rl_ndi_srcfile <dir> <libname>
  local dir="$1" lib="$2" f
  if [[ -f "$dir/$lib" ]]; then printf '%s' "$dir/$lib"; return 0; fi
  if [[ "$lib" == libndi.so.6 ]]; then
    for f in "$dir"/libndi.so.6.*; do
      [[ -f "$f" ]] && { printf '%s' "$f"; return 0; }
    done
  fi
  printf ''
}

# Directory holding the runtime for a target label, or empty.
rl_ndi_srcdir() { # rl_ndi_srcdir <label>
  local label="$1" var dir
  var="RL_NDI_DIR_$(printf '%s' "$label" | tr '[:lower:]-' '[:upper:]_')"
  dir="${!var:-}"
  if [[ -n "$dir" ]]; then printf '%s' "$dir"; return 0; fi
  # Only the host's own platform can be discovered locally.
  if [[ "$label" == macos-* && "$(uname -s)" == "Darwin" ]]; then
    for dir in "/Library/NDI SDK for Apple/lib/macOS" \
               "/Library/NDI SDK for macOS/lib/macOS" \
               "/usr/local/lib" "/opt/homebrew/lib"; do
      [[ -f "$dir/libndi.dylib" ]] && { printf '%s' "$dir"; return 0; }
    done
  fi
  printf ''
}

rl_ndi_bundle() { # rl_ndi_bundle <label> <stagedir> [--app <BundleName>]
  local label="$1" stage="$2" mode="${3:-}" appname="${4:-}"
  local lib src srcfile dest
  lib="$(rl_ndi_libname "$label")"

  # Vizrt ships no ARM64 Windows runtime — the NDI 6 redistributable contains
  # x64 and x86 only. A native ARM64 process cannot load either, so there is
  # nothing to bundle and nothing the operator could install to fix it. Skipping
  # is the honest outcome; staging the x64 DLL would look right and never load.
  if [[ -z "$lib" ]]; then
    rl_skip "${label} NDI runtime (Vizrt ships no ARM64 Windows runtime)"
    return 0
  fi

  src="$(rl_ndi_srcdir "$label")"
  srcfile="$([[ -n "$src" ]] && rl_ndi_srcfile "$src" "$lib")"

  if [[ -z "$srcfile" ]]; then
    rl_skip "${label} NDI runtime (set RL_NDI_DIR_$(printf '%s' "$label" | tr '[:lower:]-' '[:upper:]_') to a directory containing ${lib})"
    return 0
  fi

  rl_step "ndi  ${label}"
  # Where the app's own loader looks first, in every implementation in the
  # fleet: Contents/Frameworks inside a bundle, otherwise beside the binary.
  #
  # The licence text goes somewhere else inside a bundle. `codesign` treats
  # every entry in Contents/Frameworks as a subcomponent to sign, and a plain
  # .txt is not code, so leaving it there fails the whole bundle with "code
  # object is not signed at all". Contents/Resources is where non-code belongs.
  local notice_dest
  if [[ "$mode" == "--app" ]]; then
    dest="$stage/$appname/Contents/Frameworks"
    notice_dest="$stage/$appname/Contents/Resources"
  else
    dest="$stage"
    notice_dest="$stage"
  fi
  mkdir -p "$dest" "$notice_dest"
  cp "$srcfile" "$dest/$lib"

  # Vizrt requires the runtime licence text to travel with the binary.
  local notice
  for notice in "$src/../../licenses/libndi_licenses.txt" \
                "$src/libndi_licenses.txt" \
                "$src/../licenses/libndi_licenses.txt"; do
    if [[ -f "$notice" ]]; then
      cp "$notice" "$notice_dest/libndi_licenses.txt"; break
    fi
  done

  RL_NDI_BUNDLED=1
  local shown="${dest#"$stage"}"; shown="${shown#/}"
  rl_note "${lib} -> ${shown:-<stage root>}"
}

# ------------------------------------------------------------------- EULA ---
#
# Writes the licence text an installer presents, and echoes its path. The
# project's own LICENSE, plus — when rl_ndi_bundle has run — the terms Vizrt
# requires a redistributor to impose. Call *after* rl_ndi_bundle.
#
# Returns empty (and writes nothing) when there is no project LICENSE and no
# NDI to cover, so installers keep their current no-licence-page behaviour.

rl_eula() { # rl_eula [<path-to-project-LICENSE>]
  local license="${1:-}" out
  [[ -z "$license" || ! -f "$license" ]] && license=""
  if [[ -z "$license" && $RL_NDI_BUNDLED -eq 0 ]]; then printf ''; return 0; fi

  out="$(mktemp -t rl_eula).txt"
  {
    printf '%s %s\n\n' "$RL_NAME" "$RL_VERSION"
    if [[ -n "$license" ]]; then cat "$license"; printf '\n'; fi
    if (( RL_NDI_BUNDLED )); then
      cat <<'NDIEULA'

--------------------------------------------------------------------------
NDI(R) RUNTIME - ADDITIONAL TERMS
--------------------------------------------------------------------------

This installer includes the NDI(R) runtime library, redistributed under
licence from Vizrt NDI AB. NDI(R) is a registered trademark of Vizrt NDI AB.
See https://ndi.video.

By installing this software you agree that, with respect to the NDI SDK and
the NDI runtime library included here, you will NOT:

  * modify the SDK or any part of it;
  * reverse engineer, disassemble or decompile the SDK or any NDI product,
    or any part thereof;
  * circumvent any technical limitations in the SDK.

These restrictions apply to the NDI components only. They do not restrict
your rights in the rest of this software, which remain governed by its own
licence above.

NDI Tools are not included with, and are not redistributed by, this
installer. Obtain them from https://ndi.video/tools.

H.264, H.265 and AAC are separately licensable formats not covered by the
NDI SDK grant.
NDIEULA
    fi
  } >"$out"
  printf '%s' "$out"
}

# ---------------------------------------------------------------- archives --

rl_zip() {   # rl_zip <label> <stagedir>
  local label="$1" stage="$2" f="$RL_OUT/${RL_SLUG}-${RL_VERSION}-${1}.zip"
  rl_step "zip  ${label}"
  rm -f "$f"; ( cd "$stage" && zip -qr "$f" . )
  rl_note "$(basename "$f")"
}

rl_targz() { # rl_targz <label> <stagedir>
  local label="$1" stage="$2" f="$RL_OUT/${RL_SLUG}-${RL_VERSION}-${1}.tar.gz"
  rl_step "tar  ${label}"
  rm -f "$f"; ( cd "$stage" && tar czf "$f" . )
  rl_note "$(basename "$f")"
}

# -------------------------------------------------------- Windows signing ---
#
# Authenticode signing via Azure Artifact Signing, driven from the Mac by jsign.
#
#   brew install jsign
#
# Why jsign and not signtool: since June 2023 the CA/Browser Forum baseline
# requires every publicly-trusted code-signing key to live on FIPS 140-2 L2
# hardware, so there is no .pfx to hand to signtool and no key that could sit
# in a GitHub secret. Artifact Signing keeps the key in Microsoft's HSM and
# mints a fresh certificate per signature; jsign speaks that protocol over
# HTTPS and runs anywhere, which is what lets the whole fleet stay on this Mac
# instead of moving packaging into the Parallels guest.
#
# TIMESTAMPING IS NOT OPTIONAL HERE. An Artifact Signing certificate is valid for
# 72 hours. Without a countersignature from a TSA the signature is judged
# against wall-clock time, so an unstamped installer verifies fine on the day
# it is cut and is broken by the weekend — and nothing in the build tells you,
# because signing itself succeeded. Every path below stamps.
#
# Configuration (all required; unset means "skip signing", never "fail"):
#
#   RL_SIGN_ENDPOINT   regional endpoint, e.g. https://eus.codesigning.azure.net
#   RL_SIGN_ACCOUNT    Artifact Signing account name
#   RL_SIGN_PROFILE    certificate profile name
#
# Renamed from "Trusted Signing" in 2026; both names still appear in the wild.
# ELIGIBILITY: organizations only in the UK/EU; individual developers must be
# in the US or Canada. A UK sole trader qualifies under neither.

#   AZURE_TENANT_ID    service principal, as for any Azure SDK client
#   AZURE_CLIENT_ID
#   AZURE_CLIENT_SECRET
#
# Keep the secret in the keychain and wrap invocation the way cf-run does for
# Cloudflare, rather than exporting it from a dotfile. Store the *client
# secret* only — never a cached access token: tokens are multi-kilobyte JWTs
# and `security add-generic-password -w` silently truncates at 128 bytes, so a
# stored token comes back corrupted with no error. Tokens are cheap; fetch one
# per release.

RL_SIGN_TSA="${RL_SIGN_TSA:-http://timestamp.acs.microsoft.com}"
RL_SIGNED_COUNT=0

# Are we configured to sign? Quiet predicate — callers decide how to report.
rl_sign_ready() {
  [[ -n "${RL_SIGN_ENDPOINT:-}" && -n "${RL_SIGN_ACCOUNT:-}" && -n "${RL_SIGN_PROFILE:-}" ]] \
    && command -v jsign >/dev/null 2>&1
}

# Artifact Signing authenticates with a bearer token for the code-signing
# resource. jsign will shell out to `az` itself, but only if the Azure CLI is
# installed and logged in interactively — no use in an unattended release. So
# mint the token directly from the service principal and cache it for this
# process: tokens last an hour, a fleet release takes minutes, and re-fetching
# per file would be dozens of round trips.
RL_SIGN_TOKEN=""
rl_sign_token() {
  if [[ -n "$RL_SIGN_TOKEN" ]]; then printf '%s' "$RL_SIGN_TOKEN"; return 0; fi
  local resp
  resp=$(curl -fsS -X POST \
    "https://login.microsoftonline.com/${AZURE_TENANT_ID}/oauth2/v2.0/token" \
    -d "client_id=${AZURE_CLIENT_ID}" \
    -d "client_secret=${AZURE_CLIENT_SECRET}" \
    -d "scope=https://codesigning.azure.net/.default" \
    -d "grant_type=client_credentials" 2>/dev/null) || return 1
  # Avoid a jq dependency; the token is a single flat string field.
  RL_SIGN_TOKEN=$(sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p' <<<"$resp")
  [[ -n "$RL_SIGN_TOKEN" ]] || return 1
  printf '%s' "$RL_SIGN_TOKEN"
}

# Sign one PE file in place. Returns non-zero on real failure so callers can
# abort a release rather than publish a half-signed set.
rl_sign_file() { # rl_sign_file <path-to-exe-or-dll>
  local f="$1" tok
  [[ -f "$f" ]] || return 0

  # Already signed? Re-signing appends rather than replaces on some toolchains,
  # and a doubly-signed binary is a support call nobody enjoys diagnosing.
  if command -v osslsigncode >/dev/null 2>&1 \
     && osslsigncode verify "$f" >/dev/null 2>&1; then
    rl_note "already signed: $(basename "$f")"
    return 0
  fi

  tok=$(rl_sign_token) || { echo "could not obtain an Azure token" >&2; return 1; }

  jsign --storetype TRUSTEDSIGNING \
        --keystore "$RL_SIGN_ENDPOINT" \
        --storepass "$tok" \
        --alias "${RL_SIGN_ACCOUNT}/${RL_SIGN_PROFILE}" \
        --alg SHA-256 \
        --tsaurl "$RL_SIGN_TSA" \
        --tsmode RFC3161 \
        --name "$RL_NAME" \
        --url "$RL_URL" \
        "$f" >/dev/null 2>&1 || { echo "jsign failed on $f" >&2; return 1; }

  # Verify rather than trust the exit status. This library already learned that
  # lesson from makensis, which exits 0 after aborting; and an unstamped or
  # malformed signature is exactly the failure that stays invisible until a
  # user reports it weeks later.
  if command -v osslsigncode >/dev/null 2>&1; then
    if ! osslsigncode verify "$f" 2>&1 | grep -q 'Signature verification: ok'; then
      echo "signature did not verify: $f" >&2; return 1
    fi
    if ! osslsigncode verify "$f" 2>&1 | grep -qi 'timestamp'; then
      echo "signed but NOT timestamped (expires in 72h): $f" >&2; return 1
    fi
  fi

  RL_SIGNED_COUNT=$((RL_SIGNED_COUNT + 1))
  rl_note "signed $(basename "$f")"
}

# Sign every PE file in a staging tree, before it is zipped or packed into an
# installer. Order matters: payload first, installer last, because the
# installer's signature covers the compressed payload as-is.
rl_sign_windows() { # rl_sign_windows <stagedir-or-file> [...]
  local target
  if ! rl_sign_ready; then
    if [[ -n "${RL_SIGN_ENDPOINT:-}" ]] && ! command -v jsign >/dev/null 2>&1; then
      rl_skip "Windows signing (jsign not installed: brew install jsign)"
    else
      rl_skip "Windows signing (not configured)"
    fi
    return 0
  fi
  rl_step "sign windows"
  for target in "$@"; do
    if [[ -d "$target" ]]; then
      while IFS= read -r f; do
        rl_sign_file "$f" || return 1
      done < <(find "$target" -type f \( -name '*.exe' -o -name '*.dll' \) | sort)
    else
      rl_sign_file "$target" || return 1
    fi
  done
}

# ------------------------------------------------------------------- NSIS ---
#
# Two shapes of Windows installer:
#   --cli            installs into Program Files and appends to the system PATH
#   --gui <exe>      the above plus Start Menu and Desktop shortcuts
#
# Both write an uninstaller and the Add/Remove Programs registry keys. The file
# list is generated from the staging directory so callers never hand-maintain
# it. `makensis` on macOS is case-sensitive about the staging paths but writes
# Windows-style paths into the script, hence the sed dance.

# makensis builds its language tables by transcoding the BOM'd .nlf files.
# Under LC_CTYPE=C that conversion throws std::bad_alloc and aborts *with a
# zero exit status*, so force a UTF-8 locale and verify the file was written
# rather than trusting the return code.
#
# Which UTF-8 locale actually *works* varies and cannot be inferred from the
# name: macOS lists C.UTF-8 but treats it as plain C, which is exactly the case
# that aborts. So try candidates in order and keep whichever produces a file —
# the only reliable test, given makensis exits 0 even when it dies.
rl_makensis() { # rl_makensis <nsi> <expected-output> <logfile>
  local nsi="$1" outfile="$2" log="$3" loc
  rm -f "$outfile"
  for loc in en_US.UTF-8 C.UTF-8 en_GB.UTF-8 UTF-8; do
    LC_ALL="$loc" LANG="$loc" makensis -V2 "$nsi" >"$log" 2>&1 || true
    [[ -s "$outfile" ]] && return 0
  done
  return 1
}

# ------------------------------------------- NSIS signed uninstaller (opt-in) --
#
# NSIS cannot emit an uninstaller at compile time: Uninstall.exe is produced by
# the installer *stub at run time*, which means it cannot be signed on this Mac
# the way every other artefact is. The standard workaround is a two-pass build —
# compile a throwaway installer whose only job is to call WriteUninstaller, run
# it on Windows, retrieve the uninstaller it drops, sign that, and `File` it
# into the real installer instead of generating a fresh one.
#
# Running it needs Windows, so this costs a Parallels round-trip and is opt-in
# via RL_SIGN_UNINSTALLER=1. It is off by default deliberately: Uninstall.exe is
# written to disk locally rather than downloaded, so it never carries a
# Mark-of-the-Web and SmartScreen — which only consults reputation for
# MOTW-tagged files — never looks at it. The whole benefit is that the UAC
# prompt at uninstall time reads "Stoatworks Labs" instead of a yellow "Unknown
# publisher". Worth having, not worth blocking a release on.
#
# Guest requirement: none beyond a booted VM. The stub is a plain 32-bit x86
# NSIS installer, which the ARM64 guest runs under emulation happily — unlike
# Tauri's bundled makensis.exe, it is only unpacking itself.

rl_nsis_uninstaller() { # rl_nsis_uninstaller <work> <unsection-body> -> prints path
  local work="$1" unsection="$2"
  local vm="${RL_VM_NAME:-Windows 11}"
  local staging="$HOME/Projects/.release-vm"
  local gen="$staging/uninstgen-${RL_SLUG}.exe"

  command -v prlctl >/dev/null 2>&1 || { rl_skip "signed uninstaller (no prlctl)"; return 1; }
  mkdir -p "$staging"

  # SilentInstall silent so the stub writes the uninstaller and exits without
  # ever drawing UI — there is nobody in the guest to click Next.
  cat >"$work/uninstgen.nsi" <<NSI
Unicode true
Name "${RL_NAME}"
OutFile "${gen}"
InstallDir "\$TEMP\\${RL_SLUG}-uninstgen"
RequestExecutionLevel user
SilentInstall silent

VIProductVersion "$(rl_numver)"
VIAddVersionKey "ProductName"     "${RL_NAME}"
VIAddVersionKey "CompanyName"     "${RL_PUBLISHER}"
VIAddVersionKey "FileDescription" "${RL_NAME} uninstaller"
VIAddVersionKey "FileVersion"     "${RL_VERSION}"
VIAddVersionKey "ProductVersion"  "${RL_VERSION}"
VIAddVersionKey "LegalCopyright"  "${RL_PUBLISHER}"

Section "Install"
  SetOutPath "\$INSTDIR"
  WriteUninstaller "\$INSTDIR\\Uninstall.exe"
SectionEnd

${unsection}
NSI

  rl_makensis "$work/uninstgen.nsi" "$gen" "$work/uninstgen.log" || {
    rl_skip "signed uninstaller (stub compile failed)"; return 1; }

  # \\psf\Projects is the same share release-windows-vm.sh uses.
  cat >"$staging/uninstgen.ps1" <<PS1
\$ErrorActionPreference = 'Continue'
\$dir = "\$env:TEMP\\${RL_SLUG}-uninstgen"
if (Test-Path \$dir) { Remove-Item \$dir -Recurse -Force -EA 0 }
Start-Process -FilePath '\\\\psf\\Projects\\.release-vm\\uninstgen-${RL_SLUG}.exe' -Wait
if (-not (Test-Path "\$dir\\Uninstall.exe")) { Write-Output 'NO UNINSTALLER'; exit 1 }
Copy-Item "\$dir\\Uninstall.exe" '\\\\psf\\Projects\\.release-vm\\Uninstall-${RL_SLUG}.exe' -Force
Write-Output 'OK'
exit 0
PS1

  rm -f "$staging/Uninstall-${RL_SLUG}.exe"
  prlctl exec "$vm" powershell -NoProfile -ExecutionPolicy Bypass \
    -File "\\\\psf\\Projects\\.release-vm\\uninstgen.ps1" >/dev/null 2>&1 || true

  local un="$staging/Uninstall-${RL_SLUG}.exe"
  [[ -s "$un" ]] || { rl_skip "signed uninstaller (guest produced nothing)"; return 1; }

  cp "$un" "$work/Uninstall.exe"
  rm -f "$un" "$gen" "$staging/uninstgen.ps1"
  rl_sign_file "$work/Uninstall.exe" || return 1
  printf '%s' "$work/Uninstall.exe"
}

rl_nsis() { # rl_nsis <label> <stagedir> --cli | --gui <exe>
  # RL_EULA (optional, from rl_eula) adds a licence page. Required when the NDI
  # runtime is bundled — that is the condition Vizrt's redistribution grant
  # rests on, so it is not cosmetic.
  local label="$1" stage="$2" mode="$3" guiexe="${4:-}"
  if ! command -v makensis >/dev/null 2>&1; then
    rl_skip "${label} NSIS installer (makensis not installed)"; return 0
  fi
  rl_step "nsis ${label}"

  local work; work="$(mktemp -d)"
  local nsi="$work/installer.nsi"
  local outfile="$RL_OUT/${RL_SLUG}-${RL_VERSION}-${label}-setup.exe"

  # Walk the staging tree and emit File/Delete/RMDir lines in the right order.
  # These accumulate as arrays; joining with printf keeps real newlines in the
  # generated script (a "...\n" inside double quotes would be a literal).
  local -a install_arr=() unfiles_arr=() undirs_arr=()
  local d f rel frel
  while IFS= read -r d; do
    rel="${d#"$stage"}"; rel="${rel#/}"
    if [[ -n "$rel" ]]; then
      install_arr+=("  SetOutPath \"\$INSTDIR\\${rel//\//\\}\"")
    else
      install_arr+=("  SetOutPath \"\$INSTDIR\"")
    fi
    while IFS= read -r f; do
      [[ -z "$f" ]] && continue
      install_arr+=("  File \"${f}\"")
      frel="${f#"$stage"/}"
      unfiles_arr+=("  Delete \"\$INSTDIR\\${frel//\//\\}\"")
    done < <(find "$d" -maxdepth 1 -type f | sort)
  done < <(find "$stage" -type d | sort)

  # Deepest directories first so RMDir succeeds.
  while IFS= read -r d; do
    rel="${d#"$stage"}"; rel="${rel#/}"
    [[ -z "$rel" ]] && continue
    undirs_arr+=("  RMDir \"\$INSTDIR\\${rel//\//\\}\"")
  done < <(find "$stage" -mindepth 1 -type d | sort -r)

  local install_lines uninstall_files uninstall_dirs
  install_lines=$(printf '%s\n' "${install_arr[@]}")
  uninstall_files=$( ((${#unfiles_arr[@]})) && printf '%s\n' "${unfiles_arr[@]}" || true )
  uninstall_dirs=$(  ((${#undirs_arr[@]}))  && printf '%s\n' "${undirs_arr[@]}"  || true )

  # NSIS needs the licence as a CRLF file it can read at compile time.
  local licpage=""
  if [[ -n "${RL_EULA:-}" && -f "${RL_EULA:-}" ]]; then
    local lic="$work/eula.txt"
    sed -e 's/$/\r/' "$RL_EULA" >"$lic"
    licpage="!insertmacro MUI_PAGE_LICENSE \"${lic}\""
  fi

  local shortcuts="" unshortcuts="" pathblock="" unpathblock=""
  if [[ "$mode" == "--gui" ]]; then
    shortcuts=$(cat <<SC
  CreateDirectory "\$SMPROGRAMS\\${RL_NAME}"
  CreateShortcut "\$SMPROGRAMS\\${RL_NAME}\\${RL_NAME}.lnk" "\$INSTDIR\\${guiexe}"
  CreateShortcut "\$DESKTOP\\${RL_NAME}.lnk" "\$INSTDIR\\${guiexe}"
SC
)
    unshortcuts=$(cat <<SC
  Delete "\$SMPROGRAMS\\${RL_NAME}\\${RL_NAME}.lnk"
  RMDir  "\$SMPROGRAMS\\${RL_NAME}"
  Delete "\$DESKTOP\\${RL_NAME}.lnk"
SC
)
  else
    # CLI: put the install dir on the machine PATH via EnvVarUpdate-lite.
    pathblock=$(cat <<'SC'
  ; Append to the system PATH (idempotent: only if not already present)
  ReadRegStr $0 HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "Path"
  Push $0
  Push "$INSTDIR"
  Call StrStr
  Pop $1
  StrCmp $1 "" 0 pathdone
    WriteRegExpandStr HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "Path" "$0;$INSTDIR"
    SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000
  pathdone:
SC
)
  fi

  # The uninstall section is assembled separately because the signed-uninstaller
  # two-pass has to compile a stub containing an identical copy of it — the
  # uninstaller the stub drops is the one that ships, so any divergence would
  # mean shipping an uninstaller that does not match the installer.
  local unsection
  unsection=$(cat <<UNS
Section "Uninstall"
  SetRegView 64
  SetShellVarContext all
${unshortcuts}
${uninstall_files}
  Delete "\$INSTDIR\\Uninstall.exe"
${uninstall_dirs}
  RMDir "\$INSTDIR"
  DeleteRegKey HKLM "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${RL_SLUG}"
  DeleteRegKey HKLM "Software\\${RL_PUBLISHER}\\${RL_NAME}"
SectionEnd
UNS
)

  # Pass one, when enabled: get a signed Uninstall.exe to embed. `File` after
  # WriteUninstaller overwrites the freshly generated one — WriteUninstaller
  # itself has to stay, because makensis refuses to compile an Uninstall
  # section without it.
  local writeuninst="  WriteUninstaller \"\$INSTDIR\\Uninstall.exe\""
  if [[ "${RL_SIGN_UNINSTALLER:-0}" == "1" ]] && rl_sign_ready; then
    local signedun
    if signedun=$(rl_nsis_uninstaller "$work" "$unsection"); then
      writeuninst="${writeuninst}
  File \"/oname=Uninstall.exe\" \"${signedun}\""
    fi
  fi

  cat >"$nsi" <<NSI
Unicode true
!include "MUI2.nsh"
!include "WinMessages.nsh"
!include "LogicLib.nsh"

Name "${RL_NAME}"
OutFile "${outfile}"
InstallDir "\$PROGRAMFILES64\\${RL_NAME}"
InstallDirRegKey HKLM "Software\\${RL_PUBLISHER}\\${RL_NAME}" "InstallDir"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

VIProductVersion "$(rl_numver)"
VIAddVersionKey "ProductName"     "${RL_NAME}"
VIAddVersionKey "CompanyName"     "${RL_PUBLISHER}"
VIAddVersionKey "FileDescription" "${RL_NAME} installer"
VIAddVersionKey "FileVersion"     "${RL_VERSION}"
VIAddVersionKey "ProductVersion"  "${RL_VERSION}"
VIAddVersionKey "LegalCopyright"  "${RL_PUBLISHER}"

!define MUI_ABORTWARNING
${licpage}
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; Substring search used by the PATH block. Returns "" in \$1 when not found.
Function StrStr
  Exch \$R1 ; needle
  Exch
  Exch \$R2 ; haystack
  Push \$R3
  Push \$R4
  Push \$R5
  StrLen \$R3 \$R1
  StrCpy \$R4 0
  loop:
    StrCpy \$R5 \$R2 \$R3 \$R4
    StrCmp \$R5 \$R1 done
    StrCmp \$R5 "" notfound
    IntOp \$R4 \$R4 + 1
    Goto loop
  notfound:
    StrCpy \$R1 ""
    Goto out
  done:
    StrCpy \$R1 \$R5
  out:
  Pop \$R5
  Pop \$R4
  Pop \$R3
  Pop \$R2
  Exch \$R1
FunctionEnd

Section "Install"
  ; The NSIS stub is 32-bit, so without this the uninstall keys are redirected
  ; into WOW6432Node and Add/Remove Programs never shows the entry.
  SetRegView 64
  ; This installs per-machine into Program Files, so shortcuts belong in the
  ; all-users Start Menu. Without this \$SMPROGRAMS/\$DESKTOP point at whichever
  ; account ran the installer — which under a silent/service install is
  ; SYSTEM, and the shortcuts vanish into a profile nobody ever sees.
  SetShellVarContext all
${install_lines}
  SetOutPath "\$INSTDIR"
${shortcuts}
${pathblock}
${writeuninst}
  WriteRegStr HKLM "Software\\${RL_PUBLISHER}\\${RL_NAME}" "InstallDir" "\$INSTDIR"
  WriteRegStr HKLM "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${RL_SLUG}" "DisplayName"     "${RL_NAME}"
  WriteRegStr HKLM "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${RL_SLUG}" "DisplayVersion"  "${RL_VERSION}"
  WriteRegStr HKLM "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${RL_SLUG}" "Publisher"       "${RL_PUBLISHER}"
  WriteRegStr HKLM "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${RL_SLUG}" "URLInfoAbout"    "${RL_URL}"
  WriteRegStr HKLM "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${RL_SLUG}" "UninstallString" "\$INSTDIR\\Uninstall.exe"
SectionEnd

${unsection}
NSI

  local ok=0
  rl_makensis "$nsi" "$outfile" "$work/makensis.log" && ok=1

  if (( ok )); then
    rl_note "$(basename "$outfile")"
    # Sign last: the installer's signature covers its compressed payload, so
    # the staging tree must already have been signed before it was packed.
    if rl_sign_ready; then
      rl_sign_file "$outfile" || { rm -rf "$work"; return 1; }
    fi
  else
    echo "makensis failed for ${label} (tried every UTF-8 locale):" >&2
    tail -30 "$work/makensis.log" >&2
    rm -rf "$work"
    return 1
  fi
  rm -rf "$work"
}

# ---------------------------------- macOS Developer ID sign + notarisation --
#
# The real fix for Gatekeeper: sign with a Developer ID Application identity
# (hardened runtime + secure timestamp, both mandatory for notarisation), then
# have Apple notarise the artefact and staple the ticket so verification works
# offline. Configuration (unset means "fall back / skip", never "fail"):
#
#   RL_MAC_SIGN_IDENTITY       "Developer ID Application: NAME (TEAMID)"
#   RL_MAC_INSTALLER_IDENTITY  "Developer ID Installer: NAME (TEAMID)" (.pkg)
#   RL_NOTARY_PROFILE          notarytool keychain profile name
#   RL_MAC_ENTITLEMENTS        entitlements plist, auto-detected from the
#                              vendored scripts/mac-entitlements.plist
#
# Normally all four come from ~/.config/stoatworks/release-signing.env via
# rl_init. The notary profile is created once per machine with
# `xcrun notarytool store-credentials` — the app-specific password lives in
# the keychain and never appears in an environment variable.

RL_MAC_SIGNED_COUNT=0
RL_NOTARIZED_COUNT=0

rl_mac_sign_ready() { [[ -n "${RL_MAC_SIGN_IDENTITY:-}" ]]; }
rl_notary_ready()   { [[ -n "${RL_NOTARY_PROFILE:-}" ]]; }

# Sign a bundle (or single Mach-O) with the Developer ID identity, inside-out:
# nested executables, dylibs and frameworks first, the main binary and the
# bundle itself last, because an outer signature covers the contents as-is.
# Entitlements are applied ONLY to the outer app and its main executable —
# helpers must not inherit exceptions they don't need.
rl_mac_sign() { # rl_mac_sign <path-to-.app-or-binary>
  local app="$1" f ent=()
  [[ -e "$app" ]] || return 0
  [[ -n "${RL_MAC_ENTITLEMENTS:-}" && -f "${RL_MAC_ENTITLEMENTS:-}" ]] \
    && ent=(--entitlements "$RL_MAC_ENTITLEMENTS")
  local sign=(codesign --force --options runtime --timestamp \
              --sign "$RL_MAC_SIGN_IDENTITY")
  rl_step "sign $(basename "$app") (Developer ID)"

  # Already correctly signed — usually by Tauri, which signs during bundling
  # when APPLE_SIGNING_IDENTITY is exported. Do NOT re-sign: replacing the
  # signature changes the CDHash, and any DMG built from the earlier copy
  # would no longer be covered by this app's notarisation.
  if codesign --verify --strict --deep "$app" >/dev/null 2>&1 \
     && codesign -dvv "$app" 2>&1 | grep -q "Authority=$RL_MAC_SIGN_IDENTITY" \
     && codesign -d --verbose=2 "$app" 2>&1 | grep -q 'flags=.*runtime'; then
    RL_MAC_SIGNED_COUNT=$((RL_MAC_SIGNED_COUNT + 1))
    rl_note "already signed with this identity, left untouched"
    return 0
  fi

  if [[ -d "$app" ]]; then
    # Nested code: everything executable or Mach-O shaped that is not the main
    # binary. file(1) is the arbiter — resources are not re-signed.
    while IFS= read -r f; do
      file -b "$f" 2>/dev/null | grep -q 'Mach-O' || continue
      "${sign[@]}" "$f" >/dev/null 2>&1 \
        || { echo "codesign failed on nested $f" >&2; return 1; }
    done < <(find "$app/Contents" -type f \
               \( -perm -u+x -o -name '*.dylib' -o -name '*.so' \) \
               ! -path "$app/Contents/MacOS/*" 2>/dev/null | sort)
    find "$app/Contents" -name '*.framework' -maxdepth 3 -type d 2>/dev/null \
      | while IFS= read -r f; do
          "${sign[@]}" "$f" >/dev/null 2>&1 \
            || { echo "codesign failed on framework $f" >&2; exit 1; }
        done || return 1
    while IFS= read -r f; do
      "${sign[@]}" "${ent[@]}" "$f" >/dev/null 2>&1 \
        || { echo "codesign failed on $f" >&2; return 1; }
    done < <(find "$app/Contents/MacOS" -type f -perm -u+x 2>/dev/null | sort)
  fi
  "${sign[@]}" "${ent[@]}" "$app" >/dev/null 2>&1 \
    || { echo "codesign failed on $app" >&2; return 1; }

  # Verify rather than trust the exit status — same lesson as rl_sign_file.
  codesign --verify --strict --deep "$app" 2>&1 | sed 's/^/    /' | head -5
  codesign --verify --strict --deep "$app" >/dev/null 2>&1 \
    || { echo "signature did not verify: $app" >&2; return 1; }
  RL_MAC_SIGNED_COUNT=$((RL_MAC_SIGNED_COUNT + 1))
  rl_note "signed $(basename "$app")"
}

# Notarise one artefact and staple the ticket. Apps are shipped to Apple as a
# temporary zip and the ticket is stapled to the bundle itself, so anything
# packaged from it afterwards (zip, dmg) carries the ticket. dmg/pkg are
# submitted and stapled as files. Bare zips can be submitted but never
# stapled — notarise the app BEFORE zipping instead.
rl_mac_notarize() { # rl_mac_notarize <path (.app|.dmg|.pkg|.zip)>
  local target="$1" sub log
  rl_notary_ready || { rl_skip "notarisation (no RL_NOTARY_PROFILE)"; return 0; }
  [[ -e "$target" ]] || return 0
  rl_step "notarize $(basename "$target")"

  sub="$target"
  if [[ -d "$target" ]]; then
    sub="$(mktemp -d)/$(basename "$target").zip"
    ditto -c -k --keepParent "$target" "$sub"
  fi

  log="$(mktemp)"
  if ! xcrun notarytool submit "$sub" --keychain-profile "$RL_NOTARY_PROFILE" \
        --wait >"$log" 2>&1 || ! grep -q 'status: Accepted' "$log"; then
    echo "notarisation FAILED for $target:" >&2
    cat "$log" >&2
    # Surface Apple's per-binary reasons; the submission id is in the log.
    local id; id=$(grep -m1 '  id:' "$log" | awk '{print $2}')
    [[ -n "$id" ]] && xcrun notarytool log "$id" \
        --keychain-profile "$RL_NOTARY_PROFILE" >&2 || true
    rm -f "$log"; [[ "$sub" != "$target" ]] && rm -rf "$(dirname "$sub")"
    return 1
  fi
  rm -f "$log"; [[ "$sub" != "$target" ]] && rm -rf "$(dirname "$sub")"

  case "$target" in
    *.zip) rl_note "notarised (zip cannot be stapled; contents carry no ticket)" ;;
    *)  xcrun stapler staple "$target" >/dev/null \
          || { echo "stapler failed on $target" >&2; return 1; }
        rl_note "notarised and stapled" ;;
  esac
  RL_NOTARIZED_COUNT=$((RL_NOTARIZED_COUNT + 1))

  # The verdict that matches what a user's Mac will decide.
  if [[ -d "$target" ]]; then
    spctl -a -t install "$target" >/dev/null 2>&1 \
      || { echo "spctl rejected $target after notarisation" >&2; return 1; }
  fi
}

# Sign every Mach-O in a staging tree that is not already validly signed by a
# real identity. Third-party libraries (the NDI runtime is Vizrt-signed) are
# left untouched; ad-hoc signatures verify but carry no Authority line, so
# they are re-signed. This is what makes bare CLI payloads notarisable.
rl_mac_sign_tree() { # rl_mac_sign_tree <dir>
  rl_mac_sign_ready || return 0
  local f
  while IFS= read -r f; do
    file -b "$f" 2>/dev/null | grep -q 'Mach-O' || continue
    if codesign --verify --strict "$f" >/dev/null 2>&1 \
       && codesign -dvv "$f" 2>&1 | grep -q 'Authority='; then
      continue
    fi
    rl_mac_sign "$f" || return 1
  done < <(find "$1" -type f | sort)
}

# -------------------------------------------------------- macOS ad-hoc sign --
#
# arm64 Mach-O binaries come out of the linker already ad-hoc signed because the
# platform requires it; x86_64 ones do not. An entirely unsigned bundle is what
# produces the "is damaged and can't be opened" refusal, so sign every bundle
# explicitly and identically. Nested code first, outermost last — a signature
# over a bundle covers its contents, so re-signing an inner binary afterwards
# invalidates the outer one.
#
# When Developer ID signing is configured this upgrades transparently to the
# real thing — sign, notarise, staple — so existing callers get the full chain
# without edits. Ad-hoc remains the unconfigured/CI fallback.

rl_adhoc_sign() { # rl_adhoc_sign <path-to-.app>
  local app="$1" f
  [[ -d "$app" ]] || return 0
  if rl_mac_sign_ready; then
    rl_mac_sign "$app" || return 1
    rl_mac_notarize "$app" || return 1
    return 0
  fi
  rl_step "sign $(basename "$app") (ad-hoc)"
  while IFS= read -r f; do
    codesign --force --sign - --timestamp=none "$f" 2>/dev/null || true
  done < <(find "$app/Contents" -type f -perm -u+x \
             ! -path "$app/Contents/MacOS/*" 2>/dev/null)
  find "$app/Contents" -name "*.framework" -maxdepth 3 -type d 2>/dev/null \
    | while IFS= read -r f; do codesign --force --sign - "$f" 2>/dev/null || true; done
  codesign --force --sign - --timestamp=none "$app"
  codesign -dv "$app" 2>&1 | grep -m1 -E 'adhoc|Signature' | sed 's/^/    /' || true
}

# ------------------------------------------------------------ macOS .pkg ----
#
#   --cli              staged files land in /usr/local/<slug>, binaries are
#                      symlinked into /usr/local/bin by a postinstall script
#   --app "Foo.app"    the bundle is installed into /Applications

rl_pkg() { # rl_pkg <label> <stagedir> --cli | --app <BundleName>
  local label="$1" stage="$2" mode="$3" appname="${4:-}"
  rl_step "pkg  ${label}"
  local work root scripts component outfile
  work="$(mktemp -d)"; root="$work/root"; scripts="$work/scripts"
  mkdir -p "$root" "$scripts"
  component="$work/${RL_SLUG}-component.pkg"
  outfile="$RL_OUT/${RL_SLUG}-${RL_VERSION}-${label}.pkg"

  local install_location
  if [[ "$mode" == "--app" ]]; then
    install_location="/Applications"
    cp -R "$stage/$appname" "$root/"
  else
    install_location="/usr/local/${RL_SLUG}"
    cp -R "$stage/." "$root/"
    rl_mac_sign_tree "$root" || return 1
    # Link every executable file at the top level into /usr/local/bin.
    {
      echo '#!/bin/sh'
      echo 'set -e'
      echo 'mkdir -p /usr/local/bin'
      while IFS= read -r b; do
        [[ -z "$b" ]] && continue
        echo "ln -sf \"/usr/local/${RL_SLUG}/$(basename "$b")\" \"/usr/local/bin/$(basename "$b")\""
      done < <(find "$root" -maxdepth 1 -type f -perm -u+x)
      echo 'exit 0'
    } >"$scripts/postinstall"
    chmod +x "$scripts/postinstall"
  fi

  local pkgbuild_args=(--root "$root" --identifier "$RL_IDENT"
                       --version "$RL_VERSION" --install-location "$install_location")
  [[ "$mode" == "--cli" ]] && pkgbuild_args+=(--scripts "$scripts")

  pkgbuild "${pkgbuild_args[@]}" "$component" >/dev/null

  # A licence page, when there is a EULA to show. Required when the NDI runtime
  # is bundled: presenting those terms is the condition Vizrt's redistribution
  # grant rests on. productbuild resolves <license> against --resources.
  local licref=""
  if [[ -n "${RL_EULA:-}" && -f "${RL_EULA:-}" ]]; then
    mkdir -p "$work/resources"
    cp "$RL_EULA" "$work/resources/LICENSE.txt"
    licref='<license file="LICENSE.txt"/>'
  fi

  cat >"$work/distribution.xml" <<DIST
<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
    <title>${RL_NAME} ${RL_VERSION}</title>
    ${licref}
    <options customize="never" require-scripts="false" hostArchitectures="arm64,x86_64"/>
    <domains enable_localSystem="true"/>
    <pkg-ref id="${RL_IDENT}"/>
    <choices-outline><line choice="default"/></choices-outline>
    <choice id="default"><pkg-ref id="${RL_IDENT}"/></choice>
    <pkg-ref id="${RL_IDENT}" version="${RL_VERSION}" onConclusion="none">$(basename "$component")</pkg-ref>
</installer-gui-script>
DIST

  rm -f "$outfile"
  local pb_args=(--distribution "$work/distribution.xml" --package-path "$work")
  [[ -n "$licref" ]] && pb_args+=(--resources "$work/resources")
  [[ -n "${RL_MAC_INSTALLER_IDENTITY:-}" ]] \
    && pb_args+=(--sign "$RL_MAC_INSTALLER_IDENTITY" --timestamp)
  productbuild "${pb_args[@]}" "$outfile" >/dev/null
  rl_note "$(basename "$outfile")"
  rm -rf "$work"
  # An unsigned pkg cannot be notarised; Apple rejects it at intake.
  [[ -n "${RL_MAC_INSTALLER_IDENTITY:-}" ]] && { rl_mac_notarize "$outfile" || return 1; }
  return 0
}

# ------------------------------------------------- macOS multi-part .pkg ----
#
# Audio plugins do not live in one place: VST3 goes to /Library/Audio/Plug-Ins/
# VST3, AU to .../Components, the standalone to /Applications. productbuild
# takes one component package per destination, so build them separately and
# stitch them together with a distribution that installs all of them.
#
#   rl_pkg_multi <label> "<path>:<install-location>" ...
#
# <path> is a file or directory; its *contents* are placed at the destination
# when it is a directory of things, so pass the bundle itself.

rl_pkg_multi() { # rl_pkg_multi <label> <src:dest> ...
  local label="$1"; shift
  rl_step "pkg  ${label} (multi-part)"
  local work; work="$(mktemp -d)"
  local outfile="$RL_OUT/${RL_SLUG}-${RL_VERSION}-${label}.pkg"
  local -a refs=() lines=()
  local i=0 spec src dest root ident comp

  for spec in "$@"; do
    src="${spec%%:*}"; dest="${spec#*:}"
    [[ -e "$src" ]] || { rl_note "missing, skipped: $src"; continue; }
    i=$((i+1))
    root="$work/root$i"; mkdir -p "$root"
    cp -R "$src" "$root/"
    ident="${RL_IDENT}.part${i}"
    comp="$work/part${i}.pkg"
    pkgbuild --root "$root" --identifier "$ident" --version "$RL_VERSION" \
             --install-location "$dest" "$comp" >/dev/null
    refs+=("<pkg-ref id=\"$ident\"/>")
    lines+=("<line choice=\"c$i\"/>")
    lines+=("__CHOICE__$i:$ident")
  done

  if (( i == 0 )); then rl_skip "${label} .pkg (nothing to package)"; rm -rf "$work"; return 0; fi

  {
    echo '<?xml version="1.0" encoding="utf-8"?>'
    echo '<installer-gui-script minSpecVersion="2">'
    echo "  <title>${RL_NAME} ${RL_VERSION}</title>"
    echo '  <options customize="always" require-scripts="false" hostArchitectures="arm64,x86_64"/>'
    echo '  <domains enable_localSystem="true"/>'
    echo '  <choices-outline>'
    local n
    for ((n=1; n<=i; n++)); do echo "    <line choice=\"c${n}\"/>"; done
    echo '  </choices-outline>'
    for ((n=1; n<=i; n++)); do
      echo "  <choice id=\"c${n}\" visible=\"true\" title=\"${RL_NAME} part ${n}\" start_selected=\"true\">"
      echo "    <pkg-ref id=\"${RL_IDENT}.part${n}\"/>"
      echo '  </choice>'
      echo "  <pkg-ref id=\"${RL_IDENT}.part${n}\" version=\"${RL_VERSION}\">part${n}.pkg</pkg-ref>"
    done
    echo '</installer-gui-script>'
  } >"$work/distribution.xml"

  rm -f "$outfile"
  local pbm_args=(--distribution "$work/distribution.xml" --package-path "$work")
  [[ -n "${RL_MAC_INSTALLER_IDENTITY:-}" ]] \
    && pbm_args+=(--sign "$RL_MAC_INSTALLER_IDENTITY" --timestamp)
  productbuild "${pbm_args[@]}" "$outfile" >/dev/null
  rl_note "$(basename "$outfile")"
  rm -rf "$work"
  [[ -n "${RL_MAC_INSTALLER_IDENTITY:-}" ]] && { rl_mac_notarize "$outfile" || return 1; }
  return 0
}

# ------------------------------------------------------------- macOS .dmg ---
#
# For GUI apps the staging dir holds Foo.app and the image gets an /Applications
# drop target. For CLI payloads it is a plain read-only image of the folder.

rl_dmg() { # rl_dmg <label> <stagedir> [--app <BundleName>]
  local label="$1" stage="$2" mode="${3:-}" appname="${4:-}"
  local outfile="$RL_OUT/${RL_SLUG}-${RL_VERSION}-${label}.dmg"
  rl_step "dmg  ${label}"
  rm -f "$outfile"
  [[ "$mode" != "--app" ]] && { rl_mac_sign_tree "$stage" || return 1; }

  if [[ "$mode" == "--app" ]] && command -v create-dmg >/dev/null 2>&1; then
    # create-dmg exits 2 when it cannot set a custom icon position on a
    # headless/unsigned build; the image itself is still valid.
    create-dmg --volname "${RL_NAME} ${RL_VERSION}" \
               --window-size 540 380 \
               --icon "$appname" 140 190 \
               --app-drop-link 400 190 \
               --no-internet-enable \
               "$outfile" "$stage" >/dev/null 2>&1 || true
    if [[ -f "$outfile" ]]; then
      rl_note "$(basename "$outfile")"
      rl_dmg_finish "$outfile" "$mode"
      return $?
    fi
    rl_note "create-dmg failed, falling back to hdiutil"
  fi

  # -quiet hides hdiutil's diagnostics too, which turned a CI failure into a
  # bare "exit code 1". Capture the output and print it only when it matters.
  local hdlog; hdlog="$(mktemp)"
  if hdiutil create -volname "${RL_NAME} ${RL_VERSION}" \
                    -srcfolder "$stage" -ov -format UDZO "$outfile" >"$hdlog" 2>&1; then
    rm -f "$hdlog"
    rl_note "$(basename "$outfile")"
  else
    echo "hdiutil failed building ${label}:" >&2
    cat "$hdlog" >&2
    rm -f "$hdlog"
    return 1
  fi
  rl_dmg_finish "$outfile" "$mode"
}

# Sign the image itself; notarise it only when it carries a bare payload. An
# --app dmg holds a bundle that rl_adhoc_sign/rl_mac_sign already notarised, so
# Apple's hash check covers it and a second submission buys nothing. A CLI dmg
# has no bundle for a ticket to ride on, so the image is the thing to notarise.
rl_dmg_finish() { # rl_dmg_finish <dmg> <mode>
  local outfile="$1" mode="${2:-}"
  rl_mac_sign_ready || return 0
  codesign --force --timestamp --sign "$RL_MAC_SIGN_IDENTITY" "$outfile" \
    >/dev/null 2>&1 || { echo "codesign failed on $outfile" >&2; return 1; }
  if [[ "$mode" != "--app" ]]; then
    rl_mac_notarize "$outfile" || return 1
  fi
}

# --------------------------------------------------- signing status blurb ---
#
# The one sentence about signing that goes into GitHub release notes. It has to
# track what actually happened during *this* run: claiming "unsigned" on a
# signed installer trains users to click through warnings, and claiming signed
# on an unsigned one is worse. Driven by RL_SIGNED_COUNT, which only
# rl_sign_file increments, so it cannot drift from reality.
rl_notes_signing() {
  local mac win
  if (( RL_NOTARIZED_COUNT > 0 )); then
    mac="macOS artefacts are Developer ID-signed and notarised by Apple — no quarantine step needed."
  elif (( RL_MAC_SIGNED_COUNT > 0 )); then
    mac="macOS artefacts are Developer ID-signed but NOT notarised: see the README for the quarantine step."
  else
    mac="macOS artefacts are unsigned: see the README for the quarantine step."
  fi
  if (( RL_SIGNED_COUNT > 0 )); then
    win="Windows artefacts are Authenticode-signed and timestamped."
    printf '%s %s' "$win" "$mac"
  else
    printf '%s' "$mac"
  fi
}

# ------------------------------------------------------------------ report --

rl_summary() {
  echo
  rl_step "artefacts in ${RL_OUT}"
  # `ls -1sh .` segfaults here under some electron-builder runs — CoreFoundation
  # objects to being used after a fork. find + stat avoids the fork entirely.
  find "$RL_OUT" -mindepth 1 -maxdepth 1 -type f -print0 2>/dev/null \
    | sort -z \
    | while IFS= read -r -d '' f; do
        printf '    %8s  %s\n' \
          "$(du -h "$f" 2>/dev/null | cut -f1)" "$(basename "$f")"
      done
  if ((${#RL_SKIPPED[@]})); then
    echo
    rl_step "skipped"
    printf '    %s\n' "${RL_SKIPPED[@]}"
  fi
}
