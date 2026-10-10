# Gain lock — gain compensation across desks

> **Status: tested against mock desks only.** The engine, the X32 and Wing trim paths and the
> web UI are covered by tests, and the whole bridge has been run against two mock X32 consoles
> over real UDP: lock, set a preamp from the UI, a trim change made at the follower, a trim at its
> limit, a follower dropping off the network and coming back. **No real desk has followed a leader
> yet.** See [Verifying on a real rig](#verifying-on-a-real-rig) before relying on it at a show.

## What it does

Several desks share one set of stage-box preamps. One of them is the **leader**: it owns the
preamps' gain. The others are **followers**: they receive the same signals after the gain stage,
so when the leader turns a preamp up 3 dB, every follower hears that input 3 dB louder.

With the system **locked**, each follower's digital trim on that input takes the opposite move
automatically. Monitor and broadcast levels stay where their engineers set them, however the FOH
engineer rides the gain during the show. It's the cross-vendor version of what desks do among
themselves: Yamaha's Gain Compensation, Allen & Heath's and DiGiCo's Gain Tracking.

1. Add the desks as devices, as for any other mapping.
2. Name the **signals** they share: which preamp each signal is on the leader, and which input
   channel carries it on each follower.
3. Set the preamp gains, from the leader desk or from the bridge's web UI.
4. Press **Lock**. From then on every change to a leader preamp, made from its own surface, from
   any other controller or from the bridge, moves the followers' trims the other way.

## Three rules the engine keeps

**Deltas, never targets.** The engine never works out "the monitor trim should be −4 dB". A leader
move of `+d` becomes a relative move of `−d` on each follower, applied to whatever that follower
holds *at that moment*. If the monitor engineer pulls a vocal down 2 dB mid-show, that 2 dB stays:
the next compensation moves the trim from where they left it.

**Nothing is dropped.** A follower can't always take the whole move. A trim may be at the end of
its range, a trim step may be coarser than the leader's (a dLive preamp moves in 0.43 dB steps,
an X32 trim in 0.25 dB), or a desk may not answer. Whatever it can't take is kept as **owed** and
offered again with the next move. A desk that stops answering is **held**: its followers keep
accruing what they're owed without the bridge waiting on it, so a dead monitor desk can't slow
compensation for the broadcast desk. It's probed every 2 seconds, and when it answers it gets
everything it missed in one move. So for every follower,
`applied + owed = −(leader gain now − leader gain at lock)`. When the leader comes back
down, a trim that was pinned at its limit is not pushed past where it started.

**The leader's reported value is the only input.** Compensation follows the gain the engine has
seen the leader report, whoever moved it. The same value reported twice is a zero move, so an echo
can never double a compensation. A late report of a value the engine itself read or set is
recognised and ignored. On X32, Wing and dLive, a reply to a read looks exactly like a change and
arrives tens of milliseconds later. Taking it as a change would undo a gain set made in that
window.

## Configuring it

The gain lock is a `[gain_lock]` table in `bridge.toml`, alongside the devices:

```toml
[[device]]
id = "foh"
kind = "dlive-tcp"
address = "10.0.0.10"

[[device]]
id = "mon"
kind = "osc-x32"
address = "10.0.0.20"

[[device]]
id = "bcast"
kind = "osc-wing"
address = "10.0.0.30"

[gain_lock]
leader = "foh"
# lock_on_start = true   # lock as soon as the devices connect: a bridge restarted
#                        # mid-show protects the followers again by itself
# poll_ms = 500          # also re-read the leader's gains on this interval, for a
#                        # leader whose protocol doesn't push changes

[[gain_lock.signal]]
name = "Kick In"
members = [
  { device = "foh",   preamp = 1 },   # dLive MixRack socket 1
  { device = "mon",   input = 1 },    # X32 input channel 1
  { device = "bcast", input = 9 },    # Wing channel 9
]

[[gain_lock.signal]]
name = "Lead Vox"
members = [
  { device = "foh", preamp = 33 },
  { device = "mon", input = 17 },
]
```

- **`preamp`** is the desk's preamp in its adapter's own numbering: an X32 headamp (1–128), a
  dLive socket, a DM3 local input. See the channel-numbering notes in [USAGE.md](../USAGE.md).
  A member needs one to be able to lead the signal.
- **`input`** is the input channel whose trim carries the signal on that desk. A member needs one
  to follow.
- A member can have both. Then whichever desk is elected leader drives the others, and the
  leader's own trim is never touched.

The table is checked at load time, and the bridge refuses to start on:

- a device id that isn't configured;
- a signal with no preamp on the configured leader;
- a member with neither a preamp nor an input;
- one follower input in two signals;
- a signal name used twice.

The table is read at startup only. A config hot-reload applies mapping changes, not this.

## The web UI

The patch-bay UI gets a **Gain lock** tab:

- **Leader**: elect any desk that has a preamp in some signal. Disabled while locked, because
  changing who leads mid-lock would make the new leader's first reading look like a jump by
  however much its preamps differ from the old one's.
- **Lock / LOCKED**: captures every signal's current leader gain as the reference. Unlocking asks
  first, and leaves the trims where the lock put them.
- **Per signal**: the leader preamp, its gain with −1 / +1 and a set-to box, the drift since lock,
  and each follower's net trim change. A follower turns amber when it is more than 0.25 dB
  **short** (its trim is at a limit) and red when its desk is failing or held (hover for why).

The same controls are on the HTTP API. See [API.md](API.md#gain-lock).

## Which desks can do what

**Leader.** Any device whose adapter reports a preamp gain can lead. That's X32/M32, Wing (local
preamps), dLive, AHM, DM3 (SCP or OSC), AES70 devices and the Sennheiser EW G3 adapter.

How well a leader is *followed* depends on whether its protocol pushes changes:

- **X32** (`/xremote`), **Wing** (`/*s` subscription) and **DM3 over SCP** (`NOTIFY`) report
  every change as it happens.
- **dLive** is assumed to push preamp changes. Its adapter's module comment explains why that is
  an assumption.
- **DM3 over OSC** sends nothing unsolicited. Set `poll_ms` for it.

**Follower.** This needs a per-input-channel digital trim the protocol can write. Surveyed on
2026-10-10:

| Desk | Trim the bridge can move | Relative command? | Follower here |
|---|---|---|---|
| X32 / M32 / HD96 | `/ch/[01…32]/preamp/trim`, −18…+18 dB in 0.25 dB steps — **"digital sources only"** (see below) | none | **yes** |
| Wing | `/ch/[1…40]/in/set/trim`, −18…+18 dB in 0.1 dB steps; works whatever the source | OSC: none. Native protocol (TCP 2222): a step token `0xd9`, step size and limit behaviour undocumented | **yes** (OSC) |
| Yamaha DM3 | none — the console's own 177-parameter SCP dictionary (walked on a real DM3) has no per-channel digital gain | — | no |
| Yamaha RIVAGE PM | `MIXER:Current/InCh/DigitalGain`, −96…+24 dB in 0.01 dB units (seen only in Companion's parameter dump) | `setr` exists on MTX/MRX; untested on consoles | no adapter yet |
| Yamaha CL/QL, TF, DM7 | address unknown — find it by walking `prminfo` on a desk | as above | no adapter yet |
| A&H dLive / Avantis | not in the MIDI/TCP protocol | — | no |
| A&H SQ, Qu-5/6/7 | not in the protocol | NRPN increment/decrement exists, for levels and pans only | no |
| A&H AHM | Input Trim, NRPN `0x18`, −24…+24 dB over 7 bits — the curve isn't printed | increment/decrement for levels only | not yet |
| DiGiCo SD / Quantum | `…/Channel_Input/trim` over OSC (Companion's table only) | none | no adapter yet |

So the "increment/decrement where possible" goal comes down to this. None of the protocols this
bridge speaks has a documented relative command for trim. Every follower move is therefore
**read-then-write**: the adapter reads the desk's live trim (never a cached value, which a missed
push could have left stale) and writes the moved value straight after. The effect is the one a
relative command would have. A trim change made at the follower survives, because the move starts
from what the desk holds now. Wing's native step token is the one real relative command, and it's
left for a bench session that can find out what one step is worth.

### X32 as a follower: "digital sources only"

The X32 document gives the channel trim for digital sources only. A channel fed by a local or
AES50 headamp shows headamp gain in that place instead. Forum reports, not the document, say that
turning on **HA gain split** for the shared AES50 port (`/-prefs/haflags`) gives such channels
their own ±18 dB trim. That's the typical FOH/monitor split, and it's unverified. Check it on the
desk: change the leader's preamp and watch the follower's trim move.

## Verifying on a real rig

What to run the first time real desks are connected. Record the results in `docs/NOTES.md` and
update the status line at the top of this file.

1. **Wire check, leader.** Turn a preamp on the leader's surface. The bridge's Gain lock tab
   should show the new gain within a second. If it doesn't, the leader isn't pushing: set
   `poll_ms`.
2. **Wire check, follower.** Lock, then press +1 on a signal in the UI. The follower's trim on
   that input should drop 1 dB on its own screen.
3. **The point of it.** On the follower, trim a channel down 2 dB by hand. Then raise the leader
   preamp 3 dB. The follower's trim should end up 5 dB down, not 3.
4. **The limit.** Put a follower trim near its floor and raise the leader past it. The UI should
   show the follower *short*. Bring the leader back to where it was locked: the trim should
   return exactly to where it started.
5. **A reboot.** Power-cycle a follower mid-lock and move the leader while it's down. When it's
   back, the owed amount should land within a couple of seconds.
6. **X32 with headamp sources** (see above): does a trim write move the level on a channel fed by
   a shared AES50 headamp, with and without HA gain split?

## Code

- `crates/core/src/gain_lock.rs`: the engine (`GainLock`), its config and status types.
- `crates/core/src/law.rs`: `DbLaw`, the range-and-step arithmetic of a relative move.
- `DeviceAdapter::{trim_inputs, get_trim, set_trim, nudge_trim}`,
  `LocalAdapter::nudge_object` and `PluginAdapter::nudge_object`: the trim surface through the
  plugin stack. Trim objects are role `"In N Trim"`, at `channel_scheme::TRIM_ONO_BASE + N`.
- `crates/preamp-adapter-osc/src/{x32,wing}.rs`: the two follower implementations.
