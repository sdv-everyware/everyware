// check/contract.js: a read-only patch for roundtrip.js. The app's contract: every node whose
// name starts with '#' under #smarthome-root (what the fetch takes, with the Library), and
// whether a designer-only frame got into the root (it must not: the .dcf would carry it).
// CHECK_NAMES=#device/card,#home/tabs makes it fail when one of these names is missing.
const key = g => `${g.sessionID}:${g.localID}`;
module.exports = function (msg) {
  const nodes = msg.nodeChanges;
  const kids = p => nodes.filter(n => n.parentIndex && key(n.parentIndex.guid) === p);
  const root = nodes.find(n => n.name === '#smarthome-root');
  if (!root) throw new Error('no #smarthome-root');
  // The app sees a node by its name under the root, inside the component an instance there
  // shows (and the components nested in it), or under the name an instance's override gives a
  // part (a sheet names the parts of the library's #sheet that way).
  const byGuid = new Map(nodes.map(n => [key(n.guid), n]));
  const names = new Set(), stack = [root], seenSymbols = new Set();
  let designerOnly = 0;
  while (stack.length) {
    const n = stack.pop();
    if ((n.name || '').startsWith('#')) names.add(n.name);
    if (/\(overlay\)|^hotspot:|^Screen ·/.test(n.name || '')) { console.log(`CHECK designer-only frame in the root: "${n.name}"`); designerOnly++; }
    stack.push(...kids(key(n.guid)));
    if (n.type === 'INSTANCE' && n.symbolData) {
      // the component it shows, and the components its overrides swap in (#sheet/content)
      const symbols = [n.symbolData.symbolID];
      for (const o of n.symbolData.symbolOverrides || []) {
        if ((o.name || '').startsWith('#')) names.add(o.name);
        if (o.overriddenSymbolID) symbols.push(o.overriddenSymbolID);
      }
      // every variant of its set: the app switches variants (a sheet's state, a content's step)
      const variants = [];
      for (const id of symbols) {
        const sym = byGuid.get(key(id));
        if (!sym) continue;
        const set = sym.parentIndex && byGuid.get(key(sym.parentIndex.guid));
        variants.push(...(set && set.isStateGroup ? kids(key(set.guid)) : [sym]));
      }
      for (const sym of variants) {
        if (!seenSymbols.has(key(sym.guid))) { seenSymbols.add(key(sym.guid)); stack.push(sym, ...kids(key(sym.guid))); }
      }
    }
  }
  // the app's tags that are not node names (a zone's content tag for ExternalImage)
  const want = (process.env.CHECK_NAMES || '').split(',').filter(w => w && !w.startsWith('#zone/'));
  const missing = want.filter(w => !names.has(w));
  missing.forEach(m => console.log(`CHECK missing in the root: ${m}`));
  console.log(`CHECK contract: ${names.size} #names in #smarthome-root, ${designerOnly} designer-only frames there, ${missing.length} of ${want.length} wanted names missing`);
  if (designerOnly || missing.length) process.exitCode = 1;
  return msg;
};
