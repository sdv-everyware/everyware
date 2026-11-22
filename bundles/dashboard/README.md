# The dashboard bundle: the bridge to the screen

`DashboardServiceBundle` (APEX `com.everyware.dashboard`) stands between the house's topics and
the screen. It subscribes to what the dashboard shows (`weather`, `wifi-networks`,
`wifi-state`, `news`, `news-thumbnails`, `matter-devices`, `agenda`) and pushes every message to
the panel over binder: the panel serves `IScreenLink` (`interfaces/screen`, `src/screen.rs`) and
takes a call only from this bundle. What goes the other way, the screen's Wi-Fi, Matter and
calendar commands and its settings changes, comes into the sink the bundle hands the panel
(`ICommandSink`); the bundle calls the Wi-Fi bundle's RPC `WifiService`, the Matter bundle's
`MatterService` or the calendar bundle's `CalendarService` for each, and the answer goes back to
the screen (`publishWifiResult`, `publishMatterResult`, `publishCalendarResult`; a calendar
change also shows in the agenda the calendar bundle publishes before it answers). The other screens (hub, tv, pairs) serve the same
interface through the shared link (`interfaces/screen/link`); one screen serves it at a time.

```
SDV (lifecycle, discovery, authz, topics)            harry (DRM, GPU, touch: a plain process)
WeatherServiceBundle ─┐                                             ▲ IScreenLink: publishWeather,
WifiServiceBundle ────┼─ topics ─► DashboardServiceBundle ─binder──┤ publishWifi*, publishMatterDevices,
MatterServiceBundle ──┘                                            │ publishAgenda, publishSettings
                                                                   ▼ ICommandSink: onWifiCommand,
WifiServiceBundle ◄── RPC WifiService ───┐                           onMatterCommand, ...
MatterServiceBundle ◄── RPC MatterService┴─ (answer) ─binder─► publishWifiResult, publishMatterResult
                                                              smarthome_app
```

## Why a bridge at all

The harry process is no SDV participant: topics, discovery and authorization exist for
service bundles, which the lifecycle manager runs as libraries in its runner process. A
screen process with DRM master and the GPU does not live there, and making it an SDV
gateway client (as an Android app would) hands the UI the rights of a participant. So the
screen gets a bridge, as Google's reference does (`HarSdvServiceBundle` pushing into
`harry-app`):

| | the app in SDV itself | the bridge |
|---|---|---|
| topics | the app as a bundle (no DRM/GPU) or as a gateway client (~500 lines of SDV client in the UI) | the app has one binder service and no SDV code |
| rights | the UI process holds an authorization policy and talks to the SDV agents | only the bridge is in the policies; the UI sees what it is pushed and asks through one sink |
| a UI restart | subscriptions and state to rebuild in the UI | the bridge keeps the last value of every topic and sends it again when the screen's service is back |
| a new source (Matter, MQTT) | every screen subscribes on its own | one more subscription here, the screen unchanged |
| cost | | one more process, one binder hop |

The messages inside the calls are the catalog's (`catalog/*.proto`) as bytes, so the interface
freezes only the envelope. A Matter command is the same: a call of the Matter bundle's `MatterService`
(`catalog/matter_service.proto`) in a task of its own, answered when the device acknowledged
it (up to 160 s for a pairing), "the Matter service did not answer" when the call does not get
through. The Matter devices and commands run in a task of their own (`matter_link`): their
bundle may start late or not at all, and the other subscriptions wait for their publishers one
after the other. A command is a call of the Wi-Fi bundle's
`WifiService` (SDV RPC, `catalog/wifi_service.proto`) in a task of its own: a connect waits for
its outcome (up to 40 s, the client's deadline) while a scan goes on. A call that does not get
through (no server, deadline) is answered too, "the Wi-Fi service did not answer", so the
screen never waits for a connect that never started (decisions.md, 2026-11-06).

## How it is built

- `src/lib.rs`: the shape `vsidlc --services` generates (`ServiceBundle`, `SdvComms`, a
  current-thread runtime, `sdv_main`); one `handle_*_observer` per topic, `handle_screen`
  for the link to the screen (`src/screen.rs`), `handle_commands` for what comes into the sink, `call_wifi` per command, `create_wifi_client` (waits
  for the server's registration); `Last` holds the last value of every topic.
- The screen's service is looked for once a second while it is not there; a death recipient
  says at once when its process is gone (`src/screen.rs`). Every subscription
  waits for its publisher (`create_registration_event_stream`), so the bundles may start in
  any order.
- Every topic is state, so every subscription is `read_on_subscribe`: it gets the
  publication's last message at once. What a bundle published before the bridge subscribed
  (at boot: the weather's and the news' last values, kept from the previous start) reaches
  the screen with its first frame instead of with the next new value.
- `interfaces/screen`: the two AIDL interfaces, and `link/`, the screen's side as a crate.
- `apex/permissions.textproto`: subscriber of the seven topics, client of
  `com.everyware.WifiService` on the channel `wifi`, of `com.everyware.MatterService` on the
  channel `matter` and of `com.everyware.CalendarService` on the channel `calendar`.
- A calendar change or the unlink of the account (`onCalendarCommand`, a `CalendarCommand`) is a
  call of `CalendarService` in a task of its own (`call_calendar`, deadline 150 s), its answer to
  the screen with `publishCalendarResult` (one that does not get through is answered `AGAIN`); the client is made once the calendar bundle's
  server is registered (`find_calendar`). The calendar bundle checks that the caller is this
  bundle and that the agenda it published allows the change (`bundles/calendar/README.md`,
  "Changes from the screen").

## Settings

The bridge shows the device's settings on the screen as Google's HMI sample does
(`system/software_defined_vehicle/samples/user_preferences/v1/hmi_service`), in `src/settings.rs`:

```
 settings sheet --onSettingsChange (RequestSettingsChangeRequest)--> bridge
 bridge --RequestSettingsChange, NON_PERSISTENT--> user_preferences --> System bundle applies
 user_preferences --ChangeNotifier.OnSettingsChange--> bridge
 bridge --publishSettings (OnSettingsChangeRequest)--> settings sheet
```

- It finds the owner of the groups `time` and `units` by service discovery of the
  UserControllableServices, then subscribes to each group at UserPreferencesManagementService
  and serves the ChangeNotifier. Its authz policy has the three, as the HMI sample's.
- The screen gets Google's messages as they are; a new setting needs no new method.
- A change is NON_PERSISTENT: the groups are SHARED, so the System bundle keeps the value.
  A PERSISTENT change would be stored per user, and the device has no users.
- A notification carries only the keys that changed; the bridge merges it into the group it
  keeps, so a panel that comes back gets the whole group.
- user_preferences keeps its subscribers in memory only: when its server registers again (it
  restarted), the bridge subscribes to every group again. Google's sample is not restartable
  (its orchestration config), so today it comes back only with a reboot.
- It takes a change only for a group it follows, and a notification only from user_preferences.
  A change that fails sends the group's last values again, so the screen drops what it showed.

## Security

- The bridge is the only client of `WifiService`: a command reaches the Wi-Fi bundle only
  through it, and it only takes commands from the sink it handed the screen over binder.
  The screen takes a call only from this bundle: the kernel gives the caller's pid and uid,
  and the SDV identity agent says which service bundle that process is. A port on 127.0.0.1
  would be open to every local process.
- The app never looks the bundle up: it serves, the bridge calls. A compromised UI can send
  commands into the sink it was handed, nothing else; a compromised bridge is a bundle with
  exactly its policy.
- A password typed on the screen passes through here inside a `WifiConnect` and is not kept
  (`Last` holds no command, the log has the SSID only).

## Testing

- `adb logcat -s dashboard_bundle`: "subscribed to ...", "weather ...", "N Wi-Fi networks",
  "Wi-Fi WIFI_CONNECTED ...", "calling WifiService#wifi" once the Wi-Fi bundle is registered,
  "the panel serves com.everyware.screen.IScreenLink/default" and "taking the screen's commands" once the screen is found,
  `Wi-Fi connect "<ssid>": ok false "wrong password"` per answer, "the panel is gone, keeping the last values" when the app stops.
- Restart the app: the last values appear on the screen within a second (the bundle looks for the screen's service once a second).
