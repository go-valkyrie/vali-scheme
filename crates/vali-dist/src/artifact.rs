//! The artifact container (ADRs 0001 #5 and 0012 #8).
//!
//! An artifact is eight magic bytes, the format version it was written
//! with, the format features it uses, and named sections, each marked
//! required or optional, with its length. A reader refuses an artifact
//! that uses a feature it does not know, or has a required section it
//! does not know, naming it; it skips optional sections it does not know.
//! The version is informational: an artifact from a newer Vali that uses
//! no new feature still loads.

use crate::bytes::{DecodeError, Reader, Writer};

/// The bytes every artifact starts with.
pub const MAGIC: &[u8; 8] = b"\0VALI\0A\n";

/// The format version this Vali writes.
pub const FORMAT_VERSION: u32 = 2;

/// The format features this Vali knows. None yet: version 1 is the base.
pub const KNOWN_FEATURES: &[&str] = &[];

/// One section of an artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub name: Box<str>,
    /// Whether a reader that does not know it must refuse the artifact.
    pub required: bool,
    pub bytes: Vec<u8>,
}

/// An artifact's sections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    /// The format version it was written with.
    pub version: u32,
    /// The format features it uses.
    pub features: Vec<Box<str>>,
    pub sections: Vec<Section>,
}

impl Default for Artifact {
    fn default() -> Artifact {
        Artifact {
            version: FORMAT_VERSION,
            features: Vec::new(),
            sections: Vec::new(),
        }
    }
}

impl Artifact {
    /// Adds a section, replacing one of the same name.
    pub fn insert(&mut self, name: &str, required: bool, bytes: Vec<u8>) {
        self.sections.retain(|s| &*s.name != name);
        self.sections.push(Section {
            name: name.into(),
            required,
            bytes,
        });
    }

    /// The bytes of the section named `name`, if there is one.
    pub fn section(&self, name: &str) -> Option<&[u8]> {
        self.sections
            .iter()
            .find(|s| &*s.name == name)
            .map(|s| &s.bytes[..])
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.raw(MAGIC);
        w.u32(self.version);
        w.usize(self.features.len());
        for feature in &self.features {
            w.str(feature);
        }
        w.usize(self.sections.len());
        for section in &self.sections {
            w.str(&section.name);
            w.bool(section.required);
            w.bytes(&section.bytes);
        }
        w.finish()
    }

    /// Decodes an artifact whose required sections are all among `known`;
    /// optional sections not among them are dropped.
    pub fn decode(bytes: &[u8], known: &[&str]) -> Result<Artifact, DecodeError> {
        let mut r = Reader::new(bytes);
        if r.raw(MAGIC.len()).ok() != Some(&MAGIC[..]) {
            return Err(DecodeError {
                offset: 0,
                message: "not a Vali artifact".into(),
            });
        }
        let version = r.u32()?;
        let n = r.count(1)?;
        let mut features = Vec::with_capacity(n);
        for _ in 0..n {
            let feature = r.str()?;
            if !KNOWN_FEATURES.contains(&feature) {
                return Err(r.error(format!(
                    "the artifact uses the format feature {feature}, which this Vali does not know"
                )));
            }
            features.push(feature.into());
        }
        let n = r.count(3)?;
        let mut sections = Vec::with_capacity(n);
        for _ in 0..n {
            let name = r.str()?;
            let required = r.bool()?;
            let bytes = r.bytes()?;
            if !known.contains(&name) {
                if required {
                    return Err(r.error(format!(
                        "the artifact needs the section {name}, which this Vali does not know"
                    )));
                }
                continue;
            }
            if sections.iter().any(|s: &Section| &*s.name == name) {
                return Err(r.error(format!("the section {name} appears twice")));
            }
            sections.push(Section {
                name: name.into(),
                required,
                bytes: bytes.to_vec(),
            });
        }
        r.end()?;
        Ok(Artifact {
            version,
            features,
            sections,
        })
    }
}
