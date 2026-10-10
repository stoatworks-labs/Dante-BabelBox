//! Local web UI + management API for building the bridge's mapping
//! topology: a patch-bay view (line-art device rack strips, click two
//! channel jacks to connect them) and a crosspoint-matrix view, plus
//! CRUD for devices (real and virtual) and mappings. No auth, no TLS -
//! meant to run on a trusted operations network, same as a hardware
//! router's control port (mirrors `~/Projects/srt-router`'s `crates/web`
//! in shape and that precedent's stated security assumption).
//!
//! Devices (real and virtual) and mappings can all be added and removed
//! live. Removing a real device calls its `LocalAdapter::disconnect()`
//! (via `Router::deregister_device`) so its background socket task and
//! held port are actually released, not just dropped from a list.
//!
//! The wire format stays device+channel-shaped (unchanged from before the
//! OCA/plugin rework), even though the `Router` underneath now only
//! understands OCA-object-level mappings: this crate keeps its own
//! `channel_mappings` list as the "display source of truth" alongside a
//! `descriptors` map (each device's `LocalAdapter::describe()` output, or
//! a synthesized set for virtual devices), and translates between the two
//! shapes via `dante_babelbox_core::channel_mapping::resolve` - the same
//! function `preamp-cli`'s daemon uses for config-file mappings.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{delete, get, post};
use axum::{Json, Router as AxumRouter};
use dante_babelbox_core::{
    channel_mapping, channel_scheme, ChannelMapping, DeviceConfig, GainLock, GainLockStatus, PluginRegistry,
    PreampAddress, Router,
};
use dante_babelbox_oca::OcaObjectDescriptor;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use tracing::info;

const INDEX_HTML: &str = include_str!("../static/index.html");

/// How long `POST /api/devices` waits for a real device's `connect()` to
/// finish before giving up - a bad/unreachable address dialing a TCP
/// adapter (AHM/dLive) could otherwise hang the HTTP request far longer
/// than a UI interaction should tolerate.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the websocket handler checks for a state change to push.
const PUSH_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Every device the bridge knows about - real and virtual, config-seeded
/// at startup and API-added later alike - keyed by id. The single source
/// of truth the web layer reads for `/api/state` and mutates via the
/// management API; real (non-virtual) additions also drive
/// `Router::register_device` as a side effect.
#[derive(Default)]
pub struct DeviceRegistry(RwLock<HashMap<String, DeviceConfig>>);

impl DeviceRegistry {
    pub fn new(devices: Vec<DeviceConfig>) -> Arc<Self> {
        let map = devices.into_iter().map(|d| (d.id.clone(), d)).collect();
        Arc::new(Self(RwLock::new(map)))
    }

    pub fn get(&self, id: &str) -> Option<DeviceConfig> {
        self.0.read().unwrap().get(id).cloned()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.0.read().unwrap().contains_key(id)
    }

    pub fn insert(&self, device: DeviceConfig) {
        self.0.write().unwrap().insert(device.id.clone(), device);
    }

    pub fn remove(&self, id: &str) -> Option<DeviceConfig> {
        self.0.write().unwrap().remove(id)
    }

    pub fn list(&self) -> Vec<DeviceConfig> {
        let mut devices: Vec<_> = self.0.read().unwrap().values().cloned().collect();
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        devices
    }
}

#[derive(Clone)]
pub struct PatchState {
    pub router: Arc<Router>,
    pub devices: Arc<DeviceRegistry>,
    /// Builds device kinds - some statically registered, some loaded from
    /// dylib plugins. Replaces the old `build_adapter` closure: a real
    /// vendor's protocol is exactly as pluggable here as a future
    /// dynamically-loaded one.
    pub registry: Arc<PluginRegistry>,
    /// Each known device's OCA object descriptors - real devices' actual
    /// `describe()` output once connected, virtual devices' synthesized
    /// `channel_scheme` set. Needed to resolve a device+channel-shaped
    /// mapping request into the Router's OCA-object-level `Mapping`s.
    pub descriptors: Arc<RwLock<HashMap<String, Vec<OcaObjectDescriptor>>>>,
    /// The device+channel-shaped mappings as the API/UI sees them - the
    /// Router's own mapping list is OCA-object-level (up to two entries,
    /// gain and phantom, per entry here), so this is kept as the separate
    /// "display source of truth" rather than derived by reverse-decoding
    /// Onos on every request.
    pub channel_mappings: Arc<RwLock<Vec<ChannelMapping>>>,
    /// The gain-lock engine, when the config has a `[gain_lock]` table.
    pub gain_lock: Option<Arc<GainLock>>,
}

#[derive(Serialize)]
struct DeviceView {
    id: String,
    kind: String,
    #[serde(rename = "virtual")]
    is_virtual: bool,
    channels: Option<u16>,
    address: Option<IpAddr>,
    port: Option<u16>,
}

impl From<&DeviceConfig> for DeviceView {
    fn from(d: &DeviceConfig) -> Self {
        Self {
            id: d.id.clone(),
            kind: d.kind.clone(),
            is_virtual: d.is_virtual,
            channels: d.channel_count(),
            address: d.address,
            port: d.port,
        }
    }
}

#[derive(Serialize)]
struct StateResponse {
    devices: Vec<DeviceView>,
    mappings: Vec<ChannelMapping>,
    /// Absent when no `[gain_lock]` is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    gain_lock: Option<GainLockStatus>,
}

async fn snapshot(state: &PatchState) -> StateResponse {
    let gain_lock = match &state.gain_lock {
        Some(engine) => Some(engine.status().await),
        None => None,
    };
    StateResponse {
        devices: state.devices.list().iter().map(DeviceView::from).collect(),
        mappings: state.channel_mappings.read().unwrap().clone(),
        gain_lock,
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

type ApiError = (StatusCode, Json<ErrorBody>);

fn bad_request(msg: impl Into<String>) -> ApiError {
    (StatusCode::BAD_REQUEST, Json(ErrorBody { error: msg.into() }))
}

fn conflict(msg: impl Into<String>) -> ApiError {
    (StatusCode::CONFLICT, Json(ErrorBody { error: msg.into() }))
}

fn not_found(msg: impl Into<String>) -> ApiError {
    (StatusCode::NOT_FOUND, Json(ErrorBody { error: msg.into() }))
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn get_state(State(state): State<PatchState>) -> Json<StateResponse> {
    Json(snapshot(&state).await)
}

#[derive(Deserialize)]
struct AddDeviceRequest {
    id: String,
    kind: String,
    #[serde(default, rename = "virtual")]
    is_virtual: bool,
    #[serde(default)]
    address: Option<IpAddr>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    channels: Option<u16>,
}

async fn add_device(
    State(state): State<PatchState>,
    Json(req): Json<AddDeviceRequest>,
) -> Result<Json<DeviceView>, ApiError> {
    if req.id.trim().is_empty() {
        return Err(bad_request("id must not be empty"));
    }
    if state.devices.contains(&req.id) {
        return Err(conflict(format!("device '{}' already exists", req.id)));
    }

    let device = DeviceConfig {
        id: req.id.clone(),
        kind: req.kind,
        address: req.address,
        port: req.port,
        is_virtual: req.is_virtual,
        channels: req.channels,
    };

    let Some(channels) = device.channel_count() else {
        return Err(bad_request(format!(
            "device '{}': '{}' has no documented default channel count - specify 'channels' explicitly",
            device.id, device.kind
        )));
    };

    if device.is_virtual {
        state.descriptors.write().unwrap().insert(device.id.clone(), channel_scheme::descriptors_for_channels(channels));
    } else {
        let mut adapter = state.registry.create(&device.kind, &device).map_err(|e| bad_request(e.to_string()))?;
        tokio::time::timeout(CONNECT_TIMEOUT, adapter.connect())
            .await
            .map_err(|_| bad_request(format!("timed out connecting to device '{}'", device.id)))?
            .map_err(|e| bad_request(format!("failed to connect to device '{}': {e}", device.id)))?;
        state.descriptors.write().unwrap().insert(device.id.clone(), adapter.describe());
        state
            .router
            .register_device(device.id.clone(), Arc::new(AsyncMutex::new(adapter)))
            .await;
        info!(device = %device.id, "registered live device via management API");
    }

    state.devices.insert(device.clone());
    Ok(Json(DeviceView::from(&device)))
}

async fn remove_device(State(state): State<PatchState>, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    let Some(device) = state.devices.get(&id) else {
        return Err(not_found(format!("device '{id}' not found")));
    };

    if !device.is_virtual {
        state.router.deregister_device(&id).await;
    }

    for m in state.router.mappings() {
        if m.from.device_id == id || m.to.device_id == id {
            state.router.remove_mapping(&m.from, &m.to);
        }
    }
    state.channel_mappings.write().unwrap().retain(|m| m.from.device_id != id && m.to.device_id != id);
    state.descriptors.write().unwrap().remove(&id);
    state.devices.remove(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AddMappingRequest {
    from: PreampAddress,
    to: PreampAddress,
    #[serde(default)]
    bidirectional: bool,
}

fn descriptors_for(state: &PatchState, device_id: &str) -> Vec<OcaObjectDescriptor> {
    state.descriptors.read().unwrap().get(device_id).cloned().unwrap_or_default()
}

async fn add_mapping(State(state): State<PatchState>, Json(req): Json<AddMappingRequest>) -> Result<StatusCode, ApiError> {
    if !state.devices.contains(&req.from.device_id) {
        return Err(bad_request(format!("unknown device '{}'", req.from.device_id)));
    }
    if !state.devices.contains(&req.to.device_id) {
        return Err(bad_request(format!("unknown device '{}'", req.to.device_id)));
    }

    let candidate = ChannelMapping { from: req.from, to: req.to, bidirectional: req.bidirectional };
    let exists =
        state.channel_mappings.read().unwrap().iter().any(|m| m.from == candidate.from && m.to == candidate.to);
    if exists {
        return Err(conflict("mapping already exists"));
    }

    let resolved = channel_mapping::resolve(
        &candidate,
        &descriptors_for(&state, &candidate.from.device_id),
        &descriptors_for(&state, &candidate.to.device_id),
    );
    if resolved.is_empty() {
        return Err(bad_request(
            "mapping resolved to no shared objects - check that both channel numbers are in range",
        ));
    }
    for m in resolved {
        state.router.add_mapping(m);
    }
    state.channel_mappings.write().unwrap().push(candidate);
    Ok(StatusCode::NO_CONTENT)
}

/// Mapping ids are synthesized (`ChannelMapping` has no id of its own), in
/// the form `"{from.device}:{from.channel}->{to.device}:{to.channel}"` -
/// the frontend already has everything it needs to build one from
/// `/api/state` without a separate lookup. Assumes device ids don't
/// contain `:` (every id in this project's convention is kebab-case).
/// The `>` isn't a valid raw URI character, so callers building the
/// DELETE path must `encodeURIComponent()` (or equivalent) the id first
/// - axum's `Path` extractor decodes it automatically on this end.
async fn remove_mapping(State(state): State<PatchState>, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    let Some((from, to)) = parse_mapping_id(&id) else {
        return Err(bad_request("malformed mapping id"));
    };

    let removed = {
        let mut channel_mappings = state.channel_mappings.write().unwrap();
        let Some(index) = channel_mappings.iter().position(|m| m.from == from && m.to == to) else {
            return Err(not_found(format!("mapping '{id}' not found")));
        };
        channel_mappings.remove(index)
    };

    for m in channel_mapping::resolve(&removed, &descriptors_for(&state, &from.device_id), &descriptors_for(&state, &to.device_id)) {
        state.router.remove_mapping(&m.from, &m.to);
    }
    Ok(StatusCode::NO_CONTENT)
}

fn parse_mapping_id(id: &str) -> Option<(PreampAddress, PreampAddress)> {
    let (from_str, to_str) = id.split_once("->")?;
    Some((parse_endpoint(from_str)?, parse_endpoint(to_str)?))
}

fn parse_endpoint(s: &str) -> Option<PreampAddress> {
    let (device, channel) = s.rsplit_once(':')?;
    Some(PreampAddress::new(device, channel.parse().ok()?))
}

fn gain_lock(state: &PatchState) -> Result<&Arc<GainLock>, ApiError> {
    state.gain_lock.as_ref().ok_or_else(|| not_found("no [gain_lock] is configured"))
}

async fn get_gain_lock(State(state): State<PatchState>) -> Result<Json<GainLockStatus>, ApiError> {
    Ok(Json(gain_lock(&state)?.status().await))
}

async fn lock_gains(State(state): State<PatchState>) -> Result<Json<GainLockStatus>, ApiError> {
    Ok(Json(gain_lock(&state)?.lock().await))
}

async fn unlock_gains(State(state): State<PatchState>) -> Result<Json<GainLockStatus>, ApiError> {
    Ok(Json(gain_lock(&state)?.unlock().await))
}

#[derive(Deserialize)]
struct LeaderRequest {
    device: String,
}

async fn set_leader(
    State(state): State<PatchState>,
    Json(req): Json<LeaderRequest>,
) -> Result<Json<GainLockStatus>, ApiError> {
    gain_lock(&state)?.set_leader(&req.device).await.map(Json).map_err(conflict)
}

/// Either an absolute `gain_db` or a relative `delta_db`, not both.
#[derive(Deserialize)]
struct PreampRequest {
    signal: String,
    #[serde(default)]
    gain_db: Option<f32>,
    #[serde(default)]
    delta_db: Option<f32>,
}

async fn set_preamp(
    State(state): State<PatchState>,
    Json(req): Json<PreampRequest>,
) -> Result<Json<GainLockStatus>, ApiError> {
    let engine = gain_lock(&state)?;
    let result = match (req.gain_db, req.delta_db) {
        (Some(gain), None) => engine.set_preamp(&req.signal, gain).await,
        (None, Some(delta)) => engine.nudge_preamp(&req.signal, delta).await,
        _ => return Err(bad_request("give exactly one of gain_db or delta_db")),
    };
    result.map(Json).map_err(bad_request)
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<PatchState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| push_state(socket, state))
}

/// Push the current state to the client on connect and again every time
/// it changes, so the UI updates live without waiting on its own poll.
async fn push_state(mut socket: WebSocket, state: PatchState) {
    let mut last: Option<String> = None;
    loop {
        let Ok(body) = serde_json::to_string(&snapshot(&state).await) else {
            return;
        };
        if last.as_deref() != Some(body.as_str()) {
            if socket.send(Message::Text(body.clone().into())).await.is_err() {
                return;
            }
            last = Some(body);
        }
        tokio::select! {
            _ = tokio::time::sleep(PUSH_POLL_INTERVAL) => {}
            msg = socket.recv() => {
                if !matches!(msg, Some(Ok(_))) {
                    return;
                }
            }
        }
    }
}

/// The patch-bay UI/API as a standalone `axum::Router`, for callers that
/// want to `.merge()` in additional routes before serving.
pub fn app(state: PatchState) -> AxumRouter {
    AxumRouter::new()
        .route("/", get(index))
        .route("/api/state", get(get_state))
        .route("/api/devices", post(add_device))
        .route("/api/devices/{id}", delete(remove_device))
        .route("/api/mappings", post(add_mapping))
        .route("/api/mappings/{id}", delete(remove_mapping))
        .route("/api/gain-lock", get(get_gain_lock))
        .route("/api/gain-lock/lock", post(lock_gains))
        .route("/api/gain-lock/unlock", post(unlock_gains))
        .route("/api/gain-lock/leader", post(set_leader))
        .route("/api/gain-lock/preamp", post(set_preamp))
        .route("/ws", get(ws_handler))
        .with_state(state)
}

/// Bind and serve the patch-bay web UI on its own, with no additional
/// routes merged in. Runs until the process exits.
pub async fn serve(bind: SocketAddr, state: PatchState) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    info!(%bind, "patch-bay web UI listening");
    axum::serve(listener, app(state)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use dante_babelbox_core::{AdapterError, AdapterResult, DeviceInfo, LocalAdapter};
    use dante_babelbox_oca::{Ono, OcaEvent, OcaValue};
    use std::collections::HashSet;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::broadcast;
    use tower::ServiceExt;

    struct FakeAdapter {
        id: String,
        tx: broadcast::Sender<OcaEvent>,
        disconnected: Arc<StdMutex<HashSet<String>>>,
    }

    impl FakeAdapter {
        fn new(id: &str) -> Self {
            Self::new_tracked(id, Arc::new(StdMutex::new(HashSet::new())))
        }

        fn new_tracked(id: &str, disconnected: Arc<StdMutex<HashSet<String>>>) -> Self {
            let (tx, _rx) = broadcast::channel(4);
            Self { id: id.to_string(), tx, disconnected }
        }
    }

    #[async_trait]
    impl LocalAdapter for FakeAdapter {
        fn id(&self) -> &str {
            &self.id
        }
        async fn connect(&mut self) -> AdapterResult<()> {
            Ok(())
        }
        async fn disconnect(&mut self) -> AdapterResult<()> {
            self.disconnected.lock().unwrap().insert(self.id.clone());
            Ok(())
        }
        async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
            Ok(DeviceInfo { vendor: "fake".into(), model: "fake".into(), address: "127.0.0.1".parse().unwrap() })
        }
        fn describe(&self) -> Vec<OcaObjectDescriptor> {
            channel_scheme::descriptors_for_channels(24)
        }
        async fn get_object(&mut self, _ono: Ono) -> AdapterResult<OcaValue> {
            Err(AdapterError::UnsupportedChannel(0))
        }
        async fn set_object(&mut self, _ono: Ono, _value: OcaValue) -> AdapterResult<()> {
            Ok(())
        }
        fn subscribe(&self) -> broadcast::Receiver<OcaEvent> {
            self.tx.subscribe()
        }
    }

    fn registry_with(kind: &'static str, ctor: impl Fn(&DeviceConfig) -> anyhow::Result<Box<dyn LocalAdapter>> + Send + Sync + 'static) -> Arc<PluginRegistry> {
        let registry = PluginRegistry::new();
        registry.register_static(kind, ctor);
        Arc::new(registry)
    }

    fn empty_state(registry: Arc<PluginRegistry>) -> PatchState {
        PatchState {
            router: Router::new(Vec::new()),
            devices: DeviceRegistry::new(Vec::new()),
            registry,
            descriptors: Arc::new(RwLock::new(HashMap::new())),
            channel_mappings: Arc::new(RwLock::new(Vec::new())),
            gain_lock: None,
        }
    }

    fn test_state() -> PatchState {
        empty_state(registry_with("osc-x32", |device| Ok(Box::new(FakeAdapter::new(&device.id)) as Box<dyn LocalAdapter>)))
    }

    fn test_state_with_failing_adapter() -> PatchState {
        empty_state(registry_with("osc-x32", |_| anyhow::bail!("no such adapter kind")))
    }

    /// Also returns the shared set every `FakeAdapter` built by this state
    /// records its id into on `disconnect()`, so a test can assert it.
    fn test_state_with_disconnect_tracking() -> (PatchState, Arc<StdMutex<HashSet<String>>>) {
        let disconnected = Arc::new(StdMutex::new(HashSet::new()));
        let tracked = disconnected.clone();
        let state = empty_state(registry_with("osc-x32", move |device| {
            Ok(Box::new(FakeAdapter::new_tracked(&device.id, tracked.clone())) as Box<dyn LocalAdapter>)
        }));
        (state, disconnected)
    }

    /// Directly registers a virtual device the way a config file would,
    /// bypassing the `add_device` HTTP handler - but still populates
    /// `descriptors` the same way that handler would, since several
    /// existing tests rely on being able to resolve mappings against
    /// devices set up this way.
    fn insert_virtual(state: &PatchState, id: &str, kind: &str, channels: u16) {
        state.devices.insert(DeviceConfig {
            id: id.into(),
            kind: kind.into(),
            address: None,
            port: None,
            is_virtual: true,
            channels: Some(channels),
        });
        state.descriptors.write().unwrap().insert(id.into(), channel_scheme::descriptors_for_channels(channels));
    }

    async fn call(app: &AxumRouter, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let res = app.clone().oneshot(req).await.expect("request failed");
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.expect("read body");
        let body = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        (status, body)
    }

    fn post(uri: &str, json: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(json.to_string()))
            .unwrap()
    }

    fn delete_req(uri: &str) -> Request<Body> {
        Request::builder().method("DELETE").uri(uri).body(Body::empty()).unwrap()
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder().method("GET").uri(uri).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn add_virtual_device_needs_no_adapter() {
        let state = test_state_with_failing_adapter();
        let app = app(state.clone());

        let (status, body) = call(
            &app,
            post(
                "/api/devices",
                r#"{"id":"future-x32","kind":"osc-x32","virtual":true,"channels":8}"#,
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["virtual"], true);
        assert_eq!(body["channels"], 8);
        assert!(state.devices.contains("future-x32"));
    }

    #[tokio::test]
    async fn add_real_device_connects_via_registry() {
        let state = test_state();
        let app = app(state.clone());

        let (status, body) = call(
            &app,
            post("/api/devices", r#"{"id":"x32-foh","kind":"osc-x32","address":"10.0.0.5"}"#),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["channels"], 128, "expected osc-x32's documented default channel count");
        assert!(state.devices.contains("x32-foh"));
    }

    #[tokio::test]
    async fn add_real_device_surfaces_adapter_failure_as_bad_request() {
        let state = test_state_with_failing_adapter();
        let app = app(state.clone());

        let (status, body) = call(
            &app,
            post("/api/devices", r#"{"id":"x32-foh","kind":"osc-x32","address":"10.0.0.5"}"#),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("no such adapter kind"));
        assert!(!state.devices.contains("x32-foh"), "a failed connect must not register the device");
    }

    #[tokio::test]
    async fn add_device_rejects_kind_with_no_channel_count_and_no_override() {
        let state = test_state();
        let app = app(state.clone());

        let (status, body) = call(
            &app,
            post("/api/devices", r#"{"id":"sq-foh","kind":"ah-midi","address":"10.0.0.5"}"#),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("channels"));
    }

    #[tokio::test]
    async fn add_device_duplicate_id_is_a_conflict() {
        let state = test_state();
        insert_virtual(&state, "dup", "osc-wing", 8);
        let app = app(state);

        let (status, body) = call(&app, post("/api/devices", r#"{"id":"dup","kind":"osc-wing","virtual":true,"channels":8}"#)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("already exists"));
    }

    #[tokio::test]
    async fn remove_virtual_device_cascades_its_mappings() {
        let state = test_state();
        insert_virtual(&state, "future-x32", "osc-x32", 8);
        insert_virtual(&state, "console", "osc-x32", 24);
        let app = app(state.clone());

        let (status, _) = call(
            &app,
            post(
                "/api/mappings",
                r#"{"from":{"device":"future-x32","channel":1},"to":{"device":"console","channel":1},"bidirectional":true}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = call(&app, delete_req("/api/devices/future-x32")).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        assert!(!state.devices.contains("future-x32"));
        assert!(state.router.mappings().is_empty(), "removing the device must cascade-remove its mappings");
        assert!(state.channel_mappings.read().unwrap().is_empty());
    }

    #[tokio::test]
    async fn remove_real_device_disconnects_and_cascades_its_mappings() {
        let (state, disconnected) = test_state_with_disconnect_tracking();
        insert_virtual(&state, "future-x32", "osc-x32", 8);
        let app = app(state.clone());

        let (status, _) = call(
            &app,
            post("/api/devices", r#"{"id":"console","kind":"osc-x32","address":"10.0.0.1"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = call(
            &app,
            post(
                "/api/mappings",
                r#"{"from":{"device":"future-x32","channel":1},"to":{"device":"console","channel":1},"bidirectional":true}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = call(&app, delete_req("/api/devices/console")).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        assert!(!state.devices.contains("console"));
        assert!(disconnected.lock().unwrap().contains("console"), "disconnect() must be called on removal");
        assert!(state.router.mappings().is_empty(), "removing the device must cascade-remove its mappings");
    }

    #[tokio::test]
    async fn remove_unknown_device_is_not_found() {
        let app = app(test_state());
        let (status, body) = call(&app, delete_req("/api/devices/nope")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].as_str().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn add_mapping_rejects_unknown_device() {
        let state = test_state();
        insert_virtual(&state, "a", "osc-wing", 8);
        let app = app(state);

        let (status, body) = call(
            &app,
            post(
                "/api/mappings",
                r#"{"from":{"device":"a","channel":1},"to":{"device":"nope","channel":1}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("nope"));
    }

    #[tokio::test]
    async fn add_mapping_rejects_a_channel_out_of_range() {
        let state = test_state();
        insert_virtual(&state, "a", "osc-wing", 8);
        insert_virtual(&state, "b", "osc-wing", 8);
        let app = app(state);

        let (status, body) = call(
            &app,
            post(
                "/api/mappings",
                r#"{"from":{"device":"a","channel":1},"to":{"device":"b","channel":99}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("no shared objects"));
    }

    #[tokio::test]
    async fn add_duplicate_mapping_is_a_conflict() {
        let state = test_state();
        insert_virtual(&state, "a", "osc-wing", 8);
        insert_virtual(&state, "b", "osc-wing", 8);
        let app = app(state);

        let (status, _) = call(
            &app,
            post("/api/mappings", r#"{"from":{"device":"a","channel":1},"to":{"device":"b","channel":1}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = call(
            &app,
            post("/api/mappings", r#"{"from":{"device":"a","channel":1},"to":{"device":"b","channel":1}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn add_then_remove_mapping_round_trips() {
        let state = test_state();
        insert_virtual(&state, "a", "osc-wing", 8);
        insert_virtual(&state, "b", "osc-wing", 8);
        let app = app(state.clone());

        let (status, _) = call(
            &app,
            post("/api/mappings", r#"{"from":{"device":"a","channel":1},"to":{"device":"b","channel":2},"bidirectional":true}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(state.channel_mappings.read().unwrap().len(), 1);
        // Gain + phantom, per the shared per-channel scheme.
        assert_eq!(state.router.mappings().len(), 2);

        let (status, _) = call(&app, delete_req("/api/mappings/a:1-%3Eb:2")).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(state.channel_mappings.read().unwrap().is_empty());
        assert!(state.router.mappings().is_empty());
    }

    #[tokio::test]
    async fn remove_unknown_mapping_is_not_found() {
        let app = app(test_state());
        let (status, _) = call(&app, delete_req("/api/mappings/a:1-%3Eb:2")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn state_reflects_devices_and_mappings() {
        let state = test_state();
        insert_virtual(&state, "a", "osc-wing", 8);
        let app = app(state.clone());

        let (status, _) = call(
            &app,
            post("/api/mappings", r#"{"from":{"device":"a","channel":1},"to":{"device":"a","channel":2}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, body) = call(&app, get_req("/api/state")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["devices"].as_array().unwrap().len(), 1);
        assert_eq!(body["mappings"].as_array().unwrap().len(), 1);
    }

    /// A desk with one preamp and one trim, enough to drive the gain-lock API.
    struct LockDesk {
        id: String,
        tx: broadcast::Sender<OcaEvent>,
        gain: Arc<StdMutex<f32>>,
        trim: Arc<StdMutex<f32>>,
    }

    #[async_trait]
    impl LocalAdapter for LockDesk {
        fn id(&self) -> &str {
            &self.id
        }
        async fn connect(&mut self) -> AdapterResult<()> {
            Ok(())
        }
        async fn disconnect(&mut self) -> AdapterResult<()> {
            Ok(())
        }
        async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
            Err(AdapterError::Protocol("unused".into()))
        }
        fn describe(&self) -> Vec<OcaObjectDescriptor> {
            vec![channel_scheme::descriptor(1, channel_scheme::Field::Gain), channel_scheme::trim_descriptor(1)]
        }
        async fn get_object(&mut self, _ono: Ono) -> AdapterResult<OcaValue> {
            Ok(OcaValue::F32(*self.gain.lock().unwrap()))
        }
        async fn set_object(&mut self, _ono: Ono, value: OcaValue) -> AdapterResult<()> {
            *self.gain.lock().unwrap() = value.as_f32().unwrap();
            Ok(())
        }
        fn subscribe(&self) -> broadcast::Receiver<OcaEvent> {
            self.tx.subscribe()
        }
        async fn nudge_object(&mut self, _ono: Ono, delta: f32) -> AdapterResult<f32> {
            *self.trim.lock().unwrap() += delta;
            Ok(delta)
        }
    }

    async fn gain_lock_state() -> (PatchState, Arc<StdMutex<f32>>) {
        let state = test_state();
        let mon_trim = Arc::new(StdMutex::new(0.0));
        for (id, trim) in [("foh", Arc::new(StdMutex::new(0.0))), ("mon", mon_trim.clone())] {
            let desk = LockDesk { id: id.into(), tx: broadcast::channel(4).0, gain: Arc::new(StdMutex::new(20.0)), trim };
            state.router.register_device(id, Arc::new(AsyncMutex::new(Box::new(desk) as Box<dyn LocalAdapter>))).await;
        }
        let member = |device: &str, preamp: Option<u16>, input: Option<u16>| dante_babelbox_core::gain_lock::Member {
            device: device.into(),
            preamp,
            input,
        };
        let config = dante_babelbox_core::GainLockConfig {
            leader: "foh".into(),
            signals: vec![dante_babelbox_core::gain_lock::Signal {
                name: "Kick".into(),
                members: vec![member("foh", Some(1), Some(1)), member("mon", Some(1), Some(1))],
            }],
            poll_ms: None,
            lock_on_start: false,
        };
        let engine = GainLock::new(Arc::clone(&state.router), config).await;
        (PatchState { gain_lock: Some(engine), ..state }, mon_trim)
    }

    #[tokio::test]
    async fn gain_lock_routes_are_not_found_without_a_gain_lock_table() {
        let app = app(test_state());
        let (status, _) = call(&app, get_req("/api/gain-lock")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&app, post("/api/gain-lock/lock", "")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, body) = call(&app, get_req("/api/state")).await;
        assert!(body.get("gain_lock").is_none());
    }

    #[tokio::test]
    async fn lock_then_set_the_leader_preamp_compensates_the_follower() {
        let (state, mon_trim) = gain_lock_state().await;
        let app = app(state);

        let (status, body) = call(&app, post("/api/gain-lock/lock", "")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["locked"], true);
        assert_eq!(body["signals"][0]["reference_db"], 20.0);

        let (status, body) = call(&app, post("/api/gain-lock/preamp", r#"{"signal":"Kick","gain_db":24.0}"#)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(*mon_trim.lock().unwrap(), -4.0);
        assert_eq!(body["signals"][0]["followers"][0]["applied_db"], -4.0);

        let (status, _) = call(&app, post("/api/gain-lock/preamp", r#"{"signal":"Kick","delta_db":-1.0}"#)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(*mon_trim.lock().unwrap(), -3.0);

        let (_, body) = call(&app, get_req("/api/state")).await;
        assert_eq!(body["gain_lock"]["signals"][0]["gain_db"], 23.0);
    }

    #[tokio::test]
    async fn the_leader_cannot_be_changed_while_locked() {
        let (state, _) = gain_lock_state().await;
        let app = app(state);
        call(&app, post("/api/gain-lock/lock", "")).await;
        let (status, _) = call(&app, post("/api/gain-lock/leader", r#"{"device":"mon"}"#)).await;
        assert_eq!(status, StatusCode::CONFLICT);

        call(&app, post("/api/gain-lock/unlock", "")).await;
        let (status, body) = call(&app, post("/api/gain-lock/leader", r#"{"device":"mon"}"#)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["leader"], "mon");
        assert_eq!(body["signals"][0]["followers"][0]["device"], "foh");
    }

    #[tokio::test]
    async fn a_preamp_request_needs_exactly_one_of_gain_or_delta() {
        let (state, _) = gain_lock_state().await;
        let app = app(state);
        let (status, _) = call(&app, post("/api/gain-lock/preamp", r#"{"signal":"Kick","gain_db":1,"delta_db":1}"#)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&app, post("/api/gain-lock/preamp", r#"{"signal":"Kick"}"#)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&app, post("/api/gain-lock/preamp", r#"{"signal":"Nope","gain_db":1}"#)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

}
