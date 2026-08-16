# `plugin-yamaha-mbc` — design sketch

What it would take to expose `MbcAdapter` (Rio/Tio head amps over Audinate
ConMon) as a bridge `kind`, so a `[[mapping]]` can drive a Rio from another
console. Companion to [`yamaha-ha-remote-over-dante.md`](yamaha-ha-remote-over-dante.md)
and [`plugin-development-guide.md`](plugin-development-guide.md).

> **Read [`yamaha-scp-r-remote.md`](yamaha-scp-r-remote.md) first.** R Remote
> controls Rio head amps over **SCP on plain TCP**, not the `MBC` block — a far
> easier path to the same device, and one that avoids every config problem
> described below. This sketch remains valid for the console-emulation case, and
> the config-shape gap it identifies is general (Aphex hits it too), but SCP is
> the recommended route to a Rio.

**Status: not written.** This is the gap between "the MBC codec is
hardware-proven" and "the bridge can route to a Rio".

Worth being precise about how wide that gap is: `MbcAdapter` is referenced
**nowhere outside its own crate** — no plugin, no `preamp-cli` in-process path,
no bridge kind. It is library code plus its own tests, reachable from no binary
the project ships. The protocol work is done; none of it is currently wired to
anything a user can run.

## The good news

`MbcAdapter` already implements `DeviceAdapter` in full — `connect`,
`disconnect`, `identify`, `set_gain`, `set_phantom`, `get_state`, and a
`broadcast::Receiver<PreampEvent>` from `subscribe()`. So this plugin is a thin
wiring crate exactly like [`plugin-yamaha-dm3`](../crates/plugin-yamaha-dm3/src/lib.rs):
`plugin_info` + `create_adapter` + `LegacyPluginBridge`, and nothing else.

Copy that crate. It is ~50 lines of real code.

## The one real blocker: config shape

`MbcAdapter::new` needs:

```rust
MbcAdapter::new(id, identity: MbcIdentity, interface: Ipv4Addr)

pub struct MbcIdentity {
    pub src_mac: [u8; 6],        // Yamaha MAC to send as (the console's)
    pub dst_mac: [u8; 6],        // Yamaha MAC of the head-amp device
    pub sender_eui64: [u8; 8],   // ConMon envelope id
    pub message_class: u32,      // console vs device class
}
```

`RDeviceConfig` carries `{ id, address, port, channels }`. **There is nowhere to
put three MAC-shaped fields.** Every other plugin so far has been an
address/port device, so this has never come up.

Note also that MBC's destination is a fixed multicast group/port
(`CONMON_STATUS_GROUP` / `CONMON_STATUS_PORT`), so `port` is meaningless here, and
`address` would have to mean the **local interface to send from and join the group
on** — the opposite of what `address` means for every other kind. That
inconsistency alone argues against smuggling it through the existing fields.

### Options

| | Approach | Verdict |
|---|---|---|
| **a** | Add an options map to `RDeviceConfig` — e.g. `options: RVec<Tuple2<RString, RString>>`, populated from unknown keys in the `[[device]]` table | **Recommended.** |
| b | Encode MACs into the `address` string (`"169.254.1.5;dst=00:1d:c1:25:df:04"`) | Non-breaking but grim; unparseable by `init --infer-mappings` and by humans. |
| c | Plugin reads its own sidecar file keyed by device id | Legitimate stopgap for a bench prototype; splits config in two and breaks `preamp-bridge init`. |

**Why (a):** MBC will not be the last device that needs more than address/port —
Aphex needs MIDI channel / device / net number, and `aes70` already has to
*ignore* `channels` because a configured count could only contradict the device.
A general escape hatch is overdue.

It is an ABI break (the struct is `#[repr(C)]`), so every plugin needs rebuilding —
but `abi_stable` detects the layout mismatch and refuses to load a stale plugin
rather than misreading it, so it fails loudly and safely. Plugins ship versioned
with the app, so the blast radius is a rebuild, not a compatibility matrix.

Core's `DeviceConfig` needs the matching change. It currently has no
`#[serde(flatten)]` catch-all, so **extra keys in `bridge.toml` are silently
dropped today** — worth fixing regardless, since a typo'd key is currently
invisible.

## Sketch

Assuming (a). Contingent parts marked.

```rust
const KIND: &str = "yamaha-mbc";
/// Rio3224-D2 head-amp array width. Fixed by the protocol (HEADAMP_CHANNELS),
/// not a preference — a configured `channels` is validated against it, not
/// used to override it.
const CHANNELS: u16 = 32;

#[sabi_extern_fn]
fn plugin_info() -> RPluginInfo {
    RPluginInfo {
        name: "yamaha-mbc".into(),
        vendor: "Yamaha".into(),
        supported_kinds: RVec::from(vec![RString::from(KIND)]),
    }
}

#[sabi_extern_fn]
fn create_adapter(config: RDeviceConfig) -> RResult<PluginAdapterBox, RString> {
    // `address` = the LOCAL interface to send from and join the multicast
    // group on. Not the device — MBC's destination is a fixed group/port.
    let interface: Ipv4Addr = /* parse config.address, required */;

    // [contingent on option (a)]
    let src_mac      = /* opts["src_mac"]      — required, no default */;
    let dst_mac      = /* opts["dst_mac"]      — required */;
    let sender_eui64 = /* opts["sender_eui64"] — required; see derivation note */;

    let identity = MbcIdentity {
        src_mac, dst_mac, sender_eui64,
        message_class: MESSAGE_CLASS_CONSOLE,
    };

    let adapter = MbcAdapter::new(config.id.into_string(), identity, interface);
    let bridge = LegacyPluginBridge::new(Box::new(adapter), CHANNELS);
    RResult::ROk(PluginAdapter_TO::from_value(bridge, TD_Opaque))
}
```

### Do not default the identity

`MbcIdentity::reference_ql1()` carries **the actual Yamaha MAC of the QL1 in the
capture**. It is there to reproduce the verified write in tests. Shipping it as a
runtime default would make every deployment impersonate one specific real
console — a MAC collision waiting to happen if that unit is ever on the same
network, and a real device's identity baked into a public binary. Require all
three fields explicitly; fail with a clear error if absent.

### The EUI-64 derivation trap

The two devices derive `sender_eui64` **differently**: the QL1 pads its MAC with
zeroes (`00:1d:c1:17:ea:2c` → `001dc117ea2c0000`) while the Rio uses standard
`fffe` insertion (`001dc1fffe25df04`). Since the plugin sends *as a console*, it
must pad with zeroes. Do not compute it from `src_mac` with a generic EUI-64
helper — copy what the impersonated device actually sends. Worth a named
constructor and a test rather than a comment.

## Config it would enable

```toml
[[device]]
id = "rio-stage"
kind = "yamaha-mbc"
address = "169.254.1.5"          # local Dante NIC, not the Rio
src_mac = "00:a0:de:xx:xx:xx"    # console identity to present
dst_mac = "00:1d:c1:25:df:04"    # the Rio's Yamaha MAC
sender_eui64 = "00a0dexxxxxx0000"

[[mapping]]
from = { device = "sq-foh", channel = 1 }
to   = { device = "rio-stage", channel = 1 }
```

There is **no discovery shortcut** for `dst_mac`: the Rio answers nothing on
Audinate's control ports (4440/4444/4455/8800), so its Yamaha MAC has to come
from mDNS plus a ConMon observation, or from configuration. A `preamp-cli`
subcommand that watches the ConMon status group and prints observed Yamaha MACs
would make this usable; without it, the user is in Wireshark.

Note the console has **two NICs** and MBC addresses by the *Yamaha* MAC, not the
Dante one. Filtering or configuring by the Dante MAC misses every head-amp
message.

## What this still doesn't settle

- **Whether a Rio accepts MBC cold.** The §9 hardware proof was a Rio *already
  paired with a live QL1*. Presenting `MESSAGE_CLASS_CONSOLE` with a console
  EUI-64 is the design's answer to that, but whether the Rio requires a prior
  pairing exchange — not just a plausible source identity — is untested. This is
  the single risk that could make the whole path a dead end, and it needs a Rio to
  answer. Do not let the plugin's existence imply it is settled.
- **`pad`, HPF, polarity, digital trim** stay `None`. Their array widths are known
  from §6 but every observed value was a resting default, so nothing identifies
  which array is which.
- **Metering stays uncalibrated** — raw bytes, deliberately.

## Checklist

- [ ] Decide the config-shape option; if (a), land the ABI + `DeviceConfig` change first
- [ ] `crates/plugin-yamaha-mbc`, copied from `plugin-yamaha-dm3`
- [ ] Reject missing/malformed identity fields with actionable errors
- [ ] Zero-padded EUI-64 constructor + test
- [ ] Mirror DM3's three tests: kind declaration, config rejection, cdylib loads
- [ ] Round-trip test against a mock listener on the ConMon group, asserting the
      emitted frame matches the byte-for-byte packet in
      `ql1-rio3224d2-write-test-accepted.pcap`
- [ ] Register the kind in `bridge.example.toml` and drop `"yamaha"` from the
      not-implemented list
- [ ] README status table: note bridge-reachable, still never run against a device
