// SPDX-License-Identifier: Apache-2.0
//! Interned string storage with lazy decoding.
//! Each distinct byte sequence is stored once, exactly as it appeared in the
//! file; STEP escapes are decoded only when a string is actually read.

use crate::hash::fx_hash_bytes;

/// Handle to an interned byte sequence.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StrId(pub u32);

impl StrId {
    /// The empty string, always present at index 0.
    pub const EMPTY: StrId = StrId(0);
}

/// Deduplicating store of the raw byte sequences found in a file.
#[derive(Debug)]
pub struct StringArena {
    bytes: Vec<u8>,
    /// `offsets[i] .. offsets[i + 1]` is the byte range of string `i`.
    offsets: Vec<u32>,
    /// Open-addressed table of `id + 1`, 0 meaning empty.
    slots: Vec<u32>,
    mask: usize,
    live: usize,
}

impl Default for StringArena {
    fn default() -> Self {
        Self::new()
    }
}

impl StringArena {
    /// An arena containing only the empty string.
    pub fn new() -> Self {
        Self::with_capacity(1024)
    }

    /// An arena sized for roughly `cap` distinct strings.
    pub fn with_capacity(cap: usize) -> Self {
        let slots_len = (cap.next_power_of_two() * 2).max(64);
        let mut arena = StringArena {
            bytes: Vec::with_capacity(cap * 16),
            offsets: vec![0],
            slots: vec![0; slots_len],
            mask: slots_len - 1,
            live: 0,
        };
        // Index 0 is always the empty string, so StrId::EMPTY is free.
        arena.intern(b"");
        arena
    }

    /// Number of distinct strings held.
    pub fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    /// True when the arena holds nothing but the empty string.
    pub fn is_empty(&self) -> bool {
        self.len() <= 1
    }

    /// Total bytes of string data, excluding bookkeeping.
    pub fn bytes_len(&self) -> usize {
        self.bytes.len()
    }

    /// Store `s`, returning the existing id if it is already present.
    pub fn intern(&mut self, s: &[u8]) -> StrId {
        let hash = fx_hash_bytes(s);
        let mut probe = (hash as usize) & self.mask;
        loop {
            let slot = self.slots[probe];
            if slot == 0 {
                break;
            }
            let id = slot - 1;
            if self.get(StrId(id)) == s {
                return StrId(id);
            }
            probe = (probe + 1) & self.mask;
        }

        // Ids and offsets are 32 bits; a full arena keeps the empty string rather
        // than filling a slot with an id that was never appended.
        let fits = u32::try_from(self.len()).is_ok_and(|count| count < u32::MAX)
            && self
                .bytes
                .len()
                .checked_add(s.len())
                .is_some_and(|total| u32::try_from(total).is_ok());
        if !fits {
            return StrId::EMPTY;
        }

        let id = self.len() as u32;
        self.bytes.extend_from_slice(s);
        self.offsets.push(self.bytes.len() as u32);
        self.slots[probe] = id + 1;
        self.live += 1;
        if self.live * 4 > self.slots.len() * 3 {
            self.grow();
        }
        StrId(id)
    }

    /// The raw bytes of a string, still escaped; out-of-range ids yield `b""`.
    pub fn get(&self, id: StrId) -> &[u8] {
        let i = id.0 as usize;
        match (self.offsets.get(i), self.offsets.get(i + 1)) {
            (Some(&start), Some(&end)) if start <= end => {
                self.bytes.get(start as usize..end as usize).unwrap_or(b"")
            }
            _ => b"",
        }
    }

    /// Decode a string to UTF-8, resolving STEP escape sequences; allocates.
    pub fn decode(&self, id: StrId) -> String {
        decode(self.get(id))
    }

    fn grow(&mut self) {
        let new_len = self.slots.len() * 2;
        let mut slots = vec![0u32; new_len];
        let mask = new_len - 1;
        for i in 0..self.len() {
            let id = i as u32;
            let hash = fx_hash_bytes(self.get(StrId(id)));
            let mut probe = (hash as usize) & mask;
            while slots[probe] != 0 {
                probe = (probe + 1) & mask;
            }
            slots[probe] = id + 1;
        }
        self.slots = slots;
        self.mask = mask;
    }
}

/// Decode one STEP string literal body (the text between the quotes) into UTF-8.
/// Malformed escapes pass through as literal text; lone surrogates become U+FFFD.
///
/// ```
/// # use tessifc_step::strings::decode;
/// assert_eq!(decode(b"Ren\\X2\\00E9\\X0\\"), "René");
/// assert_eq!(decode(b"it''s"), "it's");
/// assert_eq!(decode(b"\\X\\E4"), "ä");
/// assert_eq!(decode(b"a\\X2\\D83DDE00\\X0\\b"), "a\u{1F600}b");
/// ```
pub fn decode(raw: &[u8]) -> String {
    // Fast path: plain ASCII with nothing to unescape.
    if !raw.iter().any(|&b| b == b'\\' || b == b'\'' || b >= 0x80) {
        return String::from_utf8_lossy(raw).into_owned();
    }

    let mut out = String::with_capacity(raw.len());
    let mut i = 0usize;
    // The ISO 8859 page selected by \P<x>\. Page 1 is Latin-1, the default.
    let mut page: u8 = 1;

    while i < raw.len() {
        let b = raw[i];
        match b {
            b'\'' if raw.get(i + 1) == Some(&b'\'') => {
                out.push('\'');
                i += 2;
            }
            b'\\' => {
                let (consumed, ok) = decode_escape(raw, i, &mut page, &mut out);
                if ok {
                    i += consumed;
                } else {
                    // Unrecognised escape: emit the backslash literally.
                    out.push('\\');
                    i += 1;
                }
            }
            0x00..=0x7f => {
                out.push(b as char);
                i += 1;
            }
            _ => {
                // Raw non-ASCII byte: exporters emit UTF-8 and Latin-1 directly.
                match utf8_len(b) {
                    Some(len) if i + len <= raw.len() => {
                        match core::str::from_utf8(&raw[i..i + len]) {
                            Ok(s) => {
                                out.push_str(s);
                                i += len;
                            }
                            Err(_) => {
                                out.push(latin1(b, page));
                                i += 1;
                            }
                        }
                    }
                    _ => {
                        out.push(latin1(b, page));
                        i += 1;
                    }
                }
            }
        }
    }
    out
}

/// Handle one backslash escape at `i`: (bytes consumed, recognised).
fn decode_escape(raw: &[u8], i: usize, page: &mut u8, out: &mut String) -> (usize, bool) {
    match raw.get(i + 1) {
        Some(b'\\') => {
            out.push('\\');
            (2, true)
        }
        Some(b'S') if raw.get(i + 2) == Some(&b'\\') => match raw.get(i + 3) {
            Some(&c) => {
                out.push(latin1(c.wrapping_add(0x80), *page));
                (4, true)
            }
            None => (0, false),
        },
        Some(b'P') => match (raw.get(i + 2), raw.get(i + 3)) {
            (Some(&p), Some(b'\\')) if p.is_ascii_uppercase() => {
                // \PA\ selects page 1, \PB\ page 2, and so on.
                *page = p - b'A' + 1;
                (4, true)
            }
            _ => (0, false),
        },
        Some(b'N') if raw.get(i + 2) == Some(&b'\\') => {
            out.push('\n');
            (3, true)
        }
        Some(b'F') if raw.get(i + 2) == Some(&b'\\') => {
            out.push('\u{000c}');
            (3, true)
        }
        Some(b'T') if raw.get(i + 2) == Some(&b'\\') => {
            out.push('\t');
            (3, true)
        }
        Some(b'X') => match raw.get(i + 2) {
            // \X\hh: one byte in the current page.
            Some(b'\\') => match hex_byte(raw, i + 3) {
                Some(v) => {
                    out.push(latin1(v, *page));
                    (5, true)
                }
                None => (0, false),
            },
            // \X2\hhhh...\X0\: UTF-16 code units.
            Some(b'2') if raw.get(i + 3) == Some(&b'\\') => {
                match decode_hex_run(raw, i + 4, 4, out) {
                    Some(n) => (n, true),
                    None => (0, false),
                }
            }
            // \X4\hhhhhhhh...\X0\: code points.
            Some(b'4') if raw.get(i + 3) == Some(&b'\\') => {
                match decode_hex_run(raw, i + 4, 8, out) {
                    Some(n) => (n, true),
                    None => (0, false),
                }
            }
            _ => (0, false),
        },
        _ => (0, false),
    }
}

/// Decode a run of fixed-width hex groups until `\X0\`; returns bytes consumed
/// from the introducer, or `None` when the very first group is malformed.
fn decode_hex_run(raw: &[u8], start: usize, width: usize, out: &mut String) -> Option<usize> {
    let mut i = start;
    let mut groups = 0usize;
    // High surrogate waiting for its pair, for the \X2\ form.
    let mut pending_high: Option<u32> = None;

    loop {
        if raw.get(i) == Some(&b'\\')
            && raw.get(i + 1) == Some(&b'X')
            && raw.get(i + 2) == Some(&b'0')
            && raw.get(i + 3) == Some(&b'\\')
        {
            i += 4;
            break;
        }
        let Some(group) = raw.get(i..i + width) else {
            break;
        };
        let Some(value) = hex_value(group) else { break };
        i += width;
        groups += 1;

        if width == 4 {
            match (pending_high, value) {
                (None, 0xd800..=0xdbff) => {
                    pending_high = Some(value);
                    continue;
                }
                (Some(high), 0xdc00..=0xdfff) => {
                    let cp = 0x10000 + ((high - 0xd800) << 10) + (value - 0xdc00);
                    push_code_point(out, cp);
                    pending_high = None;
                    continue;
                }
                (Some(_), _) => {
                    // A high surrogate that was never completed.
                    out.push('\u{fffd}');
                    pending_high = None;
                }
                _ => {}
            }
        }
        push_code_point(out, value);
    }
    if pending_high.is_some() {
        out.push('\u{fffd}');
    }
    if groups == 0 && i == start {
        // Neither a group nor the terminator: not a hex run at all.
        return None;
    }
    // 4 bytes for the "\X2\" or "\X4\" introducer that the caller passed over.
    Some(i - start + 4)
}

fn push_code_point(out: &mut String, cp: u32) {
    out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
}

/// Map a byte to a character using the selected ISO 8859 page.
/// Other pages are approximated by Latin-1 to keep tables out of the WASM binary.
fn latin1(b: u8, _page: u8) -> char {
    b as char
}

fn utf8_len(first: u8) -> Option<usize> {
    match first {
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

fn hex_byte(raw: &[u8], at: usize) -> Option<u8> {
    let group = raw.get(at..at + 2)?;
    hex_value(group).map(|v| v as u8)
}

fn hex_value(group: &[u8]) -> Option<u32> {
    let mut acc: u32 = 0;
    for &c in group {
        let digit = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return None,
        };
        acc = (acc << 4) | digit as u32;
    }
    Some(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_deduplicates() {
        let mut arena = StringArena::new();
        let a = arena.intern(b"IfcWall");
        let b = arena.intern(b"IfcWall");
        let c = arena.intern(b"IfcSlab");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(arena.get(a), b"IfcWall");
        assert_eq!(arena.len(), 3); // empty, IfcWall, IfcSlab
    }

    #[test]
    fn interning_survives_growth() {
        let mut arena = StringArena::with_capacity(4);
        let ids: Vec<_> = (0..5000)
            .map(|i| arena.intern(format!("s{i}").as_bytes()))
            .collect();
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(arena.get(*id), format!("s{i}").as_bytes());
        }
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(arena.intern(format!("s{i}").as_bytes()), *id);
        }
    }

    #[test]
    fn out_of_range_id_is_empty_not_a_panic() {
        let arena = StringArena::new();
        assert_eq!(arena.get(StrId(9999)), b"");
        assert_eq!(arena.decode(StrId(u32::MAX)), "");
    }

    #[test]
    fn plain_ascii_passes_through() {
        assert_eq!(decode(b"Basic Wall:Interior"), "Basic Wall:Interior");
    }

    #[test]
    fn doubled_apostrophe() {
        assert_eq!(decode(b"it''s"), "it's");
        assert_eq!(decode(b"''"), "'");
    }

    #[test]
    fn x_escapes() {
        assert_eq!(decode(b"\\X\\E4"), "ä");
        assert_eq!(decode(b"\\X2\\00E9\\X0\\"), "é");
        assert_eq!(decode(b"\\X2\\00E900E9\\X0\\"), "éé");
        assert_eq!(decode(b"\\X4\\0001F600\\X0\\"), "\u{1F600}");
    }

    #[test]
    fn surrogate_pairs_combine() {
        assert_eq!(decode(b"\\X2\\D83DDE00\\X0\\"), "\u{1F600}");
    }

    #[test]
    fn lone_surrogate_becomes_replacement() {
        assert_eq!(decode(b"\\X2\\D83D\\X0\\"), "\u{fffd}");
    }

    #[test]
    fn s_escape_shifts_high_bit() {
        // \S\d is 'd' + 128 = 0xE4 = a-diaeresis in Latin-1.
        assert_eq!(decode(b"\\S\\d"), "ä");
    }

    #[test]
    fn control_directives() {
        assert_eq!(decode(b"a\\N\\b"), "a\nb");
        assert_eq!(decode(b"a\\T\\b"), "a\tb");
    }

    #[test]
    fn malformed_escapes_are_kept_literally() {
        assert_eq!(decode(b"\\Q\\x"), "\\Q\\x");
        assert_eq!(decode(b"\\X2\\ZZZZ\\X0\\"), "\\X2\\ZZZZ\\X0\\");
        assert_eq!(decode(b"\\"), "\\");
        assert_eq!(decode(b"\\X\\"), "\\X\\");
    }

    #[test]
    fn truncated_escape_does_not_panic() {
        for cut in 0..12 {
            let full = b"\\X2\\00E9\\X0\\";
            let _ = decode(&full[..cut.min(full.len())]);
        }
        let _ = decode(b"\\X4\\0001");
        let _ = decode(b"\\P");
        let _ = decode(b"\\S\\");
    }

    #[test]
    fn raw_utf8_bytes_are_accepted() {
        assert_eq!(decode("Hauptstraße".as_bytes()), "Hauptstraße");
    }

    #[test]
    fn raw_latin1_bytes_fall_back() {
        // 0xE4 alone is not valid UTF-8; treat it as Latin-1.
        assert_eq!(decode(&[b'a', 0xe4, b'b']), "aäb");
    }
}
