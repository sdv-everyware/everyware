//! A service bundle's own files: the last messages it published, kept so that it can publish
//! them again at its next start, before its source answers (the network comes up seconds after
//! the bundle starts).
//!
//! The directory is the one SDV gives every bundle, `/data/service_bundle_data/<SDV package>/
//! <bundle>` (the lifecycle manager creates it with the bundle's uid; SELinux type
//! `service_bundle_data`, open to every service bundle domain). SDV has no API for the path yet
//! (b/381209295); its samples build it the same way
//! (system/software_defined_vehicle/samples/sqlite).
//!
//! A file is one protobuf message. A write goes to a temporary file that is then renamed, so a
//! power cut leaves the old message or the new one, never half of one.

use protobuf::Message;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Where SDV keeps the bundles' data.
const ROOT: &str = "/data/service_bundle_data";
/// A temporary file's suffix.
const TMP: &str = ".tmp";

/// The files of one bundle.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The store of the bundle `bundle` of the SDV package `package` (its fqin:
    /// `context.get_self_fqin()`, `get_sdv_package_name()`, `get_service_bundle_name()`).
    pub fn new(package: &str, bundle: &str) -> Self {
        Self::at(Path::new(ROOT).join(package).join(bundle))
    }

    /// A store in `dir` (tests).
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The message in `name`, when there is one, it parses and it is not older than `max_age`.
    /// A message whose file is newer than the clock (the clock not yet set at boot) counts as
    /// fresh: its age is unknown, and an old message is better than none.
    pub fn load<M: Message>(&self, name: &str, max_age: Duration) -> Option<M> {
        let path = self.dir.join(name);
        let modified = fs::metadata(&path).and_then(|m| m.modified()).ok()?;
        if SystemTime::now().duration_since(modified).is_ok_and(|age| age > max_age) {
            return None;
        }
        let bytes = fs::read(&path).ok()?;
        match M::parse_from_bytes(&bytes) {
            Ok(message) => Some(message),
            Err(err) => {
                log::warn!("{}: {err}", path.display());
                None
            }
        }
    }

    /// `message` into `name`, in place of what was there.
    pub fn save<M: Message>(&self, name: &str, message: &M) -> io::Result<()> {
        let bytes = message.write_to_bytes().map_err(io::Error::other)?;
        let path = self.dir.join(name);
        let tmp = self.dir.join(format!("{name}{TMP}"));
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protobuf::well_known_types::wrappers::StringValue;

    fn store(test: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("everyware_bundle_store_{test}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Store::at(dir)
    }

    fn text(s: &str) -> StringValue {
        let mut m = StringValue::new();
        m.value = s.into();
        m
    }

    #[test]
    fn a_message_comes_back() {
        let s = store("back");
        assert!(s.load::<StringValue>("a.pb", Duration::from_secs(60)).is_none());
        s.save("a.pb", &text("one")).unwrap();
        s.save("a.pb", &text("two")).unwrap();
        assert_eq!(s.load::<StringValue>("a.pb", Duration::from_secs(60)).unwrap().value, "two");
    }

    #[test]
    fn an_old_message_does_not() {
        let s = store("old");
        s.save("a.pb", &text("one")).unwrap();
        let old = SystemTime::now() - Duration::from_secs(120);
        fs::File::options().write(true).open(s.dir.join("a.pb")).unwrap().set_modified(old).unwrap();
        assert!(s.load::<StringValue>("a.pb", Duration::from_secs(60)).is_none());
        assert!(s.load::<StringValue>("a.pb", Duration::from_secs(600)).is_some());
    }
}
