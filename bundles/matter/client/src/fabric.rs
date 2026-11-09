//! The house's fabric: the controller is its own certificate authority. The fabric is created
//! once (a root certificate, an intermediate one that signs the nodes' certificates, the
//! controller's own certificate and key, the fabric's IPK) and kept in the bundle's directory;
//! at every start it is installed in rs-matter again, so the devices commissioned before are
//! reached over CASE without commissioning them again. The root's private key is not kept (as
//! rs-matter's commissioner example does): the intermediate one signs new nodes.
//!
//! The nodes commissioned onto it are listed next to it, with the code each was paired by.

use rs_matter::cert::gen::VALID_FOREVER;
use rs_matter::cert::MAX_CERT_TLV_AND_ASN1_LEN;
use rs_matter::crypto::{CanonAeadKey, CanonPkcSecretKey, Crypto, Rng as _, SecretKey, SigningSecretKey};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::onboard::cac::{IcacGenerator, RcacGenerator};
use rs_matter::onboard::noc::NocGenerator;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// The controller's node id on its fabric.
pub const CONTROLLER_NODE_ID: u64 = 0x0000_0000_0000_0001;
/// The vendor the controller administers the fabric as (the test vendor, as the devices'
/// certificates are test ones).
pub const ADMIN_VENDOR_ID: u16 = 0xFFF1;
/// The first node id a device gets.
pub const FIRST_NODE_ID: u64 = 0x10;

const FABRIC_FILE: &str = "fabric.bin";
const NODES_FILE: &str = "nodes.txt";
const MAGIC: &[u8; 6] = b"EWMF1\n";

/// The fabric's certificates and keys.
pub struct Ca {
    pub fabric_id: u64,
    /// Root certificate (Matter TLV).
    pub rcac: Vec<u8>,
    /// Intermediate certificate, and its key: it signs the nodes' certificates.
    pub icac: Vec<u8>,
    pub icac_key: CanonPkcSecretKey,
    /// The controller's operational certificate and key.
    pub noc: Vec<u8>,
    pub key: CanonPkcSecretKey,
    pub ipk: CanonAeadKey,
}

impl Ca {
    /// A new fabric with a random id.
    pub fn create<C: Crypto>(crypto: &C) -> Result<Self, Error> {
        let mut id = [0u8; 8];
        crypto.rand()?.fill_bytes(&mut id);
        // a fabric id is not 0
        let fabric_id = u64::from_be_bytes(id) | 1;

        let mut rcac_buf = [0u8; MAX_CERT_TLV_AND_ASN1_LEN];
        let mut rcac_gen = RcacGenerator::new(&mut rcac_buf);
        let (rcac_key, rcac) = rcac_gen.generate(crypto, fabric_id, VALID_FOREVER)?;
        let rcac = rcac.to_vec();

        let mut icac_buf = [0u8; MAX_CERT_TLV_AND_ASN1_LEN];
        let mut icac_gen = IcacGenerator::new(&mut icac_buf);
        let (icac_key, icac) = icac_gen.generate(crypto, rcac_key.reference(), &rcac, VALID_FOREVER)?;
        let icac = icac.to_vec();
        drop(rcac_key);

        let secret = crypto.generate_secret_key()?;
        let mut csr_buf = [0u8; 256];
        let csr = secret.csr(&mut csr_buf)?;
        let mut key = CanonPkcSecretKey::new();
        secret.write_canon(&mut key)?;

        let mut noc_buf = [0u8; MAX_CERT_TLV_AND_ASN1_LEN];
        let mut noc_gen = NocGenerator::create(icac_key.reference(), &rcac, &icac, &mut noc_buf)?;
        let noc = noc_gen.generate(crypto, csr, CONTROLLER_NODE_ID, &[], VALID_FOREVER)?.to_vec();

        let mut ipk = CanonAeadKey::new();
        crypto.rand()?.fill_bytes(ipk.access_mut());

        Ok(Ca { fabric_id, rcac, icac, icac_key, noc, key, ipk })
    }

    /// The fabric kept in `dir`, if there is one.
    pub fn load(dir: &Path) -> io::Result<Option<Self>> {
        let bytes = match fs::read(dir.join(FABRIC_FILE)) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        Self::decode(&bytes).map(Some).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "fabric.bin is not a fabric"))
    }

    /// Kept in `dir`, in place of what was there.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        write_atomic(dir, FABRIC_FILE, &self.encode())
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&self.fabric_id.to_be_bytes());
        for blob in [&self.rcac[..], &self.icac, self.icac_key.access(), &self.noc, self.key.access(), self.ipk.access()] {
            out.extend_from_slice(&(blob.len() as u16).to_be_bytes());
            out.extend_from_slice(blob);
        }
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let rest = bytes.strip_prefix(&MAGIC[..])?;
        let fabric_id = u64::from_be_bytes(rest.get(..8)?.try_into().ok()?);
        let mut rest = &rest[8..];
        let mut blob = || -> Option<Vec<u8>> {
            let len = u16::from_be_bytes(rest.get(..2)?.try_into().ok()?) as usize;
            let b = rest.get(2..2 + len)?.to_vec();
            rest = &rest[2 + len..];
            Some(b)
        };
        let (rcac, icac, icac_key_b, noc, key_b, ipk_b) = (blob()?, blob()?, blob()?, blob()?, blob()?, blob()?);
        let mut icac_key = CanonPkcSecretKey::new();
        icac_key.try_load_from_slice(&icac_key_b).ok()?;
        let mut key = CanonPkcSecretKey::new();
        key.try_load_from_slice(&key_b).ok()?;
        let mut ipk = CanonAeadKey::new();
        ipk.try_load_from_slice(&ipk_b).ok()?;
        Some(Ca { fabric_id, rcac, icac, icac_key, noc, key, ipk })
    }
}

/// A commissioned node and the code it was paired by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairedNode {
    pub node_id: u64,
    pub code: String,
}

/// The nodes kept in `dir` (none when there is no list yet).
pub fn load_nodes(dir: &Path) -> io::Result<Vec<PairedNode>> {
    let text = match fs::read_to_string(dir.join(NODES_FILE)) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    Ok(parse_nodes(&text))
}

/// The nodes kept in `dir`, in place of the list there.
pub fn save_nodes(dir: &Path, nodes: &[PairedNode]) -> io::Result<()> {
    let text: String = nodes.iter().map(|n| format!("{:016x} {}\n", n.node_id, n.code)).collect();
    write_atomic(dir, NODES_FILE, text.as_bytes())
}

/// "<node id hex> <code>" per line; other lines are skipped.
fn parse_nodes(text: &str) -> Vec<PairedNode> {
    text.lines()
        .filter_map(|line| {
            let (id, code) = line.trim().split_once(' ')?;
            Some(PairedNode { node_id: u64::from_str_radix(id, 16).ok()?, code: code.trim().to_string() })
        })
        .collect()
}

/// The next free node id.
pub fn next_node_id(nodes: &[PairedNode]) -> u64 {
    nodes.iter().map(|n| n.node_id + 1).max().unwrap_or(FIRST_NODE_ID).max(FIRST_NODE_ID)
}

/// `bytes` into `dir/name` through a temporary file, so a power cut leaves the old or the new.
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    let mut file = fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, dir.join(name))
}

/// The error rs-matter reports for a failed file operation.
pub fn io_error(err: io::Error) -> Error {
    log::warn!("matter: {err}");
    ErrorCode::StdIoError.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_round_trip_and_next_id() {
        let nodes = parse_nodes("0000000000000010 3497-011-2332\nbroken\n0000000000000011 1234-567-8901\n");
        assert_eq!(nodes.len(), 2);
        assert_eq!((nodes[0].node_id, nodes[0].code.as_str()), (0x10, "3497-011-2332"));
        assert_eq!(next_node_id(&nodes), 0x12);
        assert_eq!(next_node_id(&[]), FIRST_NODE_ID);
    }
}
