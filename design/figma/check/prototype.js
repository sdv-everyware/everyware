// check/prototype.js: a read-only patch for roundtrip.js. Lists every prototype tap (the node,
// the action, the target and its overlay settings) and the flow's starting point. Figma's import
// drops what it does not understand without a word: read the file back after a build.
const key = g => `${g.sessionID}:${g.localID}`;
module.exports = function (msg) {
  const nodes = msg.nodeChanges;
  const byGuid = new Map(nodes.map(n => [key(n.guid), n]));
  let taps = 0;
  for (const n of nodes) for (const pi of n.prototypeInteractions || []) for (const a of pi.actions || []) {
    taps++;
    const t = a.transitionNodeID && byGuid.get(key(a.transitionNodeID));
    const target = t ? `"${t.name}"${t.overlayPositionType ? ` overlay ${t.overlayPositionType}, outside ${t.overlayBackgroundInteraction}` : ''}` : '';
    console.log(`CHECK tap ${pi.event?.interactionType} on "${n.name}": ${a.connectionType}/${a.navigationType || '-'} ${target}`);
  }
  for (const n of nodes) if (n.prototypeStartingPoint) console.log(`CHECK starting point on "${n.name}": ${n.prototypeStartingPoint.name}`);
  console.log(`CHECK prototype: ${taps} taps`);
  return msg;
};
