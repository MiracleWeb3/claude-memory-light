/* cml memory map v2 — pure three.js, appended after three.module.js in one module scope.
   All node spheres render as ~8 InstancedMesh draw calls; links are 2 LineSegments;
   the whole brain is ~14 draw calls regardless of size. Layout comes precomputed from Rust. */
(() => {
"use strict";
const DATA = window.CML_GRAPH;
const $ = id => document.getElementById(id);
const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
window.addEventListener("error", e => {
  const l = $("loading");
  if (l && !l.classList.contains("off"))
    l.innerHTML = '<div style="color:#f87171;font:13px ui-monospace,monospace">core failed to start: ' + (e.message || "unknown") + "</div>";
});

const HEX = {
  center: "#ef4444", project: "#f59e0b", session: "#7f1d1d",
  user: "#fb923c", assistant: "#60a5fa", summary: "#94a3b8",
  memory: "#34d399", wiki: "#c084fc", code: "#22d3ee", coderoot: "#0ea5e9"
};
const COL = {}; for (const k in HEX) COL[k] = new Color(HEX[k]);

/* ---------- sound: click-level events only ---------- */
const AC = window.AudioContext ? new window.AudioContext() : null;
let soundOn = localStorage.cmlSound !== "off";
addEventListener("pointerdown", () => { if (AC && AC.state === "suspended") AC.resume(); }, { once: true });
function tone(f, d, t, v, w, s) {
  if (!soundOn || !AC || AC.state === "suspended") return;
  const at = AC.currentTime + (w || 0), o = AC.createOscillator(), g = AC.createGain();
  o.type = t || "sine"; o.frequency.setValueAtTime(f, at);
  if (s) o.frequency.exponentialRampToValueAtTime(s, at + d);
  g.gain.setValueAtTime(0.0001, at);
  g.gain.linearRampToValueAtTime(v || 0.04, at + 0.012);
  g.gain.exponentialRampToValueAtTime(0.0001, at + d);
  o.connect(g); g.connect(AC.destination); o.start(at); o.stop(at + d + 0.05);
}
const sfx = {
  boot:  () => { tone(52, 1.2, "sine", .028); tone(160, .5, "sine", .045, .08, 640); tone(420, .45, "sine", .024, .25, 1500); },
  click: () => { tone(780, .05, "triangle", .04); tone(1170, .07, "triangle", .026, .04); },
  open:  () => { tone(500, .07, "sine", .038, 0, 900); tone(1000, .09, "sine", .026, .06); },
  found: () => { tone(620, .07, "sine", .045); tone(930, .08, "sine", .04, .07); tone(1400, .1, "sine", .03, .14); },
  none:  () => tone(190, .2, "sawtooth", .018, 0, 100),
  up:    () => { tone(900, .05, "sine", .03); tone(600, .07, "sine", .026, .04); },
};

/* ---------- data prep ---------- */
const idOf = x => (typeof x === "object" ? x.id : x);
const byId = new Map(DATA.nodes.map(n => [n.id, n]));
const parentOf = new Map();
for (const l of DATA.links) if (l.kind === "leaf" || l.kind === "tether") parentOf.set(idOf(l.target), idOf(l.source));
const childCount = new Map();
for (const [, p] of parentOf) childCount.set(p, (childCount.get(p) || 0) + 1);
const P = n => new Vector3(n.fx || 0, n.fy || 0, n.fz || 0);
function radiusOf(n) {
  switch (n.group) {
    case "center": return 22;
    case "project": return 11;
    case "coderoot": return 11;
    case "session": return 4 + Math.sqrt(childCount.get(n.id) || 1) * 1.05;
    case "memory": case "wiki": return 4.2;
    case "code": return 3;
    default: return 2.55;
  }
}

/* ---------- renderer / scene ---------- */
const scene = new Scene();
const camera = new PerspectiveCamera(55, innerWidth / innerHeight, 1, 8000);
const renderer = new WebGLRenderer({ antialias: true, alpha: true, powerPreference: "high-performance" });
renderer.setPixelRatio(Math.min(devicePixelRatio || 1, 2));
renderer.setSize(innerWidth, innerHeight);
$("graph").appendChild(renderer.domElement);
scene.add(new AmbientLight(0x584444, 1.15));
const key = new DirectionalLight(0xffe2d1, 1.5); key.position.set(500, 720, 420); scene.add(key);
const rim = new DirectionalLight(0x22d3ee, 0.45); rim.position.set(-640, -180, -520); scene.add(rim);
const under = new DirectionalLight(0xef4444, 0.4); under.position.set(0, -600, 100); scene.add(under);

/* instanced node spheres — one draw call per role */
const sphereGeo = new SphereGeometry(1, 16, 12);
const lowGeo = new SphereGeometry(1, 8, 6);
const groupsOf = {};
for (const n of DATA.nodes) {
  if (n.group === "center") continue;
  (groupsOf[n.group] = groupsOf[n.group] || []).push(n);
}
const instMeshes = [];
const instByGroup = {};
const tmpM = new Matrix4(), tmpQ = new Quaternion(), tmpS = new Vector3(), tmpV = new Vector3();
const tmpC = new Color(), tmpC2 = new Color();
for (const g in groupsOf) {
  const list = groupsOf[g];
  const mat = new MeshPhongMaterial({ shininess: 42, specular: 0x2a1114 });
  const inst = new InstancedMesh(sphereGeo, mat, list.length);
  for (let i = 0; i < list.length; i++) {
    const n = list[i], r = radiusOf(n);
    tmpM.compose(P(n), tmpQ, tmpS.set(r, r, r));
    inst.setMatrixAt(i, tmpM);
    inst.setColorAt(i, COL[g] || COL.summary);
    n._g = g; n._i = i;
  }
  inst.instanceColor.setUsage(DynamicDrawUsage);
  inst.userData = { g, list };
  scene.add(inst);
  instMeshes.push(inst);
  instByGroup[g] = inst;
}
/* the core: shaded sphere + additive glow sprite */
const coreNode = byId.get("center");
const core = new Mesh(sphereGeo, new MeshPhongMaterial({ color: COL.center, emissive: 0x6b1010, shininess: 60 }));
core.scale.setScalar(22); scene.add(core);
(function glow() {
  const cnv = document.createElement("canvas"); cnv.width = cnv.height = 128;
  const c = cnv.getContext("2d"), g = c.createRadialGradient(64, 64, 0, 64, 64, 64);
  g.addColorStop(0, "rgba(255,80,70,.55)"); g.addColorStop(.45, "rgba(239,68,68,.16)"); g.addColorStop(1, "rgba(239,68,68,0)");
  c.fillStyle = g; c.fillRect(0, 0, 128, 128);
  const s = new Sprite(new SpriteMaterial({ map: new CanvasTexture(cnv), blending: AdditiveBlending, depthWrite: false, transparent: true }));
  s.scale.setScalar(150); scene.add(s);
})();

/* links — two merged LineSegments (dim structure, bright spines/wiki/code) */
const LINKC = { spine: new Color("#ef4444"), wikilink: new Color("#34d399"), code: new Color("#22d3ee"), leaf: new Color("#2c161a"), tether: new Color("#123039") };
function buildLines(kinds, opacity) {
  const ls = DATA.links.filter(l => kinds.includes(l.kind));
  const pos = new Float32Array(ls.length * 6), col = new Float32Array(ls.length * 6);
  ls.forEach((l, i) => {
    const a = byId.get(idOf(l.source)), b = byId.get(idOf(l.target));
    pos.set([a.fx, a.fy, a.fz, b.fx, b.fy, b.fz], i * 6);
    const c = LINKC[l.kind] || LINKC.leaf;
    col.set([c.r, c.g, c.b, c.r, c.g, c.b], i * 6);
  });
  const geo = new BufferGeometry();
  geo.setAttribute("position", new BufferAttribute(pos, 3));
  geo.setAttribute("color", new BufferAttribute(col, 3));
  const mesh = new LineSegments(geo, new LineBasicMaterial({ vertexColors: true, transparent: true, opacity }));
  mesh.userData.links = ls; mesh.userData.base = col.slice();
  scene.add(mesh); return mesh;
}
const dimLines = buildLines(["leaf", "tether"], 0.34);
const brightLines = buildLines(["spine", "wikilink", "code"], 0.62);

/* synaptic pulses — one Points cloud riding the bright links */
const pulseLinks = DATA.links.filter(l => l.kind === "spine" || l.kind === "wikilink");
const NP = Math.min(56, pulseLinks.length * 2 || 0);
let pulses = null;
if (NP && !reduced) {
  const pos = new Float32Array(NP * 3), col = new Float32Array(NP * 3), st = [];
  const cnv = document.createElement("canvas"); cnv.width = cnv.height = 32;
  const cx = cnv.getContext("2d"), gr = cx.createRadialGradient(16, 16, 0, 16, 16, 16);
  gr.addColorStop(0, "rgba(255,255,255,1)"); gr.addColorStop(.4, "rgba(255,255,255,.4)"); gr.addColorStop(1, "rgba(255,255,255,0)");
  cx.fillStyle = gr; cx.fillRect(0, 0, 32, 32);
  for (let i = 0; i < NP; i++) {
    const li = (Math.random() * pulseLinks.length) | 0;
    st.push({ li, t: Math.random(), sp: 0.003 + Math.random() * 0.004 });
    const c = LINKC[pulseLinks[li].kind];
    col.set([c.r * 1.4, c.g * 1.4, c.b * 1.4], i * 3);
  }
  const geo = new BufferGeometry();
  geo.setAttribute("position", new BufferAttribute(pos, 3));
  geo.setAttribute("color", new BufferAttribute(col, 3));
  pulses = new Points(geo, new PointsMaterial({
    size: 6, map: new CanvasTexture(cnv), vertexColors: true,
    blending: AdditiveBlending, depthWrite: false, transparent: true,
  }));
  pulses.userData.st = st;
  scene.add(pulses);
}
/* selection ring */
const ring = new Mesh(sphereGeo, new MeshBasicMaterial({ wireframe: true, color: 0xffffff, transparent: true, opacity: 0.45 }));
ring.visible = false; scene.add(ring);

/* ---------- orbit controls, hand-rolled: drag orbits, wheel zooms ---------- */
/* direct-drive orbit: rotation follows the hand 1:1 with a short inertia tail,
   wheel zooms TOWARD the cursor, right/middle drag pans. */
const ctrl = { dist: 800, theta: 0.55, phi: 1.12, target: new Vector3(), vt: 0, vp: 0 };
let interacted = false, dragging = 0, lastX = 0, lastY = 0, moved = 0;
const dom = renderer.domElement;
dom.addEventListener("contextmenu", e => e.preventDefault());
dom.addEventListener("pointerdown", e => {
  dragging = e.button === 2 || e.button === 1 ? 2 : 1;
  moved = 0; lastX = e.clientX; lastY = e.clientY;
  interacted = true; tween = null;
  ctrl.vt = ctrl.vp = 0;
});
addEventListener("pointerup", () => { dragging = 0; });
addEventListener("pointermove", e => {
  mouse.x = (e.clientX / innerWidth) * 2 - 1;
  mouse.y = -(e.clientY / innerHeight) * 2 + 1;
  if (!dragging) return;
  const dx = e.clientX - lastX, dy = e.clientY - lastY;
  moved += Math.abs(dx) + Math.abs(dy);
  lastX = e.clientX; lastY = e.clientY;
  if (dragging === 2) {
    // pan: slide the target along the camera plane
    const k = ctrl.dist * 0.0011;
    camera.getWorldDirection(tmpV);
    const right = tmpV.clone().cross(camera.up).normalize();
    const upv = right.clone().cross(tmpV).normalize();
    ctrl.target.addScaledVector(right, -dx * k).addScaledVector(upv, dy * k);
  } else {
    // orbit: 1:1 with the hand, plus a small tail
    ctrl.theta -= dx * 0.0034;
    ctrl.phi -= dy * 0.0034;
    ctrl.vt = -dx * 0.00055;
    ctrl.vp = -dy * 0.00055;
  }
});
dom.addEventListener("wheel", e => {
  e.preventDefault(); interacted = true; tween = null;
  const f = e.deltaY > 0 ? 1.14 : 0.877;
  if (f < 1) {
    // zoom in toward what's under the cursor
    raycaster.setFromCamera(mouse, camera);
    tmpV.copy(raycaster.ray.direction).multiplyScalar(ctrl.dist * 0.85).add(raycaster.ray.origin);
    ctrl.target.lerp(tmpV, 0.24);
  }
  ctrl.dist = Math.max(70, Math.min(2800, ctrl.dist * f));
}, { passive: false });
function applyCam() {
  ctrl.theta += ctrl.vt; ctrl.phi += ctrl.vp;
  ctrl.vt *= 0.8; ctrl.vp *= 0.8;
  ctrl.phi = Math.max(0.12, Math.min(3.0, ctrl.phi));
  if (!interacted && !reduced && !tween) ctrl.theta += 0.00045;
  const sp = Math.sin(ctrl.phi);
  camera.position.set(
    ctrl.target.x + ctrl.dist * sp * Math.sin(ctrl.theta),
    ctrl.target.y + ctrl.dist * Math.cos(ctrl.phi),
    ctrl.target.z + ctrl.dist * sp * Math.cos(ctrl.theta));
  camera.lookAt(ctrl.target);
}
/* camera flight: tween target + distance, keep current angles */
let tween = null;
const easeOutQuart = k => 1 - Math.pow(1 - k, 4);
function flyTo(vec, dist, ms) {
  if (reduced) { ctrl.target.copy(vec); ctrl.dist = dist; return; }
  tween = { t0: performance.now(), ms: ms || 900, fromT: ctrl.target.clone(), toT: vec.clone(), fromD: ctrl.dist, toD: dist };
}
function stepTween(now) {
  if (!tween) return;
  const k = Math.min(1, (now - tween.t0) / tween.ms), e = easeOutQuart(k);
  ctrl.target.lerpVectors(tween.fromT, tween.toT, e);
  ctrl.dist = tween.fromD + (tween.toD - tween.fromD) * e;
  if (k >= 1) tween = null;
}

/* ---------- level state machine: universe ▸ project ▸ session ▸ thought ---------- */
const state = { proj: null, sess: null, thought: null, matched: null };
const offRoles = new Set();
function brightSet() {
  if (state.sess) {
    const s = new Set([state.sess, "center"]);
    for (const [id, p] of parentOf) if (p === state.sess) s.add(id);
    const sn = byId.get(state.sess);
    if (sn) s.add("p:" + (sn._proj || ""));
    return s;
  }
  if (state.proj) {
    const s = new Set(["center", state.proj]);
    const pl = byId.get(state.proj)?.label;
    for (const n of DATA.nodes) if (n.project === pl || parentOf.get(n.id) === state.proj) s.add(n.id);
    for (const [id, p] of parentOf) if (s.has(p)) s.add(id);
    for (const l of DATA.links) if (l.kind === "spine" && idOf(l.source) === state.proj) s.add(idOf(l.target));
    return s;
  }
  return null;
}
const DIM = 0.14;
let curBright = null;
function colorFor(n, out) {
  const g = n._g || n.group;
  if (state.matched) return state.matched.has(n.id) ? WHITE : out.copy(COL[g]).multiplyScalar(DIM);
  if (curBright) return curBright.has(n.id) ? COL[g] : out.copy(COL[g]).multiplyScalar(DIM);
  return COL[g];
}
function applyStyles() {
  curBright = brightSet();
  const bright = curBright;
  const cTmp = new Color();
  for (const inst of instMeshes) {
    const { list } = inst.userData;
    for (let i = 0; i < list.length; i++) inst.setColorAt(i, colorFor(list[i], cTmp));
    inst.instanceColor.needsUpdate = true;
  }
  for (const lines of [dimLines, brightLines]) {
    const { links, base } = lines.userData;
    const col = lines.geometry.attributes.color;
    for (let i = 0; i < links.length; i++) {
      const l = links[i], on = !bright || (bright.has(idOf(l.source)) && bright.has(idOf(l.target)));
      const f = on ? 1 : DIM;
      for (let j = 0; j < 6; j++) col.array[i * 6 + j] = base[i * 6 + j] * f;
    }
    col.needsUpdate = true;
  }
}
/* hover: the node itself responds — grows and brightens */
function setNodeScale(n, mul) {
  const inst = instByGroup[n._g];
  if (!inst) return;
  const r = radiusOf(n) * mul;
  tmpM.compose(P(n), tmpQ, tmpS.set(r, r, r));
  inst.setMatrixAt(n._i, tmpM);
  inst.instanceMatrix.needsUpdate = true;
}
function highlightNode(n) {
  const inst = instByGroup[n._g];
  if (!inst) return;
  setNodeScale(n, 1.35);
  inst.setColorAt(n._i, tmpC.copy(colorFor(n, tmpC2)).lerp(WHITE, 0.5));
  inst.instanceColor.needsUpdate = true;
}
function unhighlightNode(n) {
  const inst = instByGroup[n._g];
  if (!inst) return;
  setNodeScale(n, 1);
  inst.setColorAt(n._i, colorFor(n, tmpC2));
  inst.instanceColor.needsUpdate = true;
}
const WHITE = new Color(0xffffff);
function crumbs() {
  const el = $("crumbs"); el.innerHTML = "";
  const add = (label, fn, last) => {
    const s = document.createElement("span");
    s.className = "crumb" + (last ? " here" : "");
    s.textContent = label;
    if (!last) s.addEventListener("click", fn);
    el.appendChild(s);
    if (!last) { const sep = document.createElement("span"); sep.className = "sep"; sep.textContent = "▸"; el.appendChild(sep); }
  };
  const depth = state.thought ? 3 : state.sess ? 2 : state.proj ? 1 : 0;
  add("◉ core", () => toUniverse(), depth === 0);
  if (state.proj) add(byId.get(state.proj)?.label || "project", () => toProject(state.proj), depth === 1);
  if (state.sess) add("session " + (byId.get(state.sess)?.label || ""), () => toSession(state.sess), depth === 2);
  if (state.thought) add("thought", null, true);
  $("back").style.display = depth ? "" : "none";
}
function toUniverse() {
  state.proj = state.sess = state.thought = null;
  ring.visible = false; hideDetail();
  applyStyles(); crumbs();
  flyTo(new Vector3(0, 0, 0), 800, 900);
}
function toProject(pid) {
  state.proj = pid; state.sess = state.thought = null;
  ring.visible = false; hideDetail();
  applyStyles(); crumbs(); sfx.open();
  flyTo(P(byId.get(pid)), 430, 900);
}
function toSession(sid) {
  const sn = byId.get(sid);
  sn._proj = sn._proj || (byId.get(sid)?.label, findProj(sid));
  state.sess = sid; state.thought = null;
  if (!state.proj) state.proj = "p:" + findProj(sid);
  ring.visible = false; hideDetail();
  applyStyles(); crumbs(); sfx.open();
  flyTo(P(sn), 150, 900);
}
function findProj(sid) {
  for (const l of DATA.links) if (l.kind === "spine" && idOf(l.target) === sid) return byId.get(idOf(l.source))?.label || "";
  return "";
}
function selectThought(n) {
  const par = parentOf.get(n.id);
  if (par && par.startsWith("s:")) { state.sess = par; if (!state.proj) state.proj = "p:" + findProj(par); }
  else if (par && par.startsWith("p:")) { state.proj = par; state.sess = null; }
  state.thought = n.id;
  applyStyles(); crumbs(); sfx.click();
  const p = P(n);
  ring.visible = true; ring.position.copy(p);
  ring.userData.r = radiusOf(n);
  showDetail(n);
  flyTo(p, 90, 800);
}
function up() {
  sfx.up();
  if (state.thought) { state.thought = null; ring.visible = false; hideDetail(); applyStyles(); crumbs(); flyTo(P(byId.get(state.sess || state.proj || "center") || coreNode), state.sess ? 150 : 430, 700); }
  else if (state.sess) { toProject(state.proj || "center"); }
  else if (state.proj) toUniverse();
}

/* ---------- detail panel (fixed, calm) ---------- */
function showDetail(n) {
  const d = $("detail");
  $("d-role").textContent = n.group;
  $("d-role").style.background = HEX[n.group] || "#94a3b8";
  $("d-meta").textContent = `${n.ts || "no date"} · ${n.project || ""}${n.session ? " · session " + n.session : ""}`;
  $("d-txt").textContent = n.snippet || n.label || "";
  const words = (n.snippet || "").split(/\s+/).filter(w => w.length > 5).slice(0, 3).join(" ");
  $("d-cmd").innerHTML = `terminal: <code>cml search "${words.replace(/"/g, "")}"</code>`;
  d.classList.add("show");
}
function hideDetail() { $("detail").classList.remove("show"); }
$("d-close").addEventListener("click", up);

/* ---------- raycast hover / click ---------- */
const raycaster = new Raycaster();
const mouse = new Vector2(-2, -2);
let hoverNode = null, rayTick = 0;
function raycast() {
  raycaster.setFromCamera(mouse, camera);
  const hits = raycaster.intersectObjects(instMeshes, false);
  let n = null;
  if (hits.length) {
    const h = hits[0];
    n = h.object.userData.list[h.instanceId];
  } else if (raycaster.intersectObject(core, false).length) n = coreNode;
  if (n !== hoverNode) {
    if (hoverNode && hoverNode._g) unhighlightNode(hoverNode);
    if (n && n._g) highlightNode(n);
    core.scale.setScalar(n === coreNode ? 25 : 22);
    hoverNode = n;
    dom.style.cursor = n ? "pointer" : "default";
    $("readout").textContent = !n ? "" :
      n.group === "session" ? `▸ session ${n.label} · ${childCount.get(n.id) || 0} thoughts · click to enter`
      : n.group === "project" ? `▸ project ${n.label} · click to enter`
      : n.group === "center" ? "▸ the core · click for overview"
      : `▸ ${n.group}${n.ts ? " · " + n.ts : ""}${n.project ? " · " + n.project : ""} · click to read`;
  }
}
dom.addEventListener("click", () => {
  if (moved > 6 || !hoverNode) return;
  const n = hoverNode;
  if (n.group === "center") return toUniverse();
  if (n.group === "project" || n.group === "coderoot") return toProject(n.id);
  if (n.group === "session") return toSession(n.id);
  selectThought(n);
});

/* ---------- labels ---------- */
const labelNodes = DATA.nodes.filter(n => n.group === "project" || n.group === "coderoot");
const labels = labelNodes.map(n => {
  const el = document.createElement("div");
  el.className = "plabel" + (n.group === "coderoot" ? " codeL" : "");
  el.textContent = n.label;
  el.addEventListener("click", () => toProject(n.id));
  document.body.appendChild(el);
  return { n, el, v: new Vector3() };
});
function projectLabels() {
  for (const L of labels) {
    L.v.set(L.n.fx, L.n.fy, L.n.fz).project(camera);
    if (L.v.z > 1) { L.el.style.display = "none"; continue; }
    L.el.style.display = "";
    const x = (L.v.x * 0.5 + 0.5) * innerWidth, y = (-L.v.y * 0.5 + 0.5) * innerHeight;
    L.el.style.transform = `translate(${(x - L.el.offsetWidth / 2) | 0}px, ${(y - 30) | 0}px)`;
  }
}

/* ---------- search: live highlight + prev/next cycling ---------- */
const q = $("q");
let matches = [], mIdx = -1, debounce = null;
function runSearch() {
  const s = q.value.trim().toLowerCase();
  if (!s) { state.matched = null; matches = []; mIdx = -1; $("mcount").textContent = ""; applyStyles(); return; }
  matches = DATA.nodes.filter(n =>
    (n.snippet || "").toLowerCase().includes(s) || (n.label || "").toLowerCase().includes(s));
  state.matched = new Set(matches.map(n => n.id));
  for (const n of matches) { const p = parentOf.get(n.id); if (p) state.matched.add(p); }
  mIdx = -1;
  $("mcount").textContent = matches.length ? `${matches.length}` : "0";
  applyStyles();
}
function cycle(dir) {
  if (!matches.length) { sfx.none(); return; }
  mIdx = (mIdx + dir + matches.length) % matches.length;
  const n = matches[mIdx];
  $("mcount").textContent = `${mIdx + 1}/${matches.length}`;
  sfx.found(); interacted = true;
  if (n.snippet) selectThought(n); else if (n.group === "session") toSession(n.id); else if (n.group === "project") toProject(n.id);
}
q.addEventListener("input", () => { clearTimeout(debounce); debounce = setTimeout(runSearch, 240); });
q.addEventListener("keydown", e => {
  e.stopPropagation();
  if (e.key === "Enter") cycle(e.shiftKey ? -1 : 1);
  if (e.key === "Escape") { q.value = ""; runSearch(); q.blur(); }
});
$("mprev").addEventListener("click", () => cycle(-1));
$("mnext").addEventListener("click", () => cycle(1));
addEventListener("keydown", e => {
  if (e.key === "/" && document.activeElement !== q) { e.preventDefault(); q.focus(); }
  if (e.key === "Escape") up();
});
$("back").addEventListener("click", up);

/* ---------- HUD: chips, nav, buttons, stats ---------- */
const roleCounts = (DATA.stats && DATA.stats.roles) || {};
for (const g of ["user", "assistant", "summary", "memory", "wiki", "code"]) {
  if (g === "code" && !groupsOf.code) continue;
  const c = roleCounts[g];
  const el = document.createElement("span");
  el.className = "chip on"; el.dataset.g = g;
  el.textContent = c ? `${g} ${c >= 1000 ? (c / 1000).toFixed(1) + "k" : c}` : g;
  el.addEventListener("click", () => {
    el.classList.toggle("on");
    const on = el.classList.contains("on");
    for (const inst of instMeshes) if (inst.userData.g === g) inst.visible = on;
  });
  $("chips").appendChild(el);
}
const perProject = {};
for (const n of DATA.nodes) if (n.project && n.group !== "project") perProject[n.project] = (perProject[n.project] || 0) + 1;
for (const p of labelNodes) {
  const row = document.createElement("div");
  row.className = "pn";
  row.innerHTML = `<span>⬢ ${p.label}</span><span class="n">${perProject[p.label] || ""}</span>`;
  row.addEventListener("click", () => toProject(p.id));
  $("projnav").appendChild(row);
}
$("b-view").addEventListener("click", () => { q.value = ""; runSearch(); toUniverse(); });
const bS = $("b-sound");
const paintSound = () => bS.textContent = soundOn ? "🔊 sound" : "🔇 muted";
paintSound();
bS.addEventListener("click", () => { soundOn = !soundOn; localStorage.cmlSound = soundOn ? "on" : "off"; paintSound(); });
(function stats() {
  const s = DATA.stats;
  $("stats").innerHTML = `<b>${s.rows.toLocaleString()}</b> memories · <b>${s.sessions}</b> sessions · <b>${s.projects}</b> projects · ${s.db_mb} MB`;
})();

/* ---------- fps + adaptive quality ---------- */
let frames = 0, fpsT = performance.now(), qLevel = 0, lowStreak = 0;
function applyQuality(l) {
  if (l === 1) renderer.setPixelRatio(1);
  if (l === 2) {
    renderer.setPixelRatio(0.75);
    if (pulses) pulses.visible = false;
    $("stars").style.display = "none";
    for (const inst of instMeshes) inst.geometry = lowGeo;
    core.geometry = lowGeo; ring.geometry = lowGeo;
  }
}
/* ---------- boot ---------- */
let booted = false;
function endBoot() {
  if (booted) return; booted = true;
  $("loading").classList.add("off");
  sfx.boot();
}
(function boot() {
  if (reduced) return endBoot();
  const log = $("bootlog");
  const lines = [
    ["CORE", "ULTRON-CLASS"],
    ["MEMORY", `${DATA.stats.rows.toLocaleString()} nodes`],
    ["ENGINE", "instanced · " + (instMeshes.length + 4) + " draw calls"],
    ["SYSTEM", "ONLINE"],
  ];
  let i = 0;
  (function next() {
    if (booted) return;
    if (i < lines.length) {
      const [k, v] = lines[i++];
      log.innerHTML += `<span class="ok">▸</span> ${k} <span class="pad">${".".repeat(Math.max(2, 12 - k.length))}</span> <span class="${i === lines.length ? "ok" : "val"}">${v}</span>\n`;
      tone(600 + i * 140, .04, "square", .012);
      setTimeout(next, 130);
    } else setTimeout(endBoot, 380);
  })();
  addEventListener("pointerdown", endBoot, { once: true });
})();

/* ---------- main loop ---------- */
addEventListener("resize", () => {
  camera.aspect = innerWidth / innerHeight; camera.updateProjectionMatrix();
  renderer.setSize(innerWidth, innerHeight);
});
document.addEventListener("visibilitychange", () => { /* rAF pauses naturally when hidden */ });
const A = byId, B = P; // keep minifier-free references stable
(function loop(now) {
  requestAnimationFrame(loop);
  stepTween(now || performance.now());
  applyCam();
  if (pulses && pulses.visible) {
    const st = pulses.userData.st, attr = pulses.geometry.attributes.position;
    for (let i = 0; i < st.length; i++) {
      const s = st[i]; s.t += s.sp;
      if (s.t >= 1) { s.t = 0; s.li = (Math.random() * pulseLinks.length) | 0; }
      const l = pulseLinks[s.li], a = byId.get(idOf(l.source)), b = byId.get(idOf(l.target));
      attr.array[i * 3] = a.fx + (b.fx - a.fx) * s.t;
      attr.array[i * 3 + 1] = a.fy + (b.fy - a.fy) * s.t;
      attr.array[i * 3 + 2] = a.fz + (b.fz - a.fz) * s.t;
    }
    attr.needsUpdate = true;
  }
  if (ring.visible) {
    const k = 1 + Math.sin((now || 0) * 0.004) * 0.12;
    ring.scale.setScalar((ring.userData.r || 3) * 1.9 * k);
  }
  if ((rayTick = (rayTick + 1) & 1) === 0) raycast();
  projectLabels();
  renderer.render(scene, camera);
  frames++;
  if ((now || performance.now()) - fpsT >= 1000) {
    const fps = frames; frames = 0; fpsT = now || performance.now();
    $("fps").textContent = fps + " fps" + (qLevel ? " ·q" + qLevel : "");
    if (fps < 22 && qLevel < 2) { if (++lowStreak >= 2) applyQuality(++qLevel), lowStreak = 0; }
    else lowStreak = 0;
  }
})(performance.now());
crumbs();
applyStyles();
})();
