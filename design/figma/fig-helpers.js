// fig-helpers.js: node authoring for the kiwi patches of the Smart HOme Figma file
// (every patch gets it from roundtrip.js as its second argument).
//
// Lessons from earlier imports, kept in the helpers:
// - a solid paint's alpha lives in paint.opacity, which Figma drops on import: translucency is
//   node opacity on a child rect;
// - a node hidden in Figma reaches the .dcf as display:none and harry never shows it again:
//   everything is visible here, the app hides what it needs.

const key = g => `${g.sessionID}:${g.localID}`;

const hex = h => [1, 3, 5].map(i => parseInt(h.slice(i, i + 2), 16) / 255);

// The dashboard's palette.
const C = {
  ink: hex('#24364c'), ink2: hex('#6c7e93'), muted: hex('#a3b1c2'),
  accent: hex('#4268b2'), accentPressed: hex('#35558f'), accentDisabled: hex('#a9bad9'),
  accentSoft: hex('#e6edf9'), accentSoftPressed: hex('#d3def2'),
  // the tones (ui-lib core tone.rs): a strong fill (its label white, ink on the warning's
  // yellow), a soft fill, the ink of a label on the soft fill or on white, and their pressed
  // shades; every label passes WCAG AA (4.5:1) on its fill
  success: hex('#2e7d32'), successPressed: hex('#256b29'), successSoft: hex('#e3f1e5'), successSoftPressed: hex('#cfe6d3'), successInk: hex('#1e6b2a'),
  warning: hex('#f4b400'), warningPressed: hex('#dca200'), warningSoft: hex('#fdf3d6'), warningSoftPressed: hex('#f9e7b0'), warningInk: hex('#7a5200'),
  error: hex('#c0392b'), errorPressed: hex('#a8322a'), errorSoft: hex('#fbe7e5'), errorSoftPressed: hex('#f6d2ce'), errorInk: hex('#a8322a'),
  panel: hex('#dfe7f3'), special: hex('#c9d6ea'), // keyboard: panel, Shift/Delete/123
  line: hex('#b9cbe5'), track: hex('#c9d6ea'), surface: hex('#f4f7fb'), page: hex('#eaf0f7'),
  white: [1, 1, 1],
};
const font = style => ({ family: 'Inter', style, postscript: 'Inter-' + style.replace(' ', '') });
const FONT = { regular: font('Regular'), medium: font('Medium'), semibold: font('Semi Bold') };

function authoring(msg, tag) {
  const nodes = msg.nodeChanges;
  const kids = p => nodes.filter(n => n.parentIndex && key(n.parentIndex.guid) === p);
  const log = (...a) => console.log(`  [${tag}]`, ...a);
  const sess = Math.max(...nodes.map(n => n.guid.sessionID)) + 1;
  let local = 0;
  const guid = () => ({ sessionID: sess, localID: ++local });
  const posCounter = new Map();
  function nextPos(parentKey) {
    if (!posCounter.has(parentKey)) {
      let max = 0x20;
      for (const n of kids(parentKey)) { const p = n.parentIndex.position; if (p && p.charCodeAt(0) > max) max = p.charCodeAt(0); }
      posCounter.set(parentKey, Math.min(max + 1, 0x7d));
    }
    const c = posCounter.get(parentKey);
    posCounter.set(parentKey, c + 1);
    // Two characters, so up to ~8000 children per parent sort in creation order, above the
    // existing single-character positions.
    const i = c - 0x21;
    return '~' + String.fromCharCode(0x21 + Math.floor(i / 90)) + String.fromCharCode(0x21 + (i % 90));
  }
  const shadow = (dy, blur, a) => [{ type: 'DROP_SHADOW', offset: { x: 0, y: dy }, radius: blur, visible: true, blendMode: 'NORMAL', spread: 0, showShadowBehindNode: false, color: { r: 0, g: 0, b: 0, a } }];
  const solid = rgb => ({ type: 'SOLID', color: { r: rgb[0], g: rgb[1], b: rgb[2], a: 1 }, opacity: 1, visible: true, blendMode: 'NORMAL' });
  const at = (x, y, deg = 0) => {
    const r = deg * Math.PI / 180, c = Math.cos(r), s = Math.sin(r);
    return { m00: c, m01: -s, m02: x, m10: s, m11: c, m12: y };
  };
  const base = (parent, type, name, x, y, w, h, extra, position) => {
    const n = Object.assign({
      guid: guid(), phase: 'CREATED', parentIndex: { guid: parent.guid, position: position || nextPos(key(parent.guid)) }, type, name,
      visible: true, opacity: 1, size: { x: w, y: h }, transform: at(x, y),
      strokeWeight: 0, strokeAlign: 'INSIDE', strokeJoin: 'MITER',
    }, extra || {});
    nodes.push(n);
    return n;
  };
  const frame = (parent, name, x, y, w, h, extra) => base(parent, 'FRAME', name, x, y, w, h, Object.assign({ fillPaints: [], cornerRadius: 0, frameMaskDisabled: true }, extra || {}));
  const rect = (parent, name, x, y, w, h, rgb, r, extra) => base(parent, 'ROUNDED_RECTANGLE', name, x, y, w, h, Object.assign({ fillPaints: rgb ? [solid(rgb)] : [], cornerRadius: r }, extra || {}));
  const ellipse = (parent, name, x, y, w, h, rgb, extra) => base(parent, 'ELLIPSE', name, x, y, w, h, Object.assign({ fillPaints: rgb ? [solid(rgb)] : [] }, extra || {}));
  const stroke = (rgb, w = 2) => ({ strokePaints: [solid(rgb)], strokeWeight: w, strokeAlign: 'INSIDE' });
  const text = (parent, name, x, y, w, h, s, size, fnt, rgb, align = 'LEFT', valign = 'TOP', extra) => base(parent, 'TEXT', name, x, y, w, h, Object.assign({
    fontSize: size, fontName: fnt, textAlignHorizontal: align, textAlignVertical: valign,
    lineHeight: { value: Math.round(size * 1.3), units: 'PIXELS' }, letterSpacing: { value: 0, units: 'PIXELS' },
    textData: { characters: s, lines: [{ lineType: 'PLAIN', styleId: 0, indentationLevel: 0, sourceDirectionality: 'AUTO', listStartOffset: 0, isFirstLineOfList: false }] },
    fillPaints: [solid(rgb)], textAutoResize: 'NONE', textUserLayoutVersion: 3, textBidiVersion: 1, fontVariations: [], fontVersion: '', autoRename: false,
  }, extra || {}));
  // A chevron of two rotated bars, centred at (cx, cy). dir: 'right' or 'down'.
  const chevron = (parent, name, cx, cy, size, rgb, dir = 'right') => {
    const g = frame(parent, name, cx - size / 2, cy - size / 2, size, size);
    const len = size * 0.62, th = 2.5, c = size / 2;
    const bar = (mx, my, deg) => {
      const r = deg * Math.PI / 180;
      // place the bar so its centre lands on (mx, my) after rotating about its top-left corner
      const ox = mx - (Math.cos(r) * len / 2 - Math.sin(r) * th / 2), oy = my - (Math.sin(r) * len / 2 + Math.cos(r) * th / 2);
      base(g, 'ROUNDED_RECTANGLE', 'bar', 0, 0, len, th, { fillPaints: [solid(rgb)], cornerRadius: th / 2, transform: at(ox, oy, deg) });
    };
    const d = len * 0.35;
    if (dir === 'right') { bar(c, c - d, 45); bar(c, c + d, -45); } else { bar(c - d, c, 45); bar(c + d, c, -45); }
    return g;
  };
  const plus = (parent, name, cx, cy, size, rgb) => {
    const g = frame(parent, name, cx - size / 2, cy - size / 2, size, size);
    rect(g, 'h', 0, size / 2 - 1.25, size, 2.5, rgb, 1.25);
    rect(g, 'v', size / 2 - 1.25, 0, 2.5, size, rgb, 1.25);
    return g;
  };
  // A component set: frame with isStateGroup and one SYMBOL per variant.
  // props: [['kind', ['primary', ...]], ['state', [...]]]; draw(symbol, values) fills a variant.
  // opts.rows: the values of the last property in rows instead of columns (wide components);
  // opts.cols: that many variants in a row (the last properties combined).
  const componentSet = (parent, name, x, y, props, w, h, gapX, gapY, draw, opts = {}) => {
    // opts.only(values): the combinations the set has (a property that makes sense for some
    // values of another only); all of them without it.
    const combos = props.reduce((acc, [p, vals]) => acc.flatMap(a => vals.map(v => Object.assign({}, a, { [p]: v }))), [{}]).filter(opts.only || (() => true));
    const [colProp] = props[props.length - 1];
    const cols = opts.cols || (opts.rows ? 1 : props[props.length - 1][1].length), rows = Math.ceil(combos.length / cols);
    const pad = 24;
    // Current Figma reads the variant values from variantPropSpecs (by property id), not from
    // the variant names; without the property definitions the set reports "invalid name".
    const defs = props.map(([p], i) => ({
      id: guid(), name: p, initialValue: {}, sortPosition: String.fromCharCode(0x21 + i), type: 'VARIANT',
      preferredValues: {}, varValue: { value: { textValue: '' }, dataType: 'STRING', resolvedDataType: 'STRING' },
    }));
    const set = frame(parent, name, x, y, pad * 2 + cols * w + (cols - 1) * gapX, pad * 2 + rows * h + (rows - 1) * gapY, {
      isStateGroup: true, isPublishable: true,
      stateGroupPropertyValueOrders: props.map(([property, values]) => ({ property, values })),
      componentPropDefs: defs,
      fillPaints: [solid(C.white)], cornerRadius: 16, frameMaskDisabled: false,
      ...stroke(C.line, 1), dashPattern: [8, 6],
    });
    const symbols = {};
    combos.forEach((values, i) => {
      const col = i % cols, row = Math.floor(i / cols);
      const vname = props.map(([p]) => `${p}=${values[p]}`).join(', ');
      const sym = base(set, 'SYMBOL', vname, pad + col * (w + gapX), pad + row * (h + gapY), w, h, {
        fillPaints: [], cornerRadius: 0, frameMaskDisabled: true, isSymbolPublishable: true,
        variantPropSpecs: props.map(([p], j) => ({ propDefId: defs[j].id, value: values[p] })),
      });
      draw(sym, values);
      symbols[vname] = sym;
    });
    log(`${name}: ${combos.length} variants (${props.map(([p, v]) => `${p} ${v.length}`).join(' x ')}), sorted by ${colProp} in ${opts.rows ? 'rows' : 'columns'}`);
    return { set, symbols };
  };
  // The instance's own name is an override on the symbol root (as Figma saves a renamed
  // instance); the node name alone is replaced by the component's on import.
  const nameOverride = (name, symbol) => ({ name, guidPath: { guids: [symbol.guid] } });
  // A DesignCompose meter (dials & gauges plugin): shared plugin data "designcompose",
  // key "vsw-meter-data". harry moves the node by MeterValue (0..100) at display-list time:
  // no tree rebuild, no layout. kind: 'progressBarData' (width = value% * endX) or
  // 'progressMarkerData' (x = startX + value% * (endX - startX), in the parent).
  const meter = (node, kind, fields) => {
    node.pluginData = (node.pluginData || []).concat([{
      pluginID: '', key: 'designcompose-vsw-meter-data',
      value: JSON.stringify({ [kind]: Object.assign({ enabled: true, discrete: false, discreteValue: 0, draggable: false, vertical: false, startX: 0, endX: 0, startY: 0, endY: 0 }, fields) }),
    }]);
    return node;
  };
  // Constraints for a part that covers its component (background, pressed layer): it follows
  // the component's size (a button hugging its label). SCALE, not STRETCH: for LEFT_RIGHT the
  // importer keeps the design's width as min_width (the part can grow, not shrink); for SCALE
  // it sets min_width auto (dc_figma_import transform_flexbox.rs). For a part at x = 0 and full
  // width SCALE is exactly a stretch; a smaller part (an icon) takes CENTER instead.
  const FILL = { horizontalConstraint: 'SCALE', verticalConstraint: 'SCALE' };
  // A deep copy of the design's node `src` (an icon: a frame with its vectors) under `parent`
  // at x, y; `edit(node)` runs on every copied node (recolouring). Vector paths stay
  // references into the file's blobs, so a copy lives in the same file as its original.
  const copyNode = (src, parent, x, y, edit) => {
    const dup = JSON.parse(JSON.stringify(src));
    for (const f of ['guid', 'parentIndex', 'phase', 'transform', 'size', 'type', 'name']) delete dup[f];
    const n = base(parent, src.type, src.name, x, y, src.size.x, src.size.y, dup);
    if (edit) edit(n);
    for (const c of kids(key(src.guid))) copyNode(c, n, c.transform.m02, c.transform.m12, edit);
    return n;
  };
  // A ring sector: an ellipse with arcData (DesignCompose draws it as an arc). Figma angles:
  // 0 = +x, positive clockwise (y down), radians.
  const arc = (parent, name, cx, cy, r, thick, a0, a1, rgb, extra) => ellipse(parent, name, cx - r, cy - r, 2 * r, 2 * r, rgb, Object.assign({ arcData: { startingAngle: a0, endingAngle: a1, innerRadius: (r - thick) / r } }, extra || {}));
  // A resized instance (extra.size other than the component's) carries the size also as an
  // override on its root, as Figma saves it (ClusterRedesign.fig: { guidPath, size }); without
  // it Figma resets the instance to the component's size on import.
  const instance = (parent, name, x, y, symbol, extra, overrides = []) => {
    const size = (extra && extra.size) || symbol.size;
    const root = nameOverride(name, symbol);
    if (size.x !== symbol.size.x || size.y !== symbol.size.y) root.size = { x: size.x, y: size.y };
    return instance0(parent, name, x, y, symbol, extra, [root, ...overrides]);
  };
  const instance0 = (parent, name, x, y, symbol, extra, overrides) => base(parent, 'INSTANCE', name, x, y, symbol.size.x, symbol.size.y, Object.assign({
    symbolData: { symbolID: symbol.guid, symbolOverrides: overrides, uniformScaleFactor: 1 }, fillPaints: [], frameMaskDisabled: true,
    // Without it Figma takes the name for an automatic one and replaces it with the component's
    // on import; the app finds instances by these names.
    hasBeenManuallyRenamed: true,
  }, extra || {}));
  // A content component that hugs an inner box of a fixed size: the component (a variant
  // symbol) becomes an auto layout that hugs its one child, the box, so an instance of it, and
  // the instance a sheet swaps it into (#sheet/content), take its size; the sheet's card hugs
  // that in turn. Draw the content into the returned box (absolute positions, as before).
  const hugBox = (symbol, w, h) => {
    Object.assign(symbol, {
      stackMode: 'VERTICAL', stackPrimarySizing: 'RESIZE_TO_FIT', stackCounterSizing: 'RESIZE_TO_FIT',
      stackWidth: 'RESIZE_TO_FIT', stackHeight: 'RESIZE_TO_FIT', size: { x: w, y: h },
    });
    return frame(symbol, 'box', 0, 0, w, h);
  };
  // The first node named `name` below `top` (breadth first), or an error.
  const under = (top, name) => {
    const stack = [...kids(key(top.guid))];
    while (stack.length) {
      const n = stack.shift();
      if (n.name === name) return n;
      stack.push(...kids(key(n.guid)));
    }
    throw new Error(`${top.name}: no ${name}`);
  };
  // A sheet on the screen: an instance of the library's #sheet (ui-lib's components-patch.js) in the
  // variant `sheetVariant` (a layout, closed or open), its content slot swapped for
  // `contentVariant` (a component of the app's design, Figma's instance swap) and its parts
  // renamed for this sheet: #<prefix>/scrim, /card, /content, so the app drives each sheet by
  // its own names (three sheets share the one component). `overrides` are more overrides
  // inside the content (a nested path: [the content instance's guid, the node's guid]).
  const sheetInstance = (parent, name, x, y, sheetVariant, contentVariant, overrides = [], extra) => {
    const prefix = name.startsWith('#') ? name : '#' + name;
    const scrim = under(sheetVariant, '#sheet/scrim'), card = under(sheetVariant, '#sheet/card'), content = under(card, '#sheet/content');
    return instance(parent, name, x, y, sheetVariant, extra, [
      { guidPath: { guids: [scrim.guid] }, name: `${prefix}/scrim` },
      { guidPath: { guids: [card.guid] }, name: `${prefix}/card` },
      // the slot takes the swapped component's size because the content components hug
      // (hugBox): a size override on a nested instance is not honoured by Figma's import
      { guidPath: { guids: [content.guid] }, name: `${prefix}/content`, overriddenSymbolID: contentVariant.guid },
      ...overrides.map(o => ({ ...o, guidPath: { guids: [content.guid, ...o.guidPath.guids] } })),
    ]);
  };
  // A scrollbar beside a list: the library's #scrollbar is drawn as high as the screen (its
  // track grows from the bottom, its thumb moves along it: meters the code sets for the list's
  // height, ui-lib slot.rs scrollbar()). Its slot is a frame as high as the list that clips, the
  // preview's bottom at the slot's bottom; the code puts the clone there too.
  const scrollbarSlot = (parent, name, x, y, h) => {
    const set = nodes.find(n => n.isStateGroup && n.name === '#scrollbar');
    const v = set && kids(key(set.guid))[0];
    if (!v) throw new Error('#scrollbar missing: run components-patch.js first');
    const f = frame(parent, name, x, y, v.size.x, h, { frameMaskDisabled: false });
    instance(f, name + '/preview', 0, h - v.size.y, v);
    return f;
  };
  return { nodes, kids, log, base, frame, rect, ellipse, text, stroke, shadow, solid, chevron, plus, componentSet, instance, nameOverride, meter, FILL, copyNode, arc, at, under, sheetInstance, scrollbarSlot, hugBox, newGuid: guid, count: () => local };
}

module.exports = { key, C, FONT, authoring };
