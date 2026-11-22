# The weather bundle

`WeatherServiceBundle` (APEX `com.everyware.weather`) is the one place of the house that talks
to the internet for the weather: every ten minutes it asks Open-Meteo for the current
conditions and the next 24 hours at the house's location and publishes them on the topic
`weather` (`catalog/weather.proto`, `catalog/everyware.vsidl`). The screen gets the message
through the dashboard bundle; nothing else needs a network for it.

```
Open-Meteo ──HTTPS──► WeatherServiceBundle ──topic weather──► DashboardServiceBundle ──binder──► screen
                      (open-meteo/)    │                      (bridge)                       (harry)
                                       ▼ weather.pb, the last reading
                      /data/service_bundle_data/com.everyware.weather/WeatherServiceBundle
```

## Why a bundle

The weather is data of the house, not of a screen: a second device, a logger or an automation
("close the blinds before the rain") subscribes to the same topic and the internet is asked
once. A bundle is also the only SDV participant: a plain process has no `Communicate`, no
topics, no authorization policy. And a bundle has its own process: a stuck request, a
certificate problem or a crash stays here and the screen keeps its last reading.

## How it is built

- `src/lib.rs`: the shape `vsidlc --services` generates (`ServiceBundle` with `SdvComms`, a
  current-thread tokio runtime, `sdv_main` with the bundle's publisher); the logic is
  `handle_weather_publisher`: the last reading first, then fetch, convert, publish, keep,
  sleep.
- `libs/bundle-store` (`libeveryware_bundle_store`): the last reading in the bundle's
  directory, for the next start.
- `open-meteo/` (`libeveryware_open_meteo`): the client, HTTPS with hyper over
  tokio-openssl over BoringSSL, the TLS stack SDV itself uses (one TLS stack per process:
  `bssl_sys` cannot be linked twice, so no rustls). The CA store is the system's
  (`/system/etc/security/cacerts`).
- `catalog/weather.proto`: the message with the size annotations vsidlc needs (`max_size`,
  `max_count`), `publication { message_count: 4, model: MULTI_PUB }`.
- `apex/`: manifest, orchestration (started at boot, instance-1), the authorization policy
  (`publisher weather`), test keys (public repository: not for a product).

## Timing

The bundle starts at about 6 s after the boot, the network is there at about 18 s (Wi-Fi,
then DHCP). So it publishes, at its start, the last reading it kept (`weather.pb`, the time of
the reading in it), when that is not older than three hours: the screen has the weather with
its first frame. The first requests then fail while there is no lease ("No address associated
with hostname"); a failed request costs nothing but the request, so it is repeated every
second until the first new reading (about 18 s), then every ten minutes (Open-Meteo updates
its current values every 15). TLS works from the first answer on: the clock is set from the
last run at boot (`chronyd -s`, `device/rb3/sdv_media_db845c/init.sdv_db845c.rc`), not only by
NTP.

The file is in SDV's directory of the bundle (the lifecycle manager creates it with the
bundle's uid; SELinux type `service_bundle_data`) and is replaced at once (written to a
temporary one, renamed).

## Security

- The bundle reaches the internet (the SDV bundle domain may: `untrusted_service_bundle` has
  tcp sockets), nothing else on the panel does for the weather.
- TLS with the system's CA store; the response is parsed into the catalog's message, the raw
  JSON never leaves the bundle.
- Only this bundle publishes `weather` (its policy); a subscriber cannot inject readings.

## Testing

- `everyware_open_meteo_test` (host): the JSON parsing and the conversion.
- On the board: `adb logcat -s weather_bundle` shows the kept reading at the start ("the last
  reading, of 19:15: 6.5 °C") and every new one ("weather: 5 °C clear, 24 hours"); the bridge:
  `-s dashboard_bundle` ("weather 5 °C at ...").
- Without a network the bundle logs "fetch failed" once a second until the link is there.
