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

//! The Power HAL of the platform: gives the threads of a client CPU speed while the client
//! needs it. See `speed` for the rules and `service` for who is served.

mod service;
mod speed;
mod threads;

use android_hardware_power::aidl::android::hardware::power::IPower::{BnPower, IPower};
use binder::BinderFeatures;

const LOG_TAG: &str = "everyware-power";

fn main() {
    let logging = logger::init(
        logger::Config::default()
            .with_tag_on_device(LOG_TAG)
            .with_max_level(log::LevelFilter::Info),
    );
    if !logging {
        panic!("{LOG_TAG}: no logger");
    }
    binder::ProcessState::set_thread_pool_max_thread_count(0);
    let name = format!("{}/default", service::Power::get_descriptor());
    let power = BnPower::new_binder(service::Power::new(), BinderFeatures::default());
    binder::add_service(&name, power.as_binder()).expect("the Power HAL is registered");
    log::info!("serving {name}");
    binder::ProcessState::join_thread_pool()
}
