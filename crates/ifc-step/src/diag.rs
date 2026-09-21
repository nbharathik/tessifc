// SPDX-License-Identifier: Apache-2.0
//! Diagnostics: what went wrong, where, and how badly.
//! Codes are stable `&'static str` constants so downstream crates can define
//! their own; messages are human-readable and not stable.

use core::fmt;

/// How much a diagnostic should worry the caller.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    /// Something to know about; output is unaffected.
    Info,
    /// Output is affected but usable: a degraded mesh, an approximated schema.
    Warning,
    /// Something was lost: an instance was skipped, a body could not be built.
    Error,
}

impl Severity {
    /// Lower-case name, as it appears in the IGP diagnostics table.
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warn",
            Severity::Error => "error",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A stable diagnostic code: `E_` errors, `W_` warnings, `I_` information.
/// Codes are never renamed or reused once released.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct DiagCode(pub &'static str);

impl DiagCode {
    /// The file does not begin with `ISO-10303-21;`.
    pub const NOT_STEP: DiagCode = DiagCode("E_NOT_A_STEP_FILE");
    /// The file ends in the middle of a section, a record or a literal.
    pub const TRUNCATED: DiagCode = DiagCode("E_TRUNCATED_FILE");
    /// A record could not be parsed and was skipped.
    pub const RECORD_SKIPPED: DiagCode = DiagCode("E_RECORD_SKIPPED");
    /// A record ended before its argument list was closed.
    pub const UNCLOSED_RECORD: DiagCode = DiagCode("E_UNCLOSED_RECORD");
    /// A string literal was never closed.
    pub const UNCLOSED_STRING: DiagCode = DiagCode("E_UNCLOSED_STRING");
    /// A comment was never closed.
    pub const UNCLOSED_COMMENT: DiagCode = DiagCode("E_UNCLOSED_COMMENT");
    /// An entity instance name did not fit in 32 bits.
    pub const ID_OVERFLOW: DiagCode = DiagCode("E_ENTITY_ID_OVERFLOW");
    /// A number could not be parsed.
    pub const BAD_NUMBER: DiagCode = DiagCode("E_BAD_NUMBER");
    /// A configured limit was reached and parsing stopped early.
    pub const LIMIT_REACHED: DiagCode = DiagCode("E_LIMIT_REACHED");
    /// Lists nested deeper than the configured limit.
    pub const NESTING_TOO_DEEP: DiagCode = DiagCode("E_NESTING_TOO_DEEP");
    /// No `DATA` section was found.
    pub const NO_DATA_SECTION: DiagCode = DiagCode("E_NO_DATA_SECTION");
    /// An IFCZIP archive this reader cannot walk; nothing was read.
    pub const IFCZIP_MALFORMED: DiagCode = DiagCode("E_IFCZIP_MALFORMED");
    /// An IFCZIP entry larger than the configured limit; nothing was read.
    pub const IFCZIP_TOO_LARGE: DiagCode = DiagCode("E_IFCZIP_TOO_LARGE");

    /// The same express id was defined more than once; the first won.
    pub const DUPLICATE_ID: DiagCode = DiagCode("W_DUPLICATE_ENTITY_ID");
    /// A class name not in the loaded schema; the instance was kept as `UNKNOWN`.
    pub const UNKNOWN_CLASS: DiagCode = DiagCode("W_UNKNOWN_CLASS");
    /// A record carries a different number of arguments than the schema says.
    pub const ARITY_MISMATCH: DiagCode = DiagCode("W_ARITY_MISMATCH");
    /// `FILE_SCHEMA` names a schema without tables; the nearest was used.
    pub const SCHEMA_APPROXIMATED: DiagCode = DiagCode("W_SCHEMA_APPROXIMATED");
    /// `FILE_SCHEMA` is missing or unreadable; the schema was guessed.
    pub const SCHEMA_GUESSED: DiagCode = DiagCode("W_SCHEMA_GUESSED");
    /// The header section is malformed.
    pub const BAD_HEADER: DiagCode = DiagCode("W_BAD_HEADER");
    /// A record was not terminated by a semicolon; it was kept anyway.
    pub const MISSING_SEMICOLON: DiagCode = DiagCode("W_MISSING_SEMICOLON");
    /// A real literal did not fit in a double; zero was stored instead.
    pub const NUMBER_OUT_OF_RANGE: DiagCode = DiagCode("W_NUMBER_OUT_OF_RANGE");
    /// A complex instance was read; its leaves are all kept.
    pub const COMPLEX_INSTANCE: DiagCode = DiagCode("I_COMPLEX_INSTANCE");
    /// An IFCZIP archive holds several `.ifc` entries; only the first was read.
    pub const IFCZIP_MULTIPLE_ENTRIES: DiagCode = DiagCode("W_IFCZIP_MULTIPLE_ENTRIES");

    /// The code text, for serialisation.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for DiagCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// One thing that went wrong.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// The instance it happened on, when one was identified.
    pub express_id: Option<u32>,
    /// One-based source line, or 0 when not applicable.
    pub line: u32,
    /// How bad it is.
    pub severity: Severity,
    /// The stable code. Branch on this, never on `message`.
    pub code: DiagCode,
    /// A human-readable explanation. Not stable across versions.
    pub message: String,
}

impl Diagnostic {
    /// An error diagnostic.
    pub fn error(code: DiagCode, line: u32, message: impl Into<String>) -> Self {
        Diagnostic {
            express_id: None,
            line,
            severity: Severity::Error,
            code,
            message: message.into(),
        }
    }

    /// A warning diagnostic.
    pub fn warning(code: DiagCode, line: u32, message: impl Into<String>) -> Self {
        Diagnostic {
            express_id: None,
            line,
            severity: Severity::Warning,
            code,
            message: message.into(),
        }
    }

    /// An informational diagnostic.
    pub fn info(code: DiagCode, line: u32, message: impl Into<String>) -> Self {
        Diagnostic {
            express_id: None,
            line,
            severity: Severity::Info,
            code,
            message: message.into(),
        }
    }

    /// Attach an express id.
    pub fn with_id(mut self, id: u32) -> Self {
        self.express_id = Some(id);
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} at line {}", self.severity, self.code, self.line)?;
        if let Some(id) = self.express_id {
            write!(f, " (#{id})")?;
        }
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

/// A capped collection of diagnostics; past the cap they are counted, not stored.
#[derive(Clone, Debug)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
    dropped: usize,
    cap: usize,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Diagnostics::with_cap(10_000)
    }
}

impl Diagnostics {
    /// A collection that stores at most `cap` diagnostics.
    pub fn with_cap(cap: usize) -> Self {
        Diagnostics {
            items: Vec::new(),
            dropped: 0,
            cap,
        }
    }

    /// Record one.
    pub fn push(&mut self, d: Diagnostic) {
        if self.items.len() < self.cap {
            self.items.push(d);
        } else {
            self.dropped += 1;
        }
    }

    /// Whether another one would be dropped, so a caller can skip formatting it.
    pub fn is_full(&self) -> bool {
        self.items.len() >= self.cap
    }

    /// Everything stored.
    pub fn items(&self) -> &[Diagnostic] {
        &self.items
    }

    /// How many were discarded after the cap was reached.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// Total recorded, including discarded ones.
    pub fn total(&self) -> usize {
        self.items.len() + self.dropped
    }

    /// True when nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// How many of a given severity are stored.
    pub fn count_of(&self, severity: Severity) -> usize {
        self.items.iter().filter(|d| d.severity == severity).count()
    }

    /// Whether any stored diagnostic has error severity.
    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cap_holds() {
        let mut d = Diagnostics::with_cap(3);
        for i in 0..10 {
            d.push(Diagnostic::error(DiagCode::BAD_NUMBER, i, "nope"));
        }
        assert_eq!(d.items().len(), 3);
        assert_eq!(d.dropped(), 7);
        assert_eq!(d.total(), 10);
    }

    #[test]
    fn display_is_readable() {
        let d = Diagnostic::warning(DiagCode::UNKNOWN_CLASS, 42, "IFCFOO").with_id(7);
        assert_eq!(
            d.to_string(),
            "warn: W_UNKNOWN_CLASS at line 42 (#7): IFCFOO"
        );
    }
}
