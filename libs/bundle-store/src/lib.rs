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
//! A file is one protobuf message, or bytes of the bundle's own (`read`, `save_private`: a
//! credential, a configuration). A write goes to a temporary file that is then renamed, so a
//! power cut leaves the old file or the new one, never half of one.

use protobuf::Message;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
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
        self.replace(name, &bytes, None)
    }

    /// The bytes in `name`, when there is such a file of at most `max` bytes.
    pub fn read(&self, name: &str, max: usize) -> Option<Vec<u8>> {
        let file = fs::File::open(self.dir.join(name)).ok()?;
        let mut bytes = Vec::new();
        file.take(max as u64 + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() > max {
            log::warn!("{}: over {max} bytes", self.dir.join(name).display());
            return None;
        }
        Some(bytes)
    }

    /// `bytes` into `name`, in place of what was there, readable and writable by the bundle's
    /// user only (0600): for a credential.
    pub fn save_private(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.replace(name, bytes, Some(0o600))
    }

    /// `name` gone; no such file is no error.
    pub fn remove(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.dir.join(name)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }

    /// `bytes` into `name` through a temporary file that is renamed; `mode`: the file's
    /// permissions from its creation on (else the default of the process).
    fn replace(&self, name: &str, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
        let path = self.dir.join(name);
        let tmp = self.dir.join(format!("{name}{TMP}"));
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        if let Some(mode) = mode {
            // a temporary file left from a power cut keeps its mode: made anew
            self.remove(&format!("{name}{TMP}"))?;
            options.mode(mode);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
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
    fn private_bytes_are_the_bundles_only() {
        use std::os::unix::fs::PermissionsExt;
        let s = store("private");
        assert!(s.read("secret", 64).is_none());
        s.save_private("secret", b"one").unwrap();
        s.save_private("secret", b"two").unwrap();
        assert_eq!(s.read("secret", 64).unwrap(), b"two");
        assert_eq!(fs::metadata(s.dir.join("secret")).unwrap().permissions().mode() & 0o777, 0o600);
        // longer than allowed: not read
        assert!(s.read("secret", 2).is_none());
        s.remove("secret").unwrap();
        s.remove("secret").unwrap();
        assert!(s.read("secret", 64).is_none());
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
