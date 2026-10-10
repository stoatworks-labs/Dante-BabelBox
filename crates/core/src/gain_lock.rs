//! Gain compensation across desks - the "gain lock".
//!
//! One desk is the **leader**: it owns the preamps on the shared stage box.
//! Every other desk is a **follower**: it gets those preamps' output after
//! the gain stage, so when the leader turns a preamp up 3 dB, every follower
//! hears 3 dB more. Lock the system and the follower desks' digital trims
//! take the opposite move automatically, so their levels stay where their
//! engineers set them however the leader rides the gain during the show.
//!
//! Three rules shape everything below:
//!
//! - **Deltas, never targets.** The engine never computes "follower trim
//!   should be X". Each leader change of `d` dB becomes a relative move of
//!   `-d` on each follower, applied to whatever that desk holds *now*
//!   ([`crate::LocalAdapter::nudge_object`]). A monitor engineer who trims a
//!   channel down 2 dB mid-show keeps that 2 dB.
//! - **Nothing is dropped.** A follower that can't take the whole move - a
//!   trim at its limit, a step coarser than the move, a desk that didn't
//!   answer - keeps the difference as `owed` and is offered it again with the
//!   next move (or the retry tick, after an error). So for every follower,
//!   `applied + owed == -(leader gain now - leader gain at lock)` always: when
//!   the leader comes back down, a trim that was pinned at its limit isn't
//!   pushed past where it started.
//! - **The leader's value is the only input.** Compensation is driven purely
//!   by the leader gain the engine has observed, whoever moved it - the
//!   leader's own surface, another controller, or this bridge's
//!   [`set_preamp`](GainLock::set_preamp). The same value reported twice is a
//!   zero delta, so an echo can never double a move - and a late report of a
//!   value the engine itself read or set is swallowed rather than taken as
//!   the leader moving back (see `Live::expected`).
//!
//! A **signal** names one source across the desks: which preamp it is on
//! each desk that can control it, and which input channel trims it on each
//! desk that receives it. Any member with a preamp can be elected leader
//! (while unlocked); every other member with an input follows.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dante_babelbox_oca::{Ono, OcaEvent, OcaValue};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, info, warn};

use crate::channel_scheme::{self, Field};
use crate::router::Router;

/// A leader move smaller than this is noise, not a change.
const EPSILON_DB: f32 = 1e-4;

/// How often a follower whose last move failed is offered its owed amount
/// again, without waiting for the leader to move.
const RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// How long to wait for a device to answer a read before treating it as
/// unreachable.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// The most a lock (or a poll) spends reading the leader, all signals
/// together. A leader that has gone away must not hang the Lock button for
/// two seconds per signal; signals left unread fall back to the last value
/// the leader reported, or arm on its next report.
const READ_BUDGET: Duration = Duration::from_secs(5);

/// A guard on one follower move (a read and a write on X32/Wing).
const NUDGE_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a value the engine caused (by reading or setting the leader) is
/// expected to come back on the event stream. See [`Live::expected`].
const ECHO_TTL: Duration = Duration::from_secs(2);

/// One desk's part in a [`Signal`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub device: String,
    /// This desk's preamp for the signal, in its adapter's own numbering
    /// (an X32 headamp, a dLive socket, a DM3 local input). Needed for this
    /// desk to be able to lead the signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamp: Option<u16>,
    /// The input channel whose trim carries the signal on this desk. Needed
    /// for this desk to follow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<u16>,
}

/// One source, as each desk addresses it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    pub name: String,
    pub members: Vec<Member>,
}

/// The `[gain_lock]` table of a `bridge.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GainLockConfig {
    pub leader: String,
    #[serde(default, rename = "signal")]
    pub signals: Vec<Signal>,
    /// Re-read the leader's gains on this interval as well as listening for
    /// its change reports - for a leader whose protocol doesn't push changes
    /// (the DM3's OSC port, for one). Off when absent or 0.
    #[serde(default)]
    pub poll_ms: Option<u64>,
    /// Lock as soon as the devices are connected, so a bridge restarted
    /// mid-show is protecting the followers again without anyone touching it.
    #[serde(default)]
    pub lock_on_start: bool,
}

impl GainLockConfig {
    /// Checks the table against itself and the configured device ids. Strict
    /// about the configured leader on purpose - a signal with no preamp on it
    /// is almost always a typo, and finding that at load beats finding it
    /// mid-show.
    pub fn validate(&self, is_device: impl Fn(&str) -> bool) -> Result<(), String> {
        if !is_device(&self.leader) {
            return Err(format!("gain_lock leader '{}' is not a configured device", self.leader));
        }
        let mut names = HashSet::new();
        let mut trims = HashSet::new();
        for signal in &self.signals {
            if !names.insert(signal.name.as_str()) {
                return Err(format!("gain_lock signal '{}' is defined twice", signal.name));
            }
            for member in &signal.members {
                if !is_device(&member.device) {
                    return Err(format!(
                        "gain_lock signal '{}' names device '{}', which is not configured",
                        signal.name, member.device
                    ));
                }
                if member.preamp.is_none() && member.input.is_none() {
                    return Err(format!(
                        "gain_lock signal '{}': member '{}' needs a preamp, an input, or both",
                        signal.name, member.device
                    ));
                }
                if let Some(input) = member.input {
                    if !trims.insert((member.device.as_str(), input)) {
                        return Err(format!(
                            "gain_lock: input {input} on '{}' is in two signals - one trim can only \
                             compensate one source",
                            member.device
                        ));
                    }
                }
            }
            let leads = signal.members.iter().any(|m| m.device == self.leader && m.preamp.is_some());
            if !leads {
                return Err(format!(
                    "gain_lock signal '{}' has no preamp on the leader '{}'",
                    signal.name, self.leader
                ));
            }
        }
        Ok(())
    }
}

/// What the engine reports about itself - the web UI's view.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GainLockStatus {
    pub leader: String,
    pub locked: bool,
    /// Every device that leads at least one signal, i.e. could be elected.
    pub candidates: Vec<String>,
    pub signals: Vec<SignalStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SignalStatus {
    pub name: String,
    pub leader_preamp: Option<u16>,
    /// Whether the leader's preamp for this signal was found, so its gain can
    /// be set from here.
    pub controllable: bool,
    /// The leader's gain as last observed.
    pub gain_db: Option<f32>,
    /// The leader's gain when the system was locked.
    pub reference_db: Option<f32>,
    /// Why this signal isn't being compensated, if it isn't.
    pub problem: Option<String>,
    pub followers: Vec<FollowerStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FollowerStatus {
    pub device: String,
    pub input: u16,
    /// Net trim change this lock has made on the follower.
    pub applied_db: f32,
    /// Compensation asked for but not (yet) made: under half a trim step, or
    /// beyond the trim's range, or waiting on a failed device.
    pub owed_db: f32,
    pub error: Option<String>,
}

struct Live {
    name: String,
    leader_preamp: Option<u16>,
    leader_ono: Option<Ono>,
    gain: Option<f32>,
    reference: Option<f32>,
    problem: Option<String>,
    followers: Vec<Follower>,
    /// Leader values the engine itself caused and expects to hear reported
    /// back, oldest first. A reply to a get looks exactly like a pushed
    /// change on X32, Wing and dLive, and arrives a polling interval or two
    /// later - so the lock's own read of 30 dB, landing after a 33 dB set
    /// made straight away, would read as the leader moving back down and
    /// undo the set's compensation.
    ///
    /// An echo is only believed if it is the *next* thing heard for the
    /// signal: the desk sends it before anything newer. Any other report
    /// clears the list, because a desk that has moved on has either echoed
    /// already or never will (not every adapter echoes a read - one answered
    /// from cache sends nothing), and a stale expectation left lying around
    /// would swallow a genuine later move back to that value. Entries also
    /// expire after [`ECHO_TTL`]. The same value-equality idea as the
    /// Router's echo suppression.
    expected: Vec<(f32, Instant)>,
}

impl Live {
    fn expect(&mut self, gain: f32) {
        self.expected.push((gain, Instant::now()));
    }

    /// Whether `gain` is the report of something the engine caused, settling
    /// the expectations either way (see [`expected`](Self::expected)).
    fn is_expected(&mut self, gain: f32) -> bool {
        self.expected.retain(|(_, at)| at.elapsed() < ECHO_TTL);
        match self.expected.iter().position(|(v, _)| (v - gain).abs() < EPSILON_DB) {
            Some(i) => {
                // Anything older than the matched echo was never coming.
                self.expected.drain(..=i);
                true
            }
            None => {
                self.expected.clear();
                false
            }
        }
    }
}

struct Follower {
    device: String,
    input: u16,
    ono: Option<Ono>,
    applied: f32,
    owed: f32,
    error: Option<String>,
}

struct Inner {
    leader: String,
    locked: bool,
    signals: Vec<Signal>,
    live: Vec<Live>,
    /// Follower desks whose last move failed. Their followers keep accruing
    /// what they're owed without touching the desk - a dead monitor desk
    /// with 32 inputs must not make every leader move wait out 32 timeouts
    /// while the healthy desks queue behind it - until the retry tick's
    /// probe finds the desk answering again.
    down: HashSet<String>,
}

/// The gain-lock engine. Construct with [`GainLock::new`], then
/// [`spawn`](GainLock::spawn) it to start following the leader.
pub struct GainLock {
    router: Arc<Router>,
    poll: Option<Duration>,
    inner: Mutex<Inner>,
}

impl GainLock {
    /// Builds the engine and resolves each signal's objects against the
    /// devices the Router has right now. Doesn't lock - see
    /// [`GainLockConfig::lock_on_start`].
    pub async fn new(router: Arc<Router>, config: GainLockConfig) -> Arc<Self> {
        let engine = Arc::new(Self {
            router,
            poll: config.poll_ms.filter(|ms| *ms > 0).map(Duration::from_millis),
            inner: Mutex::new(Inner {
                leader: config.leader,
                locked: false,
                signals: config.signals,
                live: Vec::new(),
                down: HashSet::new(),
            }),
        });
        {
            let mut inner = engine.inner.lock().await;
            engine.resolve(&mut inner).await;
        }
        engine
    }

    /// Starts following the leader's gain. Subscribes before returning, so
    /// nothing reported after this call is missed.
    pub fn spawn(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let rx = self.router.tap();
        let engine = Arc::clone(self);
        tokio::spawn(async move { engine.run(rx).await })
    }

    async fn run(self: Arc<Self>, mut rx: broadcast::Receiver<OcaEvent>) {
        let mut poll = self.poll.map(tokio::time::interval);
        let mut retry = tokio::time::interval(RETRY_INTERVAL);
        loop {
            tokio::select! {
                received = rx.recv() => match received {
                    Ok(event) => {
                        // A knob turn arrives as a burst; take everything
                        // already queued and act on the latest value per
                        // signal, not on every intermediate step.
                        let mut batch = vec![event];
                        while let Ok(event) = rx.try_recv() {
                            batch.push(event);
                        }
                        self.on_events(batch).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Leader reports were dropped, so a change may have
                        // been missed: read the leader's gains back, which
                        // turns whatever was missed into one delta.
                        warn!(dropped = n, "gain lock fell behind the event stream; re-reading the leader");
                        self.read_leader().await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = tick(&mut poll) => self.read_leader().await,
                _ = retry.tick() => self.retry_failed().await,
            }
        }
    }

    /// Captures every signal's current leader gain as the reference and
    /// starts compensating from it. Locking again re-captures - the trims
    /// keep whatever they hold, and the new lock starts its own count.
    pub async fn lock(&self) -> GainLockStatus {
        let mut inner = self.inner.lock().await;
        self.resolve(&mut inner).await;
        inner.down.clear();
        let leader = inner.leader.clone();
        let deadline = Instant::now() + READ_BUDGET;
        for live in &mut inner.live {
            for follower in &mut live.followers {
                follower.applied = 0.0;
                follower.owed = 0.0;
                follower.error = None;
            }
            live.reference = None;
            let Some(ono) = live.leader_ono else { continue };
            match self.read_by(&leader, ono, deadline).await {
                Ok(gain) => {
                    live.gain = Some(gain);
                    live.expect(gain);
                }
                Err(e) if live.gain.is_some() => {
                    warn!(signal = %live.name, error = %e, "couldn't re-read the leader's gain; locking on the last value it reported");
                }
                Err(e) => {
                    live.problem = Some(format!("not armed - couldn't read the leader's gain ({e}); arms on its first report"));
                    continue;
                }
            }
            live.reference = live.gain;
        }
        inner.locked = true;
        info!(leader = %leader, "gain lock engaged");
        snapshot(&inner)
    }

    /// Stops compensating. The trims stay where the lock left them.
    pub async fn unlock(&self) -> GainLockStatus {
        let mut inner = self.inner.lock().await;
        inner.locked = false;
        info!("gain lock released");
        snapshot(&inner)
    }

    /// Elects a new leader. Refused while locked - changing who leads
    /// mid-lock would make the next reading of the new leader look like a
    /// change of however many dB its preamps differ from the old one's.
    pub async fn set_leader(&self, device: &str) -> Result<GainLockStatus, String> {
        let mut inner = self.inner.lock().await;
        if inner.locked {
            return Err("unlock before changing the leader".into());
        }
        let leads_something = inner.signals.iter().any(|s| s.members.iter().any(|m| m.device == device && m.preamp.is_some()));
        if !leads_something {
            return Err(format!("'{device}' has no preamp in any signal, so it can't lead"));
        }
        inner.leader = device.to_string();
        self.resolve(&mut inner).await;
        info!(leader = %device, "gain lock leader elected");
        Ok(snapshot(&inner))
    }

    /// Sets a signal's preamp gain on the leader. While locked, the followers
    /// are compensated straight away rather than waiting for the leader to
    /// report the change - not every desk reports a change back to the
    /// client that made it.
    pub async fn set_preamp(&self, signal: &str, gain_db: f32) -> Result<GainLockStatus, String> {
        let mut inner = self.inner.lock().await;
        let index = inner.live.iter().position(|l| l.name == signal).ok_or_else(|| format!("no signal '{signal}'"))?;
        let ono = inner.live[index]
            .leader_ono
            .ok_or_else(|| inner.live[index].problem.clone().unwrap_or_else(|| "no preamp on the leader".into()))?;
        let device = self.router.device(&inner.leader).ok_or_else(|| format!("leader '{}' is not connected", inner.leader))?;
        device.lock().await.set_object(ono, OcaValue::F32(gain_db)).await.map_err(|e| e.to_string())?;
        // A desk that confirms the write reports this value back.
        inner.live[index].expect(gain_db);
        self.observe(&mut inner, index, gain_db).await;
        Ok(snapshot(&inner))
    }

    /// Moves a signal's leader preamp by `delta_db` from its current gain.
    pub async fn nudge_preamp(&self, signal: &str, delta_db: f32) -> Result<GainLockStatus, String> {
        let current = {
            let inner = self.inner.lock().await;
            let live = inner.live.iter().find(|l| l.name == signal).ok_or_else(|| format!("no signal '{signal}'"))?;
            live.gain.ok_or_else(|| format!("the leader's gain for '{signal}' isn't known yet"))?
        };
        self.set_preamp(signal, current + delta_db).await
    }

    pub async fn status(&self) -> GainLockStatus {
        snapshot(&*self.inner.lock().await)
    }

    async fn on_events(&self, batch: Vec<OcaEvent>) {
        let mut inner = self.inner.lock().await;
        let mut latest: Vec<(usize, f32)> = Vec::new();
        for event in &batch {
            if event.address.device_id != inner.leader {
                continue;
            }
            let Some(gain) = event.object.value.as_f32() else { continue };
            for (index, live) in inner.live.iter_mut().enumerate() {
                if live.leader_ono != Some(event.address.ono) || live.is_expected(gain) {
                    continue;
                }
                match latest.iter_mut().find(|(i, _)| *i == index) {
                    Some(entry) => entry.1 = gain,
                    None => latest.push((index, gain)),
                }
            }
        }
        for (index, gain) in latest {
            self.observe(&mut inner, index, gain).await;
        }
    }

    /// The one place a leader gain becomes compensation.
    async fn observe(&self, inner: &mut Inner, index: usize, gain: f32) {
        let Inner { locked, live, down, .. } = inner;
        let live = &mut live[index];
        let previous = live.gain.replace(gain);
        if !*locked {
            return;
        }
        let Some(previous) = previous.filter(|_| live.reference.is_some()) else {
            // Locked without a reading for this signal: this first report
            // is its reference. A change made between the lock and now
            // can't be told apart from where the gain started, so it isn't
            // compensated - the status said "not armed" for exactly that.
            live.reference = Some(gain);
            live.problem = None;
            info!(signal = %live.name, gain, "gain lock armed on the leader's first report");
            return;
        };
        let delta = gain - previous;
        if delta.abs() < EPSILON_DB {
            return;
        }
        debug!(signal = %live.name, delta, "leader gain moved; compensating followers");
        for follower in &mut live.followers {
            follower.owed -= delta;
        }
        settle(&self.router, live.followers.iter_mut().collect(), down).await;
    }

    async fn read_leader(&self) {
        let mut inner = self.inner.lock().await;
        let leader = inner.leader.clone();
        let deadline = Instant::now() + READ_BUDGET;
        for index in 0..inner.live.len() {
            let Some(ono) = inner.live[index].leader_ono else { continue };
            match self.read_by(&leader, ono, deadline).await {
                Ok(gain) => {
                    inner.live[index].expect(gain);
                    self.observe(&mut inner, index, gain).await
                }
                Err(e) => debug!(signal = %inner.live[index].name, error = %e, "leader gain read failed"),
            }
        }
    }

    /// Probes each desk marked down with a plain read - outside the lock, so
    /// a desk still away doesn't hold up compensation for the others - and
    /// settles every follower on a desk that answers.
    async fn retry_failed(&self) {
        let probes: Vec<(String, Ono)> = {
            let inner = self.inner.lock().await;
            if !inner.locked {
                return;
            }
            inner
                .down
                .iter()
                .filter_map(|device| {
                    let ono = inner.live.iter().flat_map(|l| &l.followers).find(|f| &f.device == device)?.ono?;
                    Some((device.clone(), ono))
                })
                .collect()
        };
        let mut back = Vec::new();
        for (device, ono) in probes {
            if self.read_by(&device, ono, Instant::now() + READ_TIMEOUT).await.is_ok() {
                back.push(device);
            }
        }
        if back.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().await;
        if !inner.locked {
            return;
        }
        let Inner { live, down, .. } = &mut *inner;
        for device in &back {
            info!(device = %device, "follower desk answering again; settling what it is owed");
            down.remove(device);
        }
        let followers = live.iter_mut().flat_map(|l| l.followers.iter_mut()).filter(|f| back.contains(&f.device)).collect();
        settle(&self.router, followers, down).await;
    }

    /// Reads a gain, giving up at `deadline` (and after [`READ_TIMEOUT`]
    /// whatever the deadline).
    async fn read_by(&self, device_id: &str, ono: Ono, deadline: Instant) -> Result<f32, String> {
        let wait = deadline.saturating_duration_since(Instant::now()).min(READ_TIMEOUT);
        if wait.is_zero() {
            return Err("out of time to read".into());
        }
        let device = self.router.device(device_id).ok_or_else(|| format!("'{device_id}' is not connected"))?;
        let value = tokio::time::timeout(wait, async { device.lock().await.get_object(ono).await })
            .await
            .map_err(|_| "timed out".to_string())?
            .map_err(|e| e.to_string())?;
        value.as_f32().ok_or_else(|| format!("expected a gain in dB, got {value:?}"))
    }

    /// (Re)builds the live table for the current leader, carrying over what
    /// is already known about any signal/follower that survives.
    async fn resolve(&self, inner: &mut Inner) {
        let leader_descriptors = match self.router.device(&inner.leader) {
            Some(device) => Some(device.lock().await.describe()),
            None => None,
        };
        let mut live = Vec::with_capacity(inner.signals.len());
        for signal in &inner.signals {
            let previous = inner.live.iter().find(|l| l.name == signal.name);
            let leader_preamp = signal.members.iter().find(|m| m.device == inner.leader).and_then(|m| m.preamp);

            let (leader_ono, problem) = match (leader_preamp, &leader_descriptors) {
                (None, _) => (None, Some(format!("no preamp on the leader '{}'", inner.leader))),
                (Some(_), None) => (None, Some(format!("leader '{}' is not connected", inner.leader))),
                (Some(preamp), Some(descriptors)) => {
                    let role = channel_scheme::role(preamp, Field::Gain);
                    match descriptors.iter().find(|d| d.settable && d.role == role) {
                        Some(d) => (Some(d.ono), None),
                        None => (None, Some(format!("leader '{}' has no preamp {preamp}", inner.leader))),
                    }
                }
            };

            let mut followers = Vec::new();
            for member in signal.members.iter().filter(|m| m.device != inner.leader) {
                let Some(input) = member.input else { continue };
                let carried = previous.and_then(|p| p.followers.iter().find(|f| f.device == member.device && f.input == input));
                let (ono, error) = match self.router.device(&member.device) {
                    None => (None, Some(format!("'{}' is not connected", member.device))),
                    Some(device) => {
                        let role = channel_scheme::trim_role(input);
                        match device.lock().await.describe().into_iter().find(|d| d.settable && d.role == role) {
                            Some(d) => (Some(d.ono), None),
                            None => (None, Some(format!("'{}' has no trim control for input {input}", member.device))),
                        }
                    }
                };
                followers.push(Follower {
                    device: member.device.clone(),
                    input,
                    ono,
                    applied: carried.map_or(0.0, |f| f.applied),
                    owed: carried.map_or(0.0, |f| f.owed),
                    error,
                });
            }

            // A different leader's preamp is a different gain; nothing the
            // old leader reported carries over.
            let same_leader = previous.is_some_and(|p| p.leader_ono == leader_ono && p.leader_preamp == leader_preamp);
            live.push(Live {
                name: signal.name.clone(),
                leader_preamp,
                leader_ono,
                gain: previous.filter(|_| same_leader).and_then(|p| p.gain),
                reference: previous.filter(|_| same_leader).and_then(|p| p.reference),
                problem,
                followers,
                expected: previous.filter(|_| same_leader).map(|p| p.expected.clone()).unwrap_or_default(),
            });
        }
        inner.live = live;
    }
}

/// Offers each follower what it is owed - the desks in parallel, so one slow
/// desk costs one timeout rather than one per follower - and books what each
/// took. A follower on a desk in `down` is left accruing (see
/// [`Inner::down`]); a move that fails puts its desk there.
async fn settle(router: &Router, followers: Vec<&mut Follower>, down: &mut HashSet<String>) {
    let mut moves = tokio::task::JoinSet::new();
    let mut waiting: Vec<&mut Follower> = Vec::new();
    for follower in followers {
        if follower.owed.abs() < EPSILON_DB {
            continue;
        }
        if down.contains(&follower.device) {
            if follower.error.is_none() {
                follower.error = Some(format!("'{}' isn't answering; holding this move until it does", follower.device));
            }
            continue;
        }
        let Some(ono) = follower.ono else { continue };
        let Some(device) = router.device(&follower.device) else {
            follower.error = Some(format!("'{}' is not connected", follower.device));
            continue;
        };
        let (index, owed) = (waiting.len(), follower.owed);
        waiting.push(follower);
        moves.spawn(async move {
            let result = tokio::time::timeout(NUDGE_TIMEOUT, async { device.lock().await.nudge_object(ono, owed).await })
                .await
                .map_err(|_| "timed out".to_string())
                .and_then(|r| r.map_err(|e| e.to_string()));
            (index, result)
        });
    }
    while let Some(joined) = moves.join_next().await {
        // A move task that panicked leaves its follower's owed as it was.
        let Ok((index, result)) = joined else { continue };
        let follower = &mut waiting[index];
        match result {
            Ok(applied) => {
                follower.applied += applied;
                follower.owed -= applied;
                if follower.owed.abs() < EPSILON_DB {
                    follower.owed = 0.0;
                }
                follower.error = None;
            }
            Err(e) => {
                warn!(device = %follower.device, input = follower.input, owed = follower.owed, error = %e,
                    "follower trim move failed; holding its desk's moves until it answers again");
                follower.error = Some(e);
                down.insert(follower.device.clone());
            }
        }
    }
}

fn snapshot(inner: &Inner) -> GainLockStatus {
    let mut candidates: Vec<String> = inner
        .signals
        .iter()
        .flat_map(|s| s.members.iter().filter(|m| m.preamp.is_some()).map(|m| m.device.clone()))
        .collect();
    candidates.sort();
    candidates.dedup();
    GainLockStatus {
        leader: inner.leader.clone(),
        locked: inner.locked,
        candidates,
        signals: inner
            .live
            .iter()
            .map(|l| SignalStatus {
                name: l.name.clone(),
                leader_preamp: l.leader_preamp,
                controllable: l.leader_ono.is_some(),
                gain_db: l.gain,
                reference_db: l.reference,
                problem: l.problem.clone(),
                followers: l
                    .followers
                    .iter()
                    .map(|f| FollowerStatus {
                        device: f.device.clone(),
                        input: f.input,
                        applied_db: f.applied,
                        owed_db: f.owed,
                        error: f.error.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// An interval's next tick, or never when there is no interval.
async fn tick(interval: &mut Option<tokio::time::Interval>) {
    match interval {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{AdapterError, AdapterResult, DeviceInfo};
    use crate::law::DbLaw;
    use crate::local_adapter::LocalAdapter;
    use async_trait::async_trait;
    use dante_babelbox_oca::{OcaAddress, OcaObject, OcaObjectDescriptor};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    const X32_TRIM: DbLaw = DbLaw::new(-18.0, 18.0, 0.25);

    /// One mock desk: preamps it can report and be set, trims it can be
    /// nudged on, with a real range and step so clamping and rounding are
    /// exercised rather than assumed away.
    #[derive(Clone)]
    struct Desk {
        id: String,
        tx: broadcast::Sender<OcaEvent>,
        gains: Arc<StdMutex<HashMap<u16, f32>>>,
        trims: Arc<StdMutex<HashMap<u16, f32>>>,
        law: DbLaw,
        preamps: u16,
        inputs: u16,
        /// Whether a `set_object` is reported back on the event stream - many
        /// desks don't tell the client that made a change about it.
        echo_sets: bool,
        trims_fail: Arc<AtomicBool>,
        reads_fail: Arc<AtomicBool>,
        /// Report a read's value on the event stream this long after the
        /// read - what X32, Wing and dLive do, since a reply to a get looks
        /// exactly like a pushed change.
        echo_reads_after: Option<Duration>,
        /// How many trim moves reached this desk.
        nudges: Arc<AtomicUsize>,
    }

    impl Desk {
        fn new(id: &str, preamps: u16, inputs: u16) -> Self {
            Self {
                id: id.into(),
                tx: broadcast::channel(64).0,
                gains: Arc::default(),
                trims: Arc::default(),
                law: X32_TRIM,
                preamps,
                inputs,
                echo_sets: false,
                trims_fail: Arc::default(),
                reads_fail: Arc::default(),
                echo_reads_after: None,
                nudges: Arc::default(),
            }
        }

        /// Someone turns preamp `channel` on this desk's own surface.
        fn turn(&self, channel: u16, gain_db: f32) {
            self.gains.lock().unwrap().insert(channel, gain_db);
            let _ = self.tx.send(gain_event(&self.id, channel, gain_db));
        }

        fn trim(&self, input: u16) -> f32 {
            self.trims.lock().unwrap().get(&input).copied().unwrap_or(0.0)
        }

        fn set_trim_locally(&self, input: u16, trim_db: f32) {
            self.trims.lock().unwrap().insert(input, trim_db);
        }
    }

    fn gain_event(device_id: &str, channel: u16, gain_db: f32) -> OcaEvent {
        let descriptor = channel_scheme::descriptor(channel, Field::Gain);
        OcaEvent {
            address: OcaAddress::new(device_id, descriptor.ono),
            object: OcaObject::from_descriptor(descriptor, OcaValue::F32(gain_db)),
        }
    }

    #[async_trait]
    impl LocalAdapter for Desk {
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
            Ok(DeviceInfo { vendor: "mock".into(), model: "desk".into(), address: "127.0.0.1".parse().unwrap() })
        }
        fn describe(&self) -> Vec<OcaObjectDescriptor> {
            let mut out: Vec<_> = (1..=self.preamps).map(|c| channel_scheme::descriptor(c, Field::Gain)).collect();
            out.extend(channel_scheme::trim_descriptors(self.inputs));
            out
        }
        async fn get_object(&mut self, ono: Ono) -> AdapterResult<OcaValue> {
            if self.reads_fail.load(Ordering::SeqCst) {
                return Err(AdapterError::Connection("no reply".into()));
            }
            if let Some(input) = channel_scheme::decode_trim_ono(ono, self.inputs) {
                if self.trims_fail.load(Ordering::SeqCst) {
                    return Err(AdapterError::Connection("desk unreachable".into()));
                }
                return Ok(OcaValue::F32(self.trim(input)));
            }
            let (channel, _) = channel_scheme::decode_ono(ono, self.preamps).ok_or(AdapterError::UnsupportedChannel(0))?;
            let gain = self.gains.lock().unwrap().get(&channel).copied().unwrap_or(0.0);
            if let Some(delay) = self.echo_reads_after {
                let (tx, id) = (self.tx.clone(), self.id.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    let _ = tx.send(gain_event(&id, channel, gain));
                });
            }
            Ok(OcaValue::F32(gain))
        }
        async fn set_object(&mut self, ono: Ono, value: OcaValue) -> AdapterResult<()> {
            let (channel, _) = channel_scheme::decode_ono(ono, self.preamps).ok_or(AdapterError::UnsupportedChannel(0))?;
            let gain = value.as_f32().unwrap();
            self.gains.lock().unwrap().insert(channel, gain);
            if self.echo_sets {
                let _ = self.tx.send(gain_event(&self.id, channel, gain));
            }
            Ok(())
        }
        fn subscribe(&self) -> broadcast::Receiver<OcaEvent> {
            self.tx.subscribe()
        }
        async fn nudge_object(&mut self, ono: Ono, delta: f32) -> AdapterResult<f32> {
            self.nudges.fetch_add(1, Ordering::SeqCst);
            if self.trims_fail.load(Ordering::SeqCst) {
                return Err(AdapterError::Connection("desk unreachable".into()));
            }
            let input = channel_scheme::decode_trim_ono(ono, self.inputs).ok_or(AdapterError::UnsupportedChannel(0))?;
            let mut trims = self.trims.lock().unwrap();
            let current = trims.get(&input).copied().unwrap_or(0.0);
            let (target, applied) = self.law.nudge(current, delta);
            trims.insert(input, target);
            Ok(applied)
        }
    }

    fn signal(name: &str, leader: (&str, u16), followers: &[(&str, u16)]) -> Signal {
        let mut members = vec![Member { device: leader.0.into(), preamp: Some(leader.1), input: None }];
        members.extend(followers.iter().map(|(d, i)| Member { device: (*d).into(), preamp: None, input: Some(*i) }));
        Signal { name: name.into(), members }
    }

    async fn rig(foh: &Desk, mon: &Desk, signals: Vec<Signal>) -> Arc<GainLock> {
        rig_of(&[foh, mon], signals).await
    }

    /// The first desk leads.
    async fn rig_of(desks: &[&Desk], signals: Vec<Signal>) -> Arc<GainLock> {
        let router = Router::new(Vec::new());
        for desk in desks {
            router.register_device(desk.id.clone(), Arc::new(Mutex::new(Box::new((*desk).clone()) as Box<dyn LocalAdapter>))).await;
        }
        let config = GainLockConfig { leader: desks[0].id.clone(), signals, poll_ms: None, lock_on_start: false };
        let engine = GainLock::new(router, config).await;
        engine.spawn();
        engine
    }

    /// Polls until `check` passes, so a test doesn't race the Router's
    /// listener task or guess at a sleep.
    async fn eventually(what: &str, check: impl Fn() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn raising_the_leader_trims_the_follower_down_by_the_same_amount() {
        let foh = Desk::new("foh", 8, 0);
        let mon = Desk::new("mon", 0, 8);
        foh.gains.lock().unwrap().insert(3, 30.0);
        let engine = rig(&foh, &mon, vec![signal("Kick", ("foh", 3), &[("mon", 7)])]).await;

        let status = engine.lock().await;
        assert_eq!(status.signals[0].reference_db, Some(30.0));

        foh.turn(3, 33.0);
        eventually("mon trim -3", || mon.trim(7) == -3.0).await;
        foh.turn(3, 31.5);
        eventually("mon trim -1.5", || mon.trim(7) == -1.5).await;

        let status = engine.status().await;
        assert_eq!(status.signals[0].followers[0].applied_db, -1.5);
        assert_eq!(status.signals[0].followers[0].owed_db, 0.0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nothing_moves_while_unlocked() {
        let foh = Desk::new("foh", 8, 0);
        let mon = Desk::new("mon", 0, 8);
        let engine = rig(&foh, &mon, vec![signal("Kick", ("foh", 1), &[("mon", 1)])]).await;

        foh.turn(1, 20.0);
        eventually("leader gain observed", || {
            block_on(engine.status()).signals[0].gain_db == Some(20.0)
        })
        .await;
        assert_eq!(mon.trim(1), 0.0);

        engine.lock().await;
        engine.unlock().await;
        foh.turn(1, 26.0);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(mon.trim(1), 0.0, "an unlocked system must not touch follower trims");
    }

    /// Blocks on a future from a sync closure - only for `eventually` checks
    /// that need the engine's async status.
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(f))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_trim_change_made_at_the_follower_survives_compensation() {
        let foh = Desk::new("foh", 8, 0);
        let mon = Desk::new("mon", 0, 8);
        foh.gains.lock().unwrap().insert(2, 40.0);
        let engine = rig(&foh, &mon, vec![signal("Vox", ("foh", 2), &[("mon", 2)])]).await;
        engine.lock().await;

        // The monitor engineer pulls the vocal down 2 dB mid-show...
        mon.set_trim_locally(2, -2.0);
        // ...then FOH rides the preamp up 3.
        foh.turn(2, 43.0);
        eventually("mon trim -5", || mon.trim(2) == -5.0).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_trim_pinned_at_its_limit_is_not_pushed_past_where_it_started() {
        let foh = Desk::new("foh", 8, 0);
        let mon = Desk::new("mon", 0, 8);
        foh.gains.lock().unwrap().insert(1, 30.0);
        mon.set_trim_locally(1, 17.0);
        let engine = rig(&foh, &mon, vec![signal("Kick", ("foh", 1), &[("mon", 1)])]).await;
        engine.lock().await;

        // Leader down 3: the follower wants +3 but only has 1 dB left.
        foh.turn(1, 27.0);
        eventually("mon pinned at +18", || mon.trim(1) == 18.0).await;
        let status = engine.status().await;
        assert_eq!(status.signals[0].followers[0].applied_db, 1.0);
        assert_eq!(status.signals[0].followers[0].owed_db, 2.0);

        // Leader back up 2: the owed 2 dB absorbs it - the trim stays put.
        foh.turn(1, 29.0);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(mon.trim(1), 18.0);

        // Leader back to where it was locked: the trim returns to where it was.
        foh.turn(1, 30.0);
        eventually("mon back at +17", || mon.trim(1) == 17.0).await;
        let status = engine.status().await;
        assert_eq!(status.signals[0].followers[0].owed_db, 0.0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rounding_to_the_trim_step_never_accumulates() {
        let foh = Desk::new("foh", 8, 0);
        let mon = Desk::new("mon", 0, 8);
        let engine = rig(&foh, &mon, vec![signal("Kick", ("foh", 1), &[("mon", 1)])]).await;
        engine.lock().await;

        // A dLive preamp moves in 55/127 dB steps; an X32 trim in 0.25 dB.
        let step = 55.0 / 127.0;
        let mut gain = 0.0;
        for i in 0..40 {
            gain += if i % 3 == 2 { -step } else { step };
            foh.turn(1, gain);
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        eventually("follower caught up", || {
            let status = block_on(engine.status());
            let f = &status.signals[0].followers[0];
            (f.applied_db + f.owed_db + gain).abs() < 1e-3
        })
        .await;
        assert!((mon.trim(1) + gain).abs() <= 0.125 + 1e-4, "trim {} vs exact {}", mon.trim(1), -gain);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn setting_the_preamp_here_compensates_even_when_the_leader_says_nothing() {
        let foh = Desk::new("foh", 8, 0); // echo_sets = false
        let mon = Desk::new("mon", 0, 8);
        foh.gains.lock().unwrap().insert(5, 20.0);
        let engine = rig(&foh, &mon, vec![signal("Gtr", ("foh", 5), &[("mon", 3)])]).await;
        engine.lock().await;

        engine.set_preamp("Gtr", 26.0).await.unwrap();
        assert_eq!(foh.gains.lock().unwrap()[&5], 26.0);
        assert_eq!(mon.trim(3), -6.0);

        engine.nudge_preamp("Gtr", -1.0).await.unwrap();
        assert_eq!(mon.trim(3), -5.0);
    }

    /// The reply to the lock's own read of the leader arrives on the event
    /// stream after the lock - and here after a gain set made straight away.
    /// That late 30 dB is old news, not the leader moving back down; taking
    /// it as a move undid the set's compensation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_echo_of_our_own_read_does_not_undo_a_newer_set() {
        let mut foh = Desk::new("foh", 8, 0);
        foh.echo_reads_after = Some(Duration::from_millis(40));
        let mon = Desk::new("mon", 0, 8);
        foh.gains.lock().unwrap().insert(1, 30.0);
        let engine = rig(&foh, &mon, vec![signal("Kick", ("foh", 1), &[("mon", 1)])]).await;

        engine.lock().await;
        engine.set_preamp("Kick", 33.0).await.unwrap();
        assert_eq!(mon.trim(1), -3.0);

        tokio::time::sleep(Duration::from_millis(150)).await; // the echo lands
        assert_eq!(mon.trim(1), -3.0, "the read's echo must not count as the leader moving");
        assert_eq!(engine.status().await.signals[0].gain_db, Some(33.0));

        // A real move afterwards is still followed.
        foh.turn(1, 35.0);
        eventually("real move compensated", || mon.trim(1) == -5.0).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_echo_of_our_own_set_is_not_compensated_twice() {
        let mut foh = Desk::new("foh", 8, 0);
        foh.echo_sets = true;
        let mon = Desk::new("mon", 0, 8);
        foh.gains.lock().unwrap().insert(5, 20.0);
        let engine = rig(&foh, &mon, vec![signal("Gtr", ("foh", 5), &[("mon", 3)])]).await;
        engine.lock().await;

        engine.set_preamp("Gtr", 24.0).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(mon.trim(3), -4.0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_follower_keeps_what_it_is_owed_and_takes_it_on_retry() {
        let foh = Desk::new("foh", 8, 0);
        let mon = Desk::new("mon", 0, 8);
        let engine = rig(&foh, &mon, vec![signal("Kick", ("foh", 1), &[("mon", 1)])]).await;
        engine.lock().await;

        mon.trims_fail.store(true, Ordering::SeqCst);
        foh.turn(1, 4.0);
        eventually("error recorded", || block_on(engine.status()).signals[0].followers[0].error.is_some()).await;
        assert_eq!(engine.status().await.signals[0].followers[0].owed_db, -4.0);
        assert_eq!(mon.trim(1), 0.0);

        mon.trims_fail.store(false, Ordering::SeqCst);
        engine.retry_failed().await;
        assert_eq!(mon.trim(1), -4.0);
        let status = engine.status().await;
        assert_eq!(status.signals[0].followers[0].error, None);
        assert_eq!(status.signals[0].followers[0].owed_db, 0.0);
    }

    /// A follower desk that has gone away is not called again until it
    /// answers a probe: the healthy desk keeps following without waiting on
    /// it, and the dead one gets everything it missed in one move.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dead_follower_desk_does_not_hold_up_a_healthy_one() {
        let foh = Desk::new("foh", 8, 0);
        let mon = Desk::new("mon", 0, 8);
        let bcast = Desk::new("bcast", 0, 8);
        let engine = rig_of(&[&foh, &mon, &bcast], vec![signal("Kick", ("foh", 1), &[("mon", 1), ("bcast", 1)])]).await;
        engine.lock().await;

        bcast.trims_fail.store(true, Ordering::SeqCst);
        foh.turn(1, 1.0);
        eventually("bcast marked failing", || block_on(engine.status()).signals[0].followers[1].error.is_some()).await;
        let calls_while_down = bcast.nudges.load(Ordering::SeqCst);

        for gain in [2.0, 3.0, 4.0] {
            foh.turn(1, gain);
            let expected = -gain;
            eventually("mon keeps following", || mon.trim(1) == expected).await;
        }
        assert_eq!(bcast.nudges.load(Ordering::SeqCst), calls_while_down, "a desk known down is not called");
        assert!(engine.status().await.signals[0].followers[1].error.is_some(), "a held follower says why it isn't moving");
        engine.retry_failed().await;
        assert_eq!(bcast.trim(1), 0.0, "still down: the probe fails, nothing moves");

        bcast.trims_fail.store(false, Ordering::SeqCst);
        engine.retry_failed().await;
        assert_eq!(bcast.trim(1), -4.0, "back: all it missed, in one move");
        assert_eq!(bcast.nudges.load(Ordering::SeqCst), calls_while_down + 1);
        assert_eq!(engine.status().await.signals[0].followers[1].error, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_leader_that_cannot_be_read_at_lock_arms_on_its_first_report() {
        let foh = Desk::new("foh", 8, 0);
        let mon = Desk::new("mon", 0, 8);
        let engine = rig(&foh, &mon, vec![signal("Kick", ("foh", 1), &[("mon", 1)])]).await;

        foh.reads_fail.store(true, Ordering::SeqCst);
        let status = engine.lock().await;
        assert!(status.signals[0].problem.as_deref().unwrap().starts_with("not armed"));

        foh.turn(1, 30.0);
        eventually("armed", || block_on(engine.status()).signals[0].reference_db == Some(30.0)).await;
        assert_eq!(mon.trim(1), 0.0, "the arming report is the reference, not a change");

        foh.turn(1, 32.0);
        eventually("compensated after arming", || mon.trim(1) == -2.0).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_leader_changes_only_while_unlocked() {
        let foh = Desk::new("foh", 8, 8);
        let mon = Desk::new("mon", 8, 8);
        let shared = Signal {
            name: "Kick".into(),
            members: vec![
                Member { device: "foh".into(), preamp: Some(1), input: Some(1) },
                Member { device: "mon".into(), preamp: Some(1), input: Some(1) },
            ],
        };
        let engine = rig(&foh, &mon, vec![shared]).await;

        engine.lock().await;
        assert!(engine.set_leader("mon").await.is_err());
        engine.unlock().await;
        let status = engine.set_leader("mon").await.unwrap();
        assert_eq!(status.leader, "mon");
        assert_eq!(status.signals[0].followers[0].device, "foh");

        mon.gains.lock().unwrap().insert(1, 10.0);
        engine.lock().await;
        mon.turn(1, 12.0);
        eventually("foh now follows", || foh.trim(1) == -2.0).await;
        assert_eq!(mon.trim(1), 0.0, "the leader's own trim is never touched");
    }

    #[test]
    fn config_validation_catches_the_mistakes_that_matter_mid_show() {
        let devices = |id: &str| matches!(id, "foh" | "mon");
        let ok = GainLockConfig {
            leader: "foh".into(),
            signals: vec![signal("Kick", ("foh", 1), &[("mon", 1)])],
            poll_ms: None,
            lock_on_start: false,
        };
        assert!(ok.validate(devices).is_ok());

        let mut bad = ok.clone();
        bad.leader = "nope".into();
        assert!(bad.validate(devices).unwrap_err().contains("not a configured device"));

        let mut bad = ok.clone();
        bad.signals.push(signal("Snare", ("mon", 2), &[("mon", 1)]));
        let err = bad.validate(devices).unwrap_err();
        assert!(err.contains("two signals") || err.contains("no preamp on the leader"), "{err}");

        let mut bad = ok.clone();
        bad.signals.push(signal("Snare", ("mon", 2), &[]));
        assert!(bad.validate(devices).unwrap_err().contains("no preamp on the leader"));

        let mut bad = ok.clone();
        bad.signals.push(ok.signals[0].clone());
        assert!(bad.validate(devices).unwrap_err().contains("defined twice"));
    }
}
