# tools/dcf: the .dcf tools

Two host tools on DesignCompose's importer, built with cargo (not Soong: they run on the
developer's machine, next to the kiwi tools in `design/figma`):

| Tool | What it does |
|---|---|
| `dcf-fetch` | a `.dcf` from Figma (`--doc-id`, node-scoped: the named roots and the Library, not the whole file) or from a local document (`--local <dir>`, the folder `fig2json.js` wrote from a `.fig`: no token, no network) |
| `dcf-diff` | two `.dcf` compared node by node (style and node data, ids left out), for the `.fig` to `.dcf` path against Figma's own import |

The scripts use them: `scripts/fig-to-dcf.sh` (a `.fig` to a `.dcf`), `scripts/fetch-dcf.sh` (a
file in Figma to a `.dcf`). `dcf-render`, the picture harry draws of a `.dcf` on the host, is in
display_safety's `har-rendering-parity`.

## The importer

The importer is `dc_figma_import` from our fork of DesignCompose,
`https://github.com/sdv-everyware/automotive-design-compose`, branch `everyware`: the
`v0.39.2` release and our commits on it (`Document::from_local`, a team library's component
sets so that its instances swap variants, the library's variables fetched once per file, nodes
with `geometry=paths`, a library variant's size, a disk cache of the Figma API's answers about
other files, image fills re-encoded as PNG). The `.dcf` it writes has the format version harry
reads. A change to the importer goes to that fork first; a commit there is a candidate for a
pull request to DesignCompose.

## Building and running

```bash
# WHAT: cargo with the tree's toolchain, outputs in <tree>/out/dcf-tools
# WHY: no cargo to install; the repository stays clean
vendor/sdv-everyware/everyware/tools/dcf/cargo.sh build --release
vendor/sdv-everyware/everyware/tools/dcf/cargo.sh run -q --bin dcf-fetch -- --local <dir> --nodes '#tv-root' --nodes Library --output tv.dcf
vendor/sdv-everyware/everyware/tools/dcf/cargo.sh run -q --bin dcf-diff -- <reference.dcf> <new.dcf> '#tv-root'
# WHAT: the importer from a local clone of the fork instead of GitHub
# WHY: a change to the importer before it is pushed
DCF_FORK=<clone> vendor/sdv-everyware/everyware/tools/dcf/cargo.sh build
```

The first build needs the network: the fork from GitHub and the crates from crates.io, pinned
by `Cargo.lock`.
