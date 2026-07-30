#!/usr/bin/env bash
# release-windows-vm.sh — build a Tauri launcher's Windows executable by driving
# the Parallels "Windows 11" guest.
#
# Tauri's Rust code links the MSVC runtime and pulls in WebView2, so the binary
# itself has to be produced on Windows. The *installer*, however, is built back
# on the Mac: Tauri's bundled makensis.exe is a 32-bit x86 build that cannot
# even print its version in this ARM64 guest, and packaging on the Mac has the
# happy side effect of giving every project in the fleet the same installer.
#
# So this script stops at `tauri build --no-bundle` and hands the raw exe plus
# its resources back to the caller, which packages them with release-lib.sh.
#
# ~/Projects is shared into the guest as \\psf\Projects, but cargo misbehaves
# over UNC, so the tree is mirrored to C:\build\<slug> with robocopy.
#
# Build steps go in a generated .ps1 rather than `powershell -Command`: rustup,
# cargo and npm log progress to stderr, which PowerShell turns into a
# terminating NativeCommandError under $ErrorActionPreference='Stop'.
#
# Requires in the guest: a NATIVE aarch64 Rust host (an x86_64 host runs under
# emulation and is glacially slow), Node, and VS Build Tools 2022 with the C++
# ARM64 toolset.
#
#   release-windows-vm.sh <repo> <slug> <version> <outdir> [launcher] [ProductName]
#
# Leaves <outdir>/win-<label>/ holding <ProductName>.exe and any resources.
set -euo pipefail

repo="$1"; slug="$2"; version="$3"; out="$4"
launcher="${5:-launcher}"
product="${6:-}"
vm="${RL_VM_NAME:-Windows 11}"

reponame="$(basename "$repo")"
guest="C:\\build\\${slug}"
staging="$HOME/Projects/.release-vm"
mkdir -p "$staging"

psfile() {
  prlctl exec "$vm" powershell -NoProfile -ExecutionPolicy Bypass \
    -File "\\\\psf\\Projects\\.release-vm\\$1"
}

cat >"$staging/mirror.ps1" <<PS1
\$ErrorActionPreference = 'Continue'
robocopy '\\\\psf\\Projects\\${reponame}' '${guest}' /E /NJH /NJS /NP /NFL /NDL /R:1 /W:1 \`
  /XD target node_modules .git dist-release .release-vm | Out-Null
# robocopy uses exit codes 0-7 as success bit-flags; 8+ is a real failure.
if (\$LASTEXITCODE -ge 8) { Write-Error "robocopy failed: \$LASTEXITCODE"; exit 1 }
Write-Output 'mirrored'
exit 0
PS1

echo "    mirroring ${reponame} into the guest"
psfile mirror.ps1 >/dev/null || { echo "    mirror FAILED"; exit 1; }

# arm64 first: it is the guest's native target, so it warms the shared
# dependency cache at full speed before the x64 cross build reuses it.
for pair in "aarch64:aarch64-pc-windows-msvc" "x86_64:x86_64-pc-windows-msvc"; do
  label="${pair%%:*}"; target="${pair##*:}"
  echo "    guest build windows-${label}"

  cat >"$staging/build.ps1" <<PS1
\$ErrorActionPreference = 'Continue'
\$env:RUSTUP_HOME = 'C:\rust\rustup'
\$env:CARGO_HOME  = 'C:\rust\cargo'
\$env:PATH = 'C:\rust\cargo\bin;C:\Program Files\nodejs;C:\Program Files\Git\cmd;' + \$env:PATH

function Invoke-Step(\$desc, [scriptblock]\$block) {
  Write-Output "--- \$desc"
  & \$block 2>&1 | ForEach-Object { "\$_" } | Select-Object -Last 12
  if (\$LASTEXITCODE -ne 0) { Write-Output "STEP FAILED (\$LASTEXITCODE): \$desc"; exit 1 }
}

Set-Location '${guest}'
Invoke-Step 'rustup target' { rustup target add ${target} }
Invoke-Step 'cargo build'   { cargo build --release --target ${target} --bins }

# The launcher expects the server binary beside it as a Tauri resource. Clear
# out any unix-named copies the robocopy mirror dragged over from the Mac.
New-Item -ItemType Directory -Force -Path '${guest}\\${launcher}\\src-tauri\\bin' | Out-Null
Get-ChildItem '${guest}\\${launcher}\\src-tauri\\bin' -File -EA 0 |
  Where-Object { \$_.Extension -eq '' } | Remove-Item -Force -EA 0
Get-ChildItem '${guest}\\target\\${target}\\release\\*.exe' -EA 0 |
  ForEach-Object { Copy-Item \$_.FullName '${guest}\\${launcher}\\src-tauri\\bin\\' -Force }
if (Test-Path '${guest}\\config\\example.toml') {
  Copy-Item '${guest}\\config\\example.toml' '${guest}\\${launcher}\\src-tauri\\bin\\server-config.toml' -Force
}

Set-Location '${guest}\\${launcher}'
if (-not (Test-Path node_modules)) {
  Invoke-Step 'npm install' { npm install --silent --no-audit --no-fund }
}
# --no-bundle: we package on the Mac, see the header.
Invoke-Step 'tauri build' { npx --no-install tauri build --target ${target} --no-bundle }
Write-Output 'BUILD OK'
exit 0
PS1

  if ! psfile build.ps1 2>&1 | tail -18; then
    echo "    guest build windows-${label} FAILED"
    continue
  fi

  cat >"$staging/fetch.ps1" <<PS1
\$ErrorActionPreference = 'Continue'
\$dst = '\\\\psf\\Projects\\${reponame}\\dist-release\\win-${label}'
if (Test-Path \$dst) { Remove-Item \$dst -Recurse -Force }
New-Item -ItemType Directory -Force -Path "\$dst\\bin" | Out-Null

\$rel = '${guest}\\${launcher}\\src-tauri\\target\\${target}\\release'
\$app = Get-ChildItem "\$rel\\*.exe" -EA 0 |
        Where-Object { \$_.Name -notmatch 'build-script|\.d\$' } |
        Sort-Object Length -Descending | Select-Object -First 1
if (-not \$app) { Write-Output 'no launcher exe found'; exit 1 }

\$name = if ('${product}') { '${product}.exe' } else { \$app.Name }
Copy-Item \$app.FullName (Join-Path \$dst \$name) -Force
Write-Output \$name

Get-ChildItem '${guest}\\${launcher}\\src-tauri\\bin\\*' -EA 0 | ForEach-Object {
  Copy-Item \$_.FullName "\$dst\\bin\\" -Force
  Write-Output ('bin/' + \$_.Name)
}
exit 0
PS1

  echo "    retrieving windows-${label}"
  psfile fetch.ps1 | sed 's/^/      /'
done

rm -rf "$staging"
