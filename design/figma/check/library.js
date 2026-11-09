// check/library.js: a read-only patch for roundtrip.js. Lists the component sets of the Library
// (Components page) with their positions and sizes, and fails on a set that overlaps another
// or lies outside the Library frame: in Figma such a set is hidden behind its neighbour.
const key = g => `${g.sessionID}:${g.localID}`;
module.exports = function (msg) {
  const nodes = msg.nodeChanges;
  const lib = nodes.find(n => n.name === 'Library' && n.type === 'FRAME');
  if (!lib) throw new Error('no Library frame');
  const sets = nodes.filter(n => n.isStateGroup && n.parentIndex && key(n.parentIndex.guid) === key(lib.guid))
    .map(n => ({ n: n.name, x: n.transform.m02, y: n.transform.m12, w: n.size.x, h: n.size.y }));
  let bad = 0;
  for (let i = 0; i < sets.length; i++) for (let j = i + 1; j < sets.length; j++) {
    const a = sets[i], b = sets[j];
    if (a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h) { console.log(`CHECK overlap: ${a.n} x ${b.n}`); bad++; }
  }
  for (const s of sets) if (s.x + s.w > lib.size.x || s.y + s.h > lib.size.y) { console.log(`CHECK outside the Library: ${s.n}`); bad++; }
  sets.forEach(s => console.log(`CHECK   ${s.n} @${Math.round(s.x)},${Math.round(s.y)} ${Math.round(s.w)}x${Math.round(s.h)}`));
  console.log(`CHECK library: ${sets.length} sets, ${bad} problems, Library ${lib.size.x}x${lib.size.y}`);
  if (bad) process.exitCode = 1;
  return msg;
};
