// SPDX-License-Identifier: Apache-2.0
//! Lossless, source-preserving edits for STEP-21 entity arguments.
//! A parsed image is paired with the same source bytes and only selected
//! argument values are replaced; everything else stays byte-for-byte identical.

use crate::{IndexEntry, ModelImage};
use core::fmt;
use core::ops::Range;

/// One source-level argument replacement.
#[cfg(feature = "edit")]
#[derive(Clone, Debug, PartialEq)]
pub struct AttributeEdit {
    /// The `#n` entity to edit.
    pub express_id: u32,
    /// Zero-based argument index in STEP serialization order.
    pub argument_index: usize,
    /// Complex-instance leaf class such as `IfcSIUnit`, or `None` for a normal record.
    /// The argument index is local to the named leaf.
    pub leaf_class: Option<String>,
    /// The replacement value.
    pub value: EditValue,
}

/// A STEP value accepted by the lossless editor.
#[cfg(feature = "edit")]
#[derive(Clone, Debug, PartialEq)]
pub enum EditValue {
    /// `$`, an unset optional value.
    Null,
    /// `*`, a derived value.
    Derived,
    /// A signed integer.
    Integer(i64),
    /// A finite real number.
    Real(f64),
    /// A string. Quotes and non-ASCII characters are encoded by this crate.
    String(String),
    /// An enumeration symbol without its surrounding dots.
    Enumeration(String),
    /// An entity reference.
    Reference(u32),
    /// One complete STEP value for lists, typed values and advanced edits; validated first.
    Raw(String),
}

/// Why a source edit could not be applied.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditError {
    /// The requested entity does not exist in the parsed image.
    NoSuchEntity(u32),
    /// The requested argument is beyond the record's serialized arity.
    NoSuchArgument {
        /// Entity whose argument was requested.
        express_id: u32,
        /// Zero-based argument index.
        index: usize,
    },
    /// The source buffer is not the one that produced this model image.
    SourceMismatch,
    /// Complex instances need a leaf-aware edit; the flat argument API rejects them.
    ComplexInstance(u32),
    /// The requested complex-instance leaf was not present.
    NoSuchLeaf {
        /// Complex entity whose leaf was requested.
        express_id: u32,
        /// Leaf class name.
        leaf: String,
    },
    /// A raw replacement was not exactly one valid STEP value.
    InvalidValue(String),
    /// The record holds a value shape this editor cannot span.
    UnsupportedValue(u32),
    /// Two requested edits target overlapping source bytes.
    OverlappingEdits,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::NoSuchEntity(id) => write!(f, "IFC entity #{id} does not exist"),
            EditError::NoSuchArgument { express_id, index } => {
                write!(
                    f,
                    "IFC entity #{express_id} has no argument at index {index}"
                )
            }
            EditError::SourceMismatch => {
                f.write_str("the IFC source does not match the parsed model image")
            }
            EditError::ComplexInstance(id) => write!(
                f,
                "IFC entity #{id} is a complex instance; edit a named leaf instead"
            ),
            EditError::NoSuchLeaf { express_id, leaf } => {
                write!(
                    f,
                    "IFC entity #{express_id} has no complex leaf named {leaf}"
                )
            }
            EditError::InvalidValue(message) => write!(f, "invalid STEP value: {message}"),
            EditError::UnsupportedValue(id) => {
                write!(f, "IFC entity #{id} holds a value this editor cannot read")
            }
            EditError::OverlappingEdits => f.write_str("two IFC edits overlap"),
        }
    }
}

impl std::error::Error for EditError {}

/// The original source bytes of one top-level entity argument, without surrounding trivia.
/// Complex instances are rejected because their leaves have independent attribute lists.
pub fn argument_source<'a>(
    source: &'a [u8],
    image: &ModelImage,
    express_id: u32,
    argument_index: usize,
) -> Result<&'a [u8], EditError> {
    let entry = checked_entry(source, image, express_id)?;
    if entry.flags & crate::image::ENTRY_COMPLEX != 0 {
        return Err(EditError::ComplexInstance(express_id));
    }
    let span = argument_span(source, entry, argument_index)?;
    Ok(&source[span])
}

/// Return the original source bytes for one complex-instance leaf argument.
pub fn leaf_argument_source<'a>(
    source: &'a [u8],
    image: &ModelImage,
    express_id: u32,
    leaf_class: &str,
    argument_index: usize,
) -> Result<&'a [u8], EditError> {
    let entry = checked_entry(source, image, express_id)?;
    if entry.flags & crate::image::ENTRY_COMPLEX == 0 {
        return Err(EditError::NoSuchLeaf {
            express_id,
            leaf: leaf_class.to_owned(),
        });
    }
    let span = leaf_argument_span(source, entry, leaf_class, argument_index)?;
    Ok(&source[span])
}

/// Apply edits together and return a new IFC source buffer.
/// Replacements are applied back-to-front so edits never invalidate each other's offsets.
#[cfg(feature = "edit")]
pub fn apply_edits(
    source: &[u8],
    image: &ModelImage,
    edits: &[AttributeEdit],
) -> Result<Vec<u8>, EditError> {
    let mut replacements = Vec::with_capacity(edits.len());
    for edit in edits {
        let entry = checked_entry(source, image, edit.express_id)?;
        let complex = entry.flags & crate::image::ENTRY_COMPLEX != 0;
        let span = match (&edit.leaf_class, complex) {
            (None, false) => argument_span(source, entry, edit.argument_index)?,
            (Some(leaf), true) => leaf_argument_span(source, entry, leaf, edit.argument_index)?,
            (None, true) => return Err(EditError::ComplexInstance(edit.express_id)),
            (Some(leaf), false) => {
                return Err(EditError::NoSuchLeaf {
                    express_id: edit.express_id,
                    leaf: leaf.clone(),
                });
            }
        };
        let bytes = encode_value(&edit.value)?;
        replacements.push((span, bytes));
    }

    replacements.sort_by_key(|(span, _)| span.start);
    for pair in replacements.windows(2) {
        if pair[0].0.end > pair[1].0.start {
            return Err(EditError::OverlappingEdits);
        }
    }

    let extra = replacements
        .iter()
        .map(|(span, bytes)| bytes.len().saturating_sub(span.len()))
        .sum();
    let mut output = Vec::with_capacity(source.len().saturating_add(extra));
    let mut cursor = 0usize;
    for (span, bytes) in replacements {
        output.extend_from_slice(&source[cursor..span.start]);
        output.extend_from_slice(&bytes);
        cursor = span.end;
    }
    output.extend_from_slice(&source[cursor..]);
    Ok(output)
}

fn checked_entry<'a>(
    source: &[u8],
    image: &'a ModelImage,
    express_id: u32,
) -> Result<&'a IndexEntry, EditError> {
    if image.source_len != source.len() {
        return Err(EditError::SourceMismatch);
    }
    let entry = image
        .entry(express_id)
        .ok_or(EditError::NoSuchEntity(express_id))?;
    let start = entry.source_off as usize;
    let end = start.saturating_add(entry.source_len as usize);
    if entry.source_off == u32::MAX || entry.source_len == u32::MAX || end > source.len() {
        return Err(EditError::SourceMismatch);
    }
    if source.get(start) != Some(&b'#') {
        return Err(EditError::SourceMismatch);
    }
    if crate::hash::fx_hash_bytes(&source[start..end]) != entry.source_hash {
        return Err(EditError::SourceMismatch);
    }
    Ok(entry)
}

fn argument_span(
    source: &[u8],
    entry: &IndexEntry,
    wanted: usize,
) -> Result<Range<usize>, EditError> {
    let record_start = entry.source_off as usize;
    let record_end = record_start + entry.source_len as usize;
    let mut at = record_start;

    at = skip_trivia(source, at, record_end);
    if source.get(at) != Some(&b'#') {
        return Err(EditError::SourceMismatch);
    }
    at += 1;
    while source.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
    }
    at = skip_trivia(source, at, record_end);
    if source.get(at) != Some(&b'=') {
        return Err(EditError::SourceMismatch);
    }
    at = skip_trivia(source, at + 1, record_end);
    if source.get(at) == Some(&b'(') {
        return Err(EditError::ComplexInstance(entry.express_id));
    }
    at = scan_keyword(source, at, record_end)
        .ok_or(EditError::UnsupportedValue(entry.express_id))?;
    at = skip_trivia(source, at, record_end);
    if source.get(at) != Some(&b'(') {
        return Err(EditError::SourceMismatch);
    }
    list_argument_span(source, at, record_end, wanted, entry.express_id)
}

fn list_argument_span(
    source: &[u8],
    open: usize,
    limit: usize,
    wanted: usize,
    express_id: u32,
) -> Result<Range<usize>, EditError> {
    let mut at = open + 1;
    let mut index = 0usize;
    loop {
        at = skip_trivia(source, at, limit);
        if source.get(at) == Some(&b')') {
            break;
        }
        let start = at;
        let end = scan_value(source, at, limit, 0, Origin::Source)
            .ok_or(EditError::UnsupportedValue(express_id))?;
        if index == wanted {
            return Ok(start..end);
        }
        index += 1;
        at = skip_trivia(source, end, limit);
        match source.get(at) {
            Some(b',') => at += 1,
            Some(b')') => break,
            _ => return Err(EditError::UnsupportedValue(express_id)),
        }
    }

    Err(EditError::NoSuchArgument {
        express_id,
        index: wanted,
    })
}

fn leaf_argument_span(
    source: &[u8],
    entry: &IndexEntry,
    leaf_class: &str,
    wanted: usize,
) -> Result<Range<usize>, EditError> {
    let record_start = entry.source_off as usize;
    let record_end = record_start + entry.source_len as usize;
    let mut at = skip_trivia(source, record_start, record_end);
    if source.get(at) != Some(&b'#') {
        return Err(EditError::SourceMismatch);
    }
    at += 1;
    while source.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
    }
    at = skip_trivia(source, at, record_end);
    if source.get(at) != Some(&b'=') {
        return Err(EditError::SourceMismatch);
    }
    at = skip_trivia(source, at + 1, record_end);
    if source.get(at) != Some(&b'(') {
        return Err(EditError::NoSuchLeaf {
            express_id: entry.express_id,
            leaf: leaf_class.to_owned(),
        });
    }
    at += 1;
    loop {
        at = skip_trivia(source, at, record_end);
        if source.get(at) == Some(&b')') {
            break;
        }
        let name_start = at;
        let name_end = scan_keyword(source, at, record_end)
            .ok_or(EditError::UnsupportedValue(entry.express_id))?;
        let open = skip_trivia(source, name_end, record_end);
        if source.get(open) != Some(&b'(') {
            return Err(EditError::UnsupportedValue(entry.express_id));
        }
        if source[name_start..name_end].eq_ignore_ascii_case(leaf_class.as_bytes()) {
            return list_argument_span(source, open, record_end, wanted, entry.express_id);
        }
        at = scan_list(source, open, record_end, 0, Origin::Source)
            .ok_or(EditError::UnsupportedValue(entry.express_id))?;
    }
    Err(EditError::NoSuchLeaf {
        express_id: entry.express_id,
        leaf: leaf_class.to_owned(),
    })
}

#[cfg(feature = "edit")]
fn encode_value(value: &EditValue) -> Result<Vec<u8>, EditError> {
    let encoded = match value {
        EditValue::Null => "$".to_owned(),
        EditValue::Derived => "*".to_owned(),
        EditValue::Integer(value) => value.to_string(),
        EditValue::Real(value) if value.is_finite() => {
            let mut text = value.to_string();
            if !text.contains(['.', 'e', 'E']) {
                text.push('.');
            }
            text
        }
        EditValue::Real(_) => {
            return Err(EditError::InvalidValue(
                "real numbers must be finite".to_owned(),
            ));
        }
        EditValue::String(value) => encode_string(value),
        EditValue::Enumeration(value) => {
            if value.is_empty()
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return Err(EditError::InvalidValue(
                    "enumerations may contain only ASCII letters, digits and underscores"
                        .to_owned(),
                ));
            }
            format!(".{}.", value.to_ascii_uppercase())
        }
        EditValue::Reference(value) => format!("#{value}"),
        EditValue::Raw(value) => {
            validate_raw(value.as_bytes())?;
            value.clone()
        }
    };
    Ok(encoded.into_bytes())
}

#[cfg(feature = "edit")]
fn encode_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    let mut escaped = false;
    for ch in value.chars() {
        let direct = ch.is_ascii() && (' '..='~').contains(&ch) && ch != '\\';
        if direct {
            if escaped {
                out.push_str("\\X0\\");
                escaped = false;
            }
            if ch == '\'' {
                out.push_str("''");
            } else {
                out.push(ch);
            }
        } else {
            if !escaped {
                out.push_str("\\X2\\");
                escaped = true;
            }
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units).iter() {
                use core::fmt::Write;
                let _ = write!(out, "{unit:04X}");
            }
        }
    }
    if escaped {
        out.push_str("\\X0\\");
    }
    out.push('\'');
    out
}

#[cfg(feature = "edit")]
fn validate_raw(value: &[u8]) -> Result<(), EditError> {
    let start = skip_trivia(value, 0, value.len());
    let Some(end) = scan_value(value, start, value.len(), 0, Origin::Replacement) else {
        return Err(EditError::InvalidValue("expected one value".to_owned()));
    };
    if skip_trivia(value, end, value.len()) != value.len() {
        return Err(EditError::InvalidValue(
            "trailing bytes after the value".to_owned(),
        ));
    }
    Ok(())
}

fn skip_trivia(source: &[u8], mut at: usize, limit: usize) -> usize {
    loop {
        while at < limit && source[at].is_ascii_whitespace() {
            at += 1;
        }
        if at + 1 < limit && source[at] == b'/' && source[at + 1] == b'*' {
            at += 2;
            while at + 1 < limit && !(source[at] == b'*' && source[at + 1] == b'/') {
                at += 1;
            }
            at = at.saturating_add(2).min(limit);
            continue;
        }
        return at;
    }
}

/// Whether the bytes being scanned already sit in a file, or are about to be
/// written into one. Only source may hold a bare keyword.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Origin {
    Source,
    #[cfg(feature = "edit")]
    Replacement,
}

/// A STEP keyword, matching what the parser accepts: an optional leading `!`,
/// then letters, digits, underscores and hyphens.
fn scan_keyword(source: &[u8], mut at: usize, limit: usize) -> Option<usize> {
    let start = at;
    if at < limit && source[at] == b'!' {
        at += 1;
    }
    while at < limit
        && (source[at].is_ascii_alphanumeric() || source[at] == b'_' || source[at] == b'-')
    {
        at += 1;
    }
    (at > start).then_some(at)
}

fn scan_value(
    source: &[u8],
    at: usize,
    limit: usize,
    depth: usize,
    origin: Origin,
) -> Option<usize> {
    if depth > 256 || at >= limit {
        return None;
    }
    match source[at] {
        b'$' | b'*' => Some(at + 1),
        b'#' => {
            let mut end = at + 1;
            while end < limit && source[end].is_ascii_digit() {
                end += 1;
            }
            (end > at + 1).then_some(end)
        }
        b'\'' => scan_quoted(source, at, limit, b'\'', true),
        b'"' => scan_quoted(source, at, limit, b'"', false),
        b'.' if source.get(at + 1).is_some_and(u8::is_ascii_digit) => {
            scan_number(source, at, limit)
        }
        b'.' => {
            let mut end = at + 1;
            while end < limit && source[end] != b'.' {
                if !(source[end].is_ascii_alphanumeric() || source[end] == b'_') {
                    return None;
                }
                end += 1;
            }
            (end < limit && end > at + 1).then_some(end + 1)
        }
        b'(' => scan_list(source, at, limit, depth + 1, origin),
        byte if byte.is_ascii_alphabetic() || byte == b'_' || byte == b'!' => {
            let name_end = scan_keyword(source, at, limit)?;
            let open = skip_trivia(source, name_end, limit);
            if source.get(open) == Some(&b'(') {
                scan_list(source, open, limit, depth + 1, origin)
            } else if origin == Origin::Source {
                // Some writers emit a bare keyword where a value belongs.
                Some(name_end)
            } else {
                None
            }
        }
        byte if byte.is_ascii_digit() || matches!(byte, b'+' | b'-') => {
            scan_number(source, at, limit)
        }
        _ => None,
    }
}

fn scan_number(source: &[u8], at: usize, limit: usize) -> Option<usize> {
    let mut end = at;
    if source
        .get(end)
        .is_some_and(|byte| matches!(byte, b'+' | b'-'))
    {
        end += 1;
    }
    let integer_start = end;
    while end < limit && source[end].is_ascii_digit() {
        end += 1;
    }
    let integer_digits = end - integer_start;
    let mut fraction_digits = 0;
    if source.get(end) == Some(&b'.') {
        end += 1;
        let fraction_start = end;
        while end < limit && source[end].is_ascii_digit() {
            end += 1;
        }
        fraction_digits = end - fraction_start;
    }
    if integer_digits + fraction_digits == 0 {
        return None;
    }
    if source
        .get(end)
        .is_some_and(|byte| matches!(byte, b'e' | b'E'))
    {
        end += 1;
        if source
            .get(end)
            .is_some_and(|byte| matches!(byte, b'+' | b'-'))
        {
            end += 1;
        }
        let exponent_start = end;
        while end < limit && source[end].is_ascii_digit() {
            end += 1;
        }
        if end == exponent_start {
            return None;
        }
    }
    Some(end)
}

fn scan_quoted(source: &[u8], at: usize, limit: usize, quote: u8, doubled: bool) -> Option<usize> {
    let mut end = at + 1;
    while end < limit {
        if source[end] != quote {
            end += 1;
            continue;
        }
        if doubled && end + 1 < limit && source[end + 1] == quote {
            end += 2;
            continue;
        }
        return Some(end + 1);
    }
    None
}

fn scan_list(
    source: &[u8],
    at: usize,
    limit: usize,
    depth: usize,
    origin: Origin,
) -> Option<usize> {
    if depth > 256 || source.get(at) != Some(&b'(') {
        return None;
    }
    let mut cursor = skip_trivia(source, at + 1, limit);
    if source.get(cursor) == Some(&b')') {
        return Some(cursor + 1);
    }
    loop {
        cursor = scan_value(source, cursor, limit, depth, origin)?;
        cursor = skip_trivia(source, cursor, limit);
        match source.get(cursor) {
            Some(b',') => cursor = skip_trivia(source, cursor + 1, limit),
            Some(b')') => return Some(cursor + 1),
            _ => return None,
        }
    }
}

#[cfg(all(test, feature = "edit"))]
mod tests {
    use super::*;
    use crate::{ParseOptions, parse};

    const SOURCE: &[u8] = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#1=IFCWALL('guid', /* owner */ $, 'Old name', $, $, $, $, 'tag', .SOLIDWALL.);\n\
#2=IFCVENDOREXTENSION((1.,2.),'keep');\nENDSEC;\nEND-ISO-10303-21;\n";

    #[test]
    fn a_string_edit_changes_only_the_argument() {
        let image = parse(SOURCE, &ParseOptions::default());
        let output = apply_edits(
            SOURCE,
            &image,
            &[AttributeEdit {
                express_id: 1,
                argument_index: 2,
                leaf_class: None,
                // The en dash is the fixture: it must come back as the \X2\ escape.
                value: EditValue::String("O'Brien – wall".to_owned()),
            }],
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("'O''Brien \\X2\\2013\\X0\\ wall'"));
        assert!(text.contains("/* owner */"));
        assert!(text.contains("#2=IFCVENDOREXTENSION((1.,2.),'keep');"));
    }

    #[test]
    fn edits_before_and_after_each_other_are_stable() {
        let image = parse(SOURCE, &ParseOptions::default());
        let output = apply_edits(
            SOURCE,
            &image,
            &[
                AttributeEdit {
                    express_id: 2,
                    argument_index: 0,
                    leaf_class: None,
                    value: EditValue::Raw("(3.,4.,5.)".to_owned()),
                },
                AttributeEdit {
                    express_id: 1,
                    argument_index: 7,
                    leaf_class: None,
                    value: EditValue::String("new tag".to_owned()),
                },
            ],
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("(3.,4.,5.),'keep'"));
        assert!(text.contains("'new tag', .SOLIDWALL."));
    }

    #[test]
    fn original_argument_bytes_are_available() {
        let image = parse(SOURCE, &ParseOptions::default());
        assert_eq!(
            argument_source(SOURCE, &image, 1, 2).unwrap(),
            b"'Old name'"
        );
        assert_eq!(argument_source(SOURCE, &image, 2, 0).unwrap(), b"(1.,2.)");
    }

    #[test]
    fn invalid_raw_values_never_change_the_source() {
        let image = parse(SOURCE, &ParseOptions::default());
        let error = apply_edits(
            SOURCE,
            &image,
            &[AttributeEdit {
                express_id: 1,
                argument_index: 2,
                leaf_class: None,
                value: EditValue::Raw("'one' garbage".to_owned()),
            }],
        )
        .unwrap_err();
        assert!(matches!(error, EditError::InvalidValue(_)));

        for invalid in ["+", "-", "1e", "NAME", "(.T.,1e)"] {
            let error = validate_raw(invalid.as_bytes()).unwrap_err();
            assert!(matches!(error, EditError::InvalidValue(_)), "{invalid}");
        }
        for valid in [".5", "-1.2E+3", ".T.", "TYPE(1.)", "(1.,#2,$)"] {
            validate_raw(valid.as_bytes()).unwrap();
        }
    }

    const KEYWORDS: &[u8] = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#1=IFCWALL('a',$,U,'name',$,$,$,$,T);\n\
#2=!VENDOR(1.5,'x');\nENDSEC;\nEND-ISO-10303-21;\n";

    #[test]
    fn bare_keywords_and_user_defined_records_can_be_spanned() {
        let image = parse(KEYWORDS, &ParseOptions::default());
        assert_eq!(argument_source(KEYWORDS, &image, 1, 2).unwrap(), b"U");
        assert_eq!(argument_source(KEYWORDS, &image, 1, 3).unwrap(), b"'name'");
        assert_eq!(argument_source(KEYWORDS, &image, 1, 8).unwrap(), b"T");
        assert_eq!(argument_source(KEYWORDS, &image, 2, 0).unwrap(), b"1.5");

        let output = apply_edits(
            KEYWORDS,
            &image,
            &[AttributeEdit {
                express_id: 1,
                argument_index: 3,
                leaf_class: None,
                value: EditValue::String("new".to_owned()),
            }],
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("'a',$,U,'new',$"));
    }

    #[test]
    fn a_different_source_is_refused() {
        let image = parse(SOURCE, &ParseOptions::default());
        assert_eq!(
            argument_source(&SOURCE[..SOURCE.len() - 1], &image, 1, 0),
            Err(EditError::SourceMismatch)
        );

        let mut same_length = SOURCE.to_vec();
        let at = same_length.iter().position(|byte| *byte == b'g').unwrap();
        same_length[at] = b'x';
        assert_eq!(
            argument_source(&same_length, &image, 1, 0),
            Err(EditError::SourceMismatch)
        );
    }

    #[test]
    fn a_complex_leaf_can_be_edited_without_touching_its_siblings() {
        let source = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#1=(IFCNAMEDUNIT(*,.LENGTHUNIT.) IFCSIUNIT($,.METRE.));\nENDSEC;\n";
        let image = parse(source, &ParseOptions::default());
        assert_eq!(
            leaf_argument_source(source, &image, 1, "IfcSIUnit", 1).unwrap(),
            b".METRE."
        );
        let edited = apply_edits(
            source,
            &image,
            &[AttributeEdit {
                express_id: 1,
                argument_index: 1,
                leaf_class: Some("IfcSIUnit".to_owned()),
                value: EditValue::Raw(".FOOT.".to_owned()),
            }],
        )
        .unwrap();
        let text = String::from_utf8(edited).unwrap();
        assert!(text.contains("IFCNAMEDUNIT(*,.LENGTHUNIT.) IFCSIUNIT($,.FOOT.)"));
    }
}
