---
name: figma-design
description: Design screens, sheets and overlays of the Smart HOme panel in Figma through the kiwi generators (smarthome-app's design/, the tools in everyware/design/figma), so that the harry app can drive them and a designer can see and click them - the screen as the app's contract (#names), overlays as closed/open component sets, the designer's view as visible frames with a Figma prototype (tap -> Open overlay, Navigate, Swap, Close, hotspots, starting point), versioned builds, read-back checks, the .dcf by fetch or straight from the .fig without Figma (fig-to-dcf.sh, checked with dcf-diff and dcf-render). Use when the user asks to design a new screen or tab, a sheet, dialog or overlay, to make a screen or a sheet visible or clickable for designers, to add a prototype or Play mode, or when a designer cannot find a sheet in the file. For a single library component (button, chip, ...) use ui-component instead; for a whole new EveryWare app use new-app.
---

# Design a screen or a sheet for the panel

The Figma file is generated, never edited by hand: the user's original export plus a chain of
patches at the kiwi level. Two readers use it, and the design must serve both:

```
                         ┌─► #smarthome-root + Library ──fetch──► smarthome.dcf ──► harry app
Smart HOme.fig ──build──►│      (the contract: #names, component sets, closed sheets)
 (the user's export)     └─► Page 1 frames for people (visible sheets, screens, a prototype)
                                (outside the root: the fetch never takes them)
```

- **The app** drives what is under `#smarthome-root`: it finds nodes by their `#name`, picks
  variants, sets texts and opacities. harry does **not** read Figma prototypes (no reaction or
  overlay code in the framework); every behaviour is code.
- **A designer** must see every state and click through it: Figma's own prototype (as
  DesignCompose does it, `external/automotive-design-compose-protos`, interaction/reaction.proto
  and plugin/frame_extras.proto).

## The build

```bash
# WHAT: build the panel from the designer's export (design/source/Smart HOme.fig) into
#       design/build/smarthome.fig, a build output; --dcf also writes data/smarthome.dcf
# from the tree's top
SA=packages/services/display_safety/utils/smarthome-app
$SA/design/build.sh
# WHAT: read it back and check it (read only); fails on an overlap, a missing name, a designer frame in the root
CHECK_NAMES=#device/card,#home/tabs vendor/sdv-everyware/everyware/design/figma/check-figma.sh $SA/design/build/smarthome.fig
```

**Always give the user the built .fig for a review.** After every design build, in your reply:
the full absolute path of the built file (the build script prints it as its last line, `review in
Figma (File > Import): ...`), what changed in it, and where to look (the frame or the set's name).
Not a bare file name, not a path relative to the repository.
For the panel: `<tree>/packages/services/display_safety/utils/smarthome-app/design/build/smarthome.fig`.

The chain (smarthome-app's `design/build.sh`, in this order): ui-lib's `design/components-patch.js`
(the Library: component sets, from ui-lib/components.json), then smarthome-app's `design/`: keyboard-patch.js (the Wi-Fi sheet and the keyboard dock),
dashboard-patch.js (the drawn dashboard becomes slots and named texts), screens-patch.js (tabs,
Rooms, Automations), sheet-patch.js (the device sheet), panel-patch.js, prototype-patch.js (the
designer's frames and the prototype, always last). A new screen or sheet of the panel is a new patch in
smarthome-app's `design/` and the chain, or a section of the patch it belongs to; the designer's part goes into prototype-patch.js.

Every build writes `design/build/smarthome.fig` again (not in git: it is built from the patches
and `design/source/`). The user can import it into Figma to look at it; a new export from the
designer replaces `design/source/Smart HOme.fig` in a commit. Only the .dcf is committed; a fetch
from Figma keeps its file key local (`scripts/figma-key`, git-ignored).

### The .dcf without Figma

A generated .fig needs no import and no fetch: `fig-to-dcf.sh` turns it into the .dcf with the
same importer (fig2json.js writes the REST JSON from the kiwi file, dc_figma_import reads it
with `--local`). No file key, no token, no quota. A file a designer edited by hand in Figma still
goes through the fetch.

```bash
# WHAT: the app's .dcf from a built .fig (default output: smarthome-app/data/smarthome.dcf,
#       the previous one kept next to it with its time)
vendor/sdv-everyware/everyware/scripts/fig-to-dcf.sh $SA/design/build/smarthome.fig $SA/data/smarthome.dcf    # or: $SA/design/build.sh --dcf
# the root is #<out's name>-root (tv.dcf -> #tv-root); DCF_CHECK: the names that must be in the result
DCF_CHECK='#tv-root #tv/time' \
    vendor/sdv-everyware/everyware/scripts/fig-to-dcf.sh "<file.fig>" <out.dcf>
# WHAT: check it (read only): node by node against a reference .dcf, then a picture drawn by harry on the host
vendor/sdv-everyware/everyware/tools/dcf/cargo.sh run -q --bin dcf-diff -- <reference.dcf> <new.dcf> '#smarthome-root'
cd packages/services/display_safety
cargo run -q -p har-rendering-parity --features headless --bin dcf-render -- <new.dcf> '#smarthome-root' out.png
```

dcf-render on the host needs three settings the tree does not make: the clang headers for bindgen
(`BINDGEN_EXTRA_CLANG_ARGS=-I<tree>/prebuilts/clang/host/linux-x86/clang-r596125/lib/clang/22/include`),
a `libgbm.so` for the linker (a folder in `LIBRARY_PATH` with a link to the system's `libgbm.so.1`),
and Impeller's libraries at run time (`LD_LIBRARY_PATH=<cargo target>/debug/build/impeller-rs-bindgen-*/out/lib`).

dcf-fetch and dcf-diff are in everyware's `tools/dcf` (cargo, on our fork of DesignCompose,
`tools/dcf/README.md`); dcf-render is in display_safety's `har-rendering-parity`. Measured on
v52: the dashboard drawn by harry from both .dcf differs in 2 pixels of 2073600.

## 1. The screen for the app (the contract)

- Every node the app touches gets a `#name` under `#smarthome-root`: `#<screen>/<part>`
  (`#device/card`, `#rooms/tile-3`, `#home/tabs`). The names are the contract with the app's
  `ui_model.rs`: rename both or neither. Repeated names across widgets need a parent to tell them
  apart (`find(parentName, name)` in the generators).
- A library component on a screen is a **slot**: a frame named for the app with a preview
  instance of the component inside (`slotted()`); harry clones the variant the app picks into it.
- Components by **generic role** (Material names: button, icon button, list item, segmented
  button, tabs, tile, symbol), never contextual (`#fan-selector`, `#room-tile`): a new role is a
  new library component (ui-component skill), not a one-off drawing.
- **Nothing hidden in Figma**: a node hidden there reaches the .dcf as `display: none` and harry
  never shows it again. The app hides what is not needed (visibility or opacity customization).
- Pictures sit on a plain frame (harry draws an image only on a plain rectangle), texts end in an
  ellipsis after their lines (`maxLines`, `textTruncation: 'ENDING'`), letter spacing 0.

## 2. A sheet (dialog, side panel, keyboard)

A sheet is the library's `#sheet` (components-patch.js; ui-lib `sheet.rs` has the same numbers):
`layout = dialog | left | right | top | bottom` (a 1120 x 840 card in the middle, or half the
screen along an edge) x `state = closed | open`. Its parts: `#sheet/scrim` (ink, opacity 0 closed,
0.35 open), `#sheet/card` (opaque, radius 32; closed it waits 40 px behind the edge it comes
from) and `#sheet/content`, a nested instance of the empty placeholder `#sheet-content`.

A sheet on the screen is `sheetInstance(parent, '#x', 0, 0, <#sheet variant>, <content variant>,
overrides)` (fig-helpers.js): an instance of the closed variant whose content slot is swapped for
the sheet's **content component** (Figma's instance swap, `overriddenSymbolID`) and whose parts
are renamed `#x/scrim`, `#x/card`, `#x/content`, so every sheet has names of its own. The
content is a component set of the app's design in the Library, the size of the layout's content
area (dialog: 1040 x 760; a panel's slot is resized), with the app's `#x/...` nodes and slots in
it; a step or mode of the sheet is a property of the content (`#wifi-content`, `step`).

- The instance sits in a **plain absolute frame** of `#smarthome-root` (`stackPositioning:
  'ABSOLUTE'`, 1920 x 1080), not as an absolute instance: Figma's import drops absolute
  positioning on an instance and the sheet lands in the column's flow.
- Layers in the root: content first, sheets after it (later = on top); the keyboard dock above
  the sheets.
- The app **never sets a sheet's variant**: harry takes a variant from the Library and the
  instance's renamed parts and swapped content are lost (the sheet shows open and empty). The
  instance stays the closed variant of the design; the app moves the card from behind its edge
  (`FollowPositionCustomizer`, both axes) and fades the scrim (`FollowOpacityCustomizer`),
  display-only in harry. The layout is the instance's in the design and the app's constant
  (`PANEL_LAYOUT` in state.rs) must match it. A step or mode is a variant of the **content**
  (`#wifi/content`), structural: harry builds that subtree again, ~70 ms on the RB3.
- The keyboard dock (`#keyboard-dock`) stays a set of its own: it is the screen's keyboard, not a
  sheet's.

## 3. The designer's view (prototype-patch.js)

Closed overlays lie below the screen edge, clipped by the root: a designer sees an empty layer.
So for people, outside `#smarthome-root` on Page 1, in a row below the dashboard:

- **Every sheet as a visible frame**: a frame of the card's size that clips a `sheetInstance` of
  the layout's open variant shifted by the card's position (`sheetOverlay(title, layout,
  content, overrides)`), with the overlay settings on the frame: `overlayPositionType` where the
  layout puts the card (`CENTER`, `TOP_RIGHT`, ...), `overlayBackgroundInteraction:
  'CLOSE_ON_CLICK_OUTSIDE'`, `overlayBackgroundAppearance: { backgroundType: 'SOLID_COLOR',
  backgroundColor: ink at 0.35 }`; it opens with `MOVE_FROM_<edge>`. A full-screen overlay (card
  plus keyboard) is a 1920 x 1080 frame with the scrim inside and `backgroundType: 'NONE'`.
  Preview data inside the content: overrides with paths relative to the content component
  (`sheetInstance` prefixes the content instance's guid).
- **Every screen or tab as a frame** ("Screen · Rooms (Bedroom)"): header, content, bottom bar.
- **Preview data per state**: overrides on the instance (texts by guidPath, a nested instance's
  text by `[instanceGuid, textGuid]`, a swapped glyph by `overriddenSymbolID`, an unused row by
  `visible: false`).
- **Taps** (`prototypeInteractions` on the node, `event: { interactionType: 'ON_CLICK' }`):

| Action | `connectionType` / `navigationType` | Use |
|---|---|---|
| open a sheet | `INTERNAL_NODE` / `OVERLAY`, `MOVE_FROM_<edge>`, 0.3 s | a tile's header opens its sheet, Settings the panel |
| swap the overlay | `INTERNAL_NODE` / `SWAP`, `DISSOLVE` | Wi-Fi networks -> password |
| go to a screen | `INTERNAL_NODE` / `NAVIGATE`, `DISSOLVE` | the tabs |
| close | `CLOSE` | Cancel, the close button |

- Buttons inside a card are instances; their taps are **hotspots**: transparent frames over the
  button's bounds in the content (`hotspot(overlay, content, name, label)`, 40 px inside the card),
  named `hotspot: <label>`.
- Each interaction needs a GUID of its own: a session after every node of the file
  (`max(sessionID) + 1`).
- `prototypeStartingPoint` on the first screen (`{ name, description, position: '!' }`), so Play
  works.

## 4. Check, then hand over

1. `check-figma.sh` on the new version: 0 Library problems, the wanted names present, no
   designer-only frame in the root, the taps listed as intended.
2. Tell the user what to look at (which page, which frames, what Play does) and what is not
   verified (the Figma import itself: it drops what it does not understand without a word).
3. Layout is the user's decision: for a new screen show the options as ASCII sketches with their
   trade-offs and wait for the choice before building; report the final layout as ASCII.
4. After the user's import and fetch, or after fig-to-dcf.sh: `everyware_ui_test` checks the
   contract against the app's `smarthome.dcf` (ui-component skill); the app's code for the new
   names is the use-component skill (ui-lib/skills/use-component). A new EveryWare app with its own root is the new-app skill
   (ui-lib/skills/new-app), which runs this chain for that app.

Traps: [traps.md](traps.md). Components: the ui-component skill (ui-lib/skills/ui-component).

A change lands through its own branch, a review by another agent and the user's word:
`vendor/sdv-everyware/everyware/AGENTS.md`, "How a change lands" (`scripts/feature.sh` there).
