# SDV EveryWare

SDV Media, the Rust part of Android's software defined vehicle stack, as the operating system
of embedded devices that are not cars: no Java, no Android framework, a 5 second boot, a UI
drawn from a Figma design by harry (the high availability renderer of AOSP's display safety).
The first device is a smart home panel on a Qualcomm RB3 (1920 x 1080 over HDMI, touch). The
concepts, the solution and the project's decisions are in [`docs/README.md`](docs/README.md).

Everything is built in the public AOSP tree at the release tag `android-17.0.0_r1`, plus the
repositories of this GitHub organisation, checked out by the local manifest in
[`manifest`](https://github.com/sdv-everyware/manifest) (branch `everyware-17.0.0_r1`; a new
AOSP release gets a new branch there and in the forks):

| Repository | Path in the tree | What |
|---|---|---|
| `everyware` (this one) | `vendor/sdv-everyware/everyware` | the service bundles of the house, their catalog and policies, the product additions, the design tools, the skills and the process |
| `ui-lib` | `vendor/sdv-everyware/ui-lib` | the UI library: components by role, their behaviour, the Library as a Figma file and as a `.dcf` |
| `smarthome-app`, `tv-app`, `hub-app`, `pairs-app` | `packages/services/display_safety/utils/<name>` | the apps on harry; `tv-app` is the template of a new app |
| `super-harry` | `packages/services/display_safety` | harry as AOSP has it, with the commits the panel needs (branch `everyware-17.0.0_r1`) |
| `dragonboard` | `device/linaro/dragonboard` | Linaro's board support with one policy change (branch `everyware-17.0.0_r1`) |
| `device-rb3` | `device/rb3/sdv_media_db845c` | the product of the RB3: SDV Media's core, the display, the network, the bundles |
| `dnsproxyd` | `vendor/sdv-everyware/dnsproxyd` | name resolution for a device without netd |
| `third_party-rust`, `third_party-chrony` | `vendor/sdv-everyware/third_party/{rust,chrony}` | the Rust crates the tree lacks (rs-matter, tokio-openssl), chrony |
| `automotive-design-compose` | (not in the tree) | our fork of DesignCompose: the importer the design tools use |

## What is here

| Folder | What |
|---|---|
| `catalog/` | `everyware.vsidl`: every bundle, what it publishes, subscribes to and offers; the protos; the generated descriptors |
| `bundles/` | the service bundles, one APEX each: `weather`, `news`, `wifi`, `matter`, and `dashboard`, the bridge between the bundles and the screen (gRPC for the app) |
| `libs/` | what the bundles share: `https` (TLS on BoringSSL), `bundle-store` (a bundle's last published values, so that they are there at the first frame after a boot) |
| `product/` | `everyware.mk` and the SELinux contexts of the bundles: what a device product adds to carry them |
| `design/figma/` | the kiwi tools: a Figma file written and read without Figma, the roundtrip, `fig2json` |
| `tools/` | `dcf` (a `.dcf` from Figma or from a `.fig`, the node by node comparison), `matter-generator` (a virtual Matter device on the laptop), `boot-time` (power on to the first frame, measured) |
| `scripts/` | the process (`feature.sh`, `setup-agents.sh`, the git hooks), the design path (`fetch-dcf.sh`, `fig-to-dcf.sh`), the board (`deploy-bundle-rb3.sh`, `run-smarthome-rb3.sh`) |
| `skills/` | step by step instructions for an agent: a service bundle, a Figma design, a harry app |
| `docs/` | the concepts (one idea per page), the solution, the decisions and the status |
| `AGENTS.md` | the entry for an agent working here: the rules, how a change lands |

## Building

```bash
# WHAT: the tree, the local manifest and the sync
repo init -u https://android.googlesource.com/platform/manifest -b android-17.0.0_r1
git clone -b everyware-17.0.0_r1 https://github.com/sdv-everyware/manifest .repo/local_manifests
repo sync -c -j8
# WHAT: Linaro's prebuilt vendor package for the RB3 (once)
device/linaro/dragonboard/fetch-vendor-package.sh
# WHAT: the image of the panel
source build/envsetup.sh && lunch sdv_media_db845c-trunk_staging-userdebug
m
```

The README of `device-rb3` says how to flash the board and what runs on it.

## How a change lands

A change is a branch, a review by another agent, and the user's word, in every repository of
the project: `scripts/feature.sh` does the mechanical part and `AGENTS.md` has the rules. The
git hooks (`scripts/git-hooks`, installed by `scripts/setup-agents.sh`) keep `main` for landed
changes only and let nothing private into a commit.

## Licence

Apache-2.0 (`LICENSE`). The code of Google's repositories we fork keeps its own notices.
