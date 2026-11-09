# Traps: designing screens and sheets in the generators

| Symptom | Cause | Fix |
|---|---|---|
| A designer "cannot see" a sheet or the keyboard | its instance is in the closed variant: the card lies below the screen and `#smarthome-root` clips | the visible overlay frames and the prototype of prototype-patch.js; never open the instance in the file the app gets |
| A sheet's instance lands in the column, below the screen | Figma's import drops absolute positioning on an instance | the instance inside a plain absolute frame (`stackPositioning: 'ABSOLUTE'`) |
| A component set is "missing" on the Components page | it lies over another set or outside the Library frame | `check-figma.sh` (library check); place new sets in a free column; the Library's height is computed from its content at the end of components-patch.js |
| A set grew and now covers the next one | a new variant added a row (an icon, a kind) | more columns (`{ cols }`) or a free place; check again |
| Figma fails to import, or nodes vanish | two node factories in one patch handed out the same GUIDs | one `authoring()` per patch |
| An instance is renamed on import and the app cannot find it | Figma takes the name for an automatic one | `hasBeenManuallyRenamed: true` (the helpers set it) |
| A text shows on the device but not as in Figma | letter spacing, or a box narrower than the font's advance | letter spacing 0, the box sized for the font (IBM Plex Mono: 0.6 em a character) |
| A picture never shows | it sits on a rounded rectangle | a plain frame inside a clipping frame with the corners |
| A designer-only frame reaches the .dcf | it was put under `#smarthome-root` | Page 1 outside the root; `check-figma.sh` (contract check) fails on it |
| A prototype tap does nothing in Play | the target is not a frame, or the overlay settings are missing | overlays are frames with `overlayPositionType` set; read the taps back (`check-figma.sh`, prototype) |
| A new tap works on the root but not in Play | Play starts at "Screen · Overview", a copy of the root's parts made by prototype-patch.js; a tap put on the root's node (`nodes.find`) is not on the copy | put it on the copy too: `within(ov, '#name')` (as `#settings`, the tile headers, `#news/list`); `check-figma.sh` then lists the tap twice |
| A tap inside a card in Play does nothing | the button is part of an instance | a hotspot frame over it |
| Paths break in a shell loop (`$UL:core` becomes garbage) | zsh applies `:c`, `:h`, `:r` as modifiers to `$VAR:...` | `${VAR}` or run the loop in `bash -c` |
| The app does not drive a part the design has | the part is drawn, not a named node or slot | name it (`#screen/part`) or make it a slot; tell the user if a drawn element should become a library component |
