#!/usr/bin/env node
// roundtrip.js: a .fig file (Figma's fig-kiwi in a zip) decoded, changed by a patch and written
// as a new .fig; every generated design is a chain of these (smarthome-app's design/build.sh,
// each app's design/ in its own repository).
//
//   node roundtrip.js <in.fig> <out.fig> [patch.js]
//
// The patch exports function(msg, helpers) that changes the kiwi message in place; helpers is
// fig-helpers.js (the node authoring) and json2fig.js (fromRest: Figma's fetched JSON as nodes),
// so a patch can live in any repository and needs no path to this folder. Before the patch the message is encoded again unchanged and compared
// with the original, so a schema the reader does not understand stops here and not in Figma.
// The output keeps the input's zip entries and their order. The output is a build product: the
// projects' build scripts write the result into their design/ (in git, rewritten when its content changes).
//
// Needs node, unzip and zip; the kiwi format comes with it (third_party/kiwi-schema).
const fs = require('fs'), os = require('os'), path = require('path'), zlib = require('zlib'), { execFileSync } = require('child_process');
const kiwi = require(path.join(__dirname, 'third_party/kiwi-schema/kiwi.js'));
const [,, IN, OUT, PATCH] = process.argv;
if (!IN || !OUT) { console.error('usage: node roundtrip.js <in.fig> <out.fig> [patch.js]'); process.exit(2); }

const work = fs.mkdtempSync(path.join(path.dirname(OUT), '.figrt-'));
execFileSync('unzip', ['-q', '-o', IN, '-d', work]);
const canvasPath = path.join(work, 'canvas.fig');
const buf = fs.readFileSync(canvasPath);
if (buf.slice(0, 8).toString() !== 'fig-kiwi') throw new Error('not a fig-kiwi file');
const version = buf.readUInt32LE(8);
let off = 12; const chunks = [];
while (off < buf.length) { const len = buf.readUInt32LE(off); off += 4; chunks.push(buf.slice(off, off + len)); off += len; }
console.log(`header: version ${version}, chunks ${chunks.length}: ${chunks.map(c => c.length).join(', ')}`);

const isZstd = b => b[0] === 0x28 && b[1] === 0xb5 && b[2] === 0x2f && b[3] === 0xfd;
function inflate(b) {
  if (isZstd(b)) return zlib.zstdDecompressSync(b);
  try { return zlib.inflateRawSync(b, { finishFlush: zlib.constants.Z_SYNC_FLUSH }); } catch (e) { return zlib.inflateSync(b); }
}
const schemaBytes = inflate(chunks[0]);
const schema = kiwi.compileSchema(kiwi.decodeBinarySchema(schemaBytes));
const rawMsg = inflate(chunks[1]);
const msg = schema.decodeMessage(rawMsg);
console.log(`decoded: ${msg.nodeChanges.length} nodeChanges, message ${rawMsg.length} bytes raw`);

// 1. byte-level check: encode(decode(raw)) == raw ?
const reenc = Buffer.from(schema.encodeMessage(msg));
const identical = reenc.equals(rawMsg);
console.log(`re-encode unchanged: ${reenc.length} bytes, byte-identical to original: ${identical}`);
if (!identical) {
  let i = 0; while (i < Math.min(reenc.length, rawMsg.length) && reenc[i] === rawMsg[i]) i++;
  console.log(`  first difference at byte ${i} of ${rawMsg.length}`);
  // semantic check: decode again and compare JSON
  const again = schema.decodeMessage(reenc);
  const diffs = [];
  function cmp(a, b, p) {
    if (diffs.length > 20) return;
    if (a === b) return;
    if (typeof a !== typeof b || a === null || b === null) { diffs.push(`${p}: ${JSON.stringify(a)} vs ${JSON.stringify(b)}`); return; }
    if (typeof a !== 'object') { if (typeof a === 'number' && (Math.abs(a - b) < 1e-6 || (Number.isNaN(a) && Number.isNaN(b)))) return; diffs.push(`${p}: ${a} vs ${b}`); return; }
    if (Array.isArray(a) || ArrayBuffer.isView(a)) { if (a.length !== b.length) { diffs.push(`${p}: length ${a.length} vs ${b.length}`); return; } for (let i = 0; i < a.length; i++) cmp(a[i], b[i], `${p}[${i}]`); return; }
    const ka = Object.keys(a).sort(), kb = Object.keys(b).sort();
    if (ka.join() !== kb.join()) { diffs.push(`${p}: keys ${ka.filter(k => !kb.includes(k))} / ${kb.filter(k => !ka.includes(k))}`); }
    for (const k of ka) if (kb.includes(k)) cmp(a[k], b[k], `${p}.${k}`);
  }
  cmp(msg, again, 'msg');
  console.log(`  decode(re-encoded) deep-equals decode(original) ignoring field order: ${diffs.length === 0}`);
  diffs.forEach(d => console.log('  diff ' + d));
  if (diffs.length) { fs.rmSync(work, { recursive: true, force: true }); process.exit(1); }
}

// 2. optional patch
let outMsg = msg;
if (PATCH) {
  const patch = require(path.resolve(PATCH));
  outMsg = patch(msg, { ...require('./fig-helpers.js'), ...require('./json2fig.js') }) || msg;
  console.log('patch applied');
}
const encoded = Buffer.from(schema.encodeMessage(outMsg));
// sanity: decodes back
schema.decodeMessage(encoded);

// 3. rebuild canvas.fig: same header, chunk0 untouched, chunk1 zstd
const chunk1 = isZstd(chunks[1]) ? zlib.zstdCompressSync(encoded) : zlib.deflateRawSync(encoded);
const parts = [Buffer.from('fig-kiwi'), Buffer.alloc(4)];
parts[1].writeUInt32LE(version, 0);
for (const c of [chunks[0], chunk1]) { const l = Buffer.alloc(4); l.writeUInt32LE(c.length, 0); parts.push(l, c); }
for (const c of chunks.slice(2)) { const l = Buffer.alloc(4); l.writeUInt32LE(c.length, 0); parts.push(l, c); }
fs.writeFileSync(canvasPath, Buffer.concat(parts));
console.log(`canvas.fig rewritten: ${fs.statSync(canvasPath).size} bytes (original ${buf.length})`);

// 4. rebuild zip with the original entry order; canvas.fig/images/thumbnail stored, meta.json deflated (as in the original)
const order = execFileSync('unzip', ['-Z1', IN]).toString().trim().split('\n');
if (fs.existsSync(OUT)) fs.unlinkSync(OUT);
const stored = order.filter(e => !e.endsWith('/') && e !== 'meta.json');
const deflated = order.filter(e => e === 'meta.json');
execFileSync('zip', ['-q', '-X', '-0', path.resolve(OUT), ...stored], { cwd: work });
if (deflated.length) execFileSync('zip', ['-q', '-X', '-6', path.resolve(OUT), ...deflated], { cwd: work });
fs.rmSync(work, { recursive: true, force: true });
console.log(`written ${OUT} (${fs.statSync(OUT).size} bytes)`);
