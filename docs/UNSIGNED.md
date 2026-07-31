# Running unsigned Dante-BabelBox builds

Everything this project releases is built and published unsigned. This page explains
what each operating system will say, exactly how to get past it, and how to sign the
artifacts yourself if you'd rather not click through warnings.

## Why they're unsigned

Clearing these warnings for redistribution costs money, not effort:

| Platform | What's needed | Cost |
|---|---|---|
| macOS | Apple Developer Program + a *Developer ID Application* certificate, plus notarization of every build | $99/year |
| Windows | An Authenticode code-signing certificate (OV, or EV to skip SmartScreen reputation-building) | ~$200–500/year |

This project carries neither. Nothing is wrong with the downloads — the OS simply has
no publisher identity to check them against, so it assumes the worst once and then
remembers your answer.

> If you'd rather not trust a stranger's binary at all, every release is reproducible
> from source — see the build instructions in the README.

## macOS — Gatekeeper

macOS quarantines anything downloaded from a browser, then refuses to launch it because
**"the developer cannot be verified"**. Any one of these clears it:

**1. Right-click → Open** (simplest, per-app, once)

Right-click (or Control-click) the app → **Open** → **Open** in the dialog. The plain
double-click won't offer this — it has to be the context menu.

**2. Clear the quarantine flag** (scriptable, good for deployment)

```sh
xattr -dr com.apple.quarantine "/Applications/Dante-BabelBox.app"
```

**3. Approve it after the fact**

Launch it once, let macOS block it, then go to **System Settings → Privacy & Security**
and click **Open Anyway** next to the message about the blocked app.

### The CLI build

The command-line archive isn't an app bundle, so there's no right-click → Open. Extract
it, then:

```sh
xattr -dr com.apple.quarantine ./dante-babelbox
chmod +x ./dante-babelbox
./dante-babelbox --help
```

### `.pkg` installers

A `.pkg` from an unidentified developer is blocked the same way. Right-click → **Open**,
or clear the flag on the installer itself before running it:

```sh
xattr -dr com.apple.quarantine ~/Downloads/<file>.pkg
```

### Apple Silicon and the `.zip` trap

If you copy an app out of a `.zip` with Finder the quarantine flag comes with it. Prefer the
`.dmg` or `.pkg`, or run the `xattr` command above after copying.

## Windows — SmartScreen

**"Windows protected your PC — Microsoft Defender SmartScreen prevented an
unrecognised app from starting."** Click **More info**, confirm the publisher line reads
*Unknown publisher*, then **Run anyway**.

To clear the mark-of-the-web before running instead — useful when the block is silent
rather than a prompt:

```powershell
Unblock-File .\<file>.exe
```

Or right-click the file → **Properties** → tick **Unblock** → **OK**. If you downloaded a
`.zip`, **unblock the `.zip` first, then extract** — otherwise every extracted file
inherits the flag and you'll be unblocking them one at a time.

### Defender antivirus false positives

Unsigned binaries that bundle a runtime occasionally get quarantined outright by
Defender's heuristics rather than merely warned about. If the download vanishes from
your Downloads folder, check **Windows Security → Virus & threat protection →
Protection history** and choose **Restore**. Add an exclusion for the install folder if
it keeps happening.

## Windows — Defender Firewall

Dante-BabelBox listens on the network, so the first time it starts Windows shows:

> **Windows Defender Firewall has blocked some features of Dante-BabelBox**
> Allow it to communicate on these networks: ☐ Private ☐ Public

Tick **Private networks** — and **Domain networks** too if the machine is on a managed
domain. Dante-BabelBox needs this to serve the patch-bay UI and reach preamps and radio-mic receivers on the audio LAN.

If you deny it (or the prompt appears behind another window and times out), no devices will be discovered and gain/phantom changes won't reach the hardware

**Leave Public unticked** unless you know you need it — that profile covers untrusted
networks like conference or hotel Wi-Fi.

### If you already clicked Cancel

The prompt does not come back. Fix it in **Windows Security → Firewall & network
protection → Allow an app through firewall → Change settings**, find the entry, tick
**Private**. If Dante-BabelBox isn't listed, **Allow another app…** → **Browse** to the `.exe`.

Or from an elevated PowerShell:

```powershell
New-NetFirewallRule -DisplayName "Dante-BabelBox" -Direction Inbound -Program "C:\Path\To\Dante-BabelBox.exe" -Action Allow -Profile Private,Domain
```

To remove a bad rule and get the prompt back on next launch:

```powershell
Get-NetFirewallRule -DisplayName "*Dante-BabelBox*" | Remove-NetFirewallRule
```

### macOS and Linux firewalls

macOS shows an equivalent one-off prompt — *"Do you want the application to accept
incoming network connections?"* — click **Allow**. It's under **System Settings →
Network → Firewall → Options** if you need to change it later. On Linux nothing prompts;
if you run a firewall, open the ports yourself (`ufw allow <port>/tcp`).

## Linux

No signing gate. Make the binary executable if you took the tarball:

```sh
chmod +x ./dante-babelbox
```

The `.deb` and `.rpm` packages are unsigned too, so your package manager may object:
`sudo dpkg -i <file>.deb` or `sudo rpm -i --nosignature <file>.rpm`.

## Signing it yourself

### macOS — ad-hoc (local machine only)

An ad-hoc signature stops the OS re-prompting on **your own machine**. It is **not**
notarization and will do nothing for anyone else:

```sh
codesign --force --deep --sign - "/Applications/Dante-BabelBox.app"
```

Verify it took:

```sh
codesign -dv --verbose=4 "/Applications/Dante-BabelBox.app"
spctl -a -vv "/Applications/Dante-BabelBox.app"   # still reports "rejected" — ad-hoc is not notarization
```

### macOS — real signing and notarization

With an Apple Developer Program membership and a *Developer ID Application* certificate:

```sh
codesign --force --deep --options runtime --timestamp \
  --sign "Developer ID Application: Your Name (TEAMID)" "<artifact>"
xcrun notarytool submit "<artifact>.zip" --apple-id you@example.com \
  --team-id TEAMID --password "app-specific-password" --wait
xcrun stapler staple "<artifact>"
```

Note the **hardened runtime** (`--options runtime`) — notarization rejects builds without
it, and a hardened build with an ad-hoc signature won't launch at all.

### Windows — Authenticode

```powershell
signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 `
  /f mycert.pfx /p <password> .\<file>.exe
```

An OV certificate still needs to build SmartScreen reputation over time; an EV
certificate is trusted immediately. Neither is free.

## Verifying a download

Signing proves *who* built it; a checksum proves you got *what they built* — worth doing
even unsigned. Compare against the release notes:

```sh
shasum -a 256 <file>        # macOS / Linux
certutil -hashfile <file> SHA256   # Windows
```

You can also confirm the artifact came from this repo's CI by checking the release page it
was downloaded from — GitHub shows the workflow run that produced each asset.
