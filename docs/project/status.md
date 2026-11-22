# Status (2026-11-21)

Everything below is built from the public AOSP tree at the tag `android-17.0.0_r1` with the
repositories of the project.

## Works

| | Where | Details |
|---|---|---|
| SDV Media without Java boots | RB3, Cuttlefish | RB3 boots in 5.3 s kernel time; Cuttlefish (`sdv_media_everyware_cf`) boots the same SDV agents in a VM without hardware ([1. The OS](../concepts/1-the-os.md)) |
| network, names and time after boot | RB3, Cuttlefish | Wi-Fi with DHCP (on Cuttlefish `virt_wifi` with optional `CF_STATIC_WIFI=1`), dnsproxyd for `getaddrinfo()`, chrony sets the clock ([7. Extensions](../concepts/7-extensions.md)) |
| harry with touch | RB3, Cuttlefish | RB3 renders through Linaro's prebuilt mesa; Cuttlefish renders through ANGLE on gfxstream Vulkan with virtio multi-touch in the WebRTC page (`https://localhost:1443`) |
| the house's services in the image | RB3, Cuttlefish | the bundles `weather`, `news`, `calendar`, `wifi`, `matter`, `system`, `dashboard` as APEXes of the product; `scripts/deploy-bundle.sh` (`--target rb3\|cf`) updates a rebuilt APEX between flashes or VM rebuilds |
| the smart home panel | RB3, Cuttlefish | `smarthome-app` as the vendor APEX `com.everyware.smarthome`, started by init; `scripts/run-smarthome.sh` (`--target rb3\|cf`) runs a dev build from `/data/local/tmp` |
| Wayland zones and the browser | RB3, Cuttlefish | harry serves Wayland clients in zones of the design (on a hardware plane on RB3, composited in the dashboard on Cuttlefish); the experimental browser is off by default (`EVERYWARE_BROWSER=true` enables it; verified on RB3, wired in `device-cf`) ([a browser on the panel](browser.md)) |
| the camera | RB3, Cuttlefish | `camera-app` (`utils/camera-app`), a harry zone client started by init after the panel (`everyware_camera` domain): on RB3 a USB webcam (UVC, MJPEG or YUYV) in the camera tile and live full view; "Camera offline" without a camera |
| the apps on the UI library | RB3 | `tv-app` (first frame 2.2 s after start, 7 to 8 ms per frame), `hub-app`, `pairs-app`; the design document is `<name>.dcf`, opened by name |
| the design path without Figma | host | a `.fig` written by the kiwi tools, `fig-to-dcf.sh` to the `.dcf` the app ships, `dcf-diff` against Figma's own import: equal |
| framebuffer screenshots | RB3, Cuttlefish | `tools/fbgrab` (`fbgrab`) dumps the active linear KMS framebuffer to raw RGBA on boards without SurfaceFlinger; on Cuttlefish `device/cf-everyware/sdv_media_everyware_cf/tools/run.sh screenshot` captures from the host display |

## Missing

| | Needed for | Next step |
|---|---|---|
| the browser's root as a part of the image | the experimental browser without setting up a root by hand | the root built from pinned packages, as an image file the build copies |
| the agenda from Google Calendar on the board | the agenda in the right column | the relay page hosted, the owner's OAuth client on the board, a run on the RB3 (`bundles/calendar`, built and host-tested) |
| SELinux enforcing | product quality | the denials of the permissive runs |
| automatic discovery of new devices | a real house | instead of the static peer list; options in [open points](open-points.md) |

## Next steps

1. The experimental browser's root as a part of the image.
2. SELinux enforcing.
