// SPDX-License-Identifier: Apache-2.0
//! Property tests: whatever we can write, we must be able to read back.
//! A generated tree of STEP values is rendered, parsed and compared, so
//! anything the parser silently changes shows up here.

use proptest::prelude::*;
use tessifc_step::strings::decode;
use tessifc_step::tape::RawValue;
use tessifc_step::{ParseOptions, parse};

/// A value we can both render and expect.
#[derive(Clone, Debug, PartialEq)]
enum Gen {
    Null,
    Derived,
    Int(i64),
    Real(f64),
    Str(String),
    Enum(String),
    Ref(u32),
    List(Vec<Gen>),
    Typed(String, Box<Gen>),
}

impl Gen {
    /// Render as it would appear in a STEP file.
    fn render(&self, out: &mut String) {
        match self {
            Gen::Null => out.push('$'),
            Gen::Derived => out.push('*'),
            Gen::Int(v) => out.push_str(&v.to_string()),
            Gen::Real(v) => out.push_str(&render_real(*v)),
            Gen::Str(s) => {
                out.push('\'');
                for ch in s.chars() {
                    if ch == '\'' {
                        out.push_str("''");
                    } else {
                        out.push(ch);
                    }
                }
                out.push('\'');
            }
            Gen::Enum(s) => {
                out.push('.');
                out.push_str(s);
                out.push('.');
            }
            Gen::Ref(id) => {
                out.push('#');
                out.push_str(&id.to_string());
            }
            Gen::List(items) => {
                out.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.render(out);
                }
                out.push(')');
            }
            Gen::Typed(name, inner) => {
                out.push_str(name);
                out.push('(');
                inner.render(out);
                out.push(')');
            }
        }
    }

    /// The tape values this must produce, in order.
    fn expect(&self, out: &mut Vec<Expected>) {
        match self {
            Gen::Null => out.push(Expected::Null),
            Gen::Derived => out.push(Expected::Derived),
            Gen::Int(v) => out.push(Expected::Int(*v)),
            Gen::Real(v) => out.push(Expected::Real(*v)),
            Gen::Str(s) => out.push(Expected::Str(s.clone())),
            Gen::Enum(s) => out.push(Expected::Enum(s.clone())),
            Gen::Ref(id) => out.push(Expected::Ref(*id)),
            Gen::List(items) => {
                out.push(Expected::ListStart);
                for item in items {
                    item.expect(out);
                }
                out.push(Expected::ListEnd);
            }
            Gen::Typed(name, inner) => {
                out.push(Expected::Typed(name.to_ascii_uppercase()));
                inner.expect(out);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Expected {
    Null,
    Derived,
    Int(i64),
    Real(f64),
    Str(String),
    Enum(String),
    Ref(u32),
    ListStart,
    ListEnd,
    Typed(String),
}

/// Render a float the way a STEP writer does: always with a decimal point.
fn render_real(v: f64) -> String {
    let text = format!("{v:?}");
    if text.contains('.') || text.contains('e') || text.contains('E') {
        text
    } else {
        format!("{text}.")
    }
}

fn leaf() -> impl Strategy<Value = Gen> {
    prop_oneof![
        Just(Gen::Null),
        Just(Gen::Derived),
        any::<i32>().prop_map(|v| Gen::Int(v as i64)),
        // Finite reals in a range a coordinate could plausibly take.
        (-1.0e9f64..1.0e9f64).prop_map(Gen::Real),
        // Strings from an alphabet that includes characters that break naive scanners.
        proptest::collection::vec(
            prop_oneof![
                Just('a'),
                Just('Z'),
                Just('0'),
                Just(' '),
                Just('\''),
                Just(';'),
                Just(')'),
                Just('('),
                Just(','),
                Just('#'),
                Just('/'),
                Just('*'),
                Just('$'),
                Just('.'),
            ],
            0..12
        )
        .prop_map(|chars| Gen::Str(chars.into_iter().collect())),
        "[A-Z][A-Z0-9_]{0,8}".prop_map(Gen::Enum),
        (1u32..1_000_000).prop_map(Gen::Ref),
    ]
}

fn value() -> impl Strategy<Value = Gen> {
    leaf().prop_recursive(4, 32, 5, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..5).prop_map(Gen::List),
            ("IFC[A-Z]{2,10}", inner).prop_map(|(name, v)| Gen::Typed(name, Box::new(v))),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Every value we can render must come back off the tape unchanged.
    #[test]
    fn values_round_trip(args in proptest::collection::vec(value(), 1..8)) {
        let mut record = String::from("#1=IFCTEST(");
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                record.push(',');
            }
            arg.render(&mut record);
        }
        record.push_str(");\n");

        let source = format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{record}ENDSEC;\n"
        );
        let image = parse(source.as_bytes(), &ParseOptions::default());

        prop_assert_eq!(image.len(), 1, "record was dropped: {}", record);

        let mut expected = Vec::new();
        for arg in &args {
            arg.expect(&mut expected);
        }

        let mut got = Vec::new();
        let mut cursor = image.args(1).expect("record must be on the tape");
        while let Some(value) = cursor.read() {
            got.push(match value {
                RawValue::Null => Expected::Null,
                RawValue::Derived => Expected::Derived,
                RawValue::Int(v) => Expected::Int(v),
                RawValue::Real(v) => Expected::Real(v),
                RawValue::Str(id) => Expected::Str(decode(image.strings.get(id))),
                RawValue::Enum(id) => {
                    Expected::Enum(String::from_utf8_lossy(image.strings.get(id)).into_owned())
                }
                RawValue::Binary(id) => {
                    Expected::Str(String::from_utf8_lossy(image.strings.get(id)).into_owned())
                }
                RawValue::Ref(id) => Expected::Ref(id),
                RawValue::ListStart => Expected::ListStart,
                RawValue::ListEnd => Expected::ListEnd,
                RawValue::Typed(id) => {
                    Expected::Typed(String::from_utf8_lossy(image.strings.get(id)).into_owned())
                }
                RawValue::Leaf(_) => Expected::Derived,
            });
        }

        prop_assert_eq!(&got, &expected, "\nrecord: {}\n", record);
    }

    /// Reals must survive the text round trip exactly, bit for bit.
    #[test]
    fn reals_survive_exactly(v in prop_oneof![
        -1.0e12f64..1.0e12f64,
        -1.0e-6f64..1.0e-6f64,
        Just(0.0f64),
        Just(-0.0f64),
    ]) {
        let source = format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
             #1=IFCCARTESIANPOINT(({},0.,0.));\nENDSEC;\n",
            render_real(v)
        );
        let image = parse(source.as_bytes(), &ParseOptions::default());
        let mut cursor = image.args(1).expect("record must be on the tape");
        prop_assert_eq!(cursor.read(), Some(RawValue::ListStart));
        match cursor.read() {
            Some(RawValue::Real(got)) => prop_assert_eq!(got.to_bits(), v.to_bits()),
            other => prop_assert!(false, "expected a real, got {:?}", other),
        }
    }

    /// Arbitrary bytes must never panic and must never invent instances.
    #[test]
    fn arbitrary_bytes_are_survivable(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let image = parse(&bytes, &ParseOptions::default());
        // Whatever came out, the invariants hold.
        let mut previous = 0u32;
        for entry in &image.index {
            prop_assert!(entry.express_id > previous);
            previous = entry.express_id;
            let end = entry.tape_off as usize + entry.tape_len as usize;
            prop_assert!(end <= image.tape.len());
        }
    }

    /// Valid records must yield exactly that many instances, whatever trivia surrounds them.
    #[test]
    fn whitespace_and_comments_do_not_change_the_count(
        count in 1usize..30,
        spacer in prop_oneof![
            Just(""),
            Just(" "),
            Just("\n"),
            Just("\r\n"),
            Just("\t"),
            Just("/* c */"),
            Just("\n/* multi\nline */\n"),
        ],
    ) {
        let mut data = String::new();
        for i in 1..=count {
            data.push_str(&format!(
                "#{i}{spacer}={spacer}IFCCARTESIANPOINT({spacer}({spacer}0.,1.,2.{spacer}){spacer});\n"
            ));
        }
        let source = format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{data}ENDSEC;\n"
        );
        let image = parse(source.as_bytes(), &ParseOptions::default());
        prop_assert_eq!(image.len(), count);
        prop_assert!(!image.diagnostics.has_errors());
    }
}
