// SPDX-License-Identifier: Apache-2.0
//! Values read out of a record, with the model attached so they can be used.

use crate::Model;
use tessifc_step::strings::StrId;
use tessifc_step::tape::{Cursor, RawValue};

/// A string, enumeration symbol or binary literal, still in its source form.
/// [`Text::is`] compares without decoding; use it for enumeration checks.
#[derive(Copy, Clone)]
pub struct Text<'a> {
    model: &'a Model,
    id: StrId,
}

impl<'a> Text<'a> {
    pub(crate) fn new(model: &'a Model, id: StrId) -> Self {
        Text { model, id }
    }

    /// The raw bytes, still carrying any STEP escape sequences.
    pub fn raw(&self) -> &'a [u8] {
        self.model.image().strings.get(self.id)
    }

    /// Decode to UTF-8, resolving escapes. Allocates.
    pub fn decode(&self) -> String {
        self.model.image().strings.decode(self.id)
    }

    /// True when the raw bytes equal `other`, ignoring ASCII case; allocation free.
    pub fn is(&self, other: &str) -> bool {
        self.raw().eq_ignore_ascii_case(other.as_bytes())
    }

    /// True when the raw bytes are empty.
    pub fn is_empty(&self) -> bool {
        self.raw().is_empty()
    }

    /// The interned id, for callers that want to compare two texts cheaply.
    pub fn id(&self) -> StrId {
        self.id
    }
}

impl core::fmt::Debug for Text<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", String::from_utf8_lossy(self.raw()))
    }
}

impl core::fmt::Display for Text<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.decode())
    }
}

/// One attribute value.
/// [`Value::Missing`] is an absent argument; [`Value::Null`] is one written `$`.
#[derive(Copy, Clone)]
pub enum Value<'a> {
    /// The argument is not there at all: the record is shorter than the schema.
    Missing,
    /// `$`.
    Null,
    /// `*`: a derived attribute, computed rather than stored.
    Derived,
    /// An integer.
    Int(i64),
    /// A real.
    Real(f64),
    /// A string literal.
    Str(Text<'a>),
    /// An enumeration symbol, without its dots.
    Enum(Text<'a>),
    /// A binary literal, as its raw hexadecimal text.
    Binary(Text<'a>),
    /// A reference to another instance; a dangling one reports `exists()` false.
    Ref(Entity<'a>),
    /// A list. Iterate it; it does not allocate.
    List(ListIter<'a>),
    /// A typed value such as `IFCLENGTHMEASURE(3.)`.
    Typed(TypedValue<'a>),
}

impl<'a> Value<'a> {
    /// True for [`Value::Null`] and [`Value::Missing`].
    pub fn is_nothing(&self) -> bool {
        matches!(self, Value::Null | Value::Missing)
    }

    /// The value as a float, unwrapping typed values; integers widen.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Real(v) => Some(*v),
            Value::Int(v) => Some(*v as f64),
            Value::Typed(t) => t.value().as_f64(),
            _ => None,
        }
    }

    /// The value as an integer, unwrapping typed values.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            // A real that happens to be integral: writers do this for indices.
            Value::Real(v) if v.fract() == 0.0 && v.is_finite() => Some(*v as i64),
            Value::Typed(t) => t.value().as_i64(),
            _ => None,
        }
    }

    /// The value as a boolean from `.T.` / `.F.`, unwrapping typed values; `.U.` is `None`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Enum(t) => {
                if t.is("T") || t.is("TRUE") {
                    Some(true)
                } else if t.is("F") || t.is("FALSE") {
                    Some(false)
                } else {
                    None
                }
            }
            Value::Typed(t) => t.value().as_bool(),
            _ => None,
        }
    }

    /// The value as text, for strings, enumerations and binaries.
    pub fn as_text(&self) -> Option<Text<'a>> {
        match self {
            Value::Str(t) | Value::Enum(t) | Value::Binary(t) => Some(*t),
            Value::Typed(t) => t.value().as_text(),
            _ => None,
        }
    }

    /// Decode a string value. Allocates; `as_text().is(...)` does not.
    pub fn as_string(&self) -> Option<String> {
        self.as_text().map(|t| t.decode())
    }

    /// The referenced entity, if this is a reference to one that exists.
    pub fn as_entity(&self) -> Option<Entity<'a>> {
        match self {
            Value::Ref(e) if e.exists() => Some(*e),
            Value::Typed(t) => t.value().as_entity(),
            _ => None,
        }
    }

    /// The list, if this is one.
    pub fn as_list(&self) -> Option<ListIter<'a>> {
        match self {
            Value::List(l) => Some(*l),
            Value::Typed(t) => t.value().as_list(),
            _ => None,
        }
    }

    /// Read a list of exactly `N` floats; longer lists are truncated, shorter yield `None`.
    pub fn as_floats<const N: usize>(&self) -> Option<[f64; N]> {
        let mut out = [0.0f64; N];
        let mut list = self.as_list()?;
        for slot in out.iter_mut() {
            *slot = list.next()?.as_f64()?;
        }
        Some(out)
    }

    /// The number of elements when this is a list; zero for anything else. Walks the list.
    pub fn list_len(&self) -> usize {
        match self.as_list() {
            Some(list) => list.count(),
            None => 0,
        }
    }

    /// True when this is a list with no elements; false for anything that is not a list.
    pub fn list_is_empty(&self) -> bool {
        self.as_list().is_some_and(|mut list| list.next().is_none())
    }
}

impl core::fmt::Debug for Value<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Value::Missing => f.write_str("Missing"),
            Value::Null => f.write_str("$"),
            Value::Derived => f.write_str("*"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Real(v) => write!(f, "{v}"),
            Value::Str(t) => write!(f, "{t:?}"),
            Value::Enum(t) => write!(f, ".{}.", String::from_utf8_lossy(t.raw())),
            Value::Binary(t) => write!(f, "\"{}\"", String::from_utf8_lossy(t.raw())),
            Value::Ref(e) => write!(f, "#{}", e.id()),
            Value::List(l) => {
                let items: Vec<_> = l.take(8).collect();
                write!(f, "{items:?}")
            }
            Value::Typed(t) => write!(f, "{}({:?})", t.name(), t.value()),
        }
    }
}

/// A typed value: a type name wrapping exactly one value.
#[derive(Copy, Clone)]
pub struct TypedValue<'a> {
    model: &'a Model,
    name: StrId,
    payload: Cursor<'a>,
}

impl<'a> TypedValue<'a> {
    pub(crate) fn new(model: &'a Model, name: StrId, payload: Cursor<'a>) -> Self {
        TypedValue {
            model,
            name,
            payload,
        }
    }

    /// The type name, as written, for example `IFCLENGTHMEASURE`.
    pub fn name(&self) -> Text<'a> {
        Text::new(self.model, self.name)
    }

    /// True when the type name matches, ignoring ASCII case.
    pub fn is(&self, name: &str) -> bool {
        self.name().is(name)
    }

    /// The wrapped value.
    pub fn value(&self) -> Value<'a> {
        let mut cursor = self.payload;
        crate::read_value(self.model, &mut cursor)
    }
}

/// Iterator over the elements of a list. Copy, so it can be restarted.
#[derive(Copy, Clone)]
pub struct ListIter<'a> {
    model: &'a Model,
    cursor: Cursor<'a>,
    done: bool,
}

impl<'a> ListIter<'a> {
    pub(crate) fn new(model: &'a Model, cursor: Cursor<'a>) -> Self {
        ListIter {
            model,
            cursor,
            done: false,
        }
    }

    /// The entities of a list of references, skipping anything that is not a live reference.
    pub fn entities(self) -> impl Iterator<Item = Entity<'a>> {
        self.filter_map(|v| v.as_entity())
    }

    /// Collect a list of numbers, skipping anything that is not one.
    pub fn floats(self) -> impl Iterator<Item = f64> {
        self.filter_map(|v| v.as_f64())
    }
}

impl<'a> Iterator for ListIter<'a> {
    type Item = Value<'a>;

    fn next(&mut self) -> Option<Value<'a>> {
        if self.done {
            return None;
        }
        match self.cursor.peek() {
            None | Some(RawValue::ListEnd) => {
                self.done = true;
                None
            }
            Some(_) => Some(crate::read_value(self.model, &mut self.cursor)),
        }
    }
}

use crate::Entity;
