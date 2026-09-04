//! Standard padded RFC 4648 base64 (SPEC §4.3).
//!
//! Decoding is strict: length must be a multiple of four, padding must be
//! well formed, and the unused bits of a truncated final group must be zero.
//! That last rule is what makes the encoding canonical — without it `AA==`
//! and `AB==` would both decode to the same byte, so a repository would have
//! two spellings of one value and byte-identical convergence would be lost.

use crate::error::{self, Result};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn decode_symbol(byte: u8) -> Option<u32> {
    let index = match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(index))
}

pub fn decode(text: &str) -> Result<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(error::not_canonical_base64());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut chunks = bytes.chunks_exact(4).peekable();
    while let Some(chunk) = chunks.next() {
        let is_last = chunks.peek().is_none();
        // Padding may appear only in the final group.
        let pad = chunk.iter().filter(|&&b| b == b'=').count();
        if pad > 0 && (!is_last || chunk[..4 - pad].contains(&b'=')) {
            return Err(error::not_canonical_base64());
        }
        let mut acc = 0u32;
        for &byte in &chunk[..4 - pad] {
            acc = (acc << 6) | decode_symbol(byte).ok_or_else(error::not_canonical_base64)?;
        }
        match pad {
            0 => {
                out.push((acc >> 16) as u8);
                out.push((acc >> 8) as u8);
                out.push(acc as u8);
            }
            1 => {
                // 18 significant bits carried in 3 symbols; low 2 must be zero.
                if acc & 0b11 != 0 {
                    return Err(error::not_canonical_base64());
                }
                acc >>= 2;
                out.push((acc >> 8) as u8);
                out.push(acc as u8);
            }
            2 => {
                // 12 significant bits carried in 2 symbols; low 4 must be zero.
                if acc & 0b1111 != 0 {
                    return Err(error::not_canonical_base64());
                }
                out.push((acc >> 4) as u8);
            }
            _ => return Err(error::not_canonical_base64()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_rfc_4648_vectors() {
        for (raw, encoded) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(raw), encoded, "encoding {raw:?}");
            assert_eq!(decode(encoded).unwrap(), raw, "decoding {encoded:?}");
        }
    }

    #[test]
    fn round_trips_arbitrary_bytes() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        for len in 0..bytes.len() {
            let slice = &bytes[..len];
            assert_eq!(decode(&encode(slice)).unwrap(), slice, "length {len}");
        }
    }

    #[test]
    fn rejects_non_canonical_trailing_bits() {
        // Two padding characters carry a 12-bit group for a single byte, so the
        // low 4 bits are unused and must be zero. "Zg==" is the canonical
        // spelling of b"f"; "Zh==" sets one of those unused bits and decodes to
        // the same byte, so it is a second spelling and must be refused.
        assert!(decode("Zg==").is_ok());
        assert!(decode("Zh==").is_err());
        assert!(decode("Zm8=").is_ok());
        assert!(decode("Zm9=").is_err());
    }

    #[test]
    fn rejects_malformed_input() {
        for text in [
            "Zg=",      // length not a multiple of four
            "Zg",       // unpadded
            "Zm9vYg=",  // truncated padding
            "Z===",     // over-padded
            "Zg==Zg==", // padding before the final group
            "Zm9v!g==", // symbol outside the alphabet
            "Zm9 v",    // whitespace
            "Zm9v-g==", // URL-safe alphabet is not standard base64
        ] {
            assert!(decode(text).is_err(), "{text:?} should be rejected");
        }
    }
}
