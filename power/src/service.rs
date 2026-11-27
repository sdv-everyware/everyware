/*
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The Power HAL: what the clients ask for (`speed`), applied to their threads (`threads`).
//!
//! A client is served for its own threads only: a session takes only threads of the process
//! that makes it. The service changes how other processes are scheduled, so it does that for
//! nobody but the caller. A boost is a one-way call, for which binder does not say who
//! calls: it goes to the threads of every open session.

use crate::speed::{Client, Session};
use crate::threads;
use android_hardware_power::aidl::android::hardware::power::{
    Boost::Boost,
    ChannelConfig::ChannelConfig,
    CompositionData::CompositionData,
    CompositionUpdate::CompositionUpdate,
    CpuHeadroomParams::CpuHeadroomParams,
    CpuHeadroomResult::CpuHeadroomResult,
    GpuHeadroomParams::GpuHeadroomParams,
    GpuHeadroomResult::GpuHeadroomResult,
    IPower::IPower,
    IPowerHintSession::{BnPowerHintSession, IPowerHintSession},
    Mode::Mode,
    SessionConfig::SessionConfig,
    SessionHint::SessionHint,
    SessionMode::SessionMode,
    SessionTag::SessionTag,
    SupportInfo::SupportInfo,
    WorkDuration::WorkDuration,
};
use binder::{BinderFeatures, ExceptionCode, Interface, Status, Strong, ThreadState};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The rate at which a session is asked to report its frames: once a frame at 60 Hz.
const REPORT_EVERY_NANOS: i64 = 16_666_666;

/// What the clients asked for, and what their threads were given.
#[derive(Default)]
struct State {
    /// By the id of the client's process.
    clients: HashMap<i32, Client>,
    /// The minimum of speed every thread has now, where it is more than nothing.
    given: HashMap<i32, u32>,
    last_session: i64,
}

impl State {
    /// Gives every thread what the clients ask for at `now`. Returns the next time at which
    /// that changes by itself.
    fn apply(&mut self, now: Instant) -> Option<Instant> {
        self.clients.retain(|_, client| !client.is_done());
        let mut wanted = HashMap::new();
        for client in self.clients.values() {
            client.wanted_at(now, &mut wanted);
        }
        // A thread that is gone, or one the service may not change, is not tried again.
        let give = |thread: i32, speed: u32| {
            threads::set_min_speed(thread, speed)
                .inspect_err(|error| log::debug!("thread {thread}: speed {speed}: {error}"))
                .is_ok()
        };
        self.given.retain(|thread, _| {
            let still_wanted = wanted.contains_key(thread);
            if !still_wanted {
                give(*thread, 0);
            }
            still_wanted
        });
        for (thread, speed) in wanted {
            if self.given.get(&thread) != Some(&speed) && give(thread, speed) {
                self.given.insert(thread, speed);
            }
        }
        self.clients.values().filter_map(|client| client.next_change(now)).min()
    }
}

/// The state, and the thread that takes speed back when a boost ends or a session turns idle.
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic of another call leaves the state as that call left it, which is still what
        // the clients asked for.
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Changes what the clients ask for and gives the threads their speed at once.
    fn change(&self, change: impl FnOnce(&mut State, Instant)) {
        let now = Instant::now();
        let mut state = self.lock();
        change(&mut state, now);
        state.apply(now);
        self.changed.notify_one();
    }

    /// Runs until the process ends: applies the state again whenever it changes by itself.
    fn keep(&self) {
        let mut state = self.lock();
        loop {
            let next = state.apply(Instant::now());
            state = match next {
                Some(at) => {
                    let wait = at.saturating_duration_since(Instant::now());
                    let (state, _) = self
                        .changed
                        .wait_timeout(state, wait)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state
                }
                None => self.changed.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner()),
            };
        }
    }
}

fn unsupported<T>() -> binder::Result<T> {
    Err(Status::new_exception(ExceptionCode::UNSUPPORTED_OPERATION, None))
}

fn illegal<T>(what: &str) -> binder::Result<T> {
    log::warn!("{what}");
    Err(Status::new_exception(ExceptionCode::ILLEGAL_ARGUMENT, None))
}

/// The Power HAL service.
pub struct Power {
    shared: Arc<Shared>,
}

impl Power {
    /// Creates the service and the thread that takes speed back.
    pub fn new() -> Self {
        let shared =
            Arc::new(Shared { state: Mutex::new(State::default()), changed: Condvar::new() });
        let keeper = shared.clone();
        std::thread::Builder::new()
            .name("speed".to_string())
            .spawn(move || keeper.keep())
            .expect("the thread that takes speed back");
        Self { shared }
    }

    fn new_session(
        &self,
        tgid: i32,
        thread_ids: &[i32],
        duration_nanos: i64,
    ) -> binder::Result<(i64, Strong<dyn IPowerHintSession>)> {
        let caller = ThreadState::get_calling_pid();
        if tgid != caller {
            return illegal(&format!("process {caller} asked for a session of process {tgid}"));
        }
        let Some(threads) = own_threads(thread_ids, caller) else {
            return illegal(&format!("process {caller}: a session needs threads of its own"));
        };
        let target = Duration::from_nanos(duration_nanos.max(0) as u64);
        let mut id = 0;
        self.shared.change(|state, _| {
            state.last_session += 1;
            id = state.last_session;
            state
                .clients
                .entry(caller)
                .or_default()
                .sessions()
                .insert(id, Session::new(threads, target));
        });
        log::info!(
            "session {id} of process {caller}: {} threads, {target:?} a frame",
            thread_ids.len()
        );
        let session = HintSession { shared: self.shared.clone(), process: caller, id };
        Ok((id, BnPowerHintSession::new_binder(session, BinderFeatures::default())))
    }
}

/// Returns `thread_ids` if there is at least one and all are threads of `process`.
fn own_threads(thread_ids: &[i32], process: i32) -> Option<Vec<i32>> {
    let own = thread_ids.iter().all(|thread| threads::is_thread_of(*thread, process));
    (own && !thread_ids.is_empty()).then(|| thread_ids.to_vec())
}

impl Interface for Power {}

#[allow(non_snake_case)]
impl IPower for Power {
    fn r#setMode(&self, _mode: Mode, _enabled: bool) -> binder::Result<()> {
        Ok(())
    }

    fn r#isModeSupported(&self, _mode: Mode) -> binder::Result<bool> {
        Ok(false)
    }

    fn r#setBoost(&self, boost: Boost, _duration_ms: i32) -> binder::Result<()> {
        if boost != Boost::INTERACTION {
            return Ok(());
        }
        // How long the interaction lasts is of no use here: the frames that are reported
        // after the touch decide how long the speed is needed.
        self.shared.change(|state, now| {
            state.clients.values_mut().for_each(|client| client.touch(now));
        });
        Ok(())
    }

    fn r#isBoostSupported(&self, boost: Boost) -> binder::Result<bool> {
        Ok(boost == Boost::INTERACTION)
    }

    fn r#createHintSession(
        &self,
        tgid: i32,
        _uid: i32,
        thread_ids: &[i32],
        duration_nanos: i64,
    ) -> binder::Result<Strong<dyn IPowerHintSession>> {
        Ok(self.new_session(tgid, thread_ids, duration_nanos)?.1)
    }

    fn r#getHintSessionPreferredRate(&self) -> binder::Result<i64> {
        Ok(REPORT_EVERY_NANOS)
    }

    fn r#createHintSessionWithConfig(
        &self,
        tgid: i32,
        _uid: i32,
        thread_ids: &[i32],
        duration_nanos: i64,
        _tag: SessionTag,
        config: &mut SessionConfig,
    ) -> binder::Result<Strong<dyn IPowerHintSession>> {
        let (id, session) = self.new_session(tgid, thread_ids, duration_nanos)?;
        config.r#id = id;
        Ok(session)
    }

    fn r#getSessionChannel(&self, _tgid: i32, _uid: i32) -> binder::Result<ChannelConfig> {
        unsupported()
    }

    fn r#closeSessionChannel(&self, _tgid: i32, _uid: i32) -> binder::Result<()> {
        Ok(())
    }

    fn r#getSupportInfo(&self) -> binder::Result<SupportInfo> {
        Ok(SupportInfo {
            r#usesSessions: true,
            r#boosts: 1 << Boost::INTERACTION.0,
            r#sessionHints: 1 << SessionHint::CPU_LOAD_UP.0,
            ..SupportInfo::default()
        })
    }

    fn r#getCpuHeadroom(&self, _params: &CpuHeadroomParams) -> binder::Result<CpuHeadroomResult> {
        unsupported()
    }

    fn r#getGpuHeadroom(&self, _params: &GpuHeadroomParams) -> binder::Result<GpuHeadroomResult> {
        unsupported()
    }

    fn r#sendCompositionData(&self, _data: &[CompositionData]) -> binder::Result<()> {
        Ok(())
    }

    fn r#sendCompositionUpdate(&self, _update: &CompositionUpdate) -> binder::Result<()> {
        Ok(())
    }
}

/// One session of a client.
struct HintSession {
    shared: Arc<Shared>,
    process: i32,
    id: i64,
}

impl HintSession {
    /// Changes the session, if it is still open.
    fn change(&self, change: impl FnOnce(&mut Session, Instant)) {
        self.shared.change(|state, now| {
            let session =
                state.clients.get_mut(&self.process).and_then(|c| c.sessions().get_mut(&self.id));
            if let Some(session) = session {
                change(session, now);
            }
        });
    }

    fn end(&self) {
        self.shared.change(|state, _| {
            if let Some(client) = state.clients.get_mut(&self.process) {
                client.sessions().remove(&self.id);
            }
        });
    }
}

impl Drop for HintSession {
    /// A client that ends without closing its session leaves no speed behind.
    fn drop(&mut self) {
        self.end();
    }
}

impl Interface for HintSession {}

#[allow(non_snake_case)]
impl IPowerHintSession for HintSession {
    fn r#updateTargetWorkDuration(&self, target_nanos: i64) -> binder::Result<()> {
        self.change(|session, _| {
            session.set_target(Duration::from_nanos(target_nanos.max(0) as u64))
        });
        Ok(())
    }

    fn r#reportActualWorkDuration(&self, durations: &[WorkDuration]) -> binder::Result<()> {
        let longest = durations.iter().map(|duration| duration.r#durationNanos).max();
        if let Some(longest) = longest {
            let longest = Duration::from_nanos(longest.max(0) as u64);
            self.change(|session, now| session.report(longest, now));
        }
        Ok(())
    }

    fn r#pause(&self) -> binder::Result<()> {
        self.change(|session, _| session.set_paused(true));
        Ok(())
    }

    fn r#resume(&self) -> binder::Result<()> {
        self.change(|session, _| session.set_paused(false));
        Ok(())
    }

    fn r#close(&self) -> binder::Result<()> {
        self.end();
        Ok(())
    }

    fn r#sendHint(&self, hint: SessionHint) -> binder::Result<()> {
        if hint == SessionHint::CPU_LOAD_UP {
            self.change(|session, now| session.load_up(now));
        }
        Ok(())
    }

    fn r#setThreads(&self, thread_ids: &[i32]) -> binder::Result<()> {
        let Some(threads) = own_threads(thread_ids, self.process) else {
            return illegal(&format!("session {}: threads of another process", self.id));
        };
        self.change(|session, _| session.set_threads(threads));
        Ok(())
    }

    fn r#setMode(&self, _mode: SessionMode, _enabled: bool) -> binder::Result<()> {
        Ok(())
    }

    fn r#getSessionConfig(&self) -> binder::Result<SessionConfig> {
        Ok(SessionConfig { r#id: self.id })
    }
}
