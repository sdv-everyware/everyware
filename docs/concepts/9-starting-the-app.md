# 9. Starting the app: from power on to the first frame

The panel's screen is one process, the harry app of the device (`smarthome_app`). It is not
started by a script or by a person: init starts it when the device boots, restarts it when it
ends, and nothing else draws on the display. This page shows how, with the panel as the
example, the way HAR starts its own reference app (`harry`).

## The pieces

```mermaid
flowchart LR
    subgraph APEX["com.everyware.smarthome (vendor APEX, /vendor/apex)"]
        BIN["bin/smarthome_app<br/>vendor_harplatform_exec"]
        LIB["lib64/<br/>libimpeller, libgbm, libdrm,<br/>libgrpc++, libtinyalsav2 …"]
        DCF["etc/smarthome/smarthome.dcf<br/>the design"]
        RC["etc/com.everyware.smarthome.init.rc"]
    end
    INIT["init"] -- "parses etc/*.rc of every mounted APEX" --> RC
    RC -- "service smarthome" --> BIN
    BIN -- "links" --> LIB
    BIN -- "reads" --> DCF
    OVR["/data/vendor/everyware/smarthome/<br/>smarthome.dcf (optional)"] -. "wins when present" .-> BIN
```

| Piece | Where (tree) | What it is for |
|---|---|---|
| the APEX | `packages/services/display_safety/utils/smarthome-app/apex/Android.bp` | one file with the binary, the libraries it links, the design and the init script; installed and updated as a whole |
| the init script | `apex/init.smarthome.rc` | the service `smarthome` and the trigger that starts it |
| the SELinux labels | `apex/file_contexts` | the binary in HAR's UI process domain `vendor_harplatform` |
| the product | `device/rb3/sdv_media_db845c/sdv_media_db845c.mk` | `PRODUCT_PACKAGES += com.everyware.smarthome`; HAR's own app is not in the image |
| the domain | `packages/services/display_safety/service/product/sepolicy/harplatform.te` (the board's `BoardConfig.mk` adds it to `BOARD_VENDOR_SEPOLICY_DIRS`) | what the UI process may do: DRM, GPU, input, audio, its sockets |

## The init script

```
service smarthome /apex/com.everyware.smarthome/bin/smarthome_app \
        --data-path /apex/com.everyware.smarthome/etc/smarthome \
        --override-path /data/vendor/everyware/smarthome \
        --document-id smarthome.dcf \
        --touch-size 1920x1080
    class main
    user root
    group graphics input audio
    disabled
    updatable

on property:apex.all.ready=true
    mkdir /data/vendor/everyware 0770 root graphics
    mkdir /data/vendor/everyware/smarthome 0770 root graphics
    start smarthome
```

| Line | Why |
|---|---|
| `/apex/com.everyware.smarthome/bin/smarthome_app` | the binary in the APEX; the libraries next to it in `lib64/` (the APEX's linker namespace), so the app needs no `LD_LIBRARY_PATH` |
| `--data-path …/etc/smarthome` | the design that came with the APEX |
| `--override-path /data/vendor/everyware/smarthome` | a design pushed there wins (a test of a new fetch without a new APEX) |
| `--touch-size 1920x1080` | the RB3's HDMI touch reports 0..32767; the app maps it to the display's pixels |
| `class main` | the class of the device's daemons |
| `user root` | as HAR's harry: the UI process is the display's DRM master and opens the touch device |
| `group graphics input audio` | the GPU and the display, the touch device, the sound of a tap |
| `disabled` | not started with its class: the trigger below starts it once its files exist |
| no `oneshot` | init restarts it when it ends (`restart_period`, 5 s by default); only a `critical` service makes init reboot the device after four crashes in four minutes, the panel is not one |
| `updatable` | a service an APEX may define and a newer APEX may replace (init refuses to let an APEX's script redefine a service without it) |
| `on property:apex.all.ready=true` | apexd has mounted every APEX: the binary, its libraries and its design are there |
| `mkdir /data/vendor/everyware/smarthome` | the place for a design pushed for a test |
| `start smarthome` | the service starts; the first frame follows once the design is loaded |

The trigger needs no property of its own: a product that has the APEX has the panel. HAR's APEX
starts harry on the same trigger (`service/product/harry_apex/init.display_safety.har.rc`).

## From power on to the first frame

```mermaid
sequenceDiagram
    participant K as kernel
    participant I as init
    participant A as apexd
    participant S as smarthome_app
    participant D as DashboardServiceBundle
    K->>I: first stage (the display driver is in it: /dev/dri/card0)
    I->>A: start apexd
    A->>A: mount /vendor/apex/com.everyware.smarthome.apex at /apex/com.everyware.smarthome
    A->>I: apex.all.ready=true
    I->>I: parse /apex/*/etc/*.rc (the service smarthome)
    I->>S: start smarthome (root, vendor_harplatform)
    S->>S: load the design, open card0 (DRM master), the GPU, the touch device
    S->>S: first frame (the design as fetched)
    D-->>S: gRPC 127.0.0.1:7100: weather, news, Wi-Fi (the last values first, then new ones)
```

The screen shows the design and the house's data together: the weather and the news bundles
publish their last values, kept from the previous start, as soon as the lifecycle manager
starts them (about 6 s), and the bridge gets them when it subscribes (`read_on_subscribe`).
The new values follow once the network is up (about 18 to 20 s on the RB3:
[bundles/weather](../../bundles/weather/README.md), [bundles/news](../../bundles/news/README.md)).

## The libraries

An APEX gets its own linker namespace: the process sees the libraries in the APEX's `lib64/`
and, of the platform, only those the APEX declares (`requireNativeLibs` in the manifest the
build generates). The build declares what the binary links. harry's context opens EGL and GLES
at run time (`dlopen("libEGL.so")`), so the app links `libEGL` and `libGLESv2` (`shared_libs`
in its `Android.bp`, as HAR's `harry_app` does); without that the panel aborts at its first
frame with `Unable to find libEGL.so`, and init restarts it every five seconds.

```
smarthome_app ──links──► lib64/ of the APEX (libimpeller, libgbm, libdrm, libgrpc++ …)
              ──links──► libEGL.so, libGLESv2.so (platform, LLNDK, declared by the APEX)
                              └─loads──► /vendor/lib64/egl/libEGL_mesa.so (the GPU driver, :sphal)
```

## The SELinux domain

The binary is labelled `vendor_harplatform_exec` (`apex/file_contexts`), so init runs it in
`vendor_harplatform`, the domain HAR defines for its own UI process. A harry app is the same
kind of process: renderer, display, touch. The domain is a vendor domain, so the APEX is a
vendor APEX (`vendor: true`); its libraries are `vendor_harplatform_lib_file`.

| May | From |
|---|---|
| the GPU (`gpu_device`), the display (`graphics_device`) | `harplatform.te` |
| input devices, audio devices, video devices | `harplatform.te` |
| its own sockets, vendor data files | `harplatform.te` |
| be started by init (`init_daemon_domain`) | `harplatform.te` |

A product whose panel needs less (no audio, no camera) gets a domain of its own: the same file
with fewer rules, and the label in `file_contexts`.

## Updating and developing

A product's update goes through the SDV Update Manager, with a rollback when the new version
does not start: [10. Updating the system](10-updating-the-system.md). The table is the
development's way.

| What | How | Reboot |
|---|---|---|
| a new version, installed for development | `scripts/deploy-bundle-rb3.sh smarthome` (the APEX over the image's in `/vendor/apex`, remount) | yes: apexd mounts APEXes at boot |
| a new design only | `adb push smarthome.dcf /data/vendor/everyware/smarthome/` and `adb shell restart smarthome` | no |
| back to the APEX's design | `adb shell rm /data/vendor/everyware/smarthome/smarthome.dcf` and `restart smarthome` | no |
| a development build | `scripts/run-smarthome-rb3.sh` (stops the service, runs the build from `/data/local/tmp`) | no |
| back to the installed panel | `scripts/run-smarthome-rb3.sh stop` (`start smarthome`) | no |

```mermaid
flowchart LR
    subgraph dev["development"]
        RUN["run-smarthome-rb3.sh"] -- "stop smarthome" --> X["/data/local/tmp/smarthome_app"]
    end
    subgraph installed["installed"]
        DEP["deploy-bundle-rb3.sh smarthome"] -- "/vendor/apex + reboot" --> SVC["service smarthome"]
    end
    X -- "run-smarthome-rb3.sh stop" --> SVC
```

## Checking it

```bash
# WHAT: is the panel running, and how often did init restart it
adb shell getprop init.svc.smarthome           # running / restarting / stopped
adb shell getprop ro.boottime.smarthome         # when init started it (ns since boot)
adb shell ps -A -o PID,ETIME,LABEL,ARGS | grep smarthome
# WHAT: what it says, and what SELinux refused it
adb logcat -s smarthome-app init
adb shell dmesg | grep 'avc:.*vendor_harplatform'
# WHAT: which design it loaded
adb logcat -d -s smarthome-app | grep 'the design from'
```

## How long it takes

From the kernel's start to the panel's first frame: about 7.6 to 7.9 s on the RB3 (boot
completed at about 5.4 to 5.7 s, the panel starts at about 5.4 s and needs about 2.1 s to its
first frame). The method, the script and the runs:
[tools/boot-time](../../tools/boot-time/README.md).

## Key terms

- **APEX**: a signed file system image (`.apex`) that apexd mounts at `/apex/<name>`; it brings
  its binaries, libraries, configuration and init scripts.
- **init script (`.rc`)**: services and triggers; init reads the ones of the partitions at boot
  and the `etc/*.rc` of every APEX once it is mounted.
- **`apex.all.ready`**: the property apexd sets when every APEX is mounted.
- **DRM master**: the one process that may set the display's mode and show its frames.

Related: [2. Components](2-components.md), [5. UI](5-ui.md), [6. Security](6-security.md).
