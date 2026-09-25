// SPDX-License-Identifier: Apache-2.0
//! The STEP-21 reader: one pass over the bytes producing a [`ModelImage`].
//! Recoverable errors become diagnostics. Position advances go through
//! [`Reader::advance_to`], the only place source lines are counted.

use crate::diag::{DiagCode, Diagnostic, Diagnostics};
use crate::image::{
    ENTRY_ARITY_MISMATCH, ENTRY_COMPLEX, ENTRY_UNKNOWN_CLASS, Header, IndexEntry, ModelImage,
};
use crate::strings::{StrId, StringArena};
use crate::tape::{Cursor, RawValue, TapeWriter};
use tessifc_schema::{CLASS_UNKNOWN, ClassId, Schema, SchemaId};

/// Limits and switches for [`parse`].
#[derive(Clone, Debug)]
pub struct ParseOptions {
    /// Stop after this many instances; bounds the memory a file can drive.
    pub max_entities: usize,
    /// Stop once the value tape reaches this many bytes.
    /// Tape offsets are 32 bits, so a larger ceiling cannot be represented.
    pub max_tape_bytes: usize,
    /// Refuse string literals longer than this, in bytes.
    pub max_string_len: usize,
    /// Refuse lists nested deeper than this.
    pub max_nesting_depth: usize,
    /// Store at most this many diagnostics; the rest are counted only.
    pub max_diagnostics: usize,
    /// Use this schema regardless of `FILE_SCHEMA`, for broken headers.
    pub schema_override: Option<SchemaId>,
    /// Check each record argument count against the schema; cheap, on by default.
    pub check_arity: bool,
    /// Refuse an IFCZIP entry that declares, or inflates to, more than this many bytes.
    pub max_ifczip_bytes: usize,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions {
            max_entities: 64_000_000,
            max_tape_bytes: u32::MAX as usize,
            max_string_len: 16 * 1024 * 1024,
            max_nesting_depth: 64,
            max_diagnostics: 10_000,
            schema_override: None,
            check_arity: true,
            max_ifczip_bytes: 1 << 30,
        }
    }
}

/// Read a STEP-21 file into a model image.
/// Recoverable failures become diagnostics on the image instead of `Err`.
///
/// ```
/// use tessifc_step::{parse, ParseOptions};
///
/// let src = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\n\
///             DATA;\n#1=IFCCARTESIANPOINT((0.,0.,0.));\nENDSEC;\nEND-ISO-10303-21;\n";
/// let image = parse(src, &ParseOptions::default());
/// assert_eq!(image.len(), 1);
/// assert_eq!(image.class_name_of(1), "IfcCartesianPoint");
/// ```
pub fn parse(bytes: &[u8], opts: &ParseOptions) -> ModelImage {
    let mut reader = Reader::new(bytes, opts);
    reader.run();
    reader.finish()
}

struct Reader<'a> {
    src: &'a [u8],
    pos: usize,
    line: u32,
    opts: &'a ParseOptions,

    tape: TapeWriter,
    strings: StringArena,
    index: Vec<IndexEntry>,
    diagnostics: Diagnostics,
    unknown_names: Vec<(u32, StrId)>,

    header: Header,
    schema: SchemaId,
    schema_approximate: bool,
    schema_resolved: bool,
    tables: &'static Schema,

    limit_hit: bool,
}

impl<'a> Reader<'a> {
    fn new(src: &'a [u8], opts: &'a ParseOptions) -> Self {
        // IFC4 is assumed until FILE_SCHEMA is read, before any DATA record.
        let schema = opts.schema_override.unwrap_or(SchemaId::Ifc4);
        let schema = pick_available(schema);
        Reader {
            src,
            pos: 0,
            line: 1,
            opts,
            tape: TapeWriter::with_capacity(src.len() / 2),
            strings: StringArena::with_capacity(src.len() / 64 + 64),
            index: Vec::with_capacity(src.len() / 64 + 16),
            diagnostics: Diagnostics::with_cap(opts.max_diagnostics),
            unknown_names: Vec::new(),
            header: Header::default(),
            schema,
            schema_approximate: false,
            schema_resolved: opts.schema_override.is_some(),
            tables: Schema::get(schema),
            limit_hit: false,
        }
    }

    // ---------------------------------------------------------------- scanning

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    #[inline]
    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.src.get(self.pos + offset).copied()
    }

    /// Move to `new_pos`, counting the source lines passed over.
    #[inline]
    fn advance_to(&mut self, new_pos: usize) {
        let new_pos = new_pos.min(self.src.len());
        if new_pos > self.pos {
            let span = &self.src[self.pos..new_pos];
            // memchr beats a byte loop, and this is the only place lines are counted.
            self.line = add_lines(self.line, memchr::memchr_iter(b'\n', span).count());
            self.pos = new_pos;
        }
    }

    #[inline]
    fn bump(&mut self) {
        if self.peek() == Some(b'\n') {
            self.line = add_lines(self.line, 1);
        }
        self.pos += 1;
    }

    /// Skip whitespace and comments. Returns false at end of input.
    fn skip_trivia(&mut self) -> bool {
        loop {
            match self.peek() {
                None => return false,
                Some(b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c) => self.bump(),
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    let start_line = self.line;
                    self.advance_to(self.pos + 2);
                    match find_comment_end(self.src, self.pos) {
                        Some(end) => self.advance_to(end),
                        None => {
                            self.diag(Diagnostic::error(
                                DiagCode::UNCLOSED_COMMENT,
                                start_line,
                                "comment is never closed; the rest of the file was skipped",
                            ));
                            self.advance_to(self.src.len());
                            return false;
                        }
                    }
                }
                Some(_) => return true,
            }
        }
    }

    fn diag(&mut self, d: Diagnostic) {
        self.diagnostics.push(d);
    }

    // ------------------------------------------------------------------ driver

    fn run(&mut self) {
        self.skip_bom();
        self.expect_iso_header();

        // Sections may repeat: some writers emit several DATA sections.
        loop {
            if !self.skip_trivia() {
                break;
            }
            let Some(word) = self.peek_keyword() else {
                // Not a section keyword: skip to the next semicolon and retry.
                let line = self.line;
                self.diag(Diagnostic::warning(
                    DiagCode::BAD_HEADER,
                    line,
                    "expected a section keyword",
                ));
                if !self.recover_to_semicolon() {
                    break;
                }
                continue;
            };
            match word.as_slice() {
                b"HEADER" => {
                    self.advance_to(self.pos + word.len());
                    self.consume_semicolon();
                    self.parse_header_section();
                }
                b"DATA" => {
                    self.advance_to(self.pos + word.len());
                    // A DATA section may carry a parameter list, DATA(('x'));
                    self.skip_trivia();
                    if self.peek() == Some(b'(') {
                        self.skip_balanced_parens();
                    }
                    self.consume_semicolon();
                    self.resolve_schema();
                    self.parse_data_section();
                    // Past a limit the rest of the file is not read, or every
                    // remaining record would be reported as a stray keyword.
                    if self.limit_hit {
                        break;
                    }
                }
                b"END-ISO-10303-21" => {
                    self.advance_to(self.pos + word.len());
                    self.consume_semicolon();
                    break;
                }
                // Later editions of the standard add these; skip them whole.
                b"ANCHOR" | b"REFERENCE" | b"SIGNATURE" => {
                    self.advance_to(self.pos + word.len());
                    self.consume_semicolon();
                    self.skip_to_endsec();
                }
                _ => {
                    let line = self.line;
                    let name = echo(&word);
                    self.diag(Diagnostic::warning(
                        DiagCode::BAD_HEADER,
                        line,
                        format!("unknown section {name}, skipped"),
                    ));
                    self.advance_to(self.pos + word.len());
                    if !self.recover_to_semicolon() {
                        break;
                    }
                }
            }
        }

        if self.index.is_empty() && !self.diagnostics.has_errors() {
            let line = self.line;
            self.diag(Diagnostic::error(
                DiagCode::NO_DATA_SECTION,
                line,
                "no instances were found; is this a STEP file?",
            ));
        }
    }

    fn skip_bom(&mut self) {
        if self.src.starts_with(&[0xef, 0xbb, 0xbf]) {
            self.pos = 3;
        }
    }

    fn expect_iso_header(&mut self) {
        self.skip_trivia();
        if self
            .src
            .get(self.pos..)
            .unwrap_or(&[])
            .starts_with(b"ISO-10303-21")
        {
            self.advance_to(self.pos + b"ISO-10303-21".len());
            self.consume_semicolon();
        } else {
            let line = self.line;
            self.diag(Diagnostic::error(
                DiagCode::NOT_STEP,
                line,
                "file does not start with ISO-10303-21; reading it anyway",
            ));
        }
    }

    /// Read an upper-case keyword at the current position without consuming it.
    fn peek_keyword(&self) -> Option<Vec<u8>> {
        let start = self.pos;
        let mut end = start;
        // A user-defined keyword starts with an exclamation mark.
        if self.src.get(end) == Some(&b'!') {
            end += 1;
        }
        while let Some(&b) = self.src.get(end) {
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
                end += 1;
            } else {
                break;
            }
        }
        if end == start {
            None
        } else {
            Some(self.src[start..end].to_ascii_uppercase())
        }
    }

    fn consume_semicolon(&mut self) {
        self.skip_trivia();
        if self.peek() == Some(b';') {
            self.bump();
        }
    }

    /// Skip a parenthesised group, respecting string literals and comments.
    fn skip_balanced_parens(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                None => return,
                Some(b'(') => {
                    depth += 1;
                    self.bump();
                }
                Some(b')') => {
                    self.bump();
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return;
                    }
                }
                Some(b'\'') => {
                    if !self.skip_string_literal() {
                        return;
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    if !self.skip_trivia() {
                        return;
                    }
                }
                Some(_) => self.bump(),
            }
        }
    }

    /// Skip from the opening quote past the closing quote. False if unclosed.
    fn skip_string_literal(&mut self) -> bool {
        debug_assert_eq!(self.peek(), Some(b'\''));
        self.bump();
        match find_string_end(self.src, self.pos) {
            Some(end) => {
                self.advance_to(end + 1);
                true
            }
            None => {
                self.advance_to(self.src.len());
                false
            }
        }
    }

    /// Advance past the next top-level `;`, respecting strings and comments.
    fn recover_to_semicolon(&mut self) -> bool {
        loop {
            match self.peek() {
                None => return false,
                Some(b';') => {
                    self.bump();
                    return true;
                }
                Some(b'\'') => {
                    if !self.skip_string_literal() {
                        return false;
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    if !self.skip_trivia() {
                        return false;
                    }
                }
                Some(_) => self.bump(),
            }
        }
    }

    fn skip_to_endsec(&mut self) {
        loop {
            if !self.skip_trivia() {
                return;
            }
            if let Some(word) = self.peek_keyword() {
                if word == b"ENDSEC" {
                    self.advance_to(self.pos + word.len());
                    self.consume_semicolon();
                    return;
                }
                self.advance_to(self.pos + word.len());
            }
            if !self.recover_to_semicolon() {
                return;
            }
        }
    }

    // ------------------------------------------------------------------ header

    fn parse_header_section(&mut self) {
        loop {
            if !self.skip_trivia() {
                let line = self.line;
                self.diag(Diagnostic::error(
                    DiagCode::TRUNCATED,
                    line,
                    "file ends inside the HEADER section",
                ));
                return;
            }
            let Some(word) = self.peek_keyword() else {
                if !self.recover_to_semicolon() {
                    return;
                }
                continue;
            };
            if word == b"ENDSEC" {
                self.advance_to(self.pos + word.len());
                self.consume_semicolon();
                return;
            }
            if word == b"DATA" {
                // ENDSEC was forgotten; DATA is left for the section loop to read.
                let line = self.line;
                self.diag(Diagnostic::warning(
                    DiagCode::BAD_HEADER,
                    line,
                    "the HEADER section is not closed by ENDSEC; DATA ends it",
                ));
                return;
            }
            self.advance_to(self.pos + word.len());
            self.skip_trivia();

            // Each header entity is KEYWORD(args); with no instance name.
            let mark = self.tape.pos();
            let ok = if self.peek() == Some(b'(') {
                self.parse_list(0)
            } else {
                false
            };
            if ok {
                let bytes = self.tape.as_slice()[mark..].to_vec();
                self.absorb_header_entity(&word, &bytes);
            } else {
                let line = self.line;
                let name = String::from_utf8_lossy(&word).into_owned();
                self.diag(Diagnostic::warning(
                    DiagCode::BAD_HEADER,
                    line,
                    format!("could not read header entity {name}"),
                ));
            }
            self.tape.truncate(mark);
            if !self.recover_to_semicolon() {
                return;
            }
        }
    }

    /// Pull the fields we care about out of a parsed header entity.
    fn absorb_header_entity(&mut self, keyword: &[u8], tape: &[u8]) {
        let strings = &self.strings;
        let mut cursor = Cursor::new(tape);
        if cursor.read() != Some(RawValue::ListStart) {
            return;
        }
        let args = read_header_args(&mut cursor, strings);

        let text = |i: usize| -> String {
            match args.get(i) {
                Some(HeaderArg::Text(s)) => s.clone(),
                _ => String::new(),
            }
        };
        let list = |i: usize| -> Vec<String> {
            match args.get(i) {
                Some(HeaderArg::List(v)) => v.clone(),
                Some(HeaderArg::Text(s)) if !s.is_empty() => vec![s.clone()],
                _ => Vec::new(),
            }
        };

        match keyword {
            b"FILE_DESCRIPTION" => {
                self.header.description = list(0);
                self.header.implementation_level = text(1);
            }
            b"FILE_NAME" => {
                self.header.name = text(0);
                self.header.time_stamp = text(1);
                self.header.author = list(2);
                self.header.organization = list(3);
                self.header.preprocessor_version = text(4);
                self.header.originating_system = text(5);
                self.header.authorization = text(6);
            }
            b"FILE_SCHEMA" => {
                self.header.schema_identifiers = list(0);
            }
            _ => {}
        }
    }

    /// Decide which schema tables to use, once, before any DATA record.
    fn resolve_schema(&mut self) {
        if self.schema_resolved {
            return;
        }
        self.schema_resolved = true;
        let line = self.line;

        let raw = self
            .header
            .schema_identifiers
            .first()
            .cloned()
            .unwrap_or_default();
        if raw.is_empty() {
            self.diag(Diagnostic::warning(
                DiagCode::SCHEMA_GUESSED,
                line,
                "FILE_SCHEMA is missing or empty; assuming IFC4",
            ));
            self.set_schema(SchemaId::Ifc4, false);
            return;
        }

        match SchemaId::detect(&raw) {
            Some((id, approximate)) => {
                let available = pick_available(id);
                if available != id {
                    self.diag(Diagnostic::warning(
                        DiagCode::SCHEMA_APPROXIMATED,
                        line,
                        format!(
                            "{raw} maps to {id}, which is not compiled into this build; \
                             using {available}"
                        ),
                    ));
                    self.set_schema(available, true);
                } else {
                    if approximate {
                        self.diag(Diagnostic::warning(
                            DiagCode::SCHEMA_APPROXIMATED,
                            line,
                            format!("{raw} is read with {id} tables"),
                        ));
                    }
                    self.set_schema(id, approximate);
                }
            }
            None => {
                self.diag(Diagnostic::warning(
                    DiagCode::SCHEMA_GUESSED,
                    line,
                    format!("FILE_SCHEMA says {raw}, which is not an IFC schema; assuming IFC4"),
                ));
                self.set_schema(SchemaId::Ifc4, true);
            }
        }
    }

    fn set_schema(&mut self, id: SchemaId, approximate: bool) {
        let id = pick_available(id);
        self.schema = id;
        self.schema_approximate = approximate;
        self.tables = Schema::get(id);
    }

    // -------------------------------------------------------------------- data

    fn parse_data_section(&mut self) {
        loop {
            if !self.skip_trivia() {
                let line = self.line;
                self.diag(Diagnostic::error(
                    DiagCode::TRUNCATED,
                    line,
                    "file ends inside the DATA section",
                ));
                return;
            }
            match self.peek() {
                Some(b'#') => {
                    if self.index.len() >= self.opts.max_entities {
                        if !self.limit_hit {
                            self.limit_hit = true;
                            let line = self.line;
                            let max = self.opts.max_entities;
                            self.diag(Diagnostic::error(
                                DiagCode::LIMIT_REACHED,
                                line,
                                format!("stopped after {max} instances"),
                            ));
                        }
                        return;
                    }
                    // Checked before the record so a tape offset can never wrap.
                    if self.tape.pos() >= self.opts.max_tape_bytes {
                        if !self.limit_hit {
                            self.limit_hit = true;
                            let line = self.line;
                            self.diag(Diagnostic::error(
                                DiagCode::LIMIT_REACHED,
                                line,
                                "stopped: the value tape reached its limit",
                            ));
                        }
                        return;
                    }
                    self.parse_instance();
                }
                Some(b';') => self.bump(),
                _ => {
                    let Some(word) = self.peek_keyword() else {
                        let line = self.line;
                        self.diag(Diagnostic::error(
                            DiagCode::RECORD_SKIPPED,
                            line,
                            "unexpected character in DATA section",
                        ));
                        if !self.recover_to_semicolon() {
                            return;
                        }
                        continue;
                    };
                    if word == b"ENDSEC" {
                        self.advance_to(self.pos + word.len());
                        self.consume_semicolon();
                        return;
                    }
                    if word == b"END-ISO-10303-21" {
                        // ENDSEC was forgotten; treat the file as finished.
                        return;
                    }
                    let line = self.line;
                    let name = String::from_utf8_lossy(&word).into_owned();
                    self.diag(Diagnostic::error(
                        DiagCode::RECORD_SKIPPED,
                        line,
                        format!("record without an instance name: {name}"),
                    ));
                    if !self.recover_to_semicolon() {
                        return;
                    }
                }
            }
        }
    }

    /// Parse `#123 = KEYWORD(...) ;` or `#123 = (A(...) B(...)) ;`.
    fn parse_instance(&mut self) {
        let source_start = self.pos;
        let line = self.line;
        let mark = self.tape.pos();
        self.bump(); // the '#'

        let Some(express_id) = self.read_u32() else {
            self.diag(Diagnostic::error(
                DiagCode::ID_OVERFLOW,
                line,
                "instance name is not a number that fits in 32 bits",
            ));
            self.recover_to_semicolon();
            return;
        };

        self.skip_trivia();
        if self.peek() != Some(b'=') {
            self.diag(
                Diagnostic::error(DiagCode::RECORD_SKIPPED, line, "expected = after #id")
                    .with_id(express_id),
            );
            self.recover_to_semicolon();
            return;
        }
        self.bump();
        self.skip_trivia();

        let mut flags = 0u16;
        let class_id;

        if self.peek() == Some(b'(') {
            // Complex instance: a bracketed sequence of leaves.
            flags |= ENTRY_COMPLEX;
            match self.parse_complex_leaves() {
                Some(most_derived) => class_id = most_derived,
                None => {
                    self.tape.truncate(mark);
                    self.diag(
                        Diagnostic::error(
                            DiagCode::RECORD_SKIPPED,
                            line,
                            "malformed complex instance",
                        )
                        .with_id(express_id),
                    );
                    self.recover_to_semicolon();
                    return;
                }
            }
            self.diag(
                Diagnostic::info(DiagCode::COMPLEX_INSTANCE, line, "complex instance")
                    .with_id(express_id),
            );
        } else {
            let Some(keyword) = self.peek_keyword() else {
                self.diag(
                    Diagnostic::error(DiagCode::RECORD_SKIPPED, line, "expected a class name")
                        .with_id(express_id),
                );
                self.recover_to_semicolon();
                return;
            };
            self.advance_to(self.pos + keyword.len());
            class_id = self.resolve_class(&keyword, express_id, line, &mut flags);

            self.skip_trivia();
            if self.peek() != Some(b'(') {
                self.tape.truncate(mark);
                self.diag(
                    Diagnostic::error(
                        DiagCode::RECORD_SKIPPED,
                        line,
                        "expected ( after the class name",
                    )
                    .with_id(express_id),
                );
                self.recover_to_semicolon();
                return;
            }
            // Arguments are written without list markers: argument k is the k-th value.
            if !self.parse_argument_list(0) {
                self.tape.truncate(mark);
                self.diag(
                    Diagnostic::error(
                        DiagCode::UNCLOSED_RECORD,
                        line,
                        "argument list is not closed",
                    )
                    .with_id(express_id),
                );
                self.recover_to_semicolon();
                return;
            }
        }

        let end = self.tape.pos();
        self.skip_trivia();
        if self.peek() == Some(b';') {
            self.bump();
        } else {
            // A missing semicolon is recoverable: the record itself is complete.
            self.diag(
                Diagnostic::warning(
                    DiagCode::MISSING_SEMICOLON,
                    line,
                    "record is not terminated by a semicolon",
                )
                .with_id(express_id),
            );
        }

        if self.opts.check_arity && flags & ENTRY_COMPLEX == 0 && class_id != CLASS_UNKNOWN {
            let expected = self.tables.arity(class_id);
            let found = count_values(&self.tape.as_slice()[mark..end]);
            if found != expected {
                flags |= ENTRY_ARITY_MISMATCH;
                let name = self.tables.class(class_id).name;
                self.diag(
                    Diagnostic::warning(
                        DiagCode::ARITY_MISMATCH,
                        line,
                        format!("{name} expects {expected} arguments, found {found}"),
                    )
                    .with_id(express_id),
                );
            }
        }

        let (Ok(tape_off), Ok(tape_len)) = (u32::try_from(mark), u32::try_from(end - mark)) else {
            self.tape.truncate(mark);
            self.diag(
                Diagnostic::error(
                    DiagCode::LIMIT_REACHED,
                    line,
                    "record is too large for the value tape",
                )
                .with_id(express_id),
            );
            return;
        };

        self.index.push(IndexEntry {
            express_id,
            class_id,
            flags,
            tape_off,
            tape_len,
            line,
            source_off: u32::try_from(source_start).unwrap_or(u32::MAX),
            source_len: u32::try_from(self.pos.saturating_sub(source_start)).unwrap_or(u32::MAX),
            source_hash: crate::hash::fx_hash_bytes(&self.src[source_start..self.pos]),
        });
    }

    fn resolve_class(
        &mut self,
        keyword: &[u8],
        express_id: u32,
        line: u32,
        flags: &mut u16,
    ) -> ClassId {
        // The keyword is already upper case: peek_keyword folds it.
        match core::str::from_utf8(keyword)
            .ok()
            .and_then(|s| self.tables.class_by_upper(s))
        {
            Some(id) => id,
            None => {
                *flags |= ENTRY_UNKNOWN_CLASS;
                let id = self.strings.intern(keyword);
                self.unknown_names.push((express_id, id));
                if !self.diagnostics.is_full() {
                    let name = echo(keyword);
                    self.diag(
                        Diagnostic::warning(
                            DiagCode::UNKNOWN_CLASS,
                            line,
                            format!("{name} is not in this schema; kept as an opaque instance"),
                        )
                        .with_id(express_id),
                    );
                }
                CLASS_UNKNOWN
            }
        }
    }

    /// Parse `(A(...) B(...) ...)` writing one `Leaf` per branch.
    /// Returns the most derived leaf class, or the first known leaf when none dominates.
    fn parse_complex_leaves(&mut self) -> Option<ClassId> {
        self.bump(); // the opening '('
        let mut best: ClassId = CLASS_UNKNOWN;
        let mut best_depth = -1i32;
        let mut leaves = 0usize;

        loop {
            if !self.skip_trivia() {
                return None;
            }
            match self.peek() {
                Some(b')') => {
                    self.bump();
                    return if leaves > 0 { Some(best) } else { None };
                }
                Some(b',') => {
                    // Not legal, but harmless to tolerate.
                    self.bump();
                }
                Some(_) => {
                    let keyword = self.peek_keyword()?;
                    self.advance_to(self.pos + keyword.len());
                    let class = core::str::from_utf8(&keyword)
                        .ok()
                        .and_then(|s| self.tables.class_by_upper(s))
                        .unwrap_or(CLASS_UNKNOWN);
                    self.skip_trivia();
                    if self.peek() != Some(b'(') {
                        return None;
                    }
                    self.tape.leaf(class);
                    if !self.parse_list(0) {
                        return None;
                    }
                    leaves += 1;
                    let depth = inheritance_depth(self.tables, class);
                    if depth > best_depth {
                        best_depth = depth;
                        best = class;
                    }
                }
                None => return None,
            }
        }
    }

    /// Parse a bracketed list, writing `ListStart` and `ListEnd` around it.
    fn parse_list(&mut self, depth: usize) -> bool {
        if depth >= self.opts.max_nesting_depth {
            let line = self.line;
            self.diag(Diagnostic::error(
                DiagCode::NESTING_TOO_DEEP,
                line,
                "list nesting limit reached",
            ));
            return false;
        }
        debug_assert_eq!(self.peek(), Some(b'('));
        self.tape.list_begin();
        self.bump();
        if !self.parse_values_until_close(depth + 1) {
            return false;
        }
        self.tape.list_end();
        true
    }

    /// Parse the arguments of a record: like a list, but with no markers.
    fn parse_argument_list(&mut self, depth: usize) -> bool {
        debug_assert_eq!(self.peek(), Some(b'('));
        self.bump();
        self.parse_values_until_close(depth + 1)
    }

    /// Read comma-separated values up to and including the closing bracket.
    fn parse_values_until_close(&mut self, depth: usize) -> bool {
        if !self.skip_trivia() {
            return false;
        }
        if self.peek() == Some(b')') {
            self.bump();
            return true;
        }
        loop {
            if !self.parse_value(depth) {
                return false;
            }
            if !self.skip_trivia() {
                return false;
            }
            match self.peek() {
                Some(b',') => {
                    self.bump();
                    // A trailing comma before the bracket is tolerated as a missing value.
                    if !self.skip_trivia() {
                        return false;
                    }
                    if self.peek() == Some(b')') {
                        self.tape.null();
                        self.bump();
                        return true;
                    }
                }
                Some(b')') => {
                    self.bump();
                    return true;
                }
                _ => return false,
            }
        }
    }

    /// Parse exactly one value.
    fn parse_value(&mut self, depth: usize) -> bool {
        // Checked here, not only in `parse_list`: typed values can nest
        // without ever opening a list.
        if depth >= self.opts.max_nesting_depth {
            let line = self.line;
            self.diag(Diagnostic::error(
                DiagCode::NESTING_TOO_DEEP,
                line,
                "value nesting limit reached",
            ));
            return false;
        }
        if !self.skip_trivia() {
            return false;
        }
        match self.peek() {
            None => false,
            Some(b'$') => {
                self.bump();
                self.tape.null();
                true
            }
            Some(b'*') => {
                self.bump();
                self.tape.derived();
                true
            }
            Some(b'#') => {
                self.bump();
                match self.read_u32() {
                    Some(id) => {
                        self.tape.reference(id);
                        true
                    }
                    None => {
                        let line = self.line;
                        let message = if self.peek().is_some_and(|b| b.is_ascii_digit()) {
                            "reference does not fit in 32 bits"
                        } else {
                            "reference has no number after the #"
                        };
                        self.diag(Diagnostic::error(DiagCode::ID_OVERFLOW, line, message));
                        false
                    }
                }
            }
            Some(b'\'') => self.parse_string(),
            Some(b'"') => self.parse_binary(),
            Some(b'.') => self.parse_enum(),
            Some(b'(') => self.parse_list(depth),
            Some(b'-' | b'+') => self.parse_number(),
            Some(c) if c.is_ascii_digit() => self.parse_number(),
            Some(c) if c.is_ascii_alphabetic() || c == b'!' || c == b'_' => {
                self.parse_typed_value(depth)
            }
            Some(_) => false,
        }
    }

    fn parse_string(&mut self) -> bool {
        let start_line = self.line;
        self.bump(); // opening quote
        let body_start = self.pos;
        let Some(end) = find_string_end(self.src, body_start) else {
            self.advance_to(self.src.len());
            self.diag(Diagnostic::error(
                DiagCode::UNCLOSED_STRING,
                start_line,
                "string literal is never closed",
            ));
            return false;
        };
        let len = end - body_start;
        if len > self.opts.max_string_len {
            self.advance_to(end + 1);
            self.diag(Diagnostic::error(
                DiagCode::LIMIT_REACHED,
                start_line,
                format!("string literal of {len} bytes exceeds the limit"),
            ));
            self.tape.str(StrId::EMPTY);
            return true;
        }
        let id = self.strings.intern(&self.src[body_start..end]);
        self.advance_to(end + 1);
        self.tape.str(id);
        true
    }

    fn parse_binary(&mut self) -> bool {
        let start_line = self.line;
        self.bump(); // opening quote
        let body_start = self.pos;
        let mut end = body_start;
        while let Some(&b) = self.src.get(end) {
            if b == b'"' {
                break;
            }
            end += 1;
        }
        if self.src.get(end) != Some(&b'"') {
            self.advance_to(self.src.len());
            self.diag(Diagnostic::error(
                DiagCode::UNCLOSED_STRING,
                start_line,
                "binary literal is never closed",
            ));
            return false;
        }
        let id = self.strings.intern(&self.src[body_start..end]);
        self.advance_to(end + 1);
        self.tape.binary(id);
        true
    }

    fn parse_enum(&mut self) -> bool {
        let start = self.pos;
        self.bump(); // opening dot
        let body_start = self.pos;
        let mut end = body_start;
        while let Some(&b) = self.src.get(end) {
            if b == b'.' {
                break;
            }
            if !(b.is_ascii_alphanumeric() || b == b'_') {
                // Not an enumeration: a real like ".5" without its leading zero.
                self.pos = start;
                return self.parse_number();
            }
            end += 1;
        }
        if self.src.get(end) != Some(&b'.') {
            self.pos = start;
            return self.parse_number();
        }
        let id = self.strings.intern(&self.src[body_start..end]);
        self.advance_to(end + 1);
        self.tape.enum_symbol(id);
        true
    }

    fn parse_number(&mut self) -> bool {
        let start = self.pos;
        let mut end = start;
        let mut is_real = false;
        if matches!(self.src.get(end), Some(b'-' | b'+')) {
            end += 1;
        }
        while let Some(&b) = self.src.get(end) {
            match b {
                b'0'..=b'9' => end += 1,
                b'.' => {
                    is_real = true;
                    end += 1;
                }
                b'e' | b'E' => {
                    is_real = true;
                    end += 1;
                    if matches!(self.src.get(end), Some(b'-' | b'+')) {
                        end += 1;
                    }
                }
                _ => break,
            }
        }
        let text = &self.src[start..end];
        let Ok(text) = core::str::from_utf8(text) else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        self.advance_to(end);

        if !is_real && let Ok(v) = text.parse::<i64>() {
            self.tape.int(v);
            return true;
        }
        match text.parse::<f64>() {
            Ok(v) => {
                if v.is_finite() {
                    self.tape.real(v);
                } else {
                    // Substituting zero keeps the argument count of the record intact.
                    let line = self.line;
                    self.diag(Diagnostic::warning(
                        DiagCode::NUMBER_OUT_OF_RANGE,
                        line,
                        "real literal is outside the range of a double",
                    ));
                    self.tape.real(0.0);
                }
                true
            }
            Err(_) => {
                let line = self.line;
                if !self.diagnostics.is_full() {
                    let owned = echo(text.as_bytes());
                    self.diag(Diagnostic::error(
                        DiagCode::BAD_NUMBER,
                        line,
                        format!("cannot read {owned} as a number"),
                    ));
                }
                false
            }
        }
    }

    /// Parse `IFCLENGTHMEASURE(3.)`: a type name wrapping one value.
    /// Several values get a list retrofitted so exactly one value follows the type.
    fn parse_typed_value(&mut self, depth: usize) -> bool {
        let Some(keyword) = self.peek_keyword() else {
            return false;
        };
        self.advance_to(self.pos + keyword.len());
        self.skip_trivia();
        if self.peek() != Some(b'(') {
            // Bare keyword where a value was expected: some writers emit unquoted logicals.
            let id = self.strings.intern(&keyword);
            self.tape.enum_symbol(id);
            return true;
        }
        let id = self.strings.intern(&keyword);
        self.tape.typed(id);
        let payload_start = self.tape.pos();

        self.bump(); // the opening bracket
        if !self.skip_trivia() {
            return false;
        }
        if self.peek() == Some(b')') {
            // An empty wrapper: record an empty list so the invariant holds.
            self.bump();
            self.tape.list_begin();
            self.tape.list_end();
            return true;
        }
        if !self.parse_value(depth + 1) {
            return false;
        }
        if !self.skip_trivia() {
            return false;
        }
        match self.peek() {
            Some(b')') => {
                self.bump();
                true
            }
            Some(b',') => {
                // More than one value: retrofit a list around what we wrote.
                self.tape
                    .insert_tag_at(payload_start, crate::tape::TAG_LIST_BEGIN);
                loop {
                    match self.peek() {
                        Some(b',') => {
                            self.bump();
                            if !self.parse_value(depth + 1) {
                                return false;
                            }
                        }
                        Some(b')') => {
                            self.bump();
                            self.tape.list_end();
                            return true;
                        }
                        _ => return false,
                    }
                    if !self.skip_trivia() {
                        return false;
                    }
                }
            }
            _ => false,
        }
    }

    /// Read a run of digits as a `u32`, refusing anything that overflows.
    fn read_u32(&mut self) -> Option<u32> {
        let start = self.pos;
        let mut end = start;
        while matches!(self.src.get(end), Some(b) if b.is_ascii_digit()) {
            end += 1;
        }
        if end == start {
            return None;
        }
        let mut value: u32 = 0;
        for &b in &self.src[start..end] {
            value = value.checked_mul(10)?.checked_add((b - b'0') as u32)?;
        }
        self.advance_to(end);
        Some(value)
    }

    // ------------------------------------------------------------------ finish

    fn finish(mut self) -> ModelImage {
        // Stable sort so that a duplicated id keeps its first occurrence.
        self.index.sort_by_key(|e| e.express_id);
        let mut duplicates: Vec<(u32, u32)> = Vec::new();
        let mut previous = None;
        self.index.retain(|e| {
            if previous == Some(e.express_id) {
                duplicates.push((e.express_id, e.line));
                false
            } else {
                previous = Some(e.express_id);
                true
            }
        });
        for (id, line) in duplicates {
            self.diagnostics.push(
                Diagnostic::warning(
                    DiagCode::DUPLICATE_ID,
                    line,
                    "instance name already used; this record was dropped",
                )
                .with_id(id),
            );
        }
        self.unknown_names.sort_by_key(|&(id, _)| id);
        self.unknown_names.dedup_by_key(|&mut (id, _)| id);

        let class_count = self.tables.class_count();
        let mut image = ModelImage {
            tape: self.tape.finish(),
            index: self.index,
            strings: self.strings,
            schema: self.schema,
            schema_approximate: self.schema_approximate,
            header: self.header,
            diagnostics: self.diagnostics,
            unknown_class_names: self.unknown_names,
            class_offsets: vec![0],
            class_members: Vec::new(),
            source_len: self.src.len(),
        };
        image.build_buckets(class_count);
        image
    }
}

/// The line number after `newlines` more line breaks, pinned at `u32::MAX`.
fn add_lines(line: u32, newlines: usize) -> u32 {
    line.saturating_add(u32::try_from(newlines).unwrap_or(u32::MAX))
}

/// Depth of a class in the inheritance chain, to pick the most derived leaf.
fn inheritance_depth(schema: &Schema, class: ClassId) -> i32 {
    if class == CLASS_UNKNOWN {
        return -1;
    }
    let mut depth = 0;
    let mut current = class;
    // Bounded by the class count so a malformed table cannot loop forever.
    for _ in 0..schema.class_count() {
        match schema.class(current).parent {
            Some(parent) => {
                depth += 1;
                current = parent;
            }
            None => break,
        }
    }
    depth
}

/// Which of the compiled-in schemas to actually use for a requested one:
/// the schema itself, or the nearest one compiled in.
pub(crate) fn pick_available(id: SchemaId) -> SchemaId {
    Schema::get(id).id
}

/// Byte offset of the closing quote of a string starting at `from`.
/// Doubled quotes escape a quote; backslashes cannot hide one in ISO 10303-21.
fn find_string_end(src: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    while at < src.len() {
        let rel = memchr::memchr(b'\'', &src[at..])?;
        let quote = at + rel;
        if src.get(quote + 1) == Some(&b'\'') {
            at = quote + 2;
            continue;
        }
        return Some(quote);
    }
    None
}

/// Find the byte offset just past the `*/` that closes a comment.
fn find_comment_end(src: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    while at + 1 < src.len() {
        let rel = memchr::memchr(b'*', &src[at..])?;
        let star = at + rel;
        if src.get(star + 1) == Some(&b'/') {
            return Some(star + 2);
        }
        at = star + 1;
    }
    None
}

/// Count top-level values in a tape slice.
fn count_values(tape: &[u8]) -> usize {
    let mut cursor = Cursor::new(tape);
    let mut n = 0;
    while !cursor.is_empty() {
        if !cursor.skip_value() {
            break;
        }
        n += 1;
    }
    n
}

/// A header argument, flattened to what the header actually needs.
enum HeaderArg {
    Text(String),
    List(Vec<String>),
    Other,
}

fn read_header_args(cursor: &mut Cursor<'_>, strings: &StringArena) -> Vec<HeaderArg> {
    let mut out = Vec::new();
    loop {
        match cursor.read() {
            None | Some(RawValue::ListEnd) => return out,
            Some(RawValue::Str(id)) => out.push(HeaderArg::Text(strings.decode(id))),
            Some(RawValue::ListStart) => {
                let mut items = Vec::new();
                loop {
                    match cursor.read() {
                        None | Some(RawValue::ListEnd) => break,
                        Some(RawValue::Str(id)) => items.push(strings.decode(id)),
                        Some(RawValue::ListStart) => {
                            // Nested list inside a header field: skip it.
                            let mut depth = 1;
                            while depth > 0 {
                                match cursor.read() {
                                    Some(RawValue::ListStart) => depth += 1,
                                    Some(RawValue::ListEnd) => depth -= 1,
                                    Some(_) => {}
                                    None => break,
                                }
                            }
                        }
                        Some(_) => {}
                    }
                }
                out.push(HeaderArg::List(items));
            }
            Some(RawValue::Typed(_)) => {
                let _ = cursor.skip_value();
                out.push(HeaderArg::Other);
            }
            Some(_) => out.push(HeaderArg::Other),
        }
    }
}

/// A bounded copy of file text for a message, so a huge token cannot become a huge diagnostic.
fn echo(bytes: &[u8]) -> String {
    const LIMIT: usize = 64;
    let text = String::from_utf8_lossy(bytes);
    if text.chars().count() <= LIMIT {
        text.into_owned()
    } else {
        format!("{}...", text.chars().take(LIMIT).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_count_stops_at_its_ceiling() {
        assert_eq!(add_lines(1, 2), 3);
        assert_eq!(add_lines(u32::MAX - 1, 5), u32::MAX);
        assert_eq!(add_lines(u32::MAX, 1), u32::MAX);
        assert_eq!(add_lines(1, usize::MAX), u32::MAX);
    }
}
