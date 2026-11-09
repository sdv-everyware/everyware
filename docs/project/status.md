# Status (2026-11-09)

Everything below is built from the public AOSP tree at the tag `android-17.0.0_r1` with the
repositories of the project.

## Works

| | Where | Details |
|---|---|---|
| SDV Media without Java boots | RB3 | boot complete at 5.3 s kernel time, the SDV agents running ([1. The OS](../concepts/1-the-os.md)) |
| network, names and time after boot | RB3 | Wi-Fi with DHCP, dnsproxyd for `getaddrinfo()`, chrony sets the clock ([7. Extensions](../concepts/7-extensions.md)) |
| harry with touch | RB3 | the display through Linaro's prebuilt mesa; `super-harry` carries the commits (one tree per display, pictures kept between frames, two frames in flight, touch scaled to the display) |
| the apps on the UI library | RB3 | `tv-app` (first frame 2.2 s after start, 7 to 8 ms per frame), `hub-app`, `pairs-app`; the design document is `<name>.dcf`, opened by name |
| the service bundles | build | `weather`, `news`, `wifi`, `matter` (rs-matter on the crates of `third_party-rust`), `dashboard` |
| the design path without Figma | host | a `.fig` written by the kiwi tools, `fig-to-dcf.sh` to the `.dcf` the app ships, `dcf-diff` against Figma's own import: equal |

## Missing

| | Needed for | Next step |
|---|---|---|
| the bundles on the board | the live tiles | `everyware.mk` in the product, the policies of Wi-Fi and Matter, the image |
| the smart home panel | the device's app | `smarthome-app` on the public tree, first without the Wayland zones |
| the browser zone and the Wayland clients | the camera feed, a web page on the panel | the Wayland commits of harry, then the panel with zones |
| SELinux enforcing | product quality | the denials of the permissive runs |
| automatic discovery of new devices | a real house | instead of the static peer list; options in [open points](open-points.md) |

## Next steps

1. The bundles on the board; the panel without zones.
2. The Wayland commits of harry; the panel with zones and the browser.
3. SELinux enforcing.
