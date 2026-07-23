# Writing a Device Plugin

*Reference guide for community developers.* How to add support for a new
device by building a real `.so`/`.dylib`/`.dll` that Dante-BabelBox loads
at runtime — no fork, no recompiling the host, no touching this repo's
own crates at all (though a PR is very welcome once it works).

This complements, rather than replaces, the existing
"[Contributing a new adapter](../README.md#contributing-a-new-adapter)"
path (implementing `DeviceAdapter`/`MicAdapter` directly, compiled into
the workspace). Both are legitimate ways to add device support; this
guide is specifically for the plugin path, which is the only option if
you want to ship support for your device without this project ever
seeing your code.

The canonical, fully working example this guide describes is
[`crates/plugin-osc-x32`](../crates/plugin-osc-x32) — the X32-family OSC
driver, built as a real plugin and tested end to end (including loading
the actual compiled `.dylib` back through `abi_stable`'s own loader).
Keep it open alongside this guide; every code snippet here is lifted
from it or its sibling crates, not invented for the doc.

## Two crates you need to know about

| Crate | What it is | You depend on it |
|---|---|---|
| [`dante-babelbox-oca`](../crates/oca) | The internal object model: `Ono`, `OcaClass`, `OcaValue`, `OcaObject`, `OcaObjectDescriptor`, `OcaAddress`, `OcaEvent`. Plain Rust, no FFI concerns. | Yes |
| [`dante-babelbox-oca-plugin-abi`](../crates/oca-plugin-abi) | The FFI-safe contract: `PluginRootModule` + the `PluginAdapter` trait, built on [`abi_stable`](https://docs.rs/abi_stable). | Yes |

Everything else in the host (`PluginRegistry`, `Router`, the web UI) is
plumbing you never touch — your plugin only ever talks to these two
crates.

## The mental model: OCA is the lingua franca

Every device — whatever vendor, whatever wire protocol — is modeled the
same way once it's inside Dante-BabelBox: as a flat list of **objects**,
each one a single controllable or observable value. A gain knob is an
object. A mute button is an object. A battery-percent reading is an
object. There's no bundled "channel state" struct anymore (there used to
be, for the preamp domain specifically — `PreampState{gain_db, phantom,
pad}` — this OCA model replaced it precisely so a plugin isn't forced
into that preamp-shaped mold).

> **Not a wire protocol.** This is an *internal* model only — there's no
> OCP.1 (real AES70) traffic anywhere in this project today. The name and
> class taxonomy are borrowed from the AES70/OCA standard because it's a
> genuinely good fit (confirmed against a real AES70 implementation, see
> [`crates/oca/src/lib.rs`](../crates/oca/src/lib.rs)'s doc comment) — not
> because your plugin needs to know anything about real OCP.1 framing.

The types, from [`dante-babelbox-oca`](../crates/oca/src/lib.rs):

```rust
pub struct Ono(pub u32);   // an object's identity - yours to assign, see below

pub enum OcaClass {
    Gain, Mute, Switch, Polarity, Delay,             // actuators (settable)
    BasicSensor, LevelSensor, AudioLevelSensor,
    BooleanSensor, Int32Sensor, StringSensor,         // sensors (read-only)
}

pub enum OcaValue { F32(f32), I32(i32), Bool(bool), String(String) }

pub struct OcaObjectDescriptor {
    pub ono: Ono,
    pub class: OcaClass,
    pub role: String,      // human label, e.g. "Ch 3 Gain" - see below, this matters
    pub settable: bool,    // false for sensor-only telemetry
}

pub struct OcaObject {           // a descriptor plus its current value
    pub ono: Ono, pub class: OcaClass, pub role: String, pub settable: bool,
    pub value: OcaValue,
}
```

`OcaClass` is a small, curated set — the classes this project has
actually needed so far, not an attempt at full AES70-1 coverage. If your
device genuinely needs a concept none of these eleven variants fit,
that's a real gap: the enum isn't user-extensible from outside this
repo (see "Known limitations" below), so open an issue or PR rather than
picking the closest-enough variant and hoping.

### Assigning `Ono`s

An `Ono` only has to be unique *within one device instance* — it's not a
protocol-level identity, just how the host and your plugin refer to "this
particular object" across `get_object`/`set_object`/`describe`/events.
The simplest approach, and the one every adapter in this project uses, is
a small deterministic formula over channel number and field, e.g. (from
[`crates/core/src/channel_scheme.rs`](../crates/core/src/channel_scheme.rs)):

```rust
fn gain_ono(channel: u16) -> Ono { Ono(3 * (channel as u32 - 1) + 1) }
fn phantom_ono(channel: u16) -> Ono { Ono(3 * (channel as u32 - 1) + 2) }
fn pad_ono(channel: u16) -> Ono { Ono(3 * (channel as u32 - 1) + 3) }
```

Pick whatever formula fits how many fields-per-channel your device has;
just make sure it's a pure function you can invert (decode an incoming
`Ono` back to "which channel, which field") without needing any state.

### Role strings are load-bearing, not just labels

The host resolves a `bridge.toml`/web-API mapping like:

```toml
[[mapping]]
from = { device = "my-plugin-device", channel = 3 }
to   = { device = "some-other-device", channel = 7 }
```

into actual `Ono`-level `Mapping`s by looking, on **each side
independently**, for a `settable` descriptor whose `role` is exactly
`"Ch {channel} Gain"` or `"Ch {channel} Phantom"` — see
[`crates/core/src/channel_mapping.rs`](../crates/core/src/channel_mapping.rs):

```rust
let from_role = |suffix: &str| format!("Ch {} {}", mapping.from.channel, suffix);
let to_role   = |suffix: &str| format!("Ch {} {}", mapping.to.channel, suffix);

["Gain", "Phantom"].into_iter().filter_map(|field| {
    let from_ono = from_descriptors.iter().find(|d| d.settable && d.role == from_role(field))?.ono;
    let to_ono   = to_descriptors.iter().find(|d| d.settable && d.role == to_role(field))?.ono;
    Some(Mapping { from: OcaAddress::new(..., from_ono), to: OcaAddress::new(..., to_ono), bidirectional })
})
```

**This is deliberately narrow today** — it only ever looks for `"Gain"`
and `"Phantom"`, nothing else, hardcoded. If your device exposes a
gain-equivalent and/or phantom-equivalent control, use exactly this role
format (`"Ch {channel} Gain"` / `"Ch {channel} Phantom"`, 1-based channel
number) so it participates in the existing config-file/web-UI mapping
syntax automatically. If your plugin is telemetry-only, or has a control
surface that doesn't fit "gain and/or phantom per channel," that's fine —
your objects are still fully addressable by `Ono` directly — but they
won't be found by today's device+channel mapping shorthand. Widening
`resolve()` to match arbitrary role names is a reasonable future
extension; it just doesn't exist yet, and this guide won't pretend
otherwise.

## The FFI contract

`abi_stable` gives you two different mechanisms, used for two different
things here — don't conflate them:

- **`PluginRootModule`** — a "prefix type" (a `#[repr(C)]` struct of
  `extern "C" fn` pointers). Loaded exactly once when the host opens your
  `.so`/`.dylib`/`.dll`. This is the *only* symbol your plugin exports.
- **`PluginAdapter`** — a `#[sabi_trait]`-generated FFI-safe trait object.
  Your root module's `create_adapter` function builds one of these per
  connected device instance. All the actual device-talking happens
  through this.

From [`crates/oca-plugin-abi/src/lib.rs`](../crates/oca-plugin-abi/src/lib.rs):

```rust
#[repr(C)]
#[derive(StableAbi)]
#[sabi(kind(Prefix(prefix_ref = PluginRootModule_Ref)))]
#[sabi(missing_field(panic))]
pub struct PluginRootModule {
    pub plugin_info: extern "C" fn() -> RPluginInfo,
    #[sabi(last_prefix_field)]
    pub create_adapter: extern "C" fn(RDeviceConfig) -> RResult<PluginAdapterBox, RString>,
}

#[sabi_trait]
pub trait PluginAdapter: Send + Sync {
    fn id(&self) -> RString;
    fn connect(&mut self) -> RResult<(), RString>;
    fn disconnect(&mut self) -> RResult<(), RString>;
    fn identify(&mut self) -> RResult<RDeviceInfo, RString>;
    fn describe(&self) -> RVec<OcaObjectDescriptorFfi>;
    fn get_object(&mut self, ono: u32) -> RResult<OcaValueFfi, RString>;
    fn set_object(&mut self, ono: u32, value: OcaValueFfi) -> RResult<(), RString>;
    #[sabi(last_prefix_field)]
    fn poll_events(&mut self) -> RVec<OcaEventFfi>;
}
```

`RDeviceConfig`/`RDeviceInfo`/`RPluginInfo`/`OcaObjectDescriptorFfi`/
`OcaValueFfi`/`OcaEventFfi` are `#[repr(C)]`/`StableAbi` mirrors of plain
types you already know from `dante-babelbox-oca` (`OcaValueFfi` mirrors
`OcaValue`, etc.) plus `RString`/`RVec`/`ROption`/`RResult` — `abi_stable`'s
FFI-safe stand-ins for `String`/`Vec`/`Option`/`Result`. Conversions both
ways are just `.into()` in practice.

**Everything here is synchronous.** Async doesn't cross an `abi_stable`
boundary cleanly, so there's no `async fn` anywhere in `PluginAdapter`.
If your device talk is naturally async (most `tokio`-based network code
is), you bridge it yourself — see the next section.

## Bridging async device I/O into a sync FFI trait

`crates/plugin-osc-x32` wraps the workspace's own async
`X32Adapter` (`dante_babelbox_preamp_adapter_osc::X32Adapter`, which
implements the older in-process `DeviceAdapter` trait). Each
`PluginAdapter` instance owns its **own Tokio runtime**:

```rust
struct X32PluginAdapter {
    id: String,
    inner: X32Adapter,
    runtime: Runtime,
    events: Arc<StdMutex<VecDeque<OcaEventFfi>>>,
}
```

`connect`/`disconnect`/`get_object`/`set_object` just `block_on` the
inner async call:

```rust
fn connect(&mut self) -> RResult<(), RString> {
    match self.runtime.block_on(self.inner.connect()) {
        Ok(()) => RResult::ROk(()),
        Err(e) => RResult::RErr(e.to_string().into()),
    }
}
```

> **The pitfall that actually happened building this.** The first version
> used `tokio::runtime::Builder::new_current_thread()`. It compiled, it
> passed a quick smoke test, and it was wrong: a **current-thread**
> runtime only drives spawned background tasks (the device's receive
> loop, a heartbeat) *while something is actively inside a `block_on` call
> on that runtime*. `poll_events` — which the host calls on its own timer,
> never through `block_on` — gave those tasks no chance to run in between,
> so inbound telemetry only appeared, unreliably, as a side effect of
> whatever `block_on` call happened to run next. Manual testing against a
> real mock socket caught it (it's exactly the kind of race a fast unit
> test won't reproduce reliably). The fix: use
> `Builder::new_multi_thread()` so background tasks keep running on their
> own worker thread regardless of what the FFI-calling thread is doing:
>
> ```rust
> let runtime = tokio::runtime::Builder::new_multi_thread()
>     .worker_threads(2)
>     .enable_all()
>     .build()
>     .expect("building the plugin's Tokio runtime");
> ```
>
> If your device I/O is genuinely synchronous (blocking sockets, no
> `tokio`), you don't need a runtime at all — a plain background
> `std::thread` reading the socket and pushing into the same kind of
> queue works just as well.

### `poll_events` — drain, never block

Telemetry/state-change events (a knob turned on the physical device, a
mute button pressed) don't push through a channel across the FFI
boundary — there's nowhere for a callback or a `Receiver` to live safely
on the other side. Instead, your background task pushes translated
`OcaEventFfi`s into a queue, and `poll_events` drains it on each call:

```rust
runtime.spawn(async move {
    loop {
        match rx.recv().await {
            Ok(event) => {
                let mut queue = events_for_task.lock().unwrap();
                queue.push_back(OcaEventFfi::from_event(device_id.clone(), /* ... */));
            }
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break,
        }
    }
});

// ...
fn poll_events(&mut self) -> RVec<OcaEventFfi> {
    let mut queue = self.events.lock().unwrap();
    queue.drain(..).collect::<Vec<_>>().into()
}
```

The host (`dante_babelbox_core::plugin_registry::DylibAdapter`) polls this
every 50ms on a thread dedicated to your device instance and republishes
whatever it finds as a normal `broadcast::Sender<OcaEvent>`, so the rest
of the host (the `Router`, the web UI) never has to know your device is
plugin-backed at all.

## Building the plugin crate

`Cargo.toml` — from
[`crates/plugin-osc-x32/Cargo.toml`](../crates/plugin-osc-x32/Cargo.toml):

```toml
[package]
name = "your-plugin-name"
version = "0.1.0"
edition = "2021"

[lib]
name = "your_plugin_name"          # becomes lib<name>.{dylib,so} / <name>.dll
crate-type = ["cdylib", "rlib"]    # rlib too - a pure cdylib can't be linked as a test binary

[dependencies]
dante-babelbox-oca = "..."
dante-babelbox-oca-plugin-abi = "..."
abi_stable = "0.11"
tokio = { version = "1", features = ["full"] }   # if your device I/O is async
```

Implement `PluginAdapter` for your concrete struct (a plain `impl`, not
against the generated `_TO` wrapper), then export the root module:

```rust
use abi_stable::{export_root_module, prefix_type::PrefixTypeTrait, sabi_extern_fn,
                  sabi_trait::prelude::TD_Opaque, std_types::{RResult, RString, RVec}};

#[sabi_extern_fn]
fn plugin_info() -> RPluginInfo {
    RPluginInfo {
        name: "my-device".into(),
        vendor: "Some Vendor".into(),
        supported_kinds: RVec::from(vec![RString::from("my-kind-id")]),
    }
}

#[sabi_extern_fn]
fn create_adapter(config: RDeviceConfig) -> RResult<PluginAdapterBox, RString> {
    let Some(address) = config.address.into_option() else {
        return RResult::RErr("my-kind-id requires an address".into());
    };
    // ... parse address/port, construct your adapter struct ...
    RResult::ROk(PluginAdapter_TO::from_value(my_adapter, TD_Opaque))
}

#[export_root_module]
pub fn get_library() -> PluginRootModule_Ref {
    PluginRootModule { plugin_info, create_adapter }.leak_into_prefix()
}
```

`supported_kinds` is the list of `DeviceConfig.kind` strings (what a
`bridge.toml` `[[device]] kind = "..."` line names) your plugin claims —
usually just one. This is what replaces the old closed `DeviceKind` enum:
kind ids are just strings now, open-ended, declared by whoever registers
them (a plugin, or one of the host's own statically-registered legacy
adapters).

`TD_Opaque` (vs. `TD_CanDowncast`) means the resulting trait object can't
be downcast back to your concrete type on the host side — correct here,
since the host never needs to, and it's the simpler/safer choice.

### Deploying it

```sh
cargo build --release -p your-plugin-name
mkdir -p plugins
cp target/release/libyour_plugin_name.dylib plugins/   # .so on Linux, .dll on Windows
preamp-bridge run --config bridge.toml --plugins-dir plugins
```

The host scans `--plugins-dir` (default `plugins`, relative to wherever
it's run) for `.so`/`.dylib`/`.dll` files at startup, loads each one via
`abi_stable`'s own loader, and registers every kind id it declares. A file
that fails to load (wrong ABI version, not a plugin at all, missing
symbol) is logged and skipped — one bad file never brings the host down.

## Testing

Two tiers, matching `plugin-osc-x32`'s own test suite:

1. **Direct unit tests against your concrete struct** — the bulk of your
   coverage. No FFI boundary involved; just call your `impl PluginAdapter`
   methods directly (`PluginAdapter::describe(&plugin)`,
   `plugin.connect()`, etc.) against a mock socket standing in for the
   real device, exactly like any other adapter test in this project.
2. **One "does the actual cdylib load" test** — real proof the FFI
   boundary itself works, not just your Rust logic:

   ```rust
   #[test]
   fn the_built_cdylib_loads_through_abi_stables_own_loader() {
       let path = /* path to the built lib*.dylib/.so or *.dll */;
       let root = PluginRootModule_Ref::load_from_file(&path).expect("loading the plugin");
       let info = root.plugin_info()();
       assert_eq!(info.supported_kinds.as_slice(), &[RString::from("my-kind-id")]);
   }
   ```

   `cargo test` builds your crate's `cdylib` artifact as a side effect of
   building the `rlib` test binary, so the file is already there by the
   time this test runs (see `plugin-osc-x32`'s version of this test for
   the exact path-resolution logic, which handles the case where it
   isn't).

## Known limitations

- **Toolchain matching.** The host and every plugin must be built with
  the *same* Rust compiler version at each release. `abi_stable`
  stabilizes the interface's *shape* (and refuses to load a mismatched
  plugin rather than risk undefined behaviour) — it doesn't solve Rust's
  lack of a stable compiler ABI, which is a permanent constraint of this
  approach, not a bug.
- **`oca-plugin-abi` version pinning.** The ABI-compatibility check is
  keyed off `dante-babelbox-oca-plugin-abi`'s own `Cargo.toml` version
  (via `package_version_strings!()`), evaluated wherever that crate is
  compiled — so a plugin built against one version of `oca-plugin-abi`
  needs the host to be running the matching version too.
- **No hot-unload.** Once loaded, a plugin's `.so`/`.dylib`/`.dll` stays
  mapped for the process's lifetime — only individual device instances
  built from it can be connected/disconnected. This matches `abi_stable`'s
  own stated scope ("a plugin system... without support for unloading"),
  not a limitation specific to this project.
- **`OcaClass` is closed.** The eleven variants aren't user-extensible
  from outside this repo — see "The mental model" above.
- **Mapping resolution only knows `"Gain"`/`"Phantom"`.** See "Role
  strings are load-bearing" above.
- **Every `PluginAdapter` implementor must be `Send + Sync`** — plain
  owned state (sockets, buffers), no `Rc`/`RefCell`. `sabi_trait` enforces
  this at the trait-object level (`Send + Sync` supertraits), so a type
  that isn't simply won't compile against the trait.

## What's *not* covered here

Wire-protocol reverse-engineering or spec-reading is out of scope for
this guide — see "[Contributing a new
adapter](../README.md#contributing-a-new-adapter)" in the README for that
discipline (official/community-authoritative specs only, no guessed
framing, unit tests against the spec's own worked examples). This guide
picks up *after* you already have working protocol logic (whether
freshly written or reused from an existing `DeviceAdapter`/`MicAdapter`
implementation, as `plugin-osc-x32` does) and covers only how to expose
it as a loadable plugin.

---

Part of Dante-BabelBox — see [README.md](../README.md) and
[USAGE.md](../USAGE.md) for the project overview and end-user config
reference.
