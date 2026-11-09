# kiwi-schema 0.5.0

The reader and writer of the kiwi binary format, which Figma stores a .fig file's canvas in
(`canvas.fig` inside the zip). `roundtrip.js` and `fig2json.js` need it to read and write a
.fig file.

- Source: the npm package kiwi-schema 0.5.0 (https://github.com/evanw/kiwi), its CommonJS build
  `kiwi.js` unchanged; the types (`.d.ts`) and the other builds are left out.
- License: MIT, `LICENSE.md` (from the repository; the npm package carries only the field).
- Why here: the design tools run on any machine and for any agent without npm or a network.
- Update: take `kiwi.js` from a newer package and run the projects' `design/build.sh` and `check-figma.sh`
  on the current design: the output must not change.
