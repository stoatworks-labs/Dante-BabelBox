# Running Dante-BabelBox — Gatekeeper, SmartScreen and firewalls

macOS builds are signed and notarised, so they just open. The Windows
builds are unsigned and SmartScreen will object once. This page covers
that, the firewall prompts, and how to verify a download.

## Why the Windows builds are unsigned

macOS signing is covered: this project carries an Apple Developer Program
membership and a *Developer ID Application* certificate, and every macOS
artefact is notarised by Apple.

Windows is not. An Authenticode certificate (OV, or EV to skip building
SmartScreen reputation) runs ~$200-500/year, and the certificate authorities
will only issue one to a registered legal entity — which this project is not.
Nothing is wrong with the Windows downloads; Windows simply has no publisher
identity to check them against, so it assumes the worst once and then
remembers your answer.

> If you'd rather not trust a stranger's binary at all, every release is reproducible
> from source — see the build instructions in the README.

## macOS — nothing to do

Every macOS artefact is Developer ID-signed, notarised by Apple and stapled,
so it opens on a double-click with no warning and no quarantine step. That
covers the nested helper binaries inside the bundle too, which is what the
old right-click-Open workaround never did.

To confirm it for yourself:

```sh
spctl -a -vv -t install "/Applications/<app>.app"
# accepted / source=Notarized Developer ID
```

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
