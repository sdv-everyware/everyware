// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The names the house gave its Matter devices: one per endpoint of a node, set with
//! MatterService.Rename and published with the device (MatterEndpoint.name). The bundle keeps
//! them in `names.txt` in its directory, next to the fabric: a device keeps its own label only
//! per node (Basic Information's NodeLabel), and a node with several endpoints is several
//! devices of the house. So the names are the house's, as other Matter controllers keep theirs.
//!
//! ```text
//! names.txt, one device a line: <node id, hex> <endpoint> <name>
//! 10 2 Hallway lamp
//! ```

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// The longest name, in bytes of UTF-8: the catalog's size of MatterEndpoint.name, and the
/// length of a Matter NodeLabel.
pub const MAX_NAME_BYTES: usize = 32;
const FILE: &str = "names.txt";

/// A name as it is kept: without control characters, runs of spaces as one, no space at its
/// ends, at most MAX_NAME_BYTES (cut between two characters). Empty: no name.
pub fn clean(name: &str) -> String {
    let mut out = String::new();
    for word in name.split(|c: char| c.is_whitespace() || c.is_control()).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    let mut end = out.len().min(MAX_NAME_BYTES);
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    out.truncate(out.trim_end().len());
    out
}

/// The lines of names.txt as names by (node id, endpoint); a line that is not one is left out.
fn parse(text: &str) -> BTreeMap<(u64, u16), String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            let node = u64::from_str_radix(parts.next()?, 16).ok()?;
            let endpoint: u16 = parts.next()?.parse().ok()?;
            let name = clean(parts.next()?);
            (!name.is_empty()).then_some(((node, endpoint), name))
        })
        .collect()
}

fn format(names: &BTreeMap<(u64, u16), String>) -> String {
    names.iter().map(|((node, endpoint), name)| format!("{node:x} {endpoint} {name}\n")).collect()
}

pub struct Names {
    path: PathBuf,
    names: BTreeMap<(u64, u16), String>,
}

impl Names {
    /// The names kept in `dir`; none when there is no file yet.
    pub fn load(dir: &Path) -> Names {
        let path = dir.join(FILE);
        let names = std::fs::read_to_string(&path).map(|text| parse(&text)).unwrap_or_default();
        Names { path, names }
    }

    /// The name of an endpoint, empty when it has none.
    pub fn get(&self, node: u64, endpoint: u16) -> &str {
        self.names.get(&(node, endpoint)).map_or("", String::as_str)
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Names an endpoint (`name` cleaned; an empty one takes its name away) and writes the
    /// file. Ok(false): it had that name already, nothing is written. On an error the names
    /// are as before.
    pub fn set(&mut self, node: u64, endpoint: u16, name: &str) -> std::io::Result<bool> {
        let name = clean(name);
        if self.get(node, endpoint) == name {
            return Ok(false);
        }
        let mut names = self.names.clone();
        if name.is_empty() {
            names.remove(&(node, endpoint));
        } else {
            names.insert((node, endpoint), name);
        }
        save(&self.path, &names)?;
        self.names = names;
        Ok(true)
    }
}

/// Writes the file whole or not at all: a new file beside it, then renamed over it. The
/// bundle's alone to read (0600).
fn save(path: &Path, names: &BTreeMap<(u64, u16), String>) -> std::io::Result<()> {
    let new = path.with_extension("txt.new");
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&new)?;
    file.write_all(format(names).as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&new, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("everyware-matter-names-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_name_is_cleaned_before_it_is_kept() {
        assert_eq!(clean("  Hallway   lamp \n"), "Hallway lamp");
        assert_eq!(clean("a\tb\u{7}c"), "a b c");
        assert_eq!(clean(" \n "), "");
        // 32 bytes at most, cut between two characters, no space left at the end
        assert_eq!(clean(&"x".repeat(40)), "x".repeat(32));
        assert_eq!(clean(&format!("{}ä", "x".repeat(31))), "x".repeat(31));
        assert_eq!(clean(&format!("{} yz", "x".repeat(31))), "x".repeat(31));
        assert_eq!(clean("Küche 灯"), "Küche 灯");
    }

    #[test]
    fn names_are_kept_per_endpoint_and_come_back_from_the_file() {
        let dir = dir("kept");
        let mut names = Names::load(&dir);
        assert_eq!(names.get(0x10, 2), "");
        assert!(names.set(0x10, 2, " Hallway lamp ").unwrap());
        assert!(names.set(0x10, 3, "Bedside").unwrap());
        assert!(!names.set(0x10, 3, "Bedside").unwrap());
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), "10 2 Hallway lamp\n10 3 Bedside\n");
        let again = Names::load(&dir);
        assert_eq!((again.get(0x10, 2), again.get(0x10, 3), again.get(0x11, 2), again.len()), ("Hallway lamp", "Bedside", "", 2));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_empty_name_takes_the_name_away() {
        let dir = dir("empty");
        let mut names = Names::load(&dir);
        names.set(0x10, 2, "Hallway lamp").unwrap();
        assert!(names.set(0x10, 2, "   ").unwrap());
        assert_eq!((names.get(0x10, 2), names.len()), ("", 0));
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), "");
        assert!(!names.set(0x10, 2, "").unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_name_that_cannot_be_written_leaves_the_names_as_they_were() {
        let mut names = Names::load(Path::new("/nonexistent/everyware-matter-names"));
        assert!(names.set(0x10, 2, "Hallway lamp").is_err());
        assert_eq!(names.get(0x10, 2), "");
    }

    #[test]
    fn lines_that_are_not_names_are_left_out() {
        let names = parse("10 2 Hallway lamp\n\nnot a line\nzz 2 x\n10 two x\n10 4\n1f 1  Fan  \n");
        assert_eq!(names.len(), 2);
        assert_eq!(names[&(0x10, 2)], "Hallway lamp");
        assert_eq!(names[&(0x1f, 1)], "Fan");
    }
}
