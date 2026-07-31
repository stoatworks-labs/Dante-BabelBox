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
# NOTHING HERE IS CODE-SIGNED. Unsigned .pkg and .dmg payloads are quarantined
# by Gatekeeper on download, and approving the outer app does NOT unquarantine
# nested helper binaries — they get SIGKILLed silently. Ship the documented
# `xattr -dr com.apple.quarantine` step with every macOS artefact.
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

rl_init() {
  RL_NAME="$1"; RL_SLUG="$2"; RL_VERSION="$3"; RL_IDENT="$4"; RL_OUT="$5"
  mkdir -p "$RL_OUT"
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

# Library filename for a target label.
rl_ndi_libname() { # rl_ndi_libname <label>
  case "$1" in
    macos-*)   printf 'libndi.dylib' ;;
    windows-*) printf 'Processing.NDI.Lib.x64.dll' ;;
    *)         printf 'libndi.so.6' ;;
  esac
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
  local lib src dest
  lib="$(rl_ndi_libname "$label")"
  src="$(rl_ndi_srcdir "$label")"

  if [[ -z "$src" || ! -f "$src/$lib" ]]; then
    rl_skip "${label} NDI runtime (set RL_NDI_DIR_$(printf '%s' "$label" | tr '[:lower:]-' '[:upper:]_') to a directory containing ${lib})"
    return 0
  fi

  rl_step "ndi  ${label}"
  # Where the app's own loader looks first, in every implementation in the
  # fleet: Contents/Frameworks inside a bundle, otherwise beside the binary.
  if [[ "$mode" == "--app" ]]; then
    dest="$stage/$appname/Contents/Frameworks"
  else
    dest="$stage"
  fi
  mkdir -p "$dest"
  cp "$src/$lib" "$dest/$lib"

  # Vizrt requires the runtime licence text to travel with the binary.
  local notice
  for notice in "$src/../../licenses/libndi_licenses.txt" \
                "$src/libndi_licenses.txt" \
                "$src/../licenses/libndi_licenses.txt"; do
    if [[ -f "$notice" ]]; then
      cp "$notice" "$dest/libndi_licenses.txt"; break
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
  WriteUninstaller "\$INSTDIR\\Uninstall.exe"
  WriteRegStr HKLM "Software\\${RL_PUBLISHER}\\${RL_NAME}" "InstallDir" "\$INSTDIR"
  WriteRegStr HKLM "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${RL_SLUG}" "DisplayName"     "${RL_NAME}"
  WriteRegStr HKLM "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${RL_SLUG}" "DisplayVersion"  "${RL_VERSION}"
  WriteRegStr HKLM "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${RL_SLUG}" "Publisher"       "${RL_PUBLISHER}"
  WriteRegStr HKLM "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${RL_SLUG}" "URLInfoAbout"    "${RL_URL}"
  WriteRegStr HKLM "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${RL_SLUG}" "UninstallString" "\$INSTDIR\\Uninstall.exe"
SectionEnd

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
NSI

  # makensis builds its language tables by transcoding the BOM'd .nlf files.
  # Under LC_CTYPE=C that conversion throws std::bad_alloc and aborts *with a
  # zero exit status*, so force a UTF-8 locale and verify the file was written
  # rather than trusting the return code.
  #
  # Which UTF-8 locale actually *works* varies and cannot be inferred from the
  # name: macOS lists C.UTF-8 but treats it as plain C, which is exactly the
  # case that aborts. So try candidates in order and keep whichever produces a
  # file — the only reliable test, given makensis exits 0 even when it dies.
  rm -f "$outfile"
  local loc ok=0
  for loc in en_US.UTF-8 C.UTF-8 en_GB.UTF-8 UTF-8; do
    LC_ALL="$loc" LANG="$loc" makensis -V2 "$nsi" >"$work/makensis.log" 2>&1 || true
    if [[ -s "$outfile" ]]; then ok=1; break; fi
  done

  if (( ok )); then
    rl_note "$(basename "$outfile")"
  else
    echo "makensis failed for ${label} (tried every UTF-8 locale):" >&2
    tail -30 "$work/makensis.log" >&2
    rm -rf "$work"
    return 1
  fi
  rm -rf "$work"
}

# -------------------------------------------------------- macOS ad-hoc sign --
#
# arm64 Mach-O binaries come out of the linker already ad-hoc signed because the
# platform requires it; x86_64 ones do not. An entirely unsigned bundle is what
# produces the "is damaged and can't be opened" refusal, so sign every bundle
# explicitly and identically. Nested code first, outermost last — a signature
# over a bundle covers its contents, so re-signing an inner binary afterwards
# invalidates the outer one.

rl_adhoc_sign() { # rl_adhoc_sign <path-to-.app>
  local app="$1" f
  [[ -d "$app" ]] || return 0
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
  productbuild "${pb_args[@]}" "$outfile" >/dev/null
  rl_note "$(basename "$outfile")"
  rm -rf "$work"
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
  productbuild --distribution "$work/distribution.xml" --package-path "$work" "$outfile" >/dev/null
  rl_note "$(basename "$outfile")"
  rm -rf "$work"
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

  if [[ "$mode" == "--app" ]] && command -v create-dmg >/dev/null 2>&1; then
    # create-dmg exits 2 when it cannot set a custom icon position on a
    # headless/unsigned build; the image itself is still valid.
    create-dmg --volname "${RL_NAME} ${RL_VERSION}" \
               --window-size 540 380 \
               --icon "$appname" 140 190 \
               --app-drop-link 400 190 \
               --no-internet-enable \
               "$outfile" "$stage" >/dev/null 2>&1 || true
    if [[ -f "$outfile" ]]; then rl_note "$(basename "$outfile")"; return 0; fi
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
