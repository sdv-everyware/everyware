# A browser on the panel

A web page runs in a process of its own and shows in the panel's right side; a tap on a news
story opens its page there. A fault in the browser never takes the dashboard. The device has no
browser of its own (no Java, no WebView), so the browser comes from a Debian root next to the
system. This is a development setup started by a script, not part of the image yet. It needs
the panel and harry's Wayland commits, the two rows "the smart home panel" and "the browser zone
and the Wayland clients" of [status](status.md).

## What runs

```
smarthome_app --wayland-zones --zone-plane   the panel: owns the display and the touchscreen,
   Wayland socket /data/local/tmp/har-display    serves Wayland clients (harry's Wayland bridge)
cog --platform=wl URL                        WPE WebKit in a Debian root, user browser,
                                                 renders on the GPU (Mesa freedreno in the root)
```

| What | Where |
|---|---|
| zone node | `#panel/browser` in the right panel, a zone only while the panel is open |
| zone tag | `#zone/browser`: every ordinary Wayland window goes there (`xdg_default_tag` in smarthome-app's `main.rs`) |
| page control | FIFO `/data/local/tmp/har-browser-open` -> `har-browser-relay` (user browser) -> `cogctl open` over a D-Bus bus that exists only inside the root |
| page state | Cog's output -> `har-browser-cog` -> FIFO `/data/local/tmp/har-browser-events` (`committed`, `loaded`, `failed`) -> the panel's loader |
| Debian root | `/data/glibc/rootfs`, entered with `scripts/glibc-enter.sh` (chroot, a `/dev` of its own, the socket directory as `/run/har`, DNS from the Wi-Fi lease) |
| browser user | `browser`, uid and gid 2900 (OEM range, no Android uid has it) |

## The Debian root, once per board

The root is a Debian 13 (trixie) arm64 root file system unpacked on the board, with Cog
installed in it from Debian's own packages:

1. Unpack an arm64 `debian:trixie` root file system into `/data/glibc/rootfs` (for example the
   export of the Docker image of that name).
2. `adb push scripts/glibc-enter.sh /data/glibc/`
3. In the root (`adb shell sh /data/glibc/glibc-enter.sh`): `apt-get update`, then
   `apt-get install -y cog dbus`, then a user `browser` with uid and gid 2900 and the home
   `/home/browser`.

## Run it

```bash
m smarthome_app
vendor/sdv-everyware/everyware/scripts/run-browser-rb3.sh start      # the panel with zones, Cog
vendor/sdv-everyware/everyware/scripts/run-browser-rb3.sh measure    # CPU and memory per process
vendor/sdv-everyware/everyware/scripts/run-browser-rb3.sh stop       # the installed panel again
```

`start` stops the installed panel and runs the built one from `/data/local/tmp` with zones on;
nothing in the image changes, and a reboot brings the installed panel back.

## Modes

| Mode | What it does | Cost of the panel's process |
|---|---|---|
| `--zone-plane` (the script's default) | the browser's buffer goes to a hardware plane of the display; the dashboard is not redrawn for a page's frame | 11 % of a core |
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
| `/data/local/tmp` belongs to the shell user | open | development only; a product needs a runtime directory owned by the panel |
| The relay's pid file and the logs are in the root's `/tmp`, which the browser's user can write, and root reads them | open | development only; a product keeps them in a directory of the panel |
| The D-Bus bus | narrowed | a socket in a 0700 directory of the user browser, no service activation (`browser-bus.conf`) |
| The pages the household reads | host only in the log | the app logs the host, the relay logs no URL |
| A hidden page keeps running | mostly closed | the app sends `about:blank` when the panel closes |
| A page reaches services on localhost or the LAN | open | needs a packet filter by the browser's uid |
| WebKit's sandbox is off, the root is entered as root | open | the kernel has no user namespaces; a product needs an SELinux domain and seccomp around the browser |

## Open

1. The product form: the root as an APEX, the browser as an init service with the socket from
   init, SELinux domains, `--wayland-zones` in the panel's own service.
2. Keyboard input into a page.
3. The page's real host in the title bar, back and reload.
4. Sound from a page.
5. Buffer modifiers negotiated instead of forced linear buffers.
