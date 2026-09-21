// SPDX-License-Identifier: Apache-2.0
//! The model image: everything a parsed file becomes.
//! One immutable structure per file that borrows nothing from the source
//! bytes, so the caller can drop the file as soon as parsing returns.

use crate::diag::Diagnostics;
use crate::strings::{StrId, StringArena};
use crate::tape::Cursor;
use tessifc_schema::{CLASS_UNKNOWN, ClassId, Schema, SchemaId};

/// Set on an entry written as a complex instance, `#1=(A(...)B(...))`.
pub const ENTRY_COMPLEX: u16 = 1 << 0;
/// Set on an entry whose class name was not found in the schema.
pub const ENTRY_UNKNOWN_CLASS: u16 = 1 << 1;
/// Set on an entry whose argument count did not match the schema.
pub const ENTRY_ARITY_MISMATCH: u16 = 1 << 2;

/// One instance: tape location, class, and source provenance.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct IndexEntry {
    /// The `#n` name from the file.
    pub express_id: u32,
    /// Schema class id, or [`CLASS_UNKNOWN`].
    pub class_id: ClassId,
    /// `ENTRY_*` bits.
    pub flags: u16,
    /// Byte offset of the first argument value on the tape.
    pub tape_off: u32,
    /// Byte length of this record on the tape.
    pub tape_len: u32,
    /// One-based source line, kept for diagnostics.
    pub line: u32,
    /// Byte offset of the `#` that starts this record in the source file.
    /// Provenance only; the image never retains the source bytes.
    pub source_off: u32,
    /// Byte length of the complete source record, including its semicolon.
    pub source_len: u32,
    /// Hash of the source record, checked by the editor before using offsets.
    pub source_hash: u64,
}

/// The `HEADER` section, decoded eagerly because it is small and always read.
#[derive(Clone, Debug, Default)]
pub struct Header {
    /// `FILE_DESCRIPTION` description strings.
    pub description: Vec<String>,
    /// `FILE_DESCRIPTION` implementation level, conventionally `2;1`.
    pub implementation_level: String,
    /// `FILE_NAME` name, usually the original file name.
    pub name: String,
    /// `FILE_NAME` time stamp, ISO 8601.
    pub time_stamp: String,
    /// `FILE_NAME` authors.
    pub author: Vec<String>,
    /// `FILE_NAME` organisations.
    pub organization: Vec<String>,
    /// `FILE_NAME` preprocessor version: the toolkit that wrote the file.
    pub preprocessor_version: String,
    /// `FILE_NAME` originating system: the authoring application.
    pub originating_system: String,
    /// `FILE_NAME` authorisation.
    pub authorization: String,
    /// `FILE_SCHEMA` identifiers, verbatim.
    pub schema_identifiers: Vec<String>,
}

/// A parsed STEP file.
#[derive(Debug)]
pub struct ModelImage {
    /// Packed argument values. See the [`crate::tape`] module for the layout.
    pub tape: Vec<u8>,
    /// Instances, sorted by express id.
    pub index: Vec<IndexEntry>,
    /// Interned raw strings.
    pub strings: StringArena,
    /// The schema whose tables apply to this file.
    pub schema: SchemaId,
    /// True when `FILE_SCHEMA` named a schema that was approximated.
    pub schema_approximate: bool,
    /// The decoded header.
    pub header: Header,
    /// Everything that went wrong while parsing.
    pub diagnostics: Diagnostics,
    /// Original class names of unknown-class instances, keyed by express id.
    pub unknown_class_names: Vec<(u32, StrId)>,
    /// CSR offsets into `class_members` by class id, length `class_count + 1`.
    pub(crate) class_offsets: Vec<u32>,
    /// Express ids grouped by class id, each group ascending.
    pub(crate) class_members: Vec<u32>,
    /// Bytes of source the parser consumed, for throughput reporting.
    pub source_len: usize,
}

impl ModelImage {
    /// Number of instances.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// True when the file yielded no instances at all.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// The schema tables for this image.
    pub fn schema_tables(&self) -> &'static Schema {
        Schema::get(self.schema)
    }

    /// Look up an instance by express id.
    pub fn entry(&self, express_id: u32) -> Option<&IndexEntry> {
        self.index
            .binary_search_by_key(&express_id, |e| e.express_id)
            .ok()
            .map(|i| &self.index[i])
    }

    /// The class of an instance, or [`CLASS_UNKNOWN`] if there is no such id.
    pub fn class_of(&self, express_id: u32) -> ClassId {
        self.entry(express_id).map_or(CLASS_UNKNOWN, |e| e.class_id)
    }

    /// A cursor over the arguments of an instance.
    /// For a complex instance it starts at the first `Leaf` marker.
    pub fn args(&self, express_id: u32) -> Option<Cursor<'_>> {
        let entry = self.entry(express_id)?;
        let start = entry.tape_off as usize;
        let end = start.checked_add(entry.tape_len as usize)?;
        let slice = self.tape.get(start..end)?;
        Some(Cursor::new(slice))
    }

    /// A cursor over the arguments of an instance already located.
    pub fn args_of(&self, entry: &IndexEntry) -> Cursor<'_> {
        let start = entry.tape_off as usize;
        let end = (start + entry.tape_len as usize).min(self.tape.len());
        Cursor::new(self.tape.get(start..end).unwrap_or(&[]))
    }

    /// Express ids of every instance of exactly this class, ascending; no subtypes.
    pub fn ids_of_class(&self, class: ClassId) -> &[u32] {
        let i = class as usize;
        match (self.class_offsets.get(i), self.class_offsets.get(i + 1)) {
            (Some(&start), Some(&end)) if start <= end => self
                .class_members
                .get(start as usize..end as usize)
                .unwrap_or(&[]),
            _ => &[],
        }
    }

    /// Express ids of this class or any subtype, ascending within each class.
    /// Classes are numbered in preorder, so subtypes form a contiguous id range.
    pub fn ids_of_type(&self, class: ClassId) -> impl Iterator<Item = u32> + '_ {
        let schema = self.schema_tables();
        let def = schema.class(class);
        let (lo, hi) = if class == CLASS_UNKNOWN {
            (0u16, 1u16)
        } else {
            (def.subtree_start, def.subtree_end)
        };
        (lo..hi).flat_map(move |c| self.ids_of_class(c).iter().copied())
    }

    /// How many instances of this class or any subtype there are.
    pub fn count_of_type(&self, class: ClassId) -> usize {
        let schema = self.schema_tables();
        let def = schema.class(class);
        if class == CLASS_UNKNOWN {
            return self.ids_of_class(CLASS_UNKNOWN).len();
        }
        (def.subtree_start..def.subtree_end)
            .map(|c| self.ids_of_class(c).len())
            .sum()
    }

    /// Every class id that has at least one instance, ascending.
    pub fn populated_classes(&self) -> impl Iterator<Item = (ClassId, usize)> + '_ {
        (0..self.class_offsets.len().saturating_sub(1)).filter_map(move |i| {
            let n = self.ids_of_class(i as ClassId).len();
            if n > 0 { Some((i as ClassId, n)) } else { None }
        })
    }

    /// The class name of an instance, preserving names of unknown classes.
    pub fn class_name_of(&self, express_id: u32) -> String {
        let class = self.class_of(express_id);
        if class != CLASS_UNKNOWN {
            return self.schema_tables().class(class).name.to_string();
        }
        match self
            .unknown_class_names
            .binary_search_by_key(&express_id, |&(id, _)| id)
        {
            Ok(i) => self.strings.decode(self.unknown_class_names[i].1),
            Err(_) => "UNKNOWN".to_string(),
        }
    }

    /// Approximate resident size in bytes, for reporting.
    pub fn memory_bytes(&self) -> usize {
        self.tape.capacity()
            + self.index.capacity() * core::mem::size_of::<IndexEntry>()
            + self.strings.bytes_len()
            + self.class_offsets.capacity() * 4
            + self.class_members.capacity() * 4
            + self.unknown_class_names.capacity() * 8
    }

    /// Build the class buckets; called once after the index is sorted, idempotent.
    pub(crate) fn build_buckets(&mut self, class_count: usize) {
        let mut counts = vec![0u32; class_count + 1];
        for e in &self.index {
            let i = (e.class_id as usize).min(class_count - 1);
            counts[i] += 1;
        }
        let mut offsets = vec![0u32; class_count + 1];
        let mut acc = 0u32;
        for (i, c) in counts.iter().enumerate().take(class_count) {
            offsets[i] = acc;
            acc += c;
        }
        offsets[class_count] = acc;

        let mut cursor = offsets.clone();
        let mut members = vec![0u32; acc as usize];
        // The index is sorted by express id, so buckets come out ascending.
        for e in &self.index {
            let i = (e.class_id as usize).min(class_count - 1);
            let at = cursor[i] as usize;
            members[at] = e.express_id;
            cursor[i] += 1;
        }
        self.class_offsets = offsets;
        self.class_members = members;
    }

    /// An image with nothing in it, for a file that could not be read at all.
    #[cfg(any(feature = "ifczip", test))]
    pub(crate) fn empty(schema: SchemaId) -> Self {
        ModelImage {
            tape: Vec::new(),
            index: Vec::new(),
            strings: StringArena::new(),
            schema,
            schema_approximate: false,
            header: Header::default(),
            diagnostics: Diagnostics::default(),
            unknown_class_names: Vec::new(),
            class_offsets: vec![0],
            class_members: Vec::new(),
            source_len: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_image_answers_every_query() {
        let img = ModelImage::empty(SchemaId::Ifc4);
        assert!(img.is_empty());
        assert_eq!(img.entry(1), None);
        assert_eq!(img.class_of(1), CLASS_UNKNOWN);
        assert!(img.args(1).is_none());
        assert_eq!(img.ids_of_class(5), &[] as &[u32]);
        assert_eq!(img.class_name_of(1), "UNKNOWN");
    }
}
