# A browser on the panel (experimental)

Experimental, and off by default: the browser runs WPE WebKit from a Debian root, entered as
root, without WebKit's sandbox (see Security). An image has it only when it is built with
`EVERYWARE_EXPERIMENTAL_BROWSER=true`; without it a tap on a news story opens nothing. Why it
is built this way, and the options weighed: [#24](https://github.com/sdv-everyware/everyware/issues/24).
Its open security problems: [#54](https://github.com/sdv-everyware/everyware/issues/54), points 1 and 4.

A web page runs in a process of its own and shows in the panel's right side; a tap on a news
story opens its page there. A fault in the browser never takes the dashboard. The device has no
browser of its own (no Java, no WebView), so the browser comes from a Debian root next to the
system. An image built with the flag carries that root as the APEX `com.everyware.browser`
and starts the browser by itself.

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
| Debian root | `/mnt/vendor/everyware_browser`, read-only: `rootfs.erofs` of the APEX `com.everyware.browser`, mounted by the APEX's rc (`nosuid`, SELinux type `everyware_browser_root_file`). Entered with `scripts/glibc-enter.sh`: chroot, a `/dev` of its own, `/run` and `/tmp` as tmpfs, the socket directory as `/run/har`, DNS from the Wi-Fi lease, a machine id per boot |
| browser home | `/data/vendor/everyware_browser` (0700, user browser), bound at `/home/browser`: logs and WebKit's data |
| browser user | `browser`, uid and gid 2900 (OEM range, no Android uid has it) |

## Turning it on

```
export EVERYWARE_EXPERIMENTAL_BROWSER=true   # or set it in the product's makefile
m
```

The image then has the service `everyware_browser`, its scripts, the APEX
`com.everyware.browser` with the Debian root, and `ro.vendor.everyware.browser=true`. The build
stops when the root's image of the architecture is missing (next section). The panel reads that property once at start: with it, a tap
on a story opens the page, and an ordinary Wayland window (Cog) shows in the panel. Without it the
panel keeps its Wayland socket for harry's own clients (`har_zone`) and gives no zone to other
windows. The variable lives in the shell: a later `m` without it builds the image without the
browser.

## The Debian root

The root is Debian 13 (trixie) with Cog, D-Bus and `libgles2` from Debian's own packages, and
the user `browser`. It goes into the image as one erofs file, as `com.android.virt` carries
Microdroid's images. The file is made once per architecture on a board and is not in git:

```
scripts/browser-root.sh -s <serial> --export     # on an arm64 board (RB3, Pi 5) or Cuttlefish (amd64)
```

| Step | Where | What |
|---|---|---|
| base | host | the layer of Docker Hub's `debian:trixie` for the architecture, its sha256 checked |
| packages | board | apt from snapshot.debian.org at the date the layer was made from: the archive as it was then, its signature checked |
| read-only | board | `resolv.conf` and `machine-id` point to `/run`; no apt lists, no logs |
| image | host | the tree's `mkfs.erofs` -> `browser/apex/prebuilt/<arch>/rootfs.erofs` and its manifest |

`browser/apex/root.lock` is in git: per architecture the layer, the snapshot date and every
package with its version. The first export of an architecture writes its lines; a later export
makes the same root, or stops when the packages differ. `--update-lock` moves to the newest
`debian:trixie` layer and its snapshot (Debian's updates since) and writes the lock again.
At build time `check-root.sh` takes the image into the APEX only when its sha256 is the one of
its manifest and the manifest's versions are the ones of the lock. A build with the flag and
without the image of its architecture stops at once (`build/tasks/browser.mk`).

An image built with the browser carries Debian's binaries. Whoever publishes such an image owes
their sources (GPL and LGPL); `root.lock` names every version, and snapshot.debian.org keeps the
sources of each.

For trying things with apt, `scripts/browser-root.sh -s <serial>` makes the same root writable
in `/data/glibc/rootfs`, entered by hand with `glibc-enter.sh` or by `run-browser-rb3.sh`
(below). The service never runs it: it runs as root, and runs only the APEX's root.

### The first build with the browser on a new machine

The image of the root comes from a running device, so the first build takes two rounds. On
Cuttlefish (amd64):

```
# 1. an image without the browser, and the VM
bash -c "source device/cf-everyware/sdv_media_everyware_cf/tools/env.sh && m"
CF_STATIC_WIFI=1 device/cf-everyware/sdv_media_everyware_cf/tools/run.sh start

# 2. the root, made on the VM (it needs the network), copied to browser/apex/prebuilt/amd64/
vendor/sdv-everyware/everyware/scripts/browser-root.sh -s 0.0.0.0:6526 --export

# 3. the image with the browser, and the VM again with it
EVERYWARE_EXPERIMENTAL_BROWSER=true bash -c "source device/cf-everyware/sdv_media_everyware_cf/tools/env.sh && m"
device/cf-everyware/sdv_media_everyware_cf/tools/run.sh rm
CF_STATIC_WIFI=1 device/cf-everyware/sdv_media_everyware_cf/tools/run.sh start
```

`CF_STATIC_WIFI=1` is for a host whose firewall drops DHCP on Cuttlefish's bridge. When another
machine already has the image, steps 1 and 2 are not needed: copy `rootfs.erofs` and
`rootfs.erofs.manifest` of `browser/apex/prebuilt/<arch>/` from it. The build checks them
against `root.lock` as it checks its own. The RB3 and the Pi 5 work the same way, with their
own `tools/env.sh` and the board's serial.

## How it starts

```
init: apex.all.ready -> mkdir /data/vendor/everyware/run (0750 root:2900) -> start smarthome
   the panel serves har-display there (init.smarthome.rc of smarthome-app's APEX)
init: apex.all.ready -> mount the root read-only at /mnt/vendor/everyware_browser,
   mkdir /data/vendor/everyware_browser (com.everyware.browser.rc of the browser's APEX)
init: smarthome running -> start everyware_browser (product/everyware_browser.rc)
   everyware-browser.sh: no root at /mnt/vendor/everyware_browser -> ends
                         else: the socket and the two FIFOs for the group 2900, then in the
                         root as the user browser: the bus, Cog, the relay; when one of them
                         ends or the panel restarts, all three are started again
```

The files of the image: `/vendor/bin/everyware-browser.sh`, `/vendor/bin/glibc-enter.sh`, in
`/vendor/etc/everyware/browser` the two helper scripts and the bus configuration, which the root
sees as `/usr/local/lib/har-browser`, and `/vendor/apex/com.everyware.browser.apex`. Logs:
`logcat -s everyware_browser`, and Cog's in the browser's home,
`/data/vendor/everyware_browser/cog.log`.

## A developer's build of the panel

```bash
m smarthome_app
vendor/sdv-everyware/everyware/scripts/run-browser-rb3.sh start      # the built panel with zones, Cog
vendor/sdv-everyware/everyware/scripts/run-browser-rb3.sh measure    # CPU and memory per process
vendor/sdv-everyware/everyware/scripts/run-browser-rb3.sh stop       # the installed panel and browser again
```

It needs an image built with the flag (the panel reads `ro.vendor.everyware.browser`); without
it `start` stops with a message. Cog runs from the writable root in `/data/glibc/rootfs`
(`browser-root.sh` without `--export`), not from the APEX. `start` stops the installed panel and browser and runs the built panel from `/data/local/tmp`
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
| The root changed on the board | mostly closed | a read-only image in the APEX, signed (apexd checks the signature at activation). The image's blocks are checked at run time only where vendor is under AVB (Cuttlefish; not the RB3 and the Pi 5, whose vendor has no `avb` in its fstab). Writable: the home, `/tmp`, `/dev/shm` and `/run` (tmpfs) |
| Set-user-ID programs of the root (`su`, `mount`, `passwd`) | closed | the root is mounted `nosuid` |
| One machine id on every board of an image | closed | made per boot in the root's `/run` |
| Packages changed on the way | closed | apt checks the signature of the Debian archive; the versions are locked in `root.lock` |
| A page reaches services on localhost or the LAN | open | needs a packet filter by the browser's uid |
| WebKit's sandbox is off, the root is entered as root | open | the kernel has no user namespaces; a product needs an SELinux domain and seccomp around the browser |

## Open

1. The rules of the browser's SELinux domain, which is permissive for now, and of init's mount
   of the root.
2. Keyboard input into a page.
3. The page's real host in the title bar, back and reload.
4. Sound from a page.
5. Buffer modifiers negotiated instead of forced linear buffers.
