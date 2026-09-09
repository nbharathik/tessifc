// SPDX-License-Identifier: Apache-2.0
//! The value tape: a compact, append-only encoding of parsed STEP arguments.
//! Each value is a one-byte tag plus payload (see the `TAG_*` constants). The
//! layout is stable so tape bytes can be handed to another worker as they are.

/// No value: the source wrote `$`.
pub const TAG_NULL: u8 = 0;
/// A derived attribute slot: the source wrote `*`.
pub const TAG_DERIVED: u8 = 1;
/// An integer, zigzag LEB128.
pub const TAG_INT: u8 = 2;
/// A real, eight little-endian bytes.
pub const TAG_REAL: u8 = 3;
/// A string, LEB128 id into the arena.
pub const TAG_STR: u8 = 4;
/// An enumeration symbol, LEB128 id into the arena, without the dots.
pub const TAG_ENUM: u8 = 5;
/// A reference to another instance, LEB128 express id.
pub const TAG_REF: u8 = 6;
/// Opening bracket of a list.
pub const TAG_LIST_BEGIN: u8 = 7;
/// Closing bracket of a list.
pub const TAG_LIST_END: u8 = 8;
/// A typed value: LEB128 type name id followed by exactly one value.
pub const TAG_TYPED: u8 = 9;
/// A binary literal, LEB128 id of its raw hexadecimal text.
pub const TAG_BINARY: u8 = 10;
/// One leaf of a complex instance: LEB128 class id followed by a bracketed list.
pub const TAG_LEAF: u8 = 11;

use crate::strings::StrId;
use tessifc_schema::ClassId;

/// One value read off the tape.
/// Lists and leaves are reported as markers, not collections, so reading never allocates.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum RawValue {
    /// `$`.
    Null,
    /// `*`.
    Derived,
    /// An integer literal.
    Int(i64),
    /// A real literal.
    Real(f64),
    /// A string literal, still escaped; decode through the arena.
    Str(StrId),
    /// An enumeration symbol without its dots, upper case as written.
    Enum(StrId),
    /// A reference to another instance.
    Ref(u32),
    /// The start of a list. Read values until [`RawValue::ListEnd`].
    ListStart,
    /// The end of a list.
    ListEnd,
    /// A typed value. Exactly one value follows.
    Typed(StrId),
    /// A binary literal, held as its raw hexadecimal text.
    Binary(StrId),
    /// One leaf of a complex instance. A list follows, holding its arguments.
    Leaf(ClassId),
}

/// Append-only writer over a byte buffer.
#[derive(Debug, Default)]
pub struct TapeWriter {
    bytes: Vec<u8>,
}

impl TapeWriter {
    /// A writer with room for `cap` bytes.
    pub fn with_capacity(cap: usize) -> Self {
        TapeWriter {
            bytes: Vec::with_capacity(cap),
        }
    }

    /// Current length, which is the offset the next value will be written at.
    #[inline]
    pub fn pos(&self) -> usize {
        self.bytes.len()
    }

    /// Consume the writer and yield the tape.
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }

    /// Borrow what has been written.
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// Write `$`.
    #[inline]
    pub fn null(&mut self) {
        self.bytes.push(TAG_NULL);
    }

    /// Write `*`.
    #[inline]
    pub fn derived(&mut self) {
        self.bytes.push(TAG_DERIVED);
    }

    /// Write an integer.
    #[inline]
    pub fn int(&mut self, v: i64) {
        self.bytes.push(TAG_INT);
        self.uleb(zigzag(v));
    }

    /// Write a real.
    #[inline]
    pub fn real(&mut self, v: f64) {
        self.bytes.push(TAG_REAL);
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a string reference.
    #[inline]
    pub fn str(&mut self, id: StrId) {
        self.bytes.push(TAG_STR);
        self.uleb(id.0 as u64);
    }

    /// Write an enumeration symbol reference.
    #[inline]
    pub fn enum_symbol(&mut self, id: StrId) {
        self.bytes.push(TAG_ENUM);
        self.uleb(id.0 as u64);
    }

    /// Write an instance reference.
    #[inline]
    pub fn reference(&mut self, express_id: u32) {
        self.bytes.push(TAG_REF);
        self.uleb(express_id as u64);
    }

    /// Write a list opening bracket.
    #[inline]
    pub fn list_begin(&mut self) {
        self.bytes.push(TAG_LIST_BEGIN);
    }

    /// Write a list closing bracket.
    #[inline]
    pub fn list_end(&mut self) {
        self.bytes.push(TAG_LIST_END);
    }

    /// Write the head of a typed value. Exactly one value must follow.
    #[inline]
    pub fn typed(&mut self, type_name: StrId) {
        self.bytes.push(TAG_TYPED);
        self.uleb(type_name.0 as u64);
    }

    /// Write a binary literal reference.
    #[inline]
    pub fn binary(&mut self, id: StrId) {
        self.bytes.push(TAG_BINARY);
        self.uleb(id.0 as u64);
    }

    /// Write the head of a complex-instance leaf. A list must follow.
    #[inline]
    pub fn leaf(&mut self, class: ClassId) {
        self.bytes.push(TAG_LEAF);
        self.uleb(class as u64);
    }

    /// Discard everything written after `pos`, to abandon a failed record.
    pub fn truncate(&mut self, pos: usize) {
        self.bytes.truncate(pos);
    }

    /// Insert a single tag byte at `pos`, shifting what follows.
    /// Retrofits a `LIST_BEGIN` when a typed value turns out to wrap several values.
    pub fn insert_tag_at(&mut self, pos: usize, tag: u8) {
        if pos <= self.bytes.len() {
            self.bytes.insert(pos, tag);
        }
    }

    #[inline]
    fn uleb(&mut self, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.bytes.push(byte);
                return;
            }
            self.bytes.push(byte | 0x80);
        }
    }
}

/// Reader over a tape slice; every read is bounds checked, a corrupt tape yields `None`.
#[derive(Copy, Clone, Debug)]
pub struct Cursor<'a> {
    tape: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    /// A cursor over the whole slice.
    pub fn new(tape: &'a [u8]) -> Self {
        Cursor { tape, pos: 0 }
    }

    /// A cursor positioned at `offset` within `tape`.
    pub fn at(tape: &'a [u8], offset: usize) -> Self {
        Cursor {
            tape,
            pos: offset.min(tape.len()),
        }
    }

    /// Current byte offset.
    #[inline]
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// True once every byte has been consumed.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.pos >= self.tape.len()
    }

    /// Read the next value.
    pub fn read(&mut self) -> Option<RawValue> {
        let tag = *self.tape.get(self.pos)?;
        self.pos += 1;
        let value = match tag {
            TAG_NULL => RawValue::Null,
            TAG_DERIVED => RawValue::Derived,
            TAG_INT => RawValue::Int(unzigzag(self.uleb()?)),
            TAG_REAL => {
                let bytes = self.tape.get(self.pos..self.pos + 8)?;
                self.pos += 8;
                let mut buf = [0u8; 8];
                buf.copy_from_slice(bytes);
                RawValue::Real(f64::from_le_bytes(buf))
            }
            TAG_STR => RawValue::Str(StrId(self.uleb()? as u32)),
            TAG_ENUM => RawValue::Enum(StrId(self.uleb()? as u32)),
            TAG_REF => RawValue::Ref(self.uleb()? as u32),
            TAG_LIST_BEGIN => RawValue::ListStart,
            TAG_LIST_END => RawValue::ListEnd,
            TAG_TYPED => RawValue::Typed(StrId(self.uleb()? as u32)),
            TAG_BINARY => RawValue::Binary(StrId(self.uleb()? as u32)),
            TAG_LEAF => RawValue::Leaf(self.uleb()? as ClassId),
            _ => return None,
        };
        Some(value)
    }

    /// Read the next value without consuming it.
    pub fn peek(&self) -> Option<RawValue> {
        let mut probe = *self;
        probe.read()
    }

    /// Skip exactly one value, descending through nested structures.
    /// Returns `false` if the tape ended early, leaving the cursor at the end.
    pub fn skip_value(&mut self) -> bool {
        // Looping instead of recursing keeps a corrupt tape from overflowing the stack.
        loop {
            let Some(value) = self.read() else {
                return false;
            };
            match value {
                RawValue::ListStart => return self.skip_to_list_end(),
                // A typed value wraps one value; a leaf is followed by its argument list.
                RawValue::Typed(_) | RawValue::Leaf(_) => {}
                RawValue::ListEnd => return false,
                _ => return true,
            }
        }
    }

    /// Consume values until the list opened by the caller closes.
    fn skip_to_list_end(&mut self) -> bool {
        let mut depth = 1usize;
        while depth > 0 {
            match self.read() {
                Some(RawValue::ListStart) => depth += 1,
                Some(RawValue::ListEnd) => depth -= 1,
                Some(_) => {}
                None => return false,
            }
        }
        true
    }

    /// Skip `n` values, stopping early at the end of the tape.
    pub fn skip_values(&mut self, n: usize) -> bool {
        for _ in 0..n {
            if !self.skip_value() {
                return false;
            }
        }
        true
    }

    #[inline]
    fn uleb(&mut self) -> Option<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            let byte = *self.tape.get(self.pos)?;
            self.pos += 1;
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
    }
}

#[inline]
fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[inline]
fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_round_trips() {
        for v in [
            0i64,
            1,
            -1,
            2,
            -2,
            i64::MAX,
            i64::MIN,
            123456789,
            -987654321,
        ] {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
    }

    #[test]
    fn small_negative_integers_cost_one_byte() {
        let mut w = TapeWriter::default();
        w.int(-1);
        assert_eq!(w.as_slice().len(), 2); // tag + one payload byte
    }

    #[test]
    fn values_round_trip() {
        let mut w = TapeWriter::default();
        w.null();
        w.derived();
        w.int(-42);
        w.real(1.5e-3);
        w.str(StrId(7));
        w.enum_symbol(StrId(9));
        w.reference(4_000_000_000);
        w.binary(StrId(3));
        let tape = w.finish();

        let mut c = Cursor::new(&tape);
        assert_eq!(c.read(), Some(RawValue::Null));
        assert_eq!(c.read(), Some(RawValue::Derived));
        assert_eq!(c.read(), Some(RawValue::Int(-42)));
        assert_eq!(c.read(), Some(RawValue::Real(1.5e-3)));
        assert_eq!(c.read(), Some(RawValue::Str(StrId(7))));
        assert_eq!(c.read(), Some(RawValue::Enum(StrId(9))));
        assert_eq!(c.read(), Some(RawValue::Ref(4_000_000_000)));
        assert_eq!(c.read(), Some(RawValue::Binary(StrId(3))));
        assert_eq!(c.read(), None);
    }

    #[test]
    fn reals_survive_exactly() {
        let values = [
            0.0,
            -0.0,
            1.0,
            -1.0,
            f64::MIN_POSITIVE,
            1e308,
            -1e-308,
            0.1 + 0.2,
        ];
        let mut w = TapeWriter::default();
        for v in values {
            w.real(v);
        }
        let tape = w.finish();
        let mut c = Cursor::new(&tape);
        for v in values {
            match c.read() {
                Some(RawValue::Real(got)) => assert_eq!(got.to_bits(), v.to_bits()),
                other => panic!("expected a real, got {other:?}"),
            }
        }
    }

    #[test]
    fn skipping_a_nested_list_lands_on_the_sibling() {
        // ((1,2),(3)) followed by the integer 99
        let mut w = TapeWriter::default();
        w.list_begin();
        w.list_begin();
        w.int(1);
        w.int(2);
        w.list_end();
        w.list_begin();
        w.int(3);
        w.list_end();
        w.list_end();
        w.int(99);
        let tape = w.finish();

        let mut c = Cursor::new(&tape);
        assert!(c.skip_value());
        assert_eq!(c.read(), Some(RawValue::Int(99)));
    }

    #[test]
    fn skipping_a_typed_value_consumes_its_payload() {
        let mut w = TapeWriter::default();
        w.typed(StrId(1));
        w.real(3.0);
        w.int(7);
        let tape = w.finish();

        let mut c = Cursor::new(&tape);
        assert!(c.skip_value());
        assert_eq!(c.read(), Some(RawValue::Int(7)));
    }

    #[test]
    fn skipping_a_complex_leaf_consumes_its_arguments() {
        let mut w = TapeWriter::default();
        w.leaf(12);
        w.list_begin();
        w.derived();
        w.enum_symbol(StrId(2));
        w.list_end();
        w.leaf(13);
        w.list_begin();
        w.list_end();
        let tape = w.finish();

        let mut c = Cursor::new(&tape);
        assert!(c.skip_value());
        assert_eq!(c.read(), Some(RawValue::Leaf(13)));
    }

    #[test]
    fn a_truncated_tape_reads_short_instead_of_panicking() {
        let mut w = TapeWriter::default();
        w.real(1.0);
        let mut tape = w.finish();
        tape.truncate(4);
        let mut c = Cursor::new(&tape);
        assert_eq!(c.read(), None);
    }

    #[test]
    fn an_unknown_tag_stops_reading() {
        let tape = [200u8, 0, 0];
        let mut c = Cursor::new(&tape);
        assert_eq!(c.read(), None);
    }

    #[test]
    fn a_long_run_of_typed_tags_returns_instead_of_overflowing() {
        let tape = vec![TAG_TYPED; 200_000];
        let mut c = Cursor::new(&tape);
        assert!(!c.skip_value());
    }

    #[test]
    fn skipping_past_the_end_reports_failure() {
        let mut w = TapeWriter::default();
        w.list_begin();
        w.int(1);
        let tape = w.finish(); // list never closed
        let mut c = Cursor::new(&tape);
        assert!(!c.skip_value());
        assert!(c.is_empty());
    }
}
