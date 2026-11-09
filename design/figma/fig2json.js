#!/usr/bin/env node
// fig2json: a .fig file as the Figma REST API returns it (GET /v1/files/<key>?geometry=paths),
// without Figma: the input of dcf-fetch --local (tools/dcf), so a generated .fig becomes the app's
// .dcf with no import into Figma, no file key and no token.
//
//   node fig2json.js <in.fig> <out-dir>      # writes <out-dir>/file.json and <out-dir>/images/<ref>
//
// What Figma computes on import and the REST API returns, done here:
//   - instances expanded: the component's subtree with the instance's overrides applied (a
//     partial node per guid path), ids "I<instance>;<node>" as Figma gives them, nested swaps;
//   - absoluteBoundingBox from the chain of transforms;
//   - the children of an instance resized against its component by their constraints (an
//     auto layout parent is left to the layout: harry lays it out with taffy);
//   - paints, gradients (handle positions), effects, text styles, vector paths in REST names.
// What it does not do and does not need to: lay out auto layout or text (harry does it from the
// flexbox style and its text measure). The kiwi reader comes with it (third_party/kiwi-schema).
const fs = require('fs');
const os = require('os');
const path = require('path');
const zlib = require('zlib');
const { execFileSync } = require('child_process');

const kiwi = require(path.join(__dirname, 'third_party/kiwi-schema/kiwi.js'));

// ---------------------------------------------------------------- reading the .fig

function readFig(file) {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), 'fig2json-'));
  execFileSync('unzip', ['-q', '-o', file, '-d', work]);
  const buf = fs.readFileSync(path.join(work, 'canvas.fig'));
  if (buf.slice(0, 8).toString() !== 'fig-kiwi') throw new Error(`${file}: not a fig-kiwi file`);
  let off = 12; const chunks = [];
  while (off < buf.length) { const len = buf.readUInt32LE(off); off += 4; chunks.push(buf.slice(off, off + len)); off += len; }
  const inflate = b => (b[0] === 0x28 && b[1] === 0xb5 && b[2] === 0x2f && b[3] === 0xfd) ? zlib.zstdDecompressSync(b)
    : (() => { try { return zlib.inflateRawSync(b); } catch (e) { return zlib.inflateSync(b); } })();
  const schema = kiwi.compileSchema(kiwi.decodeBinarySchema(inflate(chunks[0])));
  const msg = schema.decodeMessage(inflate(chunks[1]));
  const meta = fs.existsSync(path.join(work, 'meta.json')) ? JSON.parse(fs.readFileSync(path.join(work, 'meta.json'), 'utf8')) : {};
  return { msg, work, meta };
}

// ---------------------------------------------------------------- helpers

const key = g => `${g.sessionID}:${g.localID}`;
const hex = bytes => Buffer.from(bytes instanceof Uint8Array ? bytes : Uint8Array.from(Object.values(bytes))).toString('hex');
// Figma's REST API prints a float with 6 significant digits; kiwi keeps the float32
const num = v => (typeof v === 'number' && isFinite(v) && v !== 0) ? Number(v.toPrecision(6)) : v;
// 2x3 affine matrices [a c e; b d f] as {m00 m01 m02 m10 m11 m12}
const ident = { m00: 1, m01: 0, m02: 0, m10: 0, m11: 1, m12: 0 };
const mat = t => ({ m00: t?.m00 ?? 1, m01: t?.m01 ?? 0, m02: t?.m02 ?? 0, m10: t?.m10 ?? 0, m11: t?.m11 ?? 1, m12: t?.m12 ?? 0 });
const mul = (p, c) => ({
  m00: p.m00 * c.m00 + p.m01 * c.m10, m01: p.m00 * c.m01 + p.m01 * c.m11, m02: p.m00 * c.m02 + p.m01 * c.m12 + p.m02,
  m10: p.m10 * c.m00 + p.m11 * c.m10, m11: p.m10 * c.m01 + p.m11 * c.m11, m12: p.m10 * c.m02 + p.m11 * c.m12 + p.m12,
});
const inv = m => { const det = m.m00 * m.m11 - m.m01 * m.m10 || 1e-9;
  return { m00: m.m11 / det, m01: -m.m01 / det, m02: (m.m01 * m.m12 - m.m11 * m.m02) / det, m10: -m.m10 / det, m11: m.m00 / det, m12: (m.m10 * m.m02 - m.m00 * m.m12) / det }; };
const apply = (m, x, y) => ({ x: m.m00 * x + m.m01 * y + m.m02, y: m.m10 * x + m.m11 * y + m.m12 });
const restTransform = m => [[num(m.m00), num(m.m01), num(m.m02)], [num(m.m10), num(m.m11), num(m.m12)]];
const bbox = (m, w, h) => {
  const ps = [apply(m, 0, 0), apply(m, w, 0), apply(m, 0, h), apply(m, w, h)];
  const xs = ps.map(p => p.x), ys = ps.map(p => p.y);
  const x = Math.min(...xs), y = Math.min(...ys);
  return { x: num(x), y: num(y), width: num(Math.max(...xs) - x), height: num(Math.max(...ys) - y) };
};

// the path commands of a geometry blob: 1 byte command, float32 coordinates
function pathOf(blobs, index) {
  const b = blobs[index]; if (!b || !b.bytes) return '';
  const bytes = b.bytes instanceof Uint8Array ? b.bytes : Uint8Array.from(b.bytes);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let i = 0; const out = [];
  const f = () => { const v = view.getFloat32(i, true); i += 4; return num(v); };
  while (i < bytes.length) {
    const cmd = bytes[i++];
    if (cmd === 0) out.push('Z');
    else if (cmd === 1) out.push(`M ${f()} ${f()}`);
    else if (cmd === 2) out.push(`L ${f()} ${f()}`);
    else if (cmd === 3) out.push(`Q ${f()} ${f()} ${f()} ${f()}`);
    else if (cmd === 4) out.push(`C ${f()} ${f()} ${f()} ${f()} ${f()} ${f()}`);
    else break;
  }
  return out.join(' ');
}

// ---------------------------------------------------------------- shape paths as Figma writes them
// REST returns every shape's outline (geometry=paths); kiwi keeps it only for vectors. These are
// the paths Figma's import produces, checked number by number against the import of v52.
const P = v => Number(Number(v).toPrecision(6));
const K = 0.5522847498;   // 4/3 tan(pi/8): a quarter circle as one cubic

// a rectangle, with per-corner radii [tl, tr, br, bl] clamped to half the shorter side
function rectPath(w, h, radii) {
  const m = Math.min(w, h) / 2;
  const [tl, tr, br, bl] = radii.map(r => Math.min(Math.max(r || 0, 0), m));
  if (!tl && !tr && !br && !bl) return `M0 0L${P(w)} 0L${P(w)} ${P(h)}L0 ${P(h)}L0 0Z`;
  const c = (x0, y0, x1, y1, x2, y2) => `C${P(x0)} ${P(y0)} ${P(x1)} ${P(y1)} ${P(x2)} ${P(y2)}`;
  return `M0 ${P(tl)}` + c(0, tl - tl * K, tl - tl * K, 0, tl, 0) + `L${P(w - tr)} 0` +
    c(w - tr + tr * K, 0, w, tr - tr * K, w, tr) + `L${P(w)} ${P(h - br)}` +
    c(w, h - br + br * K, w - br + br * K, h, w - br, h) + `L${P(bl)} ${P(h)}` +
    c(bl - bl * K, h, 0, h - bl + bl * K, 0, h - bl) + `L0 ${P(tl)}Z`;
}

// an ellipse or an arc of one: the sweep cut at the quadrant boundaries, one cubic per piece
// (Figma starts a full ellipse at angle 0); with an inner radius a ring segment, without one a
// line to the centre
function arcPath(w, h, arc) {
  const rx = w / 2, ry = h / 2, cx = rx, cy = ry;
  let a0 = arc ? arc.startingAngle ?? 0 : 0, a1 = arc ? arc.endingAngle ?? 2 * Math.PI : 2 * Math.PI;
  const inner = arc ? arc.innerRadius ?? 0 : 0;
  const full = a1 - a0 >= 2 * Math.PI - 1e-6;
  const pt = (a, s) => [cx + rx * s * Math.cos(a), cy + ry * s * Math.sin(a)];
  const pieces = (from, to) => {   // the angles from..to cut at multiples of pi/2, either direction
    const out = [from], dir = to > from ? 1 : -1, q = Math.PI / 2;
    let b = dir > 0 ? Math.floor(from / q + 1e-9) * q + q : Math.ceil(from / q - 1e-9) * q - q;
    // a boundary closer than 1e-4 to the end is the end (2pi as a float32 is not Math.PI * 2)
    while (dir > 0 ? b < to - 1e-4 : b > to + 1e-4) { out.push(b); b += dir * q; }
    out.push(to); return out;
  };
  const curve = (angles, s) => {
    let d = '';
    for (let i = 1; i < angles.length; i++) {
      const p = angles[i - 1], q = angles[i], k = 4 / 3 * Math.tan((q - p) / 4);
      const [x0, y0] = pt(p, s), [x3, y3] = pt(q, s);
      const x1 = x0 - k * rx * s * Math.sin(p), y1 = y0 + k * ry * s * Math.cos(p);
      const x2 = x3 + k * rx * s * Math.sin(q), y2 = y3 - k * ry * s * Math.cos(q);
      d += `C${P(x1)} ${P(y1)} ${P(x2)} ${P(y2)} ${P(x3)} ${P(y3)}`;
    }
    return d;
  };
  const [sx, sy] = pt(a0, 1);
  let d = `M${P(sx)} ${P(sy)}` + curve(pieces(a0, a1), 1);
  if (full && !inner) return d + 'Z';
  if (full) { const [ix, iy] = pt(a1, inner); return d + `ZM${P(ix)} ${P(iy)}` + curve(pieces(a1, a0), inner) + 'Z'; }
  if (inner) { const [ix, iy] = pt(a1, inner); d += `L${P(ix)} ${P(iy)}` + curve(pieces(a1, a0), inner); }
  else d += `L${P(cx)} ${P(cy)}`;
  return d + `L${P(sx)} ${P(sy)}Z`;
}

// a path of M/L/C/Z commands drawn the other way round (the hole of a stroke ring)
function reversePath(d) {
  const segs = []; let start = null, cur = null;
  for (const m of d.matchAll(/([MLCZ])([^MLCZ]*)/g)) {
    const v = m[2].trim() ? m[2].trim().split(/[ ,]+/).map(Number) : [];
    if (m[1] === 'M') { start = cur = [v[0], v[1]]; segs.push({ move: true, to: cur }); }
    else if (m[1] === 'L') { segs.push({ from: cur, to: [v[0], v[1]] }); cur = [v[0], v[1]]; }
    else if (m[1] === 'C') { segs.push({ from: cur, c1: [v[0], v[1]], c2: [v[2], v[3]], to: [v[4], v[5]] }); cur = [v[4], v[5]]; }
    else if (m[1] === 'Z') { if (cur && start && (cur[0] !== start[0] || cur[1] !== start[1])) segs.push({ from: cur, to: start }); cur = start; }
  }
  const body = segs.filter(s => !s.move).reverse();
  if (!body.length) return '';
  let out = `M${body[0].to[0]} ${body[0].to[1]}`;
  for (const s of body) out += s.c1 ? `C${s.c2[0]} ${s.c2[1]} ${s.c1[0]} ${s.c1[1]} ${s.from[0]} ${s.from[1]}` : `L${s.from[0]} ${s.from[1]}`;
  return out + 'Z';
}

// the outline of a stroke as Figma gives it (strokeGeometry): the ring from the shape's outline
// moved in by the stroke weight to it moved out by it; harry fills the ring and clips it by the
// stroke's alignment (inside: to the fill, outside: away from it)
function rectStroke(w, h, radii, sw) {
  const outer = rectPath(w + 2 * sw, h + 2 * sw, radii.map(r => r ? r + sw : 0)).replace(/(-?[\d.e-]+) (-?[\d.e-]+)/g, (_, x, y) => `${P(+x - sw)} ${P(+y - sw)}`);
  const iw = w - 2 * sw, ih = h - 2 * sw;
  if (iw <= 0 || ih <= 0) return outer;
  const inner = rectPath(iw, ih, radii.map(r => Math.max(r - sw, 0))).replace(/(-?[\d.e-]+) (-?[\d.e-]+)/g, (_, x, y) => `${P(+x + sw)} ${P(+y + sw)}`);
  return outer + reversePath(inner);
}
function ellipseStroke(w, h, sw) {
  const outer = arcPath(w + 2 * sw, h + 2 * sw, null).replace(/(-?[\d.e-]+) (-?[\d.e-]+)/g, (_, x, y) => `${P(+x - sw)} ${P(+y - sw)}`);
  if (w - 2 * sw <= 0 || h - 2 * sw <= 0) return outer;
  const inner = arcPath(w - 2 * sw, h - 2 * sw, null).replace(/(-?[\d.e-]+) (-?[\d.e-]+)/g, (_, x, y) => `${P(+x + sw)} ${P(+y + sw)}`);
  return outer + reversePath(inner);
}

// ---------------------------------------------------------------- properties, kiwi -> REST

const color = c => ({ r: c?.r ?? 0, g: c?.g ?? 0, b: c?.b ?? 0, a: c?.a ?? 1 });
const BLEND = { NORMAL: 'NORMAL', PASS_THROUGH: 'PASS_THROUGH', MULTIPLY: 'MULTIPLY', SCREEN: 'SCREEN', OVERLAY: 'OVERLAY', DARKEN: 'DARKEN', LIGHTEN: 'LIGHTEN' };

function paint(p, images) {
  const base = { visible: p.visible !== false, opacity: p.opacity ?? 1, blendMode: BLEND[p.blendMode] || 'NORMAL' };
  switch (p.type) {
    case 'SOLID': return { ...base, type: 'SOLID', color: color(p.color) };
    case 'GRADIENT_LINEAR': case 'GRADIENT_RADIAL': case 'GRADIENT_ANGULAR': case 'GRADIENT_DIAMOND': {
      // kiwi keeps the matrix from the node's unit square to the gradient's; REST the three
      // handles in the unit square: the inverse applied to the gradient's own points
      const m = inv(mat(p.transform));
      const handles = [apply(m, 0, 0.5), apply(m, 1, 0.5), apply(m, 0, 1)].map(v => ({ x: num(v.x), y: num(v.y) }));
      return { ...base, type: p.type, gradientHandlePositions: handles,
        gradientStops: (p.stops || []).map(s => ({ position: s.position, color: color(s.color) })) };
    }
    case 'IMAGE': {
      const ref = p.image && p.image.hash ? hex(p.image.hash) : null;
      if (ref) images.add(ref);
      return { ...base, type: 'IMAGE', scaleMode: p.imageScaleMode || 'FILL', imageRef: ref };
    }
    default: return null;
  }
}
const paints = (list, images) => (list || []).map(p => paint(p, images)).filter(Boolean);

const effect = e => ({ type: e.type, visible: e.visible !== false, radius: e.radius ?? 0, color: color(e.color),
  blendMode: BLEND[e.blendMode] || 'NORMAL', offset: { x: e.offset?.x ?? 0, y: e.offset?.y ?? 0 }, spread: e.spread ?? 0 });

const H = { MIN: 'LEFT', MAX: 'RIGHT', CENTER: 'CENTER', STRETCH: 'LEFT_RIGHT', SCALE: 'SCALE' };
const V = { MIN: 'TOP', MAX: 'BOTTOM', CENTER: 'CENTER', STRETCH: 'TOP_BOTTOM', SCALE: 'SCALE' };
const constraints = n => ({ horizontal: H[n.horizontalConstraint] || 'LEFT', vertical: V[n.verticalConstraint] || 'TOP' });

const ALIGN = { MIN: 'MIN', CENTER: 'CENTER', MAX: 'MAX', SPACE_EVENLY: 'SPACE_BETWEEN', SPACE_BETWEEN: 'SPACE_BETWEEN', BASELINE: 'BASELINE' };
const WEIGHT = { Thin: 100, 'Extra Light': 200, ExtraLight: 200, Light: 300, Regular: 400, Medium: 500, 'Semi Bold': 600, SemiBold: 600, Bold: 700, 'Extra Bold': 800, ExtraBold: 800, Black: 900 };

// layout of a frame (auto layout) and of a node in its parent
function frameCommon(n, parent) {
  const horizontal = n.stackMode === 'HORIZONTAL', auto = n.stackMode === 'HORIZONTAL' || n.stackMode === 'VERTICAL';
  const parentAuto = parent && (parent.stackMode === 'HORIZONTAL' || parent.stackMode === 'VERTICAL');
  const parentHorizontal = parent && parent.stackMode === 'HORIZONTAL';
  const absolute = n.stackPositioning === 'ABSOLUTE';
  const grow = n.stackChildPrimaryGrow ?? 0, stretch = n.stackChildAlignSelf === 'STRETCH';
  // FILL along the parent's axis by grow, across it by stretch; HUG when this frame fits its
  // content. kiwi leaves out the default: an absent primary sizing hugs, an absent counter
  // sizing is fixed (Figma's import of v52 reports exactly that). A text hugs by its auto resize.
  const fits = v => typeof v === 'string' && v.startsWith('RESIZE_TO_FIT');
  const primaryHug = auto && (n.stackPrimarySizing === undefined || fits(n.stackPrimarySizing));
  const counterHug = auto && fits(n.stackCounterSizing);
  // (a text reports its sizing only to an auto layout parent; elsewhere it is FIXED)
  const textHug = (axisHorizontal) => parentAuto && n.type === 'TEXT' && (n.textAutoResize === 'WIDTH_AND_HEIGHT' || (!axisHorizontal && n.textAutoResize === 'HEIGHT'));
  const sizing = (axisHorizontal) => {
    if (parentAuto && !absolute) {
      if (axisHorizontal === parentHorizontal && grow > 0) return 'FILL';
      if (axisHorizontal !== parentHorizontal && stretch) return 'FILL';
    }
    if (auto && ((axisHorizontal === horizontal && primaryHug) || (axisHorizontal !== horizontal && counterHug))) return 'HUG';
    if (textHug(axisHorizontal)) return 'HUG';
    return 'FIXED';
  };
  const out = {
    constraints: constraints(n),
    clipsContent: n.frameMaskDisabled !== true,
    layoutMode: auto ? n.stackMode : 'NONE',
    layoutWrap: n.stackWrap === 'WRAP' ? 'WRAP' : 'NO_WRAP',
    primaryAxisSizingMode: primaryHug ? 'AUTO' : 'FIXED',
    counterAxisSizingMode: counterHug ? 'AUTO' : 'FIXED',
    primaryAxisAlignItems: ALIGN[n.stackPrimaryAlignItems] || 'MIN',
    counterAxisAlignItems: ALIGN[n.stackCounterAlignItems] || 'MIN',
    paddingLeft: n.stackHorizontalPadding ?? 0,
    paddingRight: n.stackPaddingRight ?? n.stackHorizontalPadding ?? 0,
    paddingTop: n.stackVerticalPadding ?? 0,
    paddingBottom: n.stackPaddingBottom ?? n.stackVerticalPadding ?? 0,
    itemSpacing: n.stackSpacing ?? 0,
    layoutGrow: grow,
    layoutSizingHorizontal: sizing(true),
    layoutSizingVertical: sizing(false),
    layoutPositioning: absolute ? 'ABSOLUTE' : 'AUTO',
  };
  if (parentAuto) out.layoutAlign = stretch ? 'STRETCH' : 'INHERIT';
  return out;
}
function vectorCommon(n, parent) {
  const f = frameCommon(n, parent);
  return { constraints: f.constraints, layoutGrow: f.layoutGrow, layoutSizingHorizontal: f.layoutSizingHorizontal,
    layoutSizingVertical: f.layoutSizingVertical, layoutPositioning: f.layoutPositioning, ...(f.layoutAlign ? { layoutAlign: f.layoutAlign } : {}) };
}

function textStyle(n, images) {
  const size = n.fontSize ?? 12;
  const lh = n.lineHeight || { units: 'PERCENT', value: 100 };
  const lineHeightPx = lh.units === 'PIXELS' ? lh.value : lh.units === 'RAW' ? size * lh.value : size * 1.21 * (lh.value / 100);
  const ls = n.letterSpacing || { units: 'PERCENT', value: 0 };
  const style = (n.fontName && n.fontName.style) || 'Regular';
  return {
    fontFamily: (n.fontName && n.fontName.family) || 'Inter',
    fontPostScriptName: (n.fontName && n.fontName.postscript) || null,
    italic: /Italic/.test(style),
    fontWeight: WEIGHT[style.replace(/ ?Italic/, '') || 'Regular'] || 400,
    fontSize: size,
    textAutoResize: n.textAutoResize || 'NONE',
    textTruncation: n.textTruncation || 'DISABLED',
    maxLines: n.maxLines ?? -1,
    textAlignHorizontal: n.textAlignHorizontal || 'LEFT',
    textAlignVertical: n.textAlignVertical || 'TOP',
    letterSpacing: ls.units === 'PIXELS' ? ls.value : size * (ls.value / 100),
    fills: paints(n.fillPaints, images),
    lineHeightPx: num(lineHeightPx),
    // kiwi PERCENT is Figma's "Auto" family: a percentage of the font's own line height (REST
    // INTRINSIC_%); RAW a multiple of the font size (FONT_SIZE_%); PIXELS as it is
    lineHeightPercentFontSize: lh.units === 'PERCENT' ? lh.value : num(100 * lineHeightPx / size),
    lineHeightUnit: lh.units === 'PIXELS' ? 'PIXELS' : lh.units === 'PERCENT' ? 'INTRINSIC_%' : 'FONT_SIZE_%',
  };
}

const stroked = r => r.strokeWeight > 0 && r.strokes.some(p => p.visible);

// ---------------------------------------------------------------- the tree

function convert(msg, name) {
  const nodes = msg.nodeChanges, blobs = msg.blobs || [];
  const byKey = new Map(nodes.map(n => [key(n.guid), n]));
  const children = new Map();
  for (const n of nodes) if (n.parentIndex) { const k = key(n.parentIndex.guid); if (!children.has(k)) children.set(k, []); children.get(k).push(n); }
  for (const list of children.values()) list.sort((a, b) => (a.parentIndex.position < b.parentIndex.position ? -1 : a.parentIndex.position > b.parentIndex.position ? 1 : 0));
  const kids = n => children.get(key(n.guid)) || [];
  const images = new Set();
  const components = {}, componentSets = {};
  // a component set is a FRAME with isStateGroup in kiwi, its variants are SYMBOLs inside it
  const sets = new Set(nodes.filter(n => n.isStateGroup).map(n => key(n.guid)));
  for (const k of sets) componentSets[k] = { key: k, name: byKey.get(k).name || '', description: '' };
  for (const n of nodes) {
    if (n.type !== 'SYMBOL') continue;
    const k = key(n.guid);
    components[k] = { key: k, name: n.name || '', description: '', componentSetId: n.parentIndex && sets.has(key(n.parentIndex.guid)) ? key(n.parentIndex.guid) : '' };
  }

  // the overrides of an instance, split by the first guid of their path: Map<guid, partial[]>
  const scopeOf = (inst, outer) => {
    const scope = new Map();
    const add = (g, o) => { const k = key(g); if (!scope.has(k)) scope.set(k, []); scope.get(k).push(o); };
    const take = (o) => { const gs = (o.guidPath && o.guidPath.guids) || []; if (gs.length === 1) add(gs[0], o); else if (gs.length > 1) add(gs[0], { nested: { ...o, guidPath: { guids: gs.slice(1) } } }); };
    for (const o of (inst.symbolData && inst.symbolData.symbolOverrides) || []) take(o);
    for (const x of outer) if (x.nested) take(x.nested);
    return scope;
  };
  // a node with the overrides aimed at it merged in (an override is a partial node)
  const merged = (n, overrides) => {
    if (!overrides.length) return n;
    const m = { ...n };
    for (const o of overrides) for (const [k, v] of Object.entries(o)) if (k !== 'guidPath' && k !== 'nested') m[k] = v;
    return m;
  };

  // resizes a child of a component placed in an instance of another size, by its constraints
  const resized = (n, from, to) => {
    if (!from || (from.x === to.x && from.y === to.y)) return n;
    const t = mat(n.transform), w = n.size?.x ?? 0, h = n.size?.y ?? 0;
    const axis = (c, pos, len, a, b) => {
      switch (c) {
        case 'MAX': return [pos + (b - a), len];
        case 'CENTER': return [pos + (b - a) / 2, len];
        case 'STRETCH': return [pos, len + (b - a)];
        case 'SCALE': return [pos * b / (a || 1), len * b / (a || 1)];
        default: return [pos, len];
      }
    };
    const [x, nw] = axis(n.horizontalConstraint, t.m02, w, from.x, to.x);
    const [y, nh] = axis(n.verticalConstraint, t.m12, h, from.y, to.y);
    return { ...n, transform: { ...t, m02: x, m12: y }, size: { x: nw, y: nh } };
  };

  // one node: n the kiwi node (its overrides merged), id its REST id, world the transform of its
  // parent to the page, parent the kiwi parent (for the layout of a child), scope the overrides
  // of the enclosing instance by guid (one table for the whole subtree of its component), prefix
  // the id prefix inside that instance ("I<instance id>")
  const out = (n, id, world, parent, scope, prefix) => {
    const m = mul(world, mat(n.transform));
    const w = n.size?.x ?? 0, h = n.size?.y ?? 0;
    const r = {
      id, name: n.name || '', visible: n.visible !== false, opacity: n.opacity ?? 1,
      blendMode: BLEND[n.blendMode] || 'PASS_THROUGH',
      fills: paints(n.fillPaints, images), strokes: paints(n.strokePaints, images),
      strokeWeight: n.strokeWeight ?? 0, strokeAlign: n.strokeAlign || 'INSIDE',
      effects: (n.effects || []).map(effect),
      absoluteBoundingBox: bbox(m, w, h),
      relativeTransform: restTransform(mat(n.transform)),
      size: { x: w, y: h },
    };
    // shared plugin data (pluginID ""): kiwi keys it "<namespace>-<key>", REST by namespace and
    // key; DesignCompose's meters, reactions, frame extras live under "designcompose"
    const shared = (n.pluginData || []).filter(d => !d.pluginID && d.key && d.key.includes('-'));
    if (shared.length) {
      r.sharedPluginData = {};
      for (const d of shared) { const i = d.key.indexOf('-'), ns = d.key.slice(0, i); (r.sharedPluginData[ns] ||= {})[d.key.slice(i + 1)] = d.value; }
    }
    if (n.cornerRadius) r.cornerRadius = n.cornerRadius;
    const radii = [n.rectangleTopLeftCornerRadius, n.rectangleTopRightCornerRadius, n.rectangleBottomRightCornerRadius, n.rectangleBottomLeftCornerRadius];
    if (radii.some(v => v !== undefined) && new Set(radii.map(v => v ?? n.cornerRadius ?? 0)).size > 1) r.rectangleCornerRadii = radii.map(v => v ?? n.cornerRadius ?? 0);
    if (n.strokeCap && n.strokeCap !== 'NONE') r.strokeCap = n.strokeCap;
    // an empty blob is no outline (Figma leaves the entry out)
    if (n.fillGeometry) r.fillGeometry = n.fillGeometry.map(g => ({ path: pathOf(blobs, g.commandsBlob), windingRule: g.windingRule === 'ODD' || g.windingRule === 'EVENODD' ? 'EVENODD' : 'NONZERO' })).filter(g => g.path);
    if (n.strokeGeometry) r.strokeGeometry = n.strokeGeometry.map(g => ({ path: pathOf(blobs, g.commandsBlob), windingRule: g.windingRule === 'ODD' || g.windingRule === 'EVENODD' ? 'EVENODD' : 'NONZERO' })).filter(g => g.path);

    const below = (list, childScope, childPrefix, sizes) => list.map(c => {
      const ck = key(c.guid);
      let cn = merged(c, (childScope.get(ck) || []).filter(o => !o.nested));
      if (sizes) cn = resized(cn, sizes.from, sizes.to);
      const cid = childPrefix ? `${childPrefix};${ck}` : ck;
      return out(cn, cid, m, n, childScope, childPrefix);
    });
    const none = new Map();

    switch (n.type) {
      case 'DOCUMENT': return { id, name: r.name, type: 'DOCUMENT', children: kids(n).map(c => out(c, key(c.guid), ident, n, none, '')) };
      case 'CANVAS': return { id, name: r.name, type: 'CANVAS', backgroundColor: color(n.backgroundColor), prototypeStartNodeID: null,
        children: kids(n).filter(c => c.type !== 'CANVAS').map(c => out(c, key(c.guid), ident, n, none, '')) };
      case 'TEXT': {
        const chars = (n.textData && n.textData.characters) || '';
        return { ...r, type: 'TEXT', ...vectorCommon(n, parent), characters: chars, style: textStyle(n, images), characterStyleOverrides: [], styleOverrideTable: {} };
      }
      case 'ROUNDED_RECTANGLE': case 'RECTANGLE': {
        const radii = r.rectangleCornerRadii || [0, 0, 0, 0].map(() => n.cornerRadius || 0);
        // the outline of the fill only when there is a fill (Figma leaves it out otherwise)
        if (!r.fillGeometry && (r.fills.length || stroked(r))) r.fillGeometry = [{ windingRule: 'NONZERO', path: rectPath(w, h, radii) }];
        if (!r.strokeGeometry && stroked(r)) r.strokeGeometry = [{ windingRule: 'NONZERO', path: rectStroke(w, h, radii, r.strokeWeight) }];
        return { ...r, type: 'RECTANGLE', ...vectorCommon(n, parent) };
      }
      case 'ELLIPSE':
        if (!r.fillGeometry && (r.fills.length || stroked(r))) r.fillGeometry = [{ windingRule: 'NONZERO', path: arcPath(w, h, n.arcData) }];
        if (!r.strokeGeometry && stroked(r) && !(n.arcData && (n.arcData.innerRadius || (n.arcData.endingAngle ?? 2 * Math.PI) - (n.arcData.startingAngle ?? 0) < 2 * Math.PI - 1e-6)))
          r.strokeGeometry = [{ windingRule: 'NONZERO', path: ellipseStroke(w, h, r.strokeWeight) }];
        return { ...r, type: 'ELLIPSE', ...vectorCommon(n, parent),
        arcData: { startingAngle: n.arcData?.startingAngle ?? 0, endingAngle: n.arcData?.endingAngle ?? 2 * Math.PI, innerRadius: n.arcData?.innerRadius ?? 0 } };
      case 'VECTOR': case 'LINE': case 'STAR': case 'REGULAR_POLYGON': return { ...r, type: n.type, ...vectorCommon(n, parent) };
      case 'BOOLEAN_OPERATION': return { ...r, type: 'BOOLEAN_OPERATION', ...vectorCommon(n, parent), booleanOperation: n.booleanOperation || 'UNION',
        children: below(kids(n), scope, prefix, null) };
      case 'INSTANCE': {
        const symbolId = n.overriddenSymbolID || (n.symbolData && n.symbolData.symbolID);
        const symbol = symbolId && byKey.get(key(symbolId));
        // the overrides of this instance, and those of the enclosing instances aimed into it
        const inner = scopeOf(n, scope.get(key(n.guid)) || []);
        // the instance's own name and properties from an override aimed at its component root
        const self = symbol ? (inner.get(key(symbol.guid)) || []).filter(o => !o.nested) : [];
        const nn = self.length ? merged(n, self.filter(o => o.name).map(o => ({ name: o.name }))) : n;
        const base = { ...r, name: nn.name || r.name, type: 'INSTANCE', ...frameCommon(symbol ? { ...symbol, ...pick(n, LAYOUT_CHILD) } : n, parent),
          componentId: symbol ? key(symbol.guid) : '' };
        if (!symbol) return { ...base, children: [] };
        // the instance draws its component's fills unless it overrides them
        if (!n.fillPaints || !n.fillPaints.length) base.fills = paints(symbol.fillPaints, images);
        if (!n.strokePaints || !n.strokePaints.length) base.strokes = paints(symbol.strokePaints, images);
        if (!n.effects || !n.effects.length) base.effects = (symbol.effects || []).map(effect);
        if (base.cornerRadius === undefined && symbol.cornerRadius) base.cornerRadius = symbol.cornerRadius;
        const auto = symbol.stackMode === 'HORIZONTAL' || symbol.stackMode === 'VERTICAL';
        const sizes = auto ? null : { from: symbol.size, to: n.size };
        const instPrefix = `I${id.replace(/^I/, '')}`;
        return { ...base, children: below(kids(symbol), inner, instPrefix, sizes) };
      }
      case 'SYMBOL': return { ...r, type: 'COMPONENT', ...frameCommon(n, parent), children: below(kids(n), scope, prefix, null) };
      case 'FRAME': case 'GROUP': case 'SECTION': default:
        return { ...r, type: n.isStateGroup ? 'COMPONENT_SET' : 'FRAME', ...frameCommon(n, parent),
          children: below(kids(n), scope, prefix, null) };
    }
  };

  const doc = nodes.find(n => n.type === 'DOCUMENT');
  const document = out(doc, '0:0', ident, null, new Map(), '');
  return { file: { name, version: '0', lastModified: new Date(0).toISOString(), components, componentSets, document, branches: [] }, images };
}

// the properties of an instance that place it in its parent (the rest comes from its component)
const LAYOUT_CHILD = ['stackPositioning', 'stackChildPrimaryGrow', 'stackChildAlignSelf', 'horizontalConstraint', 'verticalConstraint', 'size'];
const pick = (n, keys) => Object.fromEntries(keys.filter(k => n[k] !== undefined).map(k => [k, n[k]]));

// ---------------------------------------------------------------- main

if (require.main === module) {
  const [, , IN, OUT] = process.argv;
  if (!IN || !OUT) { console.error('usage: node fig2json.js <in.fig> <out-dir>'); process.exit(2); }
  const { msg, work } = readFig(IN);
  const { file, images } = convert(msg, path.basename(IN, '.fig'));
  fs.mkdirSync(path.join(OUT, 'images'), { recursive: true });
  fs.writeFileSync(path.join(OUT, 'file.json'), JSON.stringify(file));
  let copied = 0;
  for (const ref of images) {
    const src = path.join(work, 'images', ref);
    if (fs.existsSync(src)) { fs.copyFileSync(src, path.join(OUT, 'images', ref)); copied++; }
    else console.warn(`fig2json: image ${ref} is not in the .fig`);
  }
  fs.rmSync(work, { recursive: true, force: true });
  let count = 0; const walk = n => { count++; (n.children || []).forEach(walk); }; walk(file.document);
  console.log(`fig2json: ${count} nodes (instances expanded), ${Object.keys(file.components).length} components, ${Object.keys(file.componentSets).length} sets, ${copied} images -> ${OUT}`);
}

module.exports = { readFig, convert };
