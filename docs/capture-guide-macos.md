# Watching Two Boxes Talk — macOS Edition

*Field guide — network capture.* A step-by-step guide to recording the private
conversation between a mixing desk and its stagebox — using a laptop, two network
cables, and one free piece of software. No specialist network gear required.

No tap needed · No mirror switch needed · ~20 minutes · Bench setup only

Also available: [Windows edition](capture-guide-windows.md) · [Linux edition](capture-guide-linux.md)

## The idea, in one picture

Normally, your desk and stagebox talk directly to each other over a single cable (or
through a plain switch). For a few minutes, we're going to put your laptop directly in
the middle of that connection and tell it to quietly pass everything through unchanged
— like fitting a clear length of pipe into a hose. Wireshark then just watches what
flows past.

Every packet that would normally jump straight from desk to stagebox now physically
passes through your laptop first.

## Why we're doing this

Every desk-and-stagebox pair from the same manufacturer has a private handshake — how
they find each other, pair up, and agree on gain and phantom power — that's never been
written down publicly. To build tools that can join that conversation from outside, we
first have to record it happening between two devices that already trust each other.

> **⚠ Bench test, not showtime.** Do this on a spare desk-and-stagebox pair, before or
> after a show — never on a live network mid-event. Inserting anything into a
> production Dante network, even briefly, isn't worth the risk.

## What you'll need

| Item | Notes |
|---|---|
| The desk | Your console. **Required** |
| Its matching stagebox | Same brand and family as the desk — you want the pairing they already trust. **Required** |
| Two Ethernet cables | The ordinary ones you'd use anyway. **Required** |
| A laptop with two network ports | A built-in Ethernet port (or Thunderbolt adapter) plus a cheap USB-to-Ethernet adapter covers almost any Mac. **Required** |
| Wireshark, installed | Free at [wireshark.org](https://www.wireshark.org). The installer also installs ChmodBPF — say yes when it asks, or capture will need `sudo` every time. **Required** |
| A mirroring switch or tap | Only if you already own one — see [Alternatives](#already-own-a-mirroring-switch-or-a-tap) below. *Optional* |

## The setup, step by step

### 1. Wire it up

Run a cable from the desk's Ethernet port to **NIC A** on your laptop. Run a second
cable from **NIC B** on your laptop to the stagebox's Ethernet port. That's it — no
switch in between, your laptop *is* the connection now.

### 2. Bridge your two network ports

macOS can do this too — it's just tucked a little deeper than Windows, in a menu most
people never open. Open **System Settings → Network**, click the `···` button at the
bottom of the interface list (a gear icon on older macOS) and choose **Manage Virtual
Interfaces…**.

Click **+ → New Bridge**, tick both of your Ethernet interfaces — the built-in port and
your USB-to-Ethernet adapter — and click **Create**. macOS names the result `bridge0`.

Using Windows or Linux instead? There's a matching guide for each — the bridging step
works differently, but everything else here is the same.

### 3. Open Wireshark and pick your interface

Look for `bridge0` in Wireshark's interface list — that's the one to capture on.

If nothing shows up on `bridge0` once traffic is flowing, select both physical
interfaces together instead and start both — Wireshark merges them into one capture
automatically.

### 4. Start the capture first

Click the blue shark-fin **Start** button *before* powering anything on. This is what
catches the very first discovery packets — the part we're actually missing today.

### 5. Power everything on

Turn the stagebox and desk on in whatever order you'd normally use, then wait about 30
seconds for them to find each other over the network.

### 6. Pair them, like normal

From the desk's own screen, connect to the stagebox exactly as you always would. No
special settings — routine operation is exactly what we want recorded.

### 7. Wiggle a few things

Once, each: nudge a gain knob physically on the stagebox; nudge a different one from
the desk's on-screen control; toggle phantom power on a channel; then disconnect the
desk from the stagebox in software and reconnect it. That re-pairing moment is one of
the most useful bits in the whole capture.

### 8. Stop and save

Hit the red **Stop** square, then **File → Save As**. Give it a name that says what it
is — something like `ql1_tio1608_2026-07-14.pcapng` — and save.

### 9. Send it over, then tidy up

The `.pcapng` file on its own is enough — no need to trim or export anything yourself.
Afterward, undo the bridge: **System Settings → Network → Manage Virtual
Interfaces…**, select `bridge0`, and click the **–** button to delete it.

## Already own a mirroring switch or a tap?

Neither is necessary — the laptop-bridge method above works fine on its own — but if
you already have one of these, it's a perfectly good alternative to bridging.

**Managed switch with a mirror / SPAN port** — Plug the desk, stagebox, and your laptop
into the switch as normal, then configure one port to mirror the desk and stagebox's
traffic onto your laptop's port. Capture directly on that single NIC — no bridging
needed. *Needs admin access to the switch.*

**Hardware network tap** — A small dedicated box that sits inline between desk and
stagebox, just like the laptop-bridge method, and quietly copies every packet out a
third port to your laptop. The most bulletproof option, since it never routes the
devices' whole conversation through a general-purpose computer. *Extra kit to buy — not
required.*

## If something's not working

**I can't find "Manage Virtual Interfaces…".**
It's under the `···` (or gear) button at the very bottom of the Network settings
interface list — on older macOS versions it's a dropdown directly under the gear icon
rather than a separate button.

**Wireshark asks for my password, or the interface list is empty.**
The ChmodBPF helper wasn't installed. Re-run the Wireshark `.pkg` installer and make
sure the ChmodBPF step is ticked, then log out and back in.

**bridge0 shows no traffic.**
Capture on both individual physical interfaces at the same time instead — Wireshark
merges them into a single, correctly-ordered capture automatically.

**The desk and stagebox won't find each other through the laptop.**
Check **System Settings → Network → Firewall** isn't blocking the multicast traffic
discovery relies on, and try turning Wi-Fi off entirely so it can't interfere.

---

Part of the Dante-BabelBox emulation research. A finished `.pcapng` file is all we
need — send it over as-is.
