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

//! How much of the CPU's speed the threads of a client are given at least, and when.
//!
//! A governor that follows the load keeps the frequency low for a process that idles between
//! two bursts of work, so the first frames after a touch are computed slowly. The clients
//! tell the Power HAL what they are about to need: a touch (a boost) and, for every frame, the
//! time it had and the time it took (a hint session). From that this module decides the
//! minimum of speed of their threads.
//!
//! A client is known by its sessions: a session names the threads that work on its frames.
//! A boost is a one-way call, which does not say who asks, so it goes to the threads of
//! every open session.
//!
//! A touch gives a session half of the speed at once, so that the first frames after it are
//! not computed at the frequency of an idle device. From there the reports decide: more when
//! a frame is late, less while the frames have time to spare. All of the speed from the
//! start would cost its energy for the whole of a short movement, which is over before the
//! reports have brought it down. It holds no value of a device: the speed is counted in
//! the kernel's scale, where `FULL` is all that the fastest CPU of the device has.
//!
//! Nothing here calls the system: the times come in as arguments, so the rules are tested on
//! any machine.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// All the speed of the fastest CPU, in the scale of the kernel's utilization clamps.
pub const FULL: u32 = 1024;

/// The step a session's speed goes down by for a report of frames with time to spare. Small,
/// so that the speed settles where the frames just have room, instead of swinging between too
/// little and too much: at 60 reports a second it is halved in about a quarter of a second.
const STEP_DOWN: u32 = FULL / 32;

/// A session whose frames are late is given at least this much at once: a late frame is
/// seen, a slow climb from nothing would show as several of them.
const LATE_START: u32 = FULL / 2;

/// What a touch gives a session at least: the middle of the scale, from which its reports go
/// up or down.
pub const TOUCH: u32 = FULL / 2;

/// The step the speed goes up by for every report of late frames above `LATE_START`.
const STEP_UP: u32 = FULL / 4;

/// A frame that took less than this part of its time had time to spare. Between this and
/// all of its time the speed is the right one and stays.
const SPARE_PERCENT: u32 = 50;

/// A session that reported nothing for this long draws no frames: it needs no speed.
pub const SESSION_IDLE: Duration = Duration::from_millis(500);

/// The threads that work on the frames of a client, with the time a frame has and the speed
/// its reports have led to.
#[derive(Debug)]
pub struct Session {
    threads: Vec<i32>,
    target: Duration,
    speed: u32,
    last_report: Option<Instant>,
    paused: bool,
}

impl Session {
    /// A session of `threads` whose frames have `target` each.
    pub fn new(threads: Vec<i32>, target: Duration) -> Self {
        Self { threads, target, speed: 0, last_report: None, paused: false }
    }

    /// The threads of the session.
    pub fn threads(&self) -> &[i32] {
        &self.threads
    }

    /// Replaces the threads of the session.
    pub fn set_threads(&mut self, threads: Vec<i32>) {
        self.threads = threads;
    }

    /// Sets the time a frame has.
    pub fn set_target(&mut self, target: Duration) {
        self.target = target;
    }

    /// A paused session needs no speed until it is resumed.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Takes the longest of the frames of one report, at the time `now`.
    pub fn report(&mut self, longest: Duration, now: Instant) {
        self.last_report = Some(now);
        if self.target.is_zero() {
            return;
        }
        if longest > self.target {
            self.speed = (self.speed.max(LATE_START - STEP_UP) + STEP_UP).min(FULL);
        } else if longest * 100 < self.target * SPARE_PERCENT {
            self.speed = self.speed.saturating_sub(STEP_DOWN);
        }
    }

    /// The client says that its load goes up: all the speed, until its reports say that less
    /// is enough or it stops reporting.
    pub fn load_up(&mut self, now: Instant) {
        self.last_report = Some(now);
        self.speed = FULL;
    }

    /// The user touched the screen: at least `TOUCH`, until the reports say otherwise or the
    /// session stops reporting.
    pub fn touch(&mut self, now: Instant) {
        self.last_report = Some(now);
        self.speed = self.speed.max(TOUCH);
    }

    /// The speed the threads of the session need at `now`.
    pub fn speed_at(&self, now: Instant) -> u32 {
        match self.idle_from() {
            Some(idle) if !self.paused && now < idle => self.speed,
            _ => 0,
        }
    }

    /// The time from which the session counts as idle, if it ever reported.
    pub fn idle_from(&self) -> Option<Instant> {
        self.last_report.map(|last| last + SESSION_IDLE)
    }
}

/// What one client process asked for: its sessions, by their id.
#[derive(Debug, Default)]
pub struct Client {
    sessions: HashMap<i64, Session>,
}

impl Client {
    /// The user touched the screen: every session of the client is told.
    pub fn touch(&mut self, now: Instant) {
        self.sessions.values_mut().for_each(|session| session.touch(now));
    }

    /// The sessions of the client, by their id.
    pub fn sessions(&mut self) -> &mut HashMap<i64, Session> {
        &mut self.sessions
    }

    /// Adds for every thread of the client the speed it needs at `now` to `wanted`. A
    /// thread is in `wanted` only when it needs more than nothing.
    pub fn wanted_at(&self, now: Instant, wanted: &mut HashMap<i32, u32>) {
        for session in self.sessions.values() {
            let speed = session.speed_at(now);
            if speed > 0 {
                for thread in session.threads() {
                    let entry = wanted.entry(*thread).or_insert(0);
                    *entry = (*entry).max(speed);
                }
            }
        }
    }

    /// The next time after `now` at which what the client needs changes by itself: a
    /// session turns idle.
    pub fn next_change(&self, now: Instant) -> Option<Instant> {
        self.sessions.values().filter_map(Session::idle_from).filter(|at| *at > now).min()
    }

    /// Returns true if the client has no session any more and can be forgotten.
    pub fn is_done(&self) -> bool {
        self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_micros(16_667);

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn wanted(client: &Client, now: Instant) -> Vec<(i32, u32)> {
        let mut wanted = HashMap::new();
        client.wanted_at(now, &mut wanted);
        let mut wanted: Vec<_> = wanted.into_iter().collect();
        wanted.sort();
        wanted
    }

    #[test]
    fn a_touch_gives_the_threads_of_the_sessions_half_of_the_speed_at_once() {
        let start = Instant::now();
        let mut client = Client::default();
        assert!(client.is_done(), "no session");
        // without a session there is no thread to give it to
        client.touch(start);
        assert_eq!(wanted(&client, start), []);

        client.sessions().insert(1, Session::new(vec![7, 8], FRAME));
        assert!(!client.is_done());
        client.touch(start);
        assert_eq!(wanted(&client, start), [(7, TOUCH), (8, TOUCH)]);
        // A client that reports no frames is served by the touch alone, until the session
        // counts as idle.
        assert_eq!(client.next_change(start), Some(start + SESSION_IDLE));
        assert_eq!(wanted(&client, start + SESSION_IDLE - ms(1)), [(7, TOUCH), (8, TOUCH)]);
        assert_eq!(wanted(&client, start + SESSION_IDLE), []);
        assert_eq!(client.next_change(start + SESSION_IDLE), None);

        // a touch takes no speed away that the frames asked for
        client.sessions().get_mut(&1).expect("the session").load_up(start + ms(600));
        client.touch(start + ms(700));
        assert_eq!(wanted(&client, start + ms(700)), [(7, FULL), (8, FULL)]);
    }

    #[test]
    fn the_frames_after_a_touch_decide_how_much_of_the_speed_stays() {
        let start = Instant::now();
        let mut client = Client::default();
        client.sessions().insert(1, Session::new(vec![7], FRAME));
        client.touch(start);
        let mut report = |after_ms: u64, took_ms: u64| {
            let now = start + ms(after_ms);
            client.sessions().get_mut(&1).expect("the session").report(ms(took_ms), now);
            wanted(&client, now)
        };
        // frames with time to spare: less with every report
        assert_eq!(report(16, 3), [(7, TOUCH - STEP_DOWN)]);
        assert_eq!(report(32, 3), [(7, TOUCH - 2 * STEP_DOWN)]);
        // frames that just have room: the speed stays
        assert_eq!(report(48, 9), [(7, TOUCH - 2 * STEP_DOWN)]);
        assert_eq!(report(64, 16), [(7, TOUCH - 2 * STEP_DOWN)]);
        // a late frame: more at once, and more again while they stay late
        assert_eq!(report(80, 17), [(7, TOUCH - 2 * STEP_DOWN + STEP_UP)]);
        assert_eq!(report(96, 17), [(7, TOUCH - 2 * STEP_DOWN + 2 * STEP_UP)]);
        assert_eq!(report(112, 17), [(7, FULL)]);
    }

    #[test]
    fn a_session_with_late_frames_gets_speed_at_once_and_more_while_they_stay_late() {
        let start = Instant::now();
        let mut session = Session::new(vec![1], FRAME);
        assert_eq!(session.speed_at(start), 0, "nothing reported yet");

        session.report(ms(20), start);
        assert_eq!(session.speed_at(start), LATE_START);
        session.report(ms(20), start + ms(20));
        assert_eq!(session.speed_at(start + ms(20)), LATE_START + STEP_UP);
        session.report(ms(30), start + ms(40));
        session.report(ms(30), start + ms(60));
        session.report(ms(30), start + ms(80));
        assert_eq!(session.speed_at(start + ms(80)), FULL, "never more than all of it");
    }

    #[test]
    fn a_session_with_time_to_spare_gives_speed_back_in_small_steps() {
        let start = Instant::now();
        let mut session = Session::new(vec![1], FRAME);
        session.load_up(start);
        assert_eq!(session.speed_at(start), FULL);

        // on time, with little to spare: the speed is the right one
        session.report(ms(12), start + ms(16));
        assert_eq!(session.speed_at(start + ms(16)), FULL);
        // less than half of its time: one step down per report
        session.report(ms(8), start + ms(32));
        assert_eq!(session.speed_at(start + ms(32)), FULL - STEP_DOWN);
        for report in 0..40 {
            session.report(ms(2), start + ms(48 + 16 * report));
        }
        assert_eq!(session.speed_at(start + ms(48 + 16 * 39)), 0, "never less than nothing");
    }

    #[test]
    fn a_session_that_stops_reporting_or_is_paused_needs_no_speed() {
        let start = Instant::now();
        let mut session = Session::new(vec![1], FRAME);
        session.report(ms(20), start);
        assert_eq!(session.idle_from(), Some(start + SESSION_IDLE));
        assert_eq!(session.speed_at(start + SESSION_IDLE - ms(1)), LATE_START);
        assert_eq!(session.speed_at(start + SESSION_IDLE), 0);
        // the next report counts from the speed it had
        session.report(ms(20), start + ms(900));
        assert_eq!(session.speed_at(start + ms(900)), LATE_START + STEP_UP);

        session.set_paused(true);
        assert_eq!(session.speed_at(start + ms(900)), 0);
        session.set_paused(false);
        assert_eq!(session.speed_at(start + ms(900)), LATE_START + STEP_UP);

        // a session without a time for its frames cannot be late
        let mut session = Session::new(vec![1], Duration::ZERO);
        session.report(ms(20), start);
        assert_eq!(session.speed_at(start), 0);
        session.set_target(FRAME);
        session.report(ms(20), start);
        assert_eq!(session.speed_at(start), LATE_START);
    }

    #[test]
    fn a_thread_gets_the_most_of_what_its_boost_and_its_sessions_ask_for() {
        let start = Instant::now();
        let mut client = Client::default();
        client.sessions().insert(1, Session::new(vec![7, 8], FRAME));
        client.sessions().insert(2, Session::new(vec![8, 9], FRAME));
        assert_eq!(wanted(&client, start), [], "sessions that reported nothing");

        client.sessions().get_mut(&1).expect("session 1").report(ms(20), start);
        assert_eq!(wanted(&client, start), [(7, LATE_START), (8, LATE_START)]);
        client.sessions().get_mut(&2).expect("session 2").load_up(start + ms(100));
        let both = [(7, LATE_START), (8, FULL), (9, FULL)];
        assert_eq!(wanted(&client, start + ms(100)), both);

        // what changes next: one session after the other turns idle
        assert_eq!(client.next_change(start + ms(100)), Some(start + SESSION_IDLE));
        assert_eq!(wanted(&client, start + SESSION_IDLE), [(8, FULL), (9, FULL)]);
        let last = start + ms(100) + SESSION_IDLE;
        assert_eq!(client.next_change(start + SESSION_IDLE), Some(last));
        assert_eq!(wanted(&client, last), []);
        assert_eq!(client.next_change(last), None);
        assert!(!client.is_done(), "its sessions are still open");

        // a touch: at least its speed for every thread of the sessions
        client.touch(last);
        assert_eq!(wanted(&client, last), [(7, TOUCH), (8, FULL), (9, FULL)]);

        client.sessions().clear();
        assert!(client.is_done());
    }

    #[test]
    fn the_threads_of_a_session_can_be_replaced() {
        let start = Instant::now();
        let mut client = Client::default();
        client.sessions().insert(1, Session::new(vec![7], FRAME));
        let session = client.sessions().get_mut(&1).expect("the session");
        session.report(ms(20), start);
        session.set_threads(vec![8, 9]);
        assert_eq!(wanted(&client, start), [(8, LATE_START), (9, LATE_START)]);
    }
}
