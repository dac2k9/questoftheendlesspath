//! Fog of war bitfield encoding/decoding.
//!
//! The fog is stored as a base64-encoded bitfield where each bit represents
//! one tile on the map grid (MAP_W x MAP_H). Bit = 1 means revealed.
//!
//! Storage: ~1000 bytes base64 for a 100x80 map.

use crate::mapgen::{MAP_W, MAP_H};

/// A fog of war bitfield.
pub struct FogBitfield {
    bits: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

impl FogBitfield {
    /// Create a new fog with everything hidden.
    pub fn new() -> Self {
        let num_bytes = (MAP_W * MAP_H + 7) / 8;
        Self {
            bits: vec![0u8; num_bytes],
            width: MAP_W,
            height: MAP_H,
        }
    }

    /// Decode from base64 string (as stored in Supabase).
    pub fn from_base64(encoded: &str) -> Option<Self> {
        if encoded.is_empty() {
            return Some(Self::new());
        }
        let bits = base64_decode(encoded)?;
        let expected = (MAP_W * MAP_H + 7) / 8;
        if bits.len() < expected {
            return None;
        }
        Some(Self {
            bits,
            width: MAP_W,
            height: MAP_H,
        })
    }

    /// Encode to base64 string for storage.
    pub fn to_base64(&self) -> String {
        base64_encode(&self.bits)
    }

    /// Check if a tile is revealed.
    pub fn is_revealed(&self, x: usize, y: usize) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let idx = y * self.width + x;
        let byte_idx = idx / 8;
        let bit_idx = idx % 8;
        if byte_idx >= self.bits.len() {
            return false;
        }
        (self.bits[byte_idx] >> bit_idx) & 1 == 1
    }

    /// Reveal a single tile. Returns true if it was newly revealed.
    pub fn reveal(&mut self, x: usize, y: usize) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let idx = y * self.width + x;
        let byte_idx = idx / 8;
        let bit_idx = idx % 8;
        if byte_idx >= self.bits.len() {
            return false;
        }
        let was = (self.bits[byte_idx] >> bit_idx) & 1;
        self.bits[byte_idx] |= 1 << bit_idx;
        was == 0
    }

    /// Merge another fog into this one (OR the bits).
    pub fn merge(&mut self, other: &Self) {
        for (a, b) in self.bits.iter_mut().zip(other.bits.iter()) {
            *a |= *b;
        }
    }

    /// Reveal a circular area around a point.
    pub fn reveal_radius(&mut self, cx: usize, cy: usize, radius: usize) -> bool {
        let r = radius as i32;
        let mut any_new = false;
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy > r * r {
                    continue;
                }
                let x = cx as i32 + dx;
                let y = cy as i32 + dy;
                if x >= 0 && x < self.width as i32 && y >= 0 && y < self.height as i32 {
                    if self.reveal(x as usize, y as usize) {
                        any_new = true;
                    }
                }
            }
        }
        any_new
    }

    /// Count revealed tiles.
    pub fn count_revealed(&self) -> usize {
        self.bits
            .iter()
            .map(|b| b.count_ones() as usize)
            .sum()
    }
}

impl Default for FogBitfield {
    fn default() -> Self {
        Self::new()
    }
}

// ── Simple base64 (no external dependency) ────────────

const B64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(B64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(B64_CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(B64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(B64_CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut result = Vec::with_capacity(input.len() * 3 / 4);
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'\n' && b != b'\r' && b != b' ')
        .collect();

    for chunk in bytes.chunks(4) {
        if chunk.len() < 4 {
            break;
        }
        let a = b64_val(chunk[0])?;
        let b = b64_val(chunk[1])?;
        let c = if chunk[2] == b'=' { 0 } else { b64_val(chunk[2])? };
        let d = if chunk[3] == b'=' { 0 } else { b64_val(chunk[3])? };

        let triple = (a << 18) | (b << 12) | (c << 6) | d;
        result.push((triple >> 16) as u8);
        if chunk[2] != b'=' {
            result.push((triple >> 8) as u8);
        }
        if chunk[3] != b'=' {
            result.push(triple as u8);
        }
    }
    Some(result)
}

fn b64_val(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some((c - b'A') as u32),
        b'a'..=b'z' => Some((c - b'a' + 26) as u32),
        b'0'..=b'9' => Some((c - b'0' + 52) as u32),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_and_check() {
        let mut fog = FogBitfield::new();
        assert!(!fog.is_revealed(10, 10));
        assert!(fog.reveal(10, 10));
        assert!(fog.is_revealed(10, 10));
        assert!(!fog.reveal(10, 10)); // already revealed
    }

    #[test]
    fn reveal_radius() {
        let mut fog = FogBitfield::new();
        fog.reveal_radius(50, 40, 5);
        assert!(fog.is_revealed(50, 40));
        assert!(fog.is_revealed(52, 40));
        assert!(fog.is_revealed(50, 43));
        assert!(!fog.is_revealed(50, 46)); // outside radius
        assert!(fog.count_revealed() > 50);
    }

    #[test]
    fn base64_roundtrip() {
        let mut fog = FogBitfield::new();
        fog.reveal_radius(50, 40, 5);
        let count = fog.count_revealed();

        let encoded = fog.to_base64();
        assert!(!encoded.is_empty());

        let decoded = FogBitfield::from_base64(&encoded).unwrap();
        assert_eq!(decoded.count_revealed(), count);
        assert!(decoded.is_revealed(50, 40));
        assert!(!decoded.is_revealed(0, 0));
    }

    #[test]
    fn empty_string_creates_blank() {
        let fog = FogBitfield::from_base64("").unwrap();
        assert_eq!(fog.count_revealed(), 0);
    }
}
