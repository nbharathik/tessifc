// SPDX-License-Identifier: Apache-2.0
//! FNV-1a, a small non-cryptographic hash used only for string interning.
//! Not collision-resistant; the intern table compares bytes on every probe.

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over a byte slice.
#[inline]
pub fn fx_hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // The published FNV-1a 64-bit test vectors.
        assert_eq!(fx_hash_bytes(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fx_hash_bytes(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fx_hash_bytes(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn distinct_inputs_mostly_differ() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..10_000u32 {
            seen.insert(fx_hash_bytes(format!("IfcThing{i}").as_bytes()));
        }
        assert_eq!(seen.len(), 10_000);
    }
}
