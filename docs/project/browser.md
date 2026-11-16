# A browser on the panel (experimental)

Experimental, and off by default: the browser runs WPE WebKit from a Debian root, entered as
root, without WebKit's sandbox (see Security). An image has it only when it is built with
`EVERYWARE_BROWSER=true`; without it the panel is built without its browser code. Why it
is built this way, and the options weighed: [#24](https://github.com/sdv-everyware/everyware/issues/24).
Its open security problems: [#54](https://github.com/sdv-everyware/everyware/issues/54), points 1 and 4.

A web page runs in a process of its own and shows in the panel's right side. Nothing on the
dashboard opens a page in it: the right column is the agenda. A fault in the browser never takes
the dashboard. The device has no
browser of its own (no Java, no WebView), so the browser comes from a Debian root next to the
system. An image built with the flag starts it by itself when the device has that root; the root is put on the
device once and is not part of the image.

## What runs

```
smarthome_app --wayland-zones --zone-plane   the panel: owns the display and the touchscreen,
   Wayland socket har-display in its runtime     serves Wayland clients (harry's Wayland bridge)
   directory /data/vendor/everyware/run
everyware-browser.sh                         init's service everyware_browser: enters the root,
                                                 keeps the three processes below running
cog --platform=wl URL                        WPE WebKit in a Debian root, user browser,
                                                 renders on the GPU (Mesa freedreno in the root)
```

| What | Where |
|---|---|
| zone node | `#panel/browser` in the right panel, a zone only while the panel is open |
| zone tag | `#zone/browser`: with the flag, every ordinary Wayland window goes there (`xdg_default_tag` in smarthome-app's `main.rs`) |
| page control | FIFO `har-browser-open` in the runtime directory -> `har-browser-relay` (user browser) -> `cogctl open` over a D-Bus bus that exists only inside the root |
| page state | Cog's output -> `har-browser-cog` -> FIFO `har-browser-events` in the runtime directory (`committed`, `loaded`, `failed`) -> the panel's loader |
| Debian root | `/data/glibc/rootfs`, entered with `scripts/glibc-enter.sh` (chroot, a `/dev` of its own, the socket directory as `/run/har`, DNS from the Wi-Fi lease) |
| browser user | `browser`, uid and gid 2900 (OEM range, no Android uid has it) |

## Turning it on

```
export EVERYWARE_BROWSER=true   # or set it in the product's makefile
m
```

The image then has the service `everyware_browser`, its scripts, and
`ro.vendor.everyware.browser=true`, and the panel is built with its browser code (Soong's
`everyware`/`browser`, the cfg `browser` of smarthome_app). The panel reads the property once at
start: with it, an ordinary Wayland window (Cog) shows in the panel. Without the variable the
panel has no browser code and keeps its Wayland socket for harry's own clients (`har_zone`); it
gives no zone to other windows. The variable lives in the shell: a later `m` without it builds the image without the
browser.

## The Debian root, once per board

The root is a Debian 13 (trixie) arm64 root file system unpacked on the board, with Cog
installed in it from Debian's own packages:

1. Unpack an arm64 `debian:trixie` root file system into `/data/glibc/rootfs` (for example the
   export of the Docker image of that name).
2. `adb push scripts/glibc-enter.sh /data/glibc/`
3. In the root (`adb shell sh /data/glibc/glibc-enter.sh`): `apt-get update`, then
   `apt-get install -y cog dbus`, then a user `browser` with uid and gid 2900 and the home
   `/home/browser`.
4. Reboot, or `adb shell start everyware_browser` (an image built with the flag): the service
   looks for the root when the panel starts.

## How it starts

```
init: apex.all.ready -> mkdir /data/vendor/everyware/run (0750 root:2900) -> start smarthome
   the panel serves har-display there (init.smarthome.rc of smarthome-app's APEX)
init: smarthome running -> start everyware_browser (product/everyware_browser.rc)
   everyware-browser.sh: no root on the device -> ends
                         else: the socket and the two FIFOs for the group 2900, then in the
                         root as the user browser: the bus, Cog, the relay; when one of them
                         ends or the panel restarts, all three are started again
```

The files of the image: `/vendor/bin/everyware-browser.sh`, `/vendor/bin/glibc-enter.sh`, and
in `/vendor/etc/everyware/browser` the two helper scripts and the bus configuration, which
the root sees as `/usr/local/lib/har-browser`. Logs: `logcat -s everyware_browser`, and Cog's
in the root, `/home/browser/cog.log`.

## A developer's build of the panel

```bash
m smarthome_app
vendor/sdv-everyware/everyware/scripts/run-browser-rb3.sh start      # the built panel with zones, Cog
vendor/sdv-everyware/everyware/scripts/run-browser-rb3.sh measure    # CPU and memory per process
vendor/sdv-everyware/everyware/scripts/run-browser-rb3.sh stop       # the installed panel and browser again
```

It needs an image built with the flag (the panel reads `ro.vendor.everyware.browser`); without
it `start` stops with a message. `start` stops the installed panel and browser and runs the built panel from `/data/local/tmp`
with zones on (socket and FIFOs there); nothing in the image changes, and a reboot brings the
installed ones back.

## Modes

| Mode | What it does | Cost of the panel's process |
|---|---|---|
| `--zone-plane` (the installed panel, and the default of `run-browser-rb3.sh`) | the browser's buffer goes to a hardware plane of the display; the dashboard is not redrawn for a page's frame | 11 % of a core |
| `--no-zone-plane` | the browser's buffer is a texture the dashboard draws; every frame of the page redraws the dashboard | most of a core while the page changes |

The plane's figure is `run-browser-rb3.sh measure` on the RB3 at 1920x1080 with a static page open.

## How a frame and a touch travel

```
Cog/WebKit -> Mesa EGL -> dmabuf (linear) -> linux-dmabuf
  -> the panel: the Wayland bridge imports it as an EGLImage (no copy)
  -> a hardware plane, or a texture in the dashboard's frame -> KMS -> frame.done to Cog
touchscreen -> the panel's evdev -> a touch that starts inside the zone, on no node drawn
  over it -> wl_touch -> Cog
```

The panel speaks the protocols a toolkit expects: `wl_compositor`, `wl_shm`,
`zwp_linux_dmabuf_v1`, explicit sync, `wl_seat` (touch), `xdg_wm_base`, `wl_output`, `wl_drm`,
and `har_zone` for harry clients.

## Security

| Threat | State | What holds it |
|---|---|---|
| A crafted URL in the news feed (`javascript:`, `file:`, `data:`, a second line) | closed | the app sends only `http` and `https` and its own `about:blank`, one line of at most 2048 bytes (`browser.rs`); the relay checks the same again |
| Writing to the FIFOs from outside | closed for apps | 0660 root:2900; opened with `O_NOFOLLOW`, only if a FIFO |
| The socket and the FIFOs in a directory others can write | closed for the installed browser | `/data/vendor/everyware/run`, 0750 root:2900; a developer's run still uses `/data/local/tmp` |
| Root opening a file in a directory the browser's user can write | closed for the installed browser | the logs are opened by the browser's own shell in its home; a developer's run still keeps the relay's pid file and two logs in the root's `/tmp` |
| The D-Bus bus | narrowed | a socket in a 0700 directory of the user browser, no service activation (`browser-bus.conf`) |
| The pages the household reads | host only in the log | the app logs the host, the relay logs no URL |
| A hidden page keeps running | mostly closed | the app sends `about:blank` when the panel closes |
| A page reaches services on localhost or the LAN | open | needs a packet filter by the browser's uid |
| WebKit's sandbox is off, the root is entered as root | open | the kernel has no user namespaces; a product needs an SELinux domain and seccomp around the browser |

## Open

1. The root as a part of the image (built from pinned packages), and the rules of the
   browser's SELinux domain, which is permissive for now.
2. Keyboard input into a page.
3. The page's real host in the title bar, back and reload.
4. Sound from a page.
5. Buffer modifiers negotiated instead of forced linear buffers.
