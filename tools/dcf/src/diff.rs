//! dcf-diff: compares two .dcf files node by node, for the .fig -> .dcf converter
//! (fig2json.js + dcf-fetch --local) against Figma's own import of the same .fig.
//!
//!   dcf-diff <reference.dcf> <candidate.dcf> [root names...]
//!
//! The views are matched by the path of their names from a root (siblings with the same name
//! by their order); for each pair the style and the node's own data are printed in protobuf's
//! text format and compared line by line, ids left out (they differ by construction). Prints
//! one block per differing node and a summary: nodes compared, equal, differing, missing.
use dc_bundle::definition_file::load_design_def;
use dc_bundle::view::View;
use protobuf::MessageField;
use std::collections::{BTreeMap, HashMap};

/// A number in a line rounded to 0.01 (a hundredth of a pixel, or of an opacity): Figma prints
/// 6 significant digits of a float32, so the last digit of a converted value may differ.
fn normalize(line: &str) -> String {
    let t = line.trim_end_matches(',');
    match t.parse::<f64>() {
        Ok(v) if t.contains('.') || t.contains('e') => {
            let r = (v * 100.0).round() / 100.0;
            format!("{}", if r == 0.0 { 0.0 } else { r })
        }
        _ => {
            if let Some((k, v)) = t.split_once(": ") {
                if let Ok(x) = v.parse::<f64>() {
                    if v.contains('.') || v.contains('e') {
                        let r = (x * 100.0).round() / 100.0;
                        return format!("{k}: {}", if r == 0.0 { 0.0 } else { r });
                    }
                }
            }
            t.to_string()
        }
    }
}

/// The text of a view's own properties: style, shape or text, frame extras; no children, no ids.
fn own_text(view: &View) -> Vec<String> {
    let mut v = view.clone();
    v.id = String::new();
    v.unique_id = 0;
    v.component_info = MessageField::none();
    // the node's place in the design's own layout: no renderer reads it (harry lays out with
    // taffy), and Figma fills it from its own auto layout pass
    v.design_absolute_bounding_box = MessageField::none();
    if let Some(data) = v.data.as_mut() {
        if let Some(dc_bundle::view::view_data::View_data_type::Container(c)) = data.view_data_type.as_mut() {
            c.children.clear();
        }
    }
    format!("{v:#?}")
        .lines()
        .map(|l| normalize(l.trim()))
        .filter(|l| !l.is_empty() && !l.starts_with("special_fields") && !l.starts_with("cached_size") && l != "}" && l != "}," && l != "]" && l != "]," && l != "),")
        .collect()
}

fn children(view: &View) -> &[View] {
    match view.data.as_ref().and_then(|d| d.view_data_type.as_ref()) {
        Some(dc_bundle::view::view_data::View_data_type::Container(c)) => &c.children,
        _ => &[],
    }
}

/// path -> view, the path made of names, a repeated name numbered by its order among siblings
fn index<'a>(view: &'a View, path: String, out: &mut BTreeMap<String, &'a View>) {
    out.insert(path.clone(), view);
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for c in children(view) {
        let n = seen.entry(c.name.as_str()).or_insert(0);
        let p = if *n == 0 { format!("{path}/{}", c.name) } else { format!("{path}/{}[{n}]", c.name) };
        *n += 1;
        index(c, p, out);
    }
}

/// the lines of a that are not in b and those of b not in a (a multiset difference)
fn diff(a: &[String], b: &[String]) -> (Vec<String>, Vec<String>) {
    let mut count: HashMap<&String, i32> = HashMap::new();
    for l in b {
        *count.entry(l).or_default() += 1;
    }
    let mut only_a = Vec::new();
    for l in a {
        match count.get_mut(l) {
            Some(c) if *c > 0 => *c -= 1,
            _ => only_a.push(l.clone()),
        }
    }
    let mut count: HashMap<&String, i32> = HashMap::new();
    for l in a {
        *count.entry(l).or_default() += 1;
    }
    let mut only_b = Vec::new();
    for l in b {
        match count.get_mut(l) {
            Some(c) if *c > 0 => *c -= 1,
            _ => only_b.push(l.clone()),
        }
    }
    (only_a, only_b)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: dcf-diff <reference.dcf> <candidate.dcf> [root names...]");
        std::process::exit(2);
    }
    let (_, reference) = load_design_def(&args[1]).expect("reference .dcf");
    let (_, candidate) = load_design_def(&args[2]).expect("candidate .dcf");
    let roots: Vec<String> = if args.len() > 3 { args[3..].to_vec() } else { vec!["#smarthome-root".into(), "Library".into()] };
    let by_name = |d: &dc_bundle::design_compose_definition::DesignComposeDefinition, name: &str| {
        d.views.values().find(|v| v.name == name).cloned()
    };
    let (mut compared, mut equal, mut differing, mut missing, mut extra) = (0, 0, 0, 0, 0);
    for root in &roots {
        let (Some(r), Some(c)) = (by_name(&reference, root), by_name(&candidate, root)) else {
            println!("root {root}: missing in {}", if by_name(&reference, root).is_none() { "the reference" } else { "the candidate" });
            continue;
        };
        let (mut ri, mut ci) = (BTreeMap::new(), BTreeMap::new());
        index(&r, root.clone(), &mut ri);
        index(&c, root.clone(), &mut ci);
        // DCF_PATHS=1: the nodes whose reference has vector paths the candidate lacks, with
        // the reference's commands and coordinates on one line
        if std::env::var("DCF_PATHS").is_ok() {
            let paths = |v: &View| -> String {
                let t = own_text(v);
                let which = if std::env::var("DCF_PATHS").as_deref() == Ok("strokes") { "strokes: [" } else { "paths: [" };
                let Some(i) = t.iter().position(|l| l.starts_with(which) && !l.ends_with("[],")) else { return String::new() };
                t[i + 1..].iter().take_while(|l| !l.starts_with("strokes") && !l.starts_with("stroke_cap") && !l.starts_with("corner_radii") && !l.starts_with("is_mask")).map(|l| l.trim_end_matches(',')).filter(|l| !l.ends_with('{') && !l.ends_with('[') && !l.starts_with("Path")).collect::<Vec<_>>().join(" ")
            };
            for (path, rv) in &ri {
                let Some(cv) = ci.get(path) else { continue };
                let (rp, cp) = (paths(rv), paths(cv));
                if !rp.is_empty() && cp.is_empty() {
                    println!("NOPATH {path}\n   {}", rp.chars().take(400).collect::<String>());
                }
            }
            continue;
        }
        // DCF_SHOW=<path suffix>: print that node's own properties from both files, in full
        if let Ok(show) = std::env::var("DCF_SHOW") {
            for (path, rv) in &ri {
                if path.ends_with(&show) {
                    println!("== reference {path}\n{}", own_text(rv).join("\n"));
                    if let Some(cv) = ci.get(path) {
                        println!("== candidate {path}\n{}", own_text(cv).join("\n"));
                    }
                    return;
                }
            }
        }
        for (path, rv) in &ri {
            let Some(cv) = ci.get(path) else {
                missing += 1;
                println!("MISSING {path}");
                continue;
            };
            compared += 1;
            let (only_r, only_c) = diff(&own_text(rv), &own_text(cv));
            if only_r.is_empty() && only_c.is_empty() {
                equal += 1;
            } else {
                differing += 1;
                println!("DIFF {path}");
                for l in only_r.iter().take(12) {
                    println!("   - {l}");
                }
                for l in only_c.iter().take(12) {
                    println!("   + {l}");
                }
            }
        }
        for path in ci.keys() {
            if !ri.contains_key(path) {
                extra += 1;
                println!("EXTRA {path}");
            }
        }
    }
    println!("summary: {compared} compared, {equal} equal, {differing} differing, {missing} missing, {extra} extra");
}
