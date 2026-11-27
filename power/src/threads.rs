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

//! The minimum of speed the kernel gives a thread: its
//! utilization clamp (`sched_setattr`). A thread with a minimum is treated by the governor
//! and by the placement of tasks as if it used at least that much of the fastest CPU,
//! whenever it runs. While it sleeps the clamp asks for nothing.

use std::fs;
use std::io;

/// `struct sched_attr` of the kernel (include/uapi/linux/sched/types.h), up to the clamps.
#[repr(C)]
#[derive(Default)]
struct SchedAttr {
    size: u32,
    sched_policy: u32,
    sched_flags: u64,
    sched_nice: i32,
    sched_priority: u32,
    sched_runtime: u64,
    sched_deadline: u64,
    sched_period: u64,
    sched_util_min: u32,
    sched_util_max: u32,
}

// include/uapi/linux/sched.h
const SCHED_FLAG_KEEP_POLICY: u64 = 0x08;
const SCHED_FLAG_KEEP_PARAMS: u64 = 0x10;
const SCHED_FLAG_UTIL_CLAMP_MIN: u64 = 0x20;

/// Returns true if `thread` is a thread of the process `pid`.
pub fn is_thread_of(thread: i32, pid: i32) -> bool {
    fs::metadata(format!("/proc/{pid}/task/{thread}")).is_ok()
}

/// Sets the minimum of speed of `thread`, 0 (none) to 1024 (all of the fastest CPU). The
/// policy and the priority of the thread stay as they are.
pub fn set_min_speed(thread: i32, speed: u32) -> io::Result<()> {
    let attr = SchedAttr {
        size: std::mem::size_of::<SchedAttr>() as u32,
        sched_flags: SCHED_FLAG_KEEP_POLICY | SCHED_FLAG_KEEP_PARAMS | SCHED_FLAG_UTIL_CLAMP_MIN,
        sched_util_min: speed,
        ..SchedAttr::default()
    };
    // SAFETY: `attr` is a `struct sched_attr` of the size it names and lives through the
    // call; the kernel reads it and writes nothing back.
    let result =
        unsafe { libc::syscall(libc::SYS_sched_setattr, thread, &attr as *const SchedAttr, 0u32) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
