//! Local web UI + management API for building the bridge's mapping
//! topology: a patch-bay view (line-art device rack strips, click two
//! channel jacks to connect them) and a crosspoint-matrix view, plus
//! CRUD for devices (real and virtual) and mappings. No auth, no TLS -
//! meant to run on a trusted operations network, same as a hardware
//! router's control port (mirrors `~/Projects/srt-router`'s `crates/web`
//! in shape and that precedent's stated security assumption).
//!
//! Devices (real and virtual) and mappings can all be added and removed
//! live. Removing a real device calls its `DeviceAdapter::disconnect()`
//! (via `Router::deregister_device`) so its background socket task and
//! held port are actually released, not just dropped from a list.

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
use dante_babelbox_core::{DeviceAdapter, DeviceConfig, DeviceKind, Mapping, PreampAddress, Router};
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

pub type BuildAdapter = Arc<dyn Fn(&DeviceConfig) -> anyhow::Result<Box<dyn DeviceAdapter>> + Send + Sync>;

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
    /// Constructs (but doesn't connect) an adapter for a real device.
    /// Injected as a closure rather than this crate depending on every
    /// `adapter-*` crate directly - a real vendor's protocol is exactly
    /// as pluggable here as a future emulated one.
    pub build_adapter: BuildAdapter,
}

#[derive(Serialize)]
struct DeviceView {
    id: String,
    kind: DeviceKind,
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
            kind: d.kind,
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
    mappings: Vec<Mapping>,
}

fn snapshot(state: &PatchState) -> StateResponse {
    StateResponse {
        devices: state.devices.list().iter().map(DeviceView::from).collect(),
        mappings: state.router.mappings(),
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
    Json(snapshot(&state))
}

#[derive(Deserialize)]
struct AddDeviceRequest {
    id: String,
    kind: DeviceKind,
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

    if device.channel_count().is_none() {
        return Err(bad_request(format!(
            "device '{}': {:?} has no documented default channel count - specify 'channels' explicitly",
            device.id, device.kind
        )));
    }

    if !device.is_virtual {
        let mut adapter = (state.build_adapter)(&device).map_err(|e| bad_request(e.to_string()))?;
        tokio::time::timeout(CONNECT_TIMEOUT, adapter.connect())
            .await
            .map_err(|_| bad_request(format!("timed out connecting to device '{}'", device.id)))?
            .map_err(|e| bad_request(format!("failed to connect to device '{}': {e}", device.id)))?;
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

async fn add_mapping(State(state): State<PatchState>, Json(req): Json<AddMappingRequest>) -> Result<StatusCode, ApiError> {
    if !state.devices.contains(&req.from.device_id) {
        return Err(bad_request(format!("unknown device '{}'", req.from.device_id)));
    }
    if !state.devices.contains(&req.to.device_id) {
        return Err(bad_request(format!("unknown device '{}'", req.to.device_id)));
    }
    let exists = state.router.mappings().iter().any(|m| m.from == req.from && m.to == req.to);
    if exists {
        return Err(conflict("mapping already exists"));
    }
    state.router.add_mapping(Mapping {
        from: req.from,
        to: req.to,
        bidirectional: req.bidirectional,
    });
    Ok(StatusCode::NO_CONTENT)
}

/// Mapping ids are synthesized (`Mapping` has no id of its own), in the
/// form `"{from.device}:{from.channel}->{to.device}:{to.channel}"` - the
/// frontend already has everything it needs to build one from
/// `/api/state` without a separate lookup. Assumes device ids don't
/// contain `:` (every id in this project's convention is kebab-case).
/// The `>` isn't a valid raw URI character, so callers building the
/// DELETE path must `encodeURIComponent()` (or equivalent) the id first
/// - axum's `Path` extractor decodes it automatically on this end.
async fn remove_mapping(State(state): State<PatchState>, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    let Some((from, to)) = parse_mapping_id(&id) else {
        return Err(bad_request("malformed mapping id"));
    };
    if state.router.remove_mapping(&from, &to) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found(format!("mapping '{id}' not found")))
    }
}

fn parse_mapping_id(id: &str) -> Option<(PreampAddress, PreampAddress)> {
    let (from_str, to_str) = id.split_once("->")?;
    Some((parse_endpoint(from_str)?, parse_endpoint(to_str)?))
}

fn parse_endpoint(s: &str) -> Option<PreampAddress> {
    let (device, channel) = s.rsplit_once(':')?;
    Some(PreampAddress::new(device, channel.parse().ok()?))
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<PatchState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| push_state(socket, state))
}

/// Push the current state to the client on connect and again every time
/// it changes, so the UI updates live without waiting on its own poll.
async fn push_state(mut socket: WebSocket, state: PatchState) {
    let mut last: Option<String> = None;
    loop {
        let Ok(body) = serde_json::to_string(&snapshot(&state)) else {
            return;
        };
        if last.as_deref() != Some(body.as_str()) {
            if socket.send(Message::Text(body.clone())).await.is_err() {
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
        .route("/api/devices/:id", delete(remove_device))
        .route("/api/mappings", post(add_mapping))
        .route("/api/mappings/:id", delete(remove_mapping))
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
    use dante_babelbox_core::{AdapterError, AdapterResult, DeviceInfo, PreampEvent, PreampState};
    use std::collections::HashSet;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::broadcast;
    use tower::ServiceExt;

    struct FakeAdapter {
        id: String,
        tx: broadcast::Sender<PreampEvent>,
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
    impl DeviceAdapter for FakeAdapter {
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
            Ok(DeviceInfo {
                vendor: "fake".into(),
                model: "fake".into(),
                address: "127.0.0.1".parse().unwrap(),
            })
        }
        async fn set_gain(&mut self, _channel: u16, _gain_db: f32) -> AdapterResult<()> {
            Ok(())
        }
        async fn set_phantom(&mut self, _channel: u16, _on: bool) -> AdapterResult<()> {
            Ok(())
        }
        async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
            Err(AdapterError::UnsupportedChannel(channel))
        }
        fn subscribe(&self) -> broadcast::Receiver<PreampEvent> {
            self.tx.subscribe()
        }
    }

    fn test_state() -> PatchState {
        PatchState {
            router: Router::new(Vec::new()),
            devices: DeviceRegistry::new(Vec::new()),
            build_adapter: Arc::new(|device| Ok(Box::new(FakeAdapter::new(&device.id)))),
        }
    }

    fn test_state_with_failing_adapter() -> PatchState {
        PatchState {
            router: Router::new(Vec::new()),
            devices: DeviceRegistry::new(Vec::new()),
            build_adapter: Arc::new(|_| anyhow::bail!("no such adapter kind")),
        }
    }

    /// Also returns the shared set every `FakeAdapter` built by this state
    /// records its id into on `disconnect()`, so a test can assert it.
    fn test_state_with_disconnect_tracking() -> (PatchState, Arc<StdMutex<HashSet<String>>>) {
        let disconnected = Arc::new(StdMutex::new(HashSet::new()));
        let tracked = disconnected.clone();
        let state = PatchState {
            router: Router::new(Vec::new()),
            devices: DeviceRegistry::new(Vec::new()),
            build_adapter: Arc::new(move |device| Ok(Box::new(FakeAdapter::new_tracked(&device.id, tracked.clone())) as Box<dyn DeviceAdapter>)),
        };
        (state, disconnected)
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
    async fn add_real_device_connects_via_build_adapter() {
        let state = test_state();
        let app = app(state.clone());

        let (status, body) = call(
            &app,
            post("/api/devices", r#"{"id":"x32-foh","kind":"osc-x32","address":"10.0.0.5"}"#),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["channels"], 24, "expected osc-x32's documented default channel count");
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
        state.devices.insert(DeviceConfig {
            id: "dup".into(),
            kind: DeviceKind::OscWing,
            address: None,
            port: None,
            is_virtual: true,
            channels: Some(8),
        });
        let app = app(state);

        let (status, body) = call(&app, post("/api/devices", r#"{"id":"dup","kind":"osc-wing","virtual":true,"channels":8}"#)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("already exists"));
    }

    #[tokio::test]
    async fn remove_virtual_device_cascades_its_mappings() {
        let state = test_state();
        state.devices.insert(DeviceConfig {
            id: "future-x32".into(),
            kind: DeviceKind::OscX32,
            address: None,
            port: None,
            is_virtual: true,
            channels: Some(8),
        });
        state.devices.insert(DeviceConfig {
            id: "console".into(),
            kind: DeviceKind::OscX32,
            address: Some("10.0.0.1".parse().unwrap()),
            port: None,
            is_virtual: false,
            channels: None,
        });
        state.router.add_mapping(Mapping {
            from: PreampAddress::new("future-x32", 1),
            to: PreampAddress::new("console", 1),
            bidirectional: true,
        });
        let app = app(state.clone());

        let (status, _) = call(&app, delete_req("/api/devices/future-x32")).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        assert!(!state.devices.contains("future-x32"));
        assert!(state.router.mappings().is_empty(), "removing the device must cascade-remove its mappings");
    }

    #[tokio::test]
    async fn remove_real_device_disconnects_and_cascades_its_mappings() {
        let (state, disconnected) = test_state_with_disconnect_tracking();
        state.devices.insert(DeviceConfig {
            id: "future-x32".into(),
            kind: DeviceKind::OscX32,
            address: None,
            port: None,
            is_virtual: true,
            channels: Some(8),
        });
        let app = app(state.clone());

        let (status, _) = call(
            &app,
            post("/api/devices", r#"{"id":"console","kind":"osc-x32","address":"10.0.0.1"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        state.router.add_mapping(Mapping {
            from: PreampAddress::new("future-x32", 1),
            to: PreampAddress::new("console", 1),
            bidirectional: true,
        });

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
        state.devices.insert(DeviceConfig {
            id: "a".into(),
            kind: DeviceKind::OscWing,
            address: None,
            port: None,
            is_virtual: true,
            channels: Some(8),
        });
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
    async fn add_duplicate_mapping_is_a_conflict() {
        let state = test_state();
        for id in ["a", "b"] {
            state.devices.insert(DeviceConfig {
                id: id.into(),
                kind: DeviceKind::OscWing,
                address: None,
                port: None,
                is_virtual: true,
                channels: Some(8),
            });
        }
        state.router.add_mapping(Mapping {
            from: PreampAddress::new("a", 1),
            to: PreampAddress::new("b", 1),
            bidirectional: true,
        });
        let app = app(state);

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
        for id in ["a", "b"] {
            state.devices.insert(DeviceConfig {
                id: id.into(),
                kind: DeviceKind::OscWing,
                address: None,
                port: None,
                is_virtual: true,
                channels: Some(8),
            });
        }
        let app = app(state.clone());

        let (status, _) = call(
            &app,
            post("/api/mappings", r#"{"from":{"device":"a","channel":1},"to":{"device":"b","channel":2},"bidirectional":true}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(state.router.mappings().len(), 1);

        let (status, _) = call(&app, delete_req("/api/mappings/a:1-%3Eb:2")).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
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
        state.devices.insert(DeviceConfig {
            id: "a".into(),
            kind: DeviceKind::OscWing,
            address: None,
            port: None,
            is_virtual: true,
            channels: Some(8),
        });
        state.router.add_mapping(Mapping {
            from: PreampAddress::new("a", 1),
            to: PreampAddress::new("a", 2),
            bidirectional: false,
        });
        let app = app(state);

        let (status, body) = call(&app, get_req("/api/state")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["devices"].as_array().unwrap().len(), 1);
        assert_eq!(body["mappings"].as_array().unwrap().len(), 1);
    }
}
