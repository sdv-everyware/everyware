// json2fig.js: a subtree of the Figma REST API's JSON (GET /v1/files/.../nodes, geometry=paths,
// plugin_data=shared) as kiwi nodes in a .fig being patched: the reverse of fig2json.js. This is
// how something drawn in Figma comes into a generated design without a hand export: the fetched
// JSON is kept in git, a patch builds the nodes from it (ui-lib's components-patch.js does that
// for the component sets drawn in Figma).
//
//   const { fromRest } = helpers;                       // roundtrip.js hands it to a patch
//   const set = fromRest(A, msg, parent, restNode, { x, y, resolve });
//
// A: authoring(msg, tag) of fig-helpers.js. resolve(componentId, restInstance) gives the kiwi
// SYMBOL an instance is of (the caller finds it by the component's and its set's names).
//
// What it carries: frames with auto layout, component sets and their variants, rectangles,
// ellipses and arcs, texts, vectors (as their outlines), instances with the overrides of names,
// texts, opacity and visibility, solid and gradient paints, shadows, constraints, shared plugin
// data (DesignCompose's meters). What it does not, and stops at: image fills. A vector keeps its
// outline, not its editable network: it draws the same, in Figma its points cannot be edited.
const key = g => `${g.sessionID}:${g.localID}`;
const H = { LEFT: 'MIN', RIGHT: 'MAX', CENTER: 'CENTER', LEFT_RIGHT: 'STRETCH', SCALE: 'SCALE' };
const V = { TOP: 'MIN', BOTTOM: 'MAX', CENTER: 'CENTER', TOP_BOTTOM: 'STRETCH', SCALE: 'SCALE' };
const ALIGN = { MIN: 'MIN', CENTER: 'CENTER', MAX: 'MAX', SPACE_BETWEEN: 'SPACE_EVENLY', BASELINE: 'BASELINE' };
const WEIGHT = { 100: 'Thin', 200: 'Extra Light', 300: 'Light', 400: 'Regular', 500: 'Medium', 600: 'Semi Bold', 700: 'Bold', 800: 'Extra Bold', 900: 'Black' };
const SHAPES = new Set(['VECTOR', 'LINE', 'STAR', 'REGULAR_POLYGON', 'BOOLEAN_OPERATION']);

const color = c => ({ r: c?.r ?? 0, g: c?.g ?? 0, b: c?.b ?? 0, a: c?.a ?? 1 });
const inv = m => { const det = m.m00 * m.m11 - m.m01 * m.m10 || 1e-9;
  return { m00: m.m11 / det, m01: -m.m01 / det, m02: (m.m01 * m.m12 - m.m11 * m.m02) / det, m10: -m.m10 / det, m11: m.m00 / det, m12: (m.m10 * m.m02 - m.m00 * m.m12) / det }; };

function paint(p, where) {
  const base = { type: p.type, opacity: p.opacity ?? 1, visible: p.visible !== false, blendMode: p.blendMode || 'NORMAL' };
  if (p.type === 'SOLID') return { ...base, color: color(p.color) };
  if (/^GRADIENT_/.test(p.type)) {
    // REST: the gradient's three handles in the node's unit square; kiwi: the matrix from the
    // unit square to the gradient's (fig2json.js has the other direction)
    const [h0, h1, h2] = p.gradientHandlePositions;
    const b = 2 * (h2.x - h0.x), d = 2 * (h2.y - h0.y);
    const m = { m00: h1.x - h0.x, m01: b, m02: h0.x - 0.5 * b, m10: h1.y - h0.y, m11: d, m12: h0.y - 0.5 * d };
    return { ...base, transform: inv(m), stops: (p.gradientStops || []).map(s => ({ position: s.position, color: color(s.color) })) };
  }
  throw new Error(`${where}: a ${p.type} paint is not carried from Figma's JSON (image fills are not fetched)`);
}
const paints = (list, where) => (list || []).map(p => paint(p, where));
const effect = e => ({ type: e.type, visible: e.visible !== false, radius: e.radius ?? 0, color: color(e.color), blendMode: e.blendMode || 'NORMAL',
  offset: { x: e.offset?.x ?? 0, y: e.offset?.y ?? 0 }, spread: e.spread ?? 0, showShadowBehindNode: e.showShadowBehindNode === true });

// the same paints, for finding an instance's fill override: solid colours compared, anything
// else by its type
const samePaints = (rest, kiwi) => {
  const a = rest || [], b = kiwi || [];
  if (a.length !== b.length) return false;
  return a.every((p, i) => p.type === b[i].type && (p.visible !== false) === (b[i].visible !== false) && Math.abs((p.opacity ?? 1) - (b[i].opacity ?? 1)) < 1e-4 &&
    (p.type !== 'SOLID' || ['r', 'g', 'b', 'a'].every(c => Math.abs((p.color?.[c] ?? (c === 'a' ? 1 : 0)) - (b[i].color?.[c] ?? (c === 'a' ? 1 : 0))) < 1e-4)));
};

// an SVG path of M, L, Q, C, Z (absolute, as REST gives it) as a geometry blob: 1 byte command,
// float32 coordinates
function pathBlob(d) {
  const bytes = [];
  const f = v => { const b = Buffer.alloc(4); b.writeFloatLE(v, 0); bytes.push(...b); };
  const CMD = { Z: 0, M: 1, L: 2, Q: 3, C: 4 };
  for (const m of d.matchAll(/([MLQCZ])([^MLQCZ]*)/g)) {
    bytes.push(CMD[m[1]]);
    if (m[2].trim()) m[2].trim().split(/[ ,]+/).map(Number).forEach(f);
  }
  if (/[^MLQCZ0-9eE .,+-]/.test(d)) throw new Error(`a path with other commands than M L Q C Z: ${d.slice(0, 60)}`);
  return { bytes: Uint8Array.from(bytes) };
}

function fromRest(A, msg, parent, rest, opts = {}) {
  const byKey = () => new Map(msg.nodeChanges.map(n => [key(n.guid), n]));
  const sorted = n => A.kids(key(n.guid)).slice().sort((a, b) => (a.parentIndex.position < b.parentIndex.position ? -1 : a.parentIndex.position > b.parentIndex.position ? 1 : 0));
  const geometry = list => (list || []).filter(g => g.path).map(g => {
    msg.blobs = msg.blobs || [];
    msg.blobs.push(pathBlob(g.path));
    return { windingRule: g.windingRule === 'EVENODD' ? 'ODD' : 'NONZERO', commandsBlob: msg.blobs.length - 1, styleID: 0 };
  });

  // the properties every node has
  const common = (r, parentRest, where) => {
    const t = r.relativeTransform || [[1, 0, 0], [0, 1, 0]];
    const x = {
      visible: r.visible !== false, opacity: r.opacity ?? 1,
      transform: { m00: t[0][0], m01: t[0][1], m02: t[0][2], m10: t[1][0], m11: t[1][1], m12: t[1][2] },
      fillPaints: paints(r.fills, where), strokePaints: paints(r.strokes, where),
      strokeWeight: r.strokeWeight ?? 0, strokeAlign: r.strokeAlign || 'INSIDE',
      horizontalConstraint: H[r.constraints?.horizontal] || 'MIN', verticalConstraint: V[r.constraints?.vertical] || 'MIN',
    };
    if (r.blendMode && r.blendMode !== 'PASS_THROUGH') x.blendMode = r.blendMode;
    if (r.effects && r.effects.length) x.effects = r.effects.map(effect);
    if (r.cornerRadius) x.cornerRadius = r.cornerRadius;
    if (r.rectangleCornerRadii) [x.rectangleTopLeftCornerRadius, x.rectangleTopRightCornerRadius, x.rectangleBottomRightCornerRadius, x.rectangleBottomLeftCornerRadius] = r.rectangleCornerRadii;
    if (r.strokeCap) x.strokeCap = r.strokeCap;
    if (r.strokeDashes && r.strokeDashes.length) x.dashPattern = r.strokeDashes;
    if (r.sharedPluginData) x.pluginData = Object.entries(r.sharedPluginData).flatMap(([ns, kv]) => Object.entries(kv).map(([k, value]) => ({ pluginID: '', key: `${ns}-${k}`, value })));
    // the node in its (auto layout) parent
    if (r.layoutPositioning === 'ABSOLUTE') x.stackPositioning = 'ABSOLUTE';
    if (r.layoutGrow) x.stackChildPrimaryGrow = r.layoutGrow;
    if (r.layoutAlign === 'STRETCH') x.stackChildAlignSelf = 'STRETCH';
    return x;
  };
  // a frame's own layout
  const layout = r => {
    const x = { frameMaskDisabled: r.clipsContent === false };
    if (r.layoutMode && r.layoutMode !== 'NONE') {
      x.stackMode = r.layoutMode;
      x.stackPrimarySizing = r.primaryAxisSizingMode === 'AUTO' ? 'RESIZE_TO_FIT_WITH_IMPLICIT_SIZE' : 'FIXED';
      x.stackCounterSizing = r.counterAxisSizingMode === 'AUTO' ? 'RESIZE_TO_FIT_WITH_IMPLICIT_SIZE' : 'FIXED';
      x.stackPrimaryAlignItems = ALIGN[r.primaryAxisAlignItems] || 'MIN';
      x.stackCounterAlignItems = ALIGN[r.counterAxisAlignItems] || 'MIN';
      x.stackHorizontalPadding = r.paddingLeft ?? 0; x.stackPaddingRight = r.paddingRight ?? 0;
      x.stackVerticalPadding = r.paddingTop ?? 0; x.stackPaddingBottom = r.paddingBottom ?? 0;
      x.stackSpacing = r.itemSpacing ?? 0;
      if (r.layoutWrap === 'WRAP') x.stackWrap = 'WRAP';
    }
    return x;
  };
  const textProps = r => {
    const s = r.style || {};
    const style = (WEIGHT[s.fontWeight] || 'Regular') + (s.italic ? ' Italic' : '');
    const lineHeight = s.lineHeightUnit === 'PIXELS' ? { units: 'PIXELS', value: s.lineHeightPx }
      : s.lineHeightUnit === 'FONT_SIZE_%' ? { units: 'RAW', value: (s.lineHeightPercentFontSize ?? 100) / 100 }
      : { units: 'PERCENT', value: s.lineHeightPercent ?? (s.lineHeightUnit ? s.lineHeightPercentFontSize ?? 100 : 100) };
    const chars = r.characters || '';
    const x = {
      fontSize: s.fontSize ?? 12,
      fontName: { family: s.fontFamily || 'Inter', style, postscript: s.fontPostScriptName || `${(s.fontFamily || 'Inter').replace(/ /g, '')}-${style.replace(/ /g, '')}` },
      textAlignHorizontal: s.textAlignHorizontal || 'LEFT', textAlignVertical: s.textAlignVertical || 'TOP',
      lineHeight, letterSpacing: { units: 'PIXELS', value: s.letterSpacing ?? 0 },
      textData: { characters: chars, lines: chars.split('\n').map(() => ({ lineType: 'PLAIN', styleId: 0, indentationLevel: 0, sourceDirectionality: 'AUTO', listStartOffset: 0, isFirstLineOfList: false })) },
      textAutoResize: s.textAutoResize || 'NONE', textUserLayoutVersion: 3, textBidiVersion: 1, fontVariations: [], fontVersion: '', autoRename: false,
    };
    if (s.textTruncation && s.textTruncation !== 'DISABLED') x.textTruncation = s.textTruncation;
    if (s.maxLines !== undefined && s.maxLines !== null && s.maxLines !== -1) x.maxLines = s.maxLines;
    return x;
  };

  // the overrides of an instance: its expanded children against its component's, child by child
  // (names, texts, opacity, visibility); inside a nested instance the path goes through it
  const overridesOf = (restKids, kiwiKids, path, where) => {
    const out = [];
    if (restKids.length !== kiwiKids.length) throw new Error(`${where}: the instance has ${restKids.length} children, its component ${kiwiKids.length} (a swapped or changed component is not carried)`);
    restKids.forEach((rc, i) => {
      const kc = kiwiKids[i], o = {};
      if ((rc.name || '') !== (kc.name || '') && kc.type !== 'INSTANCE') o.name = rc.name;
      if ((rc.visible !== false) !== (kc.visible !== false)) o.visible = rc.visible !== false;
      if (Math.abs((rc.opacity ?? 1) - (kc.opacity ?? 1)) > 1e-4) o.opacity = rc.opacity ?? 1;
      if (rc.type === 'TEXT' && kc.textData && (rc.characters || '') !== (kc.textData.characters || '')) o.textData = { ...kc.textData, characters: rc.characters || '' };
      if (kc.type !== 'INSTANCE' && rc.fills && !samePaints(rc.fills, kc.fillPaints)) o.fillPaints = paints(rc.fills, where);
      const here = [...path, kc.guid];
      if (kc.type === 'INSTANCE') {
        const sym = byKey().get(key(kc.overriddenSymbolID || kc.symbolData.symbolID));
        // the nested instance's own name is an override on its component's root, as fig-helpers writes it
        const own = (kc.symbolData.symbolOverrides || []).find(v => v.guidPath.guids.length === 1 && key(v.guidPath.guids[0]) === key(sym.guid));
        if ((rc.name || '') !== ((own && own.name) || kc.name || '')) out.push({ name: rc.name, guidPath: { guids: [...here, sym.guid] } });
        out.push(...overridesOf(rc.children || [], sorted(sym), here, `${where}/${rc.name}`));
      } else if (rc.children) out.push(...overridesOf(rc.children, sorted(kc), path, `${where}/${rc.name}`));
      if (Object.keys(o).length) out.push({ ...o, guidPath: { guids: here } });
    });
    return out;
  };

  const make = (r, parentKiwi, parentRest, where, place) => {
    const w = r.size ? r.size.x : r.absoluteBoundingBox.width, h = r.size ? r.size.y : r.absoluteBoundingBox.height;
    const c = common(r, parentRest, where);
    if (place) c.transform = { ...c.transform, m02: place.x, m12: place.y };
    const here = `${where}/${r.name}`;
    const below = n => (r.children || []).forEach(k => make(k, n, r, here));
    switch (r.type) {
      case 'COMPONENT_SET': {
        // the variant properties from the variants' names ("state=default, size=regular"), in the
        // order they first appear: Figma reads the values from variantPropSpecs by property id
        const variants = (r.children || []).filter(k => k.type === 'COMPONENT');
        const parsed = variants.map(v => v.name.split(',').map(s => s.trim().split('=')));
        const props = [];
        for (const pv of parsed) for (const [p, val] of pv) { let e = props.find(x => x[0] === p); if (!e) props.push(e = [p, []]); if (!e[1].includes(val)) e[1].push(val); }
        const defs = props.map(([p], i) => ({ id: A.newGuid(), name: p, initialValue: {}, sortPosition: String.fromCharCode(0x21 + i), type: 'VARIANT',
          preferredValues: {}, varValue: { value: { textValue: '' }, dataType: 'STRING', resolvedDataType: 'STRING' } }));
        const set = A.base(parentKiwi, 'FRAME', r.name, 0, 0, w, h, { ...c, ...layout(r), isStateGroup: true, isPublishable: true,
          stateGroupPropertyValueOrders: props.map(([property, values]) => ({ property, values })), componentPropDefs: defs });
        variants.forEach((v, i) => {
          const sym = make(v, set, r, here);
          sym.variantPropSpecs = parsed[i].map(([p, val]) => ({ propDefId: defs[props.findIndex(x => x[0] === p)].id, value: val }));
        });
        return set;
      }
      case 'COMPONENT': { const n = A.base(parentKiwi, 'SYMBOL', r.name, 0, 0, w, h, { ...c, ...layout(r), isSymbolPublishable: true }); below(n); return n; }
      case 'FRAME': case 'GROUP': case 'SECTION': {
        // (a frame's own outline, when the JSON has it: Figma's files keep it on the frame too)
        const outline = {};
        if (r.fillGeometry && r.fillGeometry.length) outline.fillGeometry = geometry(r.fillGeometry);
        if (r.strokeGeometry && r.strokeGeometry.length) outline.strokeGeometry = geometry(r.strokeGeometry);
        const n = A.base(parentKiwi, 'FRAME', r.name, 0, 0, w, h, { ...c, ...layout(r), ...outline }); below(n); return n;
      }
      case 'RECTANGLE': return A.base(parentKiwi, 'ROUNDED_RECTANGLE', r.name, 0, 0, w, h, { ...c, cornerRadius: r.cornerRadius || 0 });
      case 'ELLIPSE': {
        const a = r.arcData, whole = !a || (!a.innerRadius && !a.startingAngle && Math.abs((a.endingAngle ?? 2 * Math.PI) - 2 * Math.PI) < 1e-4);
        return A.base(parentKiwi, 'ELLIPSE', r.name, 0, 0, w, h, { ...c, ...(whole ? {} : { arcData: { startingAngle: a.startingAngle ?? 0, endingAngle: a.endingAngle ?? 2 * Math.PI, innerRadius: a.innerRadius ?? 0 } }) });
      }
      case 'TEXT': return A.base(parentKiwi, 'TEXT', r.name, 0, 0, w, h, { ...c, ...textProps(r) });
      case 'INSTANCE': {
        const sym = opts.resolve && opts.resolve(r.componentId, r);
        if (!sym) throw new Error(`${here}: an instance of a component that is not in the Library (${r.componentId}): draw it in the set itself or add that component first`);
        const extra = { visible: c.visible, opacity: c.opacity, horizontalConstraint: c.horizontalConstraint, verticalConstraint: c.verticalConstraint };
        for (const k of ['stackPositioning', 'stackChildPrimaryGrow', 'stackChildAlignSelf', 'pluginData', 'effects']) if (c[k] !== undefined) extra[k] = c[k];
        if (Math.abs(w - sym.size.x) > 1e-3 || Math.abs(h - sym.size.y) > 1e-3) extra.size = { x: w, y: h };
        const n = A.instance(parentKiwi, r.name, 0, 0, sym, extra, overridesOf(r.children || [], sorted(sym), [], here));
        n.transform = c.transform;
        return n;
      }
      default:
        if (SHAPES.has(r.type)) {
          const outline = {};
          if (r.fillGeometry && r.fillGeometry.length) outline.fillGeometry = geometry(r.fillGeometry);
          if (r.strokeGeometry && r.strokeGeometry.length) outline.strokeGeometry = geometry(r.strokeGeometry);
          return A.base(parentKiwi, r.type === 'BOOLEAN_OPERATION' ? 'VECTOR' : r.type, r.name, 0, 0, w, h, { ...c, strokeJoin: r.strokeJoin || 'MITER', ...outline });
        }
        throw new Error(`${here}: a ${r.type} node is not carried from Figma's JSON`);
    }
  };
  return make(rest, parent, null, opts.where || '', opts.x === undefined ? null : { x: opts.x, y: opts.y });
}

module.exports = { fromRest, pathBlob };
