//! Where every node sits, and the JSON the page reads.
//!
//! Projects sit on a ring around the core, sessions on a sphere around their
//! project, messages on a sphere around their session — every position computed
//! here, so the same index always draws the same picture and the browser runs zero
//! physics for 9,000 nodes.
//!
//! The C++ predecessor hand-wrote this JSON, keys in alphabetical order, to stay
//! byte-comparable with a serde_json writer. Here that writer is the one running,
//! so ~40 lines of escaping and float formatting are simply gone: `serde_json::Map`
//! is a `BTreeMap`, and ryu already emits the `5.0` form.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

const RING_R: f64 = 300.0;

#[derive(Clone, Copy, Default)]
struct V3 {
    x: f64,
    y: f64,
    z: f64,
}

#[derive(Default)]
pub struct MRow {
    pub rowid: i64,
    pub role: String,
    pub pidx: usize,
    /// `None` for the rows with no session of their own: memory notes hang off
    /// their project, wiki pages off the core. The C++ struct carried a parallel
    /// `has_sess` bool that could disagree with `sidx`; this cannot.
    pub sidx: Option<usize>,
    pub date: String,
    pub snip: String,
    pub sess8: String,
    pub project: String,
}

pub struct CodeNode {
    pub id: String,
    pub label: String,
    pub snippet: String,
}

#[derive(Default)]
pub struct MapData {
    pub proj_names: Vec<String>,
    /// (session id, project index)
    pub sess_list: Vec<(String, usize)>,
    pub msgs: Vec<MRow>,
    /// `BTreeMap`, so the legend the page draws is in a stable order.
    pub role_counts: std::collections::BTreeMap<String, usize>,
    /// lowercased note name -> "m:<rowid>"
    pub note_ids: HashMap<String, String>,
    pub pending_wikilinks: Vec<(String, Vec<String>)>,
    pub code_nodes: Vec<CodeNode>,
    pub code_edges: Vec<(String, String)>,
    pub code_root_label: String,
    pub db_mb: Option<String>,
}

pub struct Payload {
    pub json: String,
    pub nodes: usize,
    pub links: usize,
    pub code: usize,
}

/// Places every node on its shell and serializes the graph the page reads.
pub fn build_payload(d: &MapData) -> Payload {
    let has_code = !d.code_nodes.is_empty();
    let slots = d.proj_names.len() + usize::from(has_code);
    let denom = slots.max(1) as f64;
    let slot_pos = |k: usize| V3 {
        x: RING_R * (std::f64::consts::TAU * k as f64 / denom).cos(),
        y: ((k % 3) as f64 - 1.0) * 46.0,
        z: RING_R * (std::f64::consts::TAU * k as f64 / denom).sin(),
    };

    // Sessions on shells around their project.
    let mut sess_by_p: Vec<Vec<usize>> = vec![Vec::new(); d.proj_names.len()];
    for (si, (_, p)) in d.sess_list.iter().enumerate() {
        sess_by_p[*p].push(si);
    }
    let mut spos = vec![V3::default(); d.sess_list.len()];
    for (p, list) in sess_by_p.iter().enumerate() {
        let base = slot_pos(p);
        let r = 110.0 + (list.len() as f64).sqrt() * 7.0;
        for (j, &si) in list.iter().enumerate() {
            let u = fib_sphere(j, list.len());
            spos[si] =
                V3 { x: base.x + u.x * r, y: base.y + u.y * r * 0.72, z: base.z + u.z * r };
        }
    }

    let mut nodes: Vec<Value> = vec![json!({
        "fx": 0.0, "fy": 0.0, "fz": 0.0,
        "group": "center", "id": "center", "label": "memory", "val": 34
    })];
    let mut links: Vec<Value> = Vec::new();

    for (p, name) in d.proj_names.iter().enumerate() {
        let id = format!("p:{name}");
        let c = slot_pos(p);
        nodes.push(json!({
            "fx": c.x, "fy": c.y, "fz": c.z,
            "group": "project", "id": id, "label": name, "val": 16
        }));
        link(&mut links, "center", &id, "spine");
    }

    for (si, (sid, p)) in d.sess_list.iter().enumerate() {
        let id = format!("s:{sid}");
        let c = spos[si];
        nodes.push(json!({
            "fx": c.x, "fy": c.y, "fz": c.z,
            "group": "session", "id": id, "label": super::take_chars(sid, 8), "val": 7
        }));
        link(&mut links, &format!("p:{}", d.proj_names[*p]), &id, "spine");
    }

    // Which shell a message hangs on: 0 = its session, 1 = its project (memory
    // notes have no session), 2 = the core (wiki pages).
    let key_of = |m: &MRow| -> (u8, usize) {
        match (m.sidx, m.role.as_str()) {
            (Some(s), _) => (0, s),
            (None, "wiki") => (2, 0),
            (None, _) => (1, m.pidx),
        }
    };
    let mut totals: HashMap<(u8, usize), usize> = HashMap::new();
    for m in &d.msgs {
        *totals.entry(key_of(m)).or_default() += 1;
    }
    let mut order: HashMap<(u8, usize), usize> = HashMap::new();

    for m in &d.msgs {
        let k = key_of(m);
        let seen = order.entry(k).or_insert(0);
        let idx = *seen;
        *seen += 1;
        let n = totals[&k];
        let (base, r, parent) = match k.0 {
            // f64::cbrt is not glibc's cbrt — they disagree by 1 ULP on most
            // inputs, which moves a dot by ~1e-13 units. Left alone rather than
            // vendoring musl's cbrt to chase it.
            0 => (spos[k.1], 26.0 + (n as f64).cbrt() * 7.0, format!("s:{}", d.sess_list[k.1].0)),
            1 => (slot_pos(k.1), 78.0, format!("p:{}", d.proj_names[k.1])),
            _ => (V3::default(), 140.0, "center".to_string()),
        };
        let u = fib_sphere(idx, n);
        let s = m.rowid as u64;
        let id = format!("m:{}", m.rowid);
        // Notes draw bigger than turns: they are the curated layer, and at 6,000
        // rows a note the same size as its neighbours is invisible.
        let val = if m.role == "memory" || m.role == "wiki" { 5.0 } else { 1.6 };
        nodes.push(json!({
            "fx": base.x + u.x * r + jit(s) * 4.0,
            "fy": base.y + u.y * r + jit(s ^ 0xA5A5) * 4.0,
            "fz": base.z + u.z * r + jit(s ^ 0x5A5A) * 4.0,
            "group": m.role, "id": id, "label": m.date, "project": m.project,
            "session": m.sess8, "snippet": m.snip, "ts": m.date, "val": val
        }));
        link(&mut links, &parent, &id, "leaf");
    }

    for (from, targets) in &d.pending_wikilinks {
        for to in targets.iter().filter_map(|name| d.note_ids.get(name)) {
            if to != from {
                link(&mut links, from, to, "wikilink");
            }
        }
    }

    if has_code {
        let root = format!("code:{}", d.code_root_label);
        let c = slot_pos(slots - 1);
        nodes.push(json!({
            "fx": c.x, "fy": c.y, "fz": c.z,
            "group": "coderoot", "id": root, "label": "code", "val": 18
        }));
        link(&mut links, "center", &root, "spine");

        let cr = 60.0 + (d.code_nodes.len() as f64).cbrt() * 9.0;
        for (i, cnode) in d.code_nodes.iter().enumerate() {
            let u = fib_sphere(i, d.code_nodes.len());
            let id = format!("c:{}", cnode.id);
            nodes.push(json!({
                "fx": c.x + u.x * cr, "fy": c.y + u.y * cr, "fz": c.z + u.z * cr,
                "group": "code", "id": id, "label": cnode.label, "project": "code",
                "session": "", "snippet": cnode.snippet, "ts": "", "val": 2.4
            }));
            link(&mut links, &root, &id, "tether");
        }
        for (s, t) in &d.code_edges {
            link(&mut links, &format!("c:{s}"), &format!("c:{t}"), "code");
        }
    }

    let roles: Map<String, Value> =
        d.role_counts.iter().map(|(role, n)| (role.clone(), json!(n))).collect();
    let counts = (nodes.len(), links.len(), d.code_nodes.len());
    let out = json!({
        "links": links,
        "nodes": nodes,
        "stats": {
            "db_mb": d.db_mb.as_deref().unwrap_or("?"),
            "projects": d.proj_names.len(),
            "roles": Value::Object(roles),
            "rows": d.msgs.len(),
            "sessions": d.sess_list.len(),
        }
    })
    .to_string();

    Payload {
        // "</" would close the inline <script> this payload sits inside, and a
        // snippet quoting HTML is not rare.
        json: out.replace("</", "<\\/"),
        nodes: counts.0,
        links: counts.1,
        code: counts.2,
    }
}

fn link(into: &mut Vec<Value>, source: &str, target: &str, kind: &str) {
    into.push(json!({"kind": kind, "source": source, "target": target}));
}

/// i-th of n points on a unit sphere (golden-angle fibonacci lattice).
fn fib_sphere(i: usize, n: usize) -> V3 {
    let y = 1.0 - 2.0 * (i as f64 + 0.5) / n.max(1) as f64;
    let r = (1.0 - y * y).max(0.0).sqrt();
    let phi = i as f64 * 2.399_963_229_728_653;
    V3 { x: r * phi.cos(), y, z: r * phi.sin() }
}

/// Deterministic pseudo-random in [-1,1] from a seed — layout jitter without an RNG,
/// so the picture is reproducible from the index alone.
fn jit(seed: u64) -> f64 {
    let x = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    f64::from((x >> 33) as u32) / 4_294_967_295.0 * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row gets a node, every node a parent link, and the payload never
    /// closes the `<script>` it is embedded in.
    #[test]
    fn payload_places_every_row_and_keeps_the_script_open() {
        let mut d = MapData {
            proj_names: vec!["proj".into()],
            sess_list: vec![("abcdef0123".into(), 0)],
            ..Default::default()
        };
        d.role_counts.insert("user".into(), 1);
        d.msgs.push(MRow {
            rowid: 1,
            role: "user".into(),
            sidx: Some(0),
            date: "2026-07-21".into(),
            snip: "a </script> snippet".into(),
            sess8: "abcdef01".into(),
            project: "proj".into(),
            ..Default::default()
        });
        d.msgs.push(MRow {
            rowid: 2,
            role: "wiki".into(),
            date: "2026-07-21".into(),
            snip: "note".into(),
            project: "wiki".into(),
            ..Default::default()
        });
        d.note_ids.insert("target".into(), "m:2".into());
        d.pending_wikilinks.push(("m:1".into(), vec!["target".into(), "missing".into()]));

        let p = build_payload(&d);
        assert_eq!(p.nodes, 5, "center + project + session + 2 messages");
        assert_eq!(p.links, 5, "3 spine/leaf pairs plus the one resolvable wikilink");
        assert_eq!(p.code, 0);
        assert!(!p.json.contains("</script>"), "an inline <script> must stay open");
        assert!(p.json.contains("<\\/script>"), "...because the slash is escaped");
        assert!(
            p.json.contains(r#"{"kind":"wikilink","source":"m:1","target":"m:2"}"#),
            "a wikilink resolves to the note's node, and a dangling one is dropped"
        );
        assert!(
            p.json.contains(
                r#""stats":{"db_mb":"?","projects":1,"roles":{"user":1},"rows":2,"sessions":1}"#
            ),
            "stats block: {}",
            &p.json[p.json.find(r#""stats""#).unwrap_or(0)..]
        );
    }

    /// The page does no physics, so a rebuild from the same index must draw the
    /// same picture — and the integer `val`s must not drift into floats, which is
    /// what a hand-written JSON writer got wrong.
    #[test]
    fn layout_is_deterministic_and_val_types_are_stable() {
        let d = MapData {
            proj_names: vec!["p".into()],
            msgs: vec![MRow { rowid: 7, role: "memory".into(), ..Default::default() }],
            ..Default::default()
        };
        assert_eq!(build_payload(&d).json, build_payload(&d).json);
        let json = build_payload(&d).json;
        assert!(json.contains(r#""label":"memory","val":34"#), "center keeps an integer val");
        assert!(json.contains(r#""val":5.0"#), "a note keeps a float val");
    }
}
