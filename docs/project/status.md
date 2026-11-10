# Status (2026-11-10)

Everything below is built from the public AOSP tree at the tag `android-17.0.0_r1` with the
repositories of the project.

## Works

| | Where | Details |
|---|---|---|
| SDV Media without Java boots | RB3 | boot complete at 5.3 s kernel time, the SDV agents running ([1. The OS](../concepts/1-the-os.md)) |
| network, names and time after boot | RB3 | Wi-Fi with DHCP, dnsproxyd for `getaddrinfo()`, chrony sets the clock ([7. Extensions](../concepts/7-extensions.md)) |
| harry with touch | RB3 | the display through Linaro's prebuilt mesa; `super-harry` carries the commits (one tree per display, pictures kept between frames, two frames in flight, touch scaled to the display) |
| the house's services in the image | RB3 | the bundles `weather`, `news`, `wifi`, `matter`, `dashboard` as APEXes of the product; `adb remount` puts a rebuilt one on the board |
| the smart home panel | RB3 | `smarthome-app` as the vendor APEX `com.everyware.smarthome`, started by init |
| Wayland zones and the browser | RB3 | harry serves Wayland clients in zones of the design, also on a hardware plane; a tap on a news story opens its page in Cog, started by the image when the device has the Debian root ([a browser on the panel](browser.md)) |
| the apps on the UI library | RB3 | `tv-app` (first frame 2.2 s after start, 7 to 8 ms per frame), `hub-app`, `pairs-app`; the design document is `<name>.dcf`, opened by name |
| the design path without Figma | host | a `.fig` written by the kiwi tools, `fig-to-dcf.sh` to the `.dcf` the app ships, `dcf-diff` against Figma's own import: equal |

## Missing

| | Needed for | Next step |
|---|---|---|
| the browser's root as a part of the image | a browser out of the box | the root built from pinned packages, as an image file the build copies |
| the camera feed in its zone | the camera tile | a client that draws the stream into `#zone/camera` |
| SELinux enforcing | product quality | the denials of the permissive runs |
| automatic discovery of new devices | a real house | instead of the static peer list; options in [open points](open-points.md) |

## Next steps

1. The browser's root as a part of the image.
2. SELinux enforcing.
