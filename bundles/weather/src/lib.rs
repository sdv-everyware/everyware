//! WeatherServiceBundle: fetches the outdoor weather from Open-Meteo every ten minutes and
//! publishes it on the topic `weather` (catalog/everyware.vsidl) for the house. The screen gets
//! it through the dashboard bundle; nothing else in the house talks to the internet for it.
//!
//! The shape is the service bundle vsidlc generates (`vsidlc --services`, as Android Studio
//! for Platform does): a `ServiceBundle` with `SdvComms`, a current-thread runtime, `sdv_main`
//! with the bundle's publishers; the business logic is in `handle_weather_publisher`.

use log::{debug, error, info, warn};
use sdv::mw::{clientlib, Communicate, Publisher, SdvComms};
use sdv_mw_rs_com_everyware_weather_service_bundle::{PublisherDescriptors, WeatherServiceBundle as Bundle};
use everyware_bundle_store::Store;
use everyware_catalog::weather::{Hour, Weather};
use everyware_open_meteo as open_meteo;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

/// The house's location.
const LOCATION: open_meteo::Location = open_meteo::BERLIN;
/// Open-Meteo updates its current values every 15 minutes.
const POLL_INTERVAL: Duration = Duration::from_secs(600);
/// The bundle starts before DHCP has a lease and before chrony has set the clock (the TLS
/// certificate is then "not yet valid"), so the first attempts fail; a failed attempt costs
/// nothing but a request, so try again at once.
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// The last reading, published again at the next start until the first new one: the screen
/// has the weather at once instead of after the network (about 18 s after the boot).
const LAST: &str = "weather.pb";
/// Older than this the last reading is not shown: its forecast covers the next 24 hours, its
/// current values only the hour they were read.
const LAST_MAX_AGE: Duration = Duration::from_secs(3 * 3600);

sdv::lifecycle::register_service_bundle!(WeatherServiceBundle);

struct WeatherServiceBundle {
    cancellation_token: Option<CancellationToken>,
    context: ContextRef,
    /// The bundle's directory (/data/service_bundle_data/<package>/<bundle>).
    store: Arc<Store>,
}

impl ServiceBundle for WeatherServiceBundle {
    fn new(context: ContextRef) -> Self {
        if let Err(err) = sdv::tracing::try_init_tracing() {
            warn!("Failed to init service bundles tracing: {err}");
        }
        let fqin = context.get_self_fqin();
        let store = Arc::new(Store::new(fqin.get_sdv_package_name(), fqin.get_service_bundle_name()));
        Self { cancellation_token: None, context, store }
    }

    fn on_start(&mut self) {
        // The runner has set up the logger already, with log_tag from the bundle manifest.
        let context = self.context;
        let store = self.store.clone();
        let cancellation_token = CancellationToken::new();
        self.cancellation_token = Some(cancellation_token.clone());
        let Ok(runtime) =
            tokio::runtime::Builder::new_current_thread().enable_time().enable_io().build()
        else {
            panic!("Failed to start the runtime!");
        };
        let _ = std::thread::spawn(move || {
            runtime.block_on(async move {
                let comms = Arc::new(SdvComms { context });
                let res = tokio::select! {
                    res = sdv_main(comms, store) => res,
                    () = cancellation_token.cancelled() => Err(sdv::status::SdvStatus::new(sdv::status::SdvStatusCode::Cancelled)),
                };
                error!("Service bundle loop stopped: {res:?}.");
            });
            drop(runtime);
        });
    }

    fn on_stop(&mut self) {
        self.cancellation_token.as_mut().expect("Must have been created on_start").cancel();
        self.cancellation_token = None;
    }
}

async fn sdv_main(comms: Arc<dyn Communicate>, store: Arc<Store>) -> sdv::status::SdvResult<()> {
    info!("Starting service bundle 'WeatherServiceBundle'");
    let _service_bundle = match Bundle::builder().set_comms(comms.clone()).build().await {
        Ok(service) => service,
        Err(e) => {
            error!("Failed to build service bundle: {e}");
            return Err(sdv::status::SdvStatus::with_message(sdv::status::SdvStatusCode::Internal, e.to_string()));
        }
    };
    let mut futures_join_set = tokio::task::JoinSet::new();
    match clientlib::create_publisher(comms.as_ref(), PublisherDescriptors::<Weather>::WEATHER).await {
        Ok(publisher) => {
            futures_join_set.spawn(handle_weather_publisher(publisher, store));
        }
        Err(e) => error!("Failed to create publisher Weather#weather: {e:?}"),
    }
    while let Some(e) = futures_join_set.join_next().await {
        error!("Future joined with result: {e:?}");
    }
    Ok(())
}

/// The last reading first, then Open-Meteo every ten minutes; every reading is published and
/// kept.
async fn handle_weather_publisher(publisher: Publisher<Weather>, store: Arc<Store>) {
    if let Some(last) = store.load::<Weather>(LAST, LAST_MAX_AGE) {
        info!("the last reading, of {}: {} °C", last.time, last.temperature_c);
        if let Err(e) = publisher.publish(&last) {
            error!("Failed to publish on Weather#weather: {e:?}");
        }
    }
    info!("polling Open-Meteo every {} s", POLL_INTERVAL.as_secs());
    loop {
        let wait = match open_meteo::fetch(&LOCATION).await {
            Ok(weather) => {
                info!("weather: {weather}, {} hours", weather.hourly.len());
                let message = to_message(&weather);
                match publisher.publish(&message) {
                    Ok(()) => {
                        if let Err(err) = store.save(LAST, &message) {
                            warn!("the reading not kept: {err}");
                        }
                        POLL_INTERVAL
                    }
                    Err(e) => {
                        error!("Failed to publish on Weather#weather: {e:?}");
                        RETRY_INTERVAL
                    }
                }
            }
            Err(err) => {
                warn!("fetch failed: {err}");
                RETRY_INTERVAL
            }
        };
        debug!("next reading in {} s", wait.as_secs());
        sleep(wait).await;
    }
}

/// The reading as the catalog's message.
fn to_message(weather: &open_meteo::Weather) -> Weather {
    let mut message = Weather::new();
    message.place = LOCATION.name.to_string();
    message.time = weather.time.clone();
    message.temperature_c = weather.temperature_c;
    message.apparent_c = weather.apparent_c;
    message.humidity_percent = weather.humidity_percent;
    message.wind_kmh = weather.wind_kmh;
    message.code = weather.code;
    message.hourly = weather
        .hourly
        .iter()
        .map(|h| {
            let mut hour = Hour::new();
            hour.hour = h.hour as u32;
            hour.temperature_c = h.temperature_c;
            hour.precipitation_percent = h.precipitation_percent;
            hour.code = h.code;
            hour
        })
        .collect();
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_carries_the_reading() {
        let reading = open_meteo::Weather {
            time: "11:45".into(),
            temperature_c: 9.4,
            apparent_c: 7.1,
            humidity_percent: 81.0,
            wind_kmh: 14.3,
            code: 2,
            hourly: vec![open_meteo::Hour { hour: 11, temperature_c: 9.0, precipitation_percent: 20.0, code: 2 }],
        };
        let message = to_message(&reading);
        assert_eq!((message.place.as_str(), message.time.as_str(), message.code), ("Berlin", "11:45", 2));
        assert_eq!((message.hourly.len(), message.hourly[0].hour, message.hourly[0].precipitation_percent), (1, 11, 20.0));
    }
}
