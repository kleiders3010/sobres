//! Versioned binary encoding for everything that is stored or sent.
//!
//! Every encoded value starts with a 2-byte magic tag and a u16 format version
//! (principle 8). Decoding rejects unknown tags and versions newer than we know,
//! and never panics on malformed input.

use serde::{de::DeserializeOwned, Serialize};

use crate::{Error, Result};

/// Maximum size of any single decoded wire value (defense against memory bombs).
pub const MAX_WIRE_BYTES: usize = 8 * 1024 * 1024;

pub fn encode<T: Serialize>(tag: [u8; 2], version: u16, value: &T) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&tag);
    out.extend_from_slice(&version.to_le_bytes());
    let body = postcard::to_stdvec(value).map_err(|e| Error::Encoding(e.to_string()))?;
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decode a value, returning its format version. `max_version` is the newest we understand.
pub fn decode<T: DeserializeOwned>(tag: [u8; 2], max_version: u16, bytes: &[u8]) -> Result<(u16, T)> {
    if bytes.len() > MAX_WIRE_BYTES {
        return Err(Error::Rejected(format!("value too large: {} bytes", bytes.len())));
    }
    if bytes.len() < 4 || bytes[..2] != tag {
        return Err(Error::Encoding("wrong or missing tag".into()));
    }
    let version = u16::from_le_bytes([bytes[2], bytes[3]]);
    if version == 0 {
        return Err(Error::Encoding("version 0".into()));
    }
    if version > max_version {
        return Err(Error::Newer(format!("version {version}, this build reads up to {max_version}")));
    }
    let value = postcard::from_bytes(&bytes[4..]).map_err(|e| Error::Encoding(e.to_string()))?;
    Ok((version, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct T {
        a: u32,
        b: Vec<u8>,
    }

    #[test]
    fn roundtrip_and_rejects() {
        let v = T { a: 7, b: vec![1, 2, 3] };
        let bytes = encode(*b"TT", 1, &v).unwrap();
        assert_eq!(decode::<T>(*b"TT", 1, &bytes).unwrap(), (1, v));
        assert!(decode::<T>(*b"XX", 1, &bytes).is_err());
        let newer = encode(*b"TT", 2, &T { a: 1, b: vec![] }).unwrap();
        assert!(matches!(decode::<T>(*b"TT", 1, &newer), Err(Error::Newer(_))), "newer, not broken");
        assert!(decode::<T>(*b"TT", 1, &bytes[..5]).is_err());
        assert!(decode::<T>(*b"TT", 1, &[]).is_err());
    }
}
