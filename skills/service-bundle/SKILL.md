---
name: service-bundle
description: Add an SDV service bundle to EveryWare (a publisher, a subscriber, an RPC server or client, a bridge to the screen, a bundle that owns a device such as Wi-Fi) the way Android Studio for Platform does it - a message in the catalog, the bundle in everyware.vsidl, vsidlc generates the descriptors, the bundle crate in the generated shape, an APEX per bundle with its authz policy, deploy to the RB3, verify by logs. Use when the user asks to add a bundle, a topic, a publisher or subscriber, to move logic out of the harry app into a bundle, or when a bundle does not load or does not get its topic.
---

# Add a service bundle

The logic of the house lives in service bundles; the harry process is only the screen
(`docs/concepts/3-service-bundles.md`, `docs/concepts/4-communication.md`). A bundle is made the
way Android Studio for Platform (ASfP) makes one: the catalog is the source, `vsidlc` generates
the rest.

```
catalog/weather.proto   the message (proto3 + vsidl annotations)       ─┐
catalog/everyware.vsidl the bundles: who publishes / subscribes what    ├─► vsidlc (Soong genrule)
                                                                        │     generated crate per bundle:
bundles/<name>/src/lib.rs   the bundle, in the shape vsidlc generates   ◄┘     PublisherDescriptors, SubscriberDescriptors
bundles/<name>/apex/        APEX: manifest, orchestration, authz policy
product/everyware.mk        PRODUCT_PACKAGES += com.everyware.<name>
scripts/deploy-bundle-rb3.sh <name>   push the APEX, reboot, show the bundle's log
```

Paths are relative to `vendor/sdv-everyware/everyware`; product
`sdv_media_db845c` in `out_rb3` (`source device/rb3/sdv_media_db845c/tools/env.sh`, in bash). Read
`traps.md` before step 3.

Official guides: [Create and execute SDV service bundles](https://source.android.com/docs/automotive/sdv/workstreams/core/service-bundle-development)
(vsidl, `vsidlc`, `PRODUCT_PACKAGES`, run on Cuttlefish) and
[Configure the Orchestrator](https://source.android.com/docs/automotive/sdv/core-areas/orchestrator).

## What ASfP does (and what we take from it)

ASfP's "New Service Bundle" dialog asks for the bundle name, the package, the topic or interface,
the Target API and the path of `vsidlc` (ours: `out_rb3/host/linux-x86/bin/vsidlc`). It has two
template sets, **pub/sub** (`data.proto`, `pubsub.vsidl`, `Android.bp`) and **RPC**
(`service.proto` with a `service X { rpc ... }`, `rpc.vsidl` with a `server` and a `client`
bundle, `Android.bp`), plus `CATALOG_UPDATE` with `vsidlc_gen_args: "--services --apex
--target-api N"`; it writes the catalog and runs

```bash
vsidlc -c <catalog> -o <out> --services --apex --target-api latest
```

and copies the output into the project: `generated_rs/` (descriptor crates), `services/<Bundle>/`
(a `rust_ffi_shared` with `src/main.rs` to fill in), `apex/<package>/` (one APEX for the whole
package: manifest, `linker.config.json`, keys, per-bundle `orchestrator.textproto`,
`permissions.textproto`, `vsidl-config.binpb`), `vm/<package>/` (VM orchestration groups). Run it
with `--no-genrule` to see the files instead of genrule stubs:

```bash
cd <tree> && ANDROID_BUILD_TOP=$PWD out_rb3/host/linux-x86/bin/vsidlc -c vendor/sdv-everyware/everyware/catalog \
  -o /tmp/vsidlc-out --services --apex --no-genrule --target-api latest --rust-formatter none --android-bp-formatter none
```

We keep the catalog and the generated descriptor crates exactly as vsidlc lays them out, write
the bundle crate in the generated shape, and keep one APEX per bundle (decision of 2026-11-03)
instead of one per package. The generated `main.rs` is the reference for the bundle's skeleton;
the generated `services/<Bundle>/Android.bp` is the reference for its dependencies. Two parts of
the generated APEX are not used yet and are open points (`docs/project/open-points.md`): the
message schemas file `vsidl-config.binpb` (for readers outside the catalog: AI agents, SOME/IP)
and the orchestration by power state (`vm/<package>/...`: groups started and stopped on
`ON`/`SUSPEND`, for devices that sleep). A bundle today is one instance, always started.

## 1. The catalog: `catalog/`

1. The message in `catalog/<topic>.proto`, `package com.everyware`, `import
   "sdv/vsidl/v1/annotations.proto"`. Every message a bundle publishes needs
   `option (.sdv.vsidl.v1.publication) = { message_count: N model: MULTI_PUB }` and a bound for
   every variable field: `string s = 1 [(.sdv.vsidl.v1.variable_type_max_size) = 32];`,
   `repeated Hour h = 2 [(.sdv.vsidl.v1.repeated_field_max_count) = 24];` (vsidlc E511/E513
   otherwise).
2. The bundle in `catalog/everyware.vsidl`:
   ```
   service_bundle {
       name: "WeatherServiceBundle"
       publisher { message: "Weather" topic: "weather" capacity: 4 }
   }
   service_bundle {
       name: "DashboardServiceBundle"
       subscriber { message: "Weather" topic: "weather" }
   }
   ```
   RPC between bundles (ASfP's `rpc` templates): the service in a proto,
   ```
   service WifiService {
       rpc Connect(WifiConnect) returns (WifiResult);
   }
   ```
   and in the vsidl the bundle that offers it and the ones that call it:
   ```
   service_bundle {
       name: "WifiServiceBundle"
       server { service: "WifiService" channel: "wifi" }
   }
   service_bundle {
       name: "DashboardServiceBundle"
       client { service: "WifiService" channel: "wifi" }
   }
   ```
   The keywords are those of `sdv/vsidl/v1/syntax.proto`, as ASfP's templates write them:
   `service_bundle`, `message:`, `service:` (`sdv_service_bundle`, `type:`, `interface:` are
   their deprecated names, still in older vsidl files such as HAR's). vsidlc generates an RPC
   crate per service (`extract_catalog_zip ... --rpc com.everyware WifiService` →
   `libcom_everyware_wifi_service_rpc_rs`, crate `com_everyware_wifi_service_rpc`: the
   `Interface` trait and the `Client`), the server's builder gets `set_server_<channel>`, the
   client's bundle crate `ClientDescriptors::<Client>::<CHANNEL>`. Rust, as in `bundles/wifi`
   and `bundles/dashboard`:
   ```rust
   // server: implement the trait, hand it to the builder
   #[async_trait::async_trait]
   impl com_everyware_wifi_service_rpc::Interface for WifiServiceRpc {
       async fn Connect(&self, _caller: sdv::comms::id::ServiceFqin, request: &WifiConnect) -> SdvResult<WifiResult> { ... }
   }
   Bundle::builder().set_comms(comms.clone()).set_server_wifi(Arc::new(rpc)).build().await?;

   // client: wait for the server, then the client with a deadline
   let descriptor = ClientDescriptors::<WifiClient>::WIFI;
   let mut events = clientlib::create_server_registration_event_stream(comms, descriptor).await?;
   while let Some(event) = events.next().await { if event == Availability::Available { break } }
   let options = sdv::mw::ClientOptions::builder().timeout(Duration::from_secs(40)).build()?;
   let wifi = clientlib::create_rpc_client(comms, descriptor, options).await?;
   ```
   The authz policy of each side names the service (`server { service:
   "com.everyware.WifiService" channel: "wifi" }`, `client { ... }`). A command with an answer
   (join this network: ok / wrong password) is an RPC; data that many read (the networks
   around, the state) is a topic. The Wi-Fi bundle has both: topics for what it sees, the
   `WifiService` for what it is asked to do (`docs/project/decisions.md`, 2026-11-06).
3. `catalog/generated/Android.bp`: a genrule + `rust_library` pair per bundle (copy an existing
   pair; the crate name is `sdv_mw_rs_com_everyware_<snake_bundle_name>`, the genrule runs
   `extract_catalog_zip ... --service-bundle com.everyware <BundleName>`). No `prefer_rlib`.
4. Build the chain: `m libeveryware_catalog libsdv_mw_rs_com_everyware_<name>_service_bundle`.
   The generated `service_bundle.rs` (in `out_rb3/soong/.intermediates/.../gen/`) has
   `PublisherDescriptors::<Msg>::TOPIC`, `SubscriberDescriptors::<Msg>::TOPIC` and the bundle's
   `builder()`.

## 2. The bundle: `bundles/<name>/`

`src/lib.rs` in the generated shape (see `bundles/weather/src/lib.rs`): plain `ServiceBundle`
(`sdv::lifecycle::register_service_bundle!`), `SdvComms { context }`, a current-thread tokio
runtime on its own thread, `sdv_main` that builds the bundle (`Bundle::builder().set_comms()`),
creates the publishers (`clientlib::create_publisher`) or waits for the publisher and subscribes
(`create_registration_event_stream`, `clientlib::create_observer`), and spawns one `handle_*`
task per unit into a `JoinSet`. The business logic is in the `handle_*` functions only.

`Android.bp`: `rust_ffi_shared`, `defaults: ["sdv_tracing@rust_defaults"]`, the rustlibs of the
generated bundle (`libtokio`, `libtokio_util`, `libanyhow`, `libfutures`, `libfutures_util`,
`libsdv_rs`, the descriptor crate, `liblog_rust`, `libprotobuf`, `libeveryware_catalog`,
`libsdv_status_rs`) plus the bundle's own, `proc_macros: ["libasync_trait"]`, `product_specific:
true`, `min_sdk_version: "35"`, `apex_available` platform and anyapex. No `prefer_rlib`, no
`shared_libs`, no `ld_flags`, no HAR `har_sdv_service_bundle_common`.

A bundle that talks to the harry process is a gRPC **client** on 127.0.0.1 (the harry process is
the server, `smarthome-app/src/house_link.rs`), with a heartbeat and the last values kept for a
reconnect: `bundles/dashboard`. Nothing in a bundle draws or knows the Figma names.

### The three shapes we have (read their READMEs)

| Bundle | Shape | README |
|---|---|---|
| `bundles/weather` | publisher: polls the internet, publishes a topic | `bundles/weather/README.md` |
| `bundles/news` | publisher of two topics: a feed and a picture per item; reads pages and pictures from the internet (untrusted data, decoded in the bundle), HTTPS through `libs/https` | `bundles/news/README.md` |
| `bundles/dashboard` | the bridge: subscribes to the topics the screen shows, pushes them over gRPC to the harry process (no SDV participant), takes the screen's commands from a gRPC server stream and calls `WifiService` for each, the answer back to the screen | `bundles/dashboard/README.md` (why a bridge at all) |
| `bundles/wifi` | owns a device: talks to wpa_supplicant's control interface, publishes state, serves the RPC `WifiService` | `bundles/wifi/README.md` (how the supplicant is reached, the security) |
| `bundles/calendar` | publisher for an account: links a Google account by OAuth (a setup page on the local network while there is none, a QR code on the screen), keeps the refresh token in its directory, reads an API with it | `bundles/calendar/README.md` (the link, the relay page, the security) |

### A bundle that owns a device (the Wi-Fi pattern)

- The bundle talks to the daemon or the kernel interface itself (here: hostap's control
  protocol in ~150 lines of Rust, `bundles/wifi/src/wpa_ctrl.rs`); no helper daemon, no
  framework HAL that cannot do the job (ISupplicant has no scan).
- **Its own SELinux domain.** The lifecycle manager starts every bundle as
  `untrusted_service_bundle` unless `/product/etc/service_bundle_contexts` maps
  `<package>.<Bundle>` to a domain (`product/service_bundle_contexts`, `prebuilt_etc`
  `service_bundle_contexts` in `product/everyware.mk`). The domain lives in
  `device/rb3/sdv_media_db845c/sepolicy/system_ext/{public,private}/<name>.te`
  (`service_bundle_domain(<name>)` + what it may do more); the vendor side of a rule (a vendor
  type such as `net_bringup`) goes to `sepolicy/vendor/<name>.te`, because system_ext policy
  cannot name vendor types.
- **Sockets to a vendor daemon** are a Treble violation by neverallow (`domain.te`): mark
  **both** domains `socket_between_core_and_vendor_violators` (the core one in system_ext
  policy, the vendor one in vendor policy) and say why in the README.
- **Prefer an abstract unix socket** over a file in a daemon's directory: no DAC (the bundle
  is in no group), SELinux alone decides. The daemon's start line (`device/rb3`'s
  `net_bringup.sh`) is vendor: the board runs the old one until the vendor image is reflashed,
  so for a test restart the daemon by hand with the new flags (README "State of the board").
- The topics carry no secret that does not have to travel (a password goes down the command
  and is stored by the daemon, never published back).
- `README.md` in the bundle: why this way, what was weighed, the security it rests on, how to
  test. A decision line in `docs/project/decisions.md`.

## 3. The APEX: `bundles/<name>/apex/`

Copy `bundles/weather/apex/` and rename: `Android.bp` (`apex` com.everyware.<name>, `apex_key`,
`prebuilt_etc` for the manifest, the orchestration and the permissions, `linker_config`),
`apex_manifest.json` (name; `requireNativeLibs` are the four `libsdv_comms_*_ffi.so`),
`file_contexts`, `linker.config.json` (`{"visible": true}`),
`sdv_service_bundles_manifest.textproto` (bundle name, `native_library_path`, `log_tag`,
`orchestration_config_path`, `authorization_policy_path`),
`<Bundle>Orchestration.textproto` (one instance, started), `permissions.textproto` with the same
topics as the vsidl:

```
publisher  { message: "com.everyware.Weather" topic: "weather" }     # or
subscriber { message: "com.everyware.Weather" topic: "weather" }
```

Keys: `openssl genrsa -out keys/com.everyware.<name>.pem 4096` and
`external/avb/avbtool.py extract_public_key --key ... --output keys/com.everyware.<name>.avbpubkey`.
Add `com.everyware.<name>` to `product/everyware.mk`.

## 4. Build, deploy, verify

```bash
m com.everyware.<name>                               # the APEX (and the bundle library)
./vendor/sdv-everyware/everyware/scripts/deploy-bundle-rb3.sh <name> [<name>...]
```

The script pushes the APEX to `/product/apex` (the board's copy is kept in
`/data/local/tmp/apex-backup`), reboots, waits for boot, lists the active APEXes and prints the
bundles' log. Then:

- `adb logcat -s <name>_bundle`: `Starting service bundle`, then the bundle's own lines
  (`subscribed to`, `weather: ...`). A load failure prints `Failed to load bundle libraries` with
  the dlopen reason under the tag `ApexSupport` (see `traps.md`).
- The chain end to end: `adb logcat -s weather_bundle dashboard_bundle smarthome-app` shows the
  publish, the bridge receiving it (ms later) and `house link: up` in the app.
- Crash isolation: `kill` the bundle's runner, the lifecycle agent restarts it and the screen
  keeps its values; stop and start the app, the bridge resends the last values at once.

## 5. Done when

- [ ] `bundles/<name>/README.md`: why a bundle, how it is built, security, testing

- [ ] catalog, vsidl, authz policy name the same message and topics
- [ ] `m com.everyware.<name>` builds; `llvm-readelf -d` of the bundle library shows the SDV
      deps as `.dylib.so` and no `cannot locate symbol` on the board
- [ ] the bundle's log shows it running from a cold boot; the data reaches the screen
- [ ] docs: `docs/project/decisions.md` row, `docs/project/status.md`,
      `docs/solution/devices-and-dashboard.md` (which bundle drives which tile)
- [ ] commits in `smarthome` (author `Daniel Georg <passenger6@gmail.com>`, no AI trailer)

A change lands through its own branch, a review by another agent and the user's word:
`vendor/sdv-everyware/everyware/AGENTS.md`, "How a change lands" (`scripts/feature.sh` there).
