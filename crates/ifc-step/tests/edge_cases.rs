// SPDX-License-Identifier: Apache-2.0
//! Edge cases from ISO 10303-21 and from what real exporters actually write.
//! No input may panic, and every deliberate deviation from the standard is
//! written down as a test. The standard is cited, never copied.

use tessifc_step::tape::RawValue;
use tessifc_step::{DiagCode, ModelImage, ParseOptions, SchemaId, Severity, parse};

/// Wrap a DATA body in the smallest valid envelope.
fn ifc(schema: &str, data: &str) -> String {
    format!(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
         FILE_NAME('t','2026-08-29T00:00:00',(''),(''),'','','');\n\
         FILE_SCHEMA(('{schema}'));\nENDSEC;\nDATA;\n{data}ENDSEC;\nEND-ISO-10303-21;\n"
    )
}

fn read(source: &str) -> ModelImage {
    parse(source.as_bytes(), &ParseOptions::default())
}

fn read_bytes(source: &[u8]) -> ModelImage {
    parse(source, &ParseOptions::default())
}

fn has_code(image: &ModelImage, code: DiagCode) -> bool {
    image.diagnostics.items().iter().any(|d| d.code == code)
}

fn args_of(image: &ModelImage, id: u32) -> Vec<RawValue> {
    let mut out = Vec::new();
    if let Some(mut cursor) = image.args(id) {
        while let Some(value) = cursor.read() {
            out.push(value);
        }
    }
    out
}

// --------------------------------------------------------------- the envelope

#[test]
fn minimal_file() {
    let image = read(&ifc("IFC4", "#1=IFCCARTESIANPOINT((0.,0.,0.));\n"));
    assert_eq!(image.len(), 1);
    assert_eq!(image.schema, SchemaId::Ifc4);
    assert_eq!(image.class_name_of(1), "IfcCartesianPoint");
    assert!(!image.diagnostics.has_errors());
}

#[test]
fn an_empty_file_is_diagnosed_not_crashed() {
    let image = read_bytes(b"");
    assert_eq!(image.len(), 0);
    assert!(image.diagnostics.has_errors());
}

#[test]
fn whitespace_only() {
    let image = read_bytes(b"   \r\n\t  \n");
    assert_eq!(image.len(), 0);
    assert!(image.diagnostics.has_errors());
}

#[test]
fn missing_iso_header_still_parses_the_data() {
    let source = "HEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
                  #1=IFCWALL('g',$,$,$,$,$,$,$,$);\nENDSEC;\n";
    let image = read(source);
    assert_eq!(image.len(), 1);
    assert!(has_code(&image, DiagCode::NOT_STEP));
}

#[test]
fn a_byte_order_mark_is_skipped() {
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(ifc("IFC4", "#1=IFCWALL('g',$,$,$,$,$,$,$,$);\n").as_bytes());
    let image = read_bytes(&bytes);
    assert_eq!(image.len(), 1);
    assert!(!has_code(&image, DiagCode::NOT_STEP));
}

#[test]
fn crlf_line_endings() {
    let source = ifc("IFC4", "#1=IFCWALL('g',$,$,$,$,$,$,$,$);\n").replace('\n', "\r\n");
    let image = read(&source);
    assert_eq!(image.len(), 1);
    assert!(!image.diagnostics.has_errors());
}

#[test]
fn several_data_sections() {
    let source = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\n\
                  DATA;\n#1=IFCWALL('a',$,$,$,$,$,$,$,$);\nENDSEC;\n\
                  DATA;\n#2=IFCSLAB('b',$,$,$,$,$,$,$,$);\nENDSEC;\nEND-ISO-10303-21;\n";
    let image = read(source);
    assert_eq!(image.len(), 2);
}

#[test]
fn a_parameterised_data_section_header() {
    // ISO 10303-21 allows DATA(('urn:x')); TessIFC skips the parameters.
    let source = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\n\
                  DATA(('urn:example'));\n#1=IFCWALL('a',$,$,$,$,$,$,$,$);\nENDSEC;\n\
                  END-ISO-10303-21;\n";
    let image = read(source);
    assert_eq!(image.len(), 1);
}

#[test]
fn a_missing_endsec_does_not_lose_the_records() {
    let source = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
                  #1=IFCWALL('a',$,$,$,$,$,$,$,$);\nEND-ISO-10303-21;\n";
    let image = read(source);
    assert_eq!(image.len(), 1);
}

// ------------------------------------------------------------------- the header

#[test]
fn the_header_is_decoded() {
    let image = read(&ifc("IFC4", ""));
    assert_eq!(image.header.name, "t");
    assert_eq!(image.header.time_stamp, "2026-08-29T00:00:00");
    assert_eq!(image.header.schema_identifiers, vec!["IFC4".to_string()]);
}

#[test]
fn a_header_comment_block_before_file_description() {
    // bim-whale-ifc-samples opens its header with a C-style comment block.
    let source = "ISO-10303-21;\nHEADER;\n/* a comment\n   over lines */\n\
                  FILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
                  #1=IFCWALL('a',$,$,$,$,$,$,$,$);\nENDSEC;\nEND-ISO-10303-21;\n";
    let image = read(source);
    assert_eq!(image.schema, SchemaId::Ifc4);
    assert_eq!(image.len(), 1);
}

#[test]
fn a_missing_file_schema_falls_back_to_ifc4() {
    let source = "ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n\
                  #1=IFCWALL('a',$,$,$,$,$,$,$,$);\nENDSEC;\nEND-ISO-10303-21;\n";
    let image = read(source);
    assert_eq!(image.schema, SchemaId::Ifc4);
    assert!(has_code(&image, DiagCode::SCHEMA_GUESSED));
}

/// Only meaningful when every schema is compiled in; single-schema builds map everything to it.
#[test]
#[cfg(all(
    feature = "schema-ifc2x3",
    feature = "schema-ifc4",
    feature = "schema-ifc4x3"
))]
fn schema_variants_map_to_tables() {
    for (declared, expected) in [
        ("IFC2X3", SchemaId::Ifc2x3),
        ("IFC2X3_TC1", SchemaId::Ifc2x3),
        ("IFC4", SchemaId::Ifc4),
        ("IFC4_ADD2_TC1", SchemaId::Ifc4),
        ("IFC4X3", SchemaId::Ifc4x3),
        ("IFC4X3_ADD2", SchemaId::Ifc4x3),
    ] {
        let image = read(&ifc(declared, "#1=IFCWALL('a',$,$,$,$,$,$,$,$);\n"));
        assert_eq!(image.schema, expected, "for {declared}");
    }
}

#[test]
fn an_unrecognised_schema_is_approximated_loudly() {
    let image = read(&ifc("IFC9X9", "#1=IFCWALL('a',$,$,$,$,$,$,$,$);\n"));
    assert_eq!(image.schema, SchemaId::Ifc4);
    assert!(has_code(&image, DiagCode::SCHEMA_GUESSED));
}

#[test]
#[cfg(all(feature = "schema-ifc4", feature = "schema-ifc4x3"))]
fn a_withdrawn_schema_maps_to_the_nearest() {
    let image = read(&ifc("IFC4X1", "#1=IFCWALL('a',$,$,$,$,$,$,$,$);\n"));
    assert_eq!(image.schema, SchemaId::Ifc4x3);
    assert!(has_code(&image, DiagCode::SCHEMA_APPROXIMATED));
}

// ---------------------------------------------------------------- instance names

#[test]
fn leading_zeros_in_an_instance_name_are_not_significant() {
    // ISO 10303-21 clause 6.4.4.3: "#001" is the same identifier as "#1".
    let image = read(&ifc("IFC4", "#001=IFCWALL('a',$,$,$,$,$,$,$,$);\n"));
    assert!(image.entry(1).is_some());
    assert_eq!(image.len(), 1);
}

#[test]
fn an_instance_name_too_large_for_u32_is_reported() {
    let image = read(&ifc(
        "IFC4",
        "#99999999999999=IFCWALL('a',$,$,$,$,$,$,$,$);\n",
    ));
    assert_eq!(image.len(), 0);
    assert!(has_code(&image, DiagCode::ID_OVERFLOW));
}

#[test]
fn a_duplicate_instance_name_keeps_the_first() {
    let image = read(&ifc(
        "IFC4",
        "#1=IFCWALL('first',$,$,$,$,$,$,$,$);\n#1=IFCSLAB('second',$,$,$,$,$,$,$,$);\n",
    ));
    assert_eq!(image.len(), 1);
    assert_eq!(image.class_name_of(1), "IfcWall");
    assert!(has_code(&image, DiagCode::DUPLICATE_ID));
}

#[test]
fn forward_references_are_normal() {
    let image = read(&ifc(
        "IFC4",
        "#1=IFCAXIS2PLACEMENT3D(#2,$,$);\n#2=IFCCARTESIANPOINT((0.,0.,0.));\n",
    ));
    assert_eq!(image.len(), 2);
    assert_eq!(args_of(&image, 1)[0], RawValue::Ref(2));
}

#[test]
fn a_dangling_reference_is_kept_as_written() {
    let image = read(&ifc("IFC4", "#1=IFCAXIS2PLACEMENT3D(#999,$,$);\n"));
    assert_eq!(args_of(&image, 1)[0], RawValue::Ref(999));
    assert!(image.entry(999).is_none());
}

// --------------------------------------------------------------------- values

#[test]
fn null_and_derived_slots() {
    let image = read(&ifc("IFC4", "#1=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n"));
    let args = args_of(&image, 1);
    assert_eq!(args[0], RawValue::Derived);
    assert!(matches!(args[1], RawValue::Enum(_)));
    assert_eq!(args[2], RawValue::Null);
    assert!(matches!(args[3], RawValue::Enum(_)));
}

#[test]
fn number_forms_from_the_standard() {
    let image = read(&ifc(
        "IFC4",
        "#1=IFCCARTESIANPOINT((1.,-1.5E-3,1.E+2));\n#2=IFCCARTESIANPOINT((0.,-0.,3.));\n",
    ));
    let mut cursor = image.args(1).unwrap();
    assert_eq!(cursor.read(), Some(RawValue::ListStart));
    assert_eq!(cursor.read(), Some(RawValue::Real(1.0)));
    assert_eq!(cursor.read(), Some(RawValue::Real(-1.5e-3)));
    assert_eq!(cursor.read(), Some(RawValue::Real(1.0e2)));
    assert_eq!(cursor.read(), Some(RawValue::ListEnd));
}

#[test]
fn a_real_too_large_for_a_double_is_warned_about_not_stored_as_infinity() {
    let image = read(&ifc("IFC4", "#1=IFCCARTESIANPOINT((1e999,-1E999,1.5));\n"));
    let args = args_of(&image, 1);
    assert_eq!(args[1], RawValue::Real(0.0));
    assert_eq!(args[2], RawValue::Real(0.0));
    assert_eq!(args[3], RawValue::Real(1.5));
    assert_eq!(args[4], RawValue::ListEnd);
    assert!(has_code(&image, DiagCode::NUMBER_OUT_OF_RANGE));
    assert!(!image.diagnostics.has_errors());
}

#[test]
fn an_integer_stays_an_integer() {
    let image = read(&ifc("IFC4", "#1=IFCINTEGER(42);\n#2=IFCINTEGER(-7);\n"));
    assert_eq!(args_of(&image, 1)[0], RawValue::Int(42));
    assert_eq!(args_of(&image, 2)[0], RawValue::Int(-7));
}

#[test]
fn leading_dot_reals_are_accepted_although_non_conformant() {
    // ISO requires a digit before the dot, but a hand-edited file might omit it.
    let image = read(&ifc("IFC4", "#1=IFCCARTESIANPOINT((.5,0.,0.));\n"));
    let mut cursor = image.args(1).unwrap();
    cursor.read();
    assert_eq!(cursor.read(), Some(RawValue::Real(0.5)));
}

#[test]
fn lowercase_exponents_are_accepted_although_non_conformant() {
    let image = read(&ifc("IFC4", "#1=IFCCARTESIANPOINT((1.5e3,0.,0.));\n"));
    let mut cursor = image.args(1).unwrap();
    cursor.read();
    assert_eq!(cursor.read(), Some(RawValue::Real(1500.0)));
}

#[test]
fn enumerations_and_logicals() {
    let image = read(&ifc("IFC4", "#1=IFCWALL('a',$,$,$,$,$,$,$,.SOLIDWALL.);\n"));
    let args = args_of(&image, 1);
    match args[8] {
        RawValue::Enum(id) => assert_eq!(image.strings.get(id), b"SOLIDWALL"),
        ref other => panic!("expected an enumeration, got {other:?}"),
    }
}

#[test]
fn nested_lists() {
    let image = read(&ifc(
        "IFC4",
        "#1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,1.,1.)),$);\n",
    ));
    let args = args_of(&image, 1);
    // ListStart, ListStart, 3 reals, ListEnd, ListStart, 3 reals, ListEnd, ListEnd, Null
    assert_eq!(args[0], RawValue::ListStart);
    assert_eq!(args[1], RawValue::ListStart);
    assert_eq!(args.last(), Some(&RawValue::Null));
}

#[test]
fn an_empty_list() {
    let image = read(&ifc("IFC4", "#1=IFCSHAPEREPRESENTATION($,$,$,());\n"));
    let args = args_of(&image, 1);
    assert_eq!(args[3], RawValue::ListStart);
    assert_eq!(args[4], RawValue::ListEnd);
}

#[test]
fn a_typed_value_wraps_exactly_one_value() {
    let image = read(&ifc(
        "IFC4",
        "#1=IFCPROPERTYSINGLEVALUE('P',$,IFCLENGTHMEASURE(3.5),$);\n",
    ));
    let mut cursor = image.args(1).unwrap();
    cursor.skip_values(2);
    match cursor.read() {
        Some(RawValue::Typed(id)) => assert_eq!(image.strings.get(id), b"IFCLENGTHMEASURE"),
        other => panic!("expected a typed value, got {other:?}"),
    }
    assert_eq!(cursor.read(), Some(RawValue::Real(3.5)));
}

#[test]
fn a_typed_value_with_several_values_becomes_a_list() {
    let image = read(&ifc(
        "IFC4",
        "#1=IFCPROPERTYSINGLEVALUE('P',$,IFCCOMPLEXNUMBER(1.,2.),$);\n",
    ));
    let mut cursor = image.args(1).unwrap();
    cursor.skip_values(2);
    assert!(matches!(cursor.read(), Some(RawValue::Typed(_))));
    assert_eq!(cursor.read(), Some(RawValue::ListStart));
    assert_eq!(cursor.read(), Some(RawValue::Real(1.0)));
    assert_eq!(cursor.read(), Some(RawValue::Real(2.0)));
    assert_eq!(cursor.read(), Some(RawValue::ListEnd));
}

#[test]
fn a_binary_literal() {
    let image = read(&ifc(
        "IFC4",
        "#1=IFCPIXELTEXTURE(1,1,$,$,$,$,(\"00FF00\"));\n",
    ));
    let args = args_of(&image, 1);
    let found = args.iter().any(|v| matches!(v, RawValue::Binary(_)));
    assert!(found, "expected a binary literal in {args:?}");
}

// -------------------------------------------------------------------- strings

#[test]
fn a_doubled_apostrophe_is_one_character() {
    let image = read(&ifc("IFC4", "#1=IFCWALL('it''s',$,$,$,$,$,$,$,$);\n"));
    match args_of(&image, 1)[0] {
        RawValue::Str(id) => assert_eq!(image.strings.decode(id), "it's"),
        ref other => panic!("expected a string, got {other:?}"),
    }
}

#[test]
fn a_semicolon_inside_a_string_does_not_end_the_record() {
    let image = read(&ifc("IFC4", "#1=IFCWALL('a;b);c',$,$,$,$,$,$,$,'TAG');\n"));
    assert_eq!(image.len(), 1);
    match args_of(&image, 1)[0] {
        RawValue::Str(id) => assert_eq!(image.strings.decode(id), "a;b);c"),
        ref other => panic!("expected a string, got {other:?}"),
    }
}

#[test]
fn escaped_unicode_in_a_string() {
    let image = read(&ifc(
        "IFC4",
        "#1=IFCWALL('Ren\\X2\\00E9\\X0\\',$,$,$,$,$,$,$,$);\n",
    ));
    match args_of(&image, 1)[0] {
        RawValue::Str(id) => assert_eq!(image.strings.decode(id), "René"),
        ref other => panic!("expected a string, got {other:?}"),
    }
}

#[test]
fn a_raw_newline_inside_a_string_is_tolerated() {
    // Edition 3 deletes line breaks inside strings. Real files contain them,
    // so TessIFC keeps them and keeps counting lines correctly.
    let image = read(&ifc(
        "IFC4",
        "#1=IFCWALL('two\nlines',$,$,$,$,$,$,$,$);\n#2=IFCSLAB('after',$,$,$,$,$,$,$,$);\n",
    ));
    assert_eq!(image.len(), 2);
    let first_line = image.entry(1).unwrap().line;
    let second_line = image.entry(2).unwrap().line;
    assert_eq!(
        second_line,
        first_line + 2,
        "the embedded newline must be counted"
    );
}

#[test]
fn an_unterminated_string_skips_the_record_and_reports() {
    let source = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
                  #1=IFCWALL('never closed,$,$,$,$,$,$,$,$);\n";
    let image = read(source);
    assert!(has_code(&image, DiagCode::UNCLOSED_STRING));
    assert_eq!(image.len(), 0);
}

#[test]
fn raw_non_ascii_bytes_survive() {
    let mut bytes = ifc("IFC4", "#1=IFCWALL('X',$,$,$,$,$,$,$,$);\n").into_bytes();
    // Replace the X with a raw Latin-1 byte, which is not valid UTF-8 alone.
    let at = bytes.windows(3).position(|w| w == b"'X'").unwrap() + 1;
    bytes[at] = 0xe4;
    let image = read_bytes(&bytes);
    assert_eq!(image.len(), 1);
    match args_of(&image, 1)[0] {
        RawValue::Str(id) => assert_eq!(image.strings.decode(id), "ä"),
        ref other => panic!("expected a string, got {other:?}"),
    }
}

// ------------------------------------------------------------------- comments

#[test]
fn comments_between_any_two_tokens() {
    let image = read(&ifc(
        "IFC4",
        "#1 /* a */ = /* b */ IFCWALL /* c */ ( /* d */ 'g' /* e */ , $,$,$,$,$,$,$ ) ;\n",
    ));
    assert_eq!(image.len(), 1);
    assert_eq!(image.class_name_of(1), "IfcWall");
}

#[test]
fn a_multi_line_comment_keeps_the_line_count_right() {
    let image = read(&ifc(
        "IFC4",
        "#1=IFCWALL('a',$,$,$,$,$,$,$,$);\n/* one\ntwo\nthree */\n#2=IFCSLAB('b',$,$,$,$,$,$,$,$);\n",
    ));
    let first = image.entry(1).unwrap().line;
    let second = image.entry(2).unwrap().line;
    assert_eq!(second, first + 4);
}

#[test]
fn an_unterminated_comment_ends_the_file_cleanly() {
    let source = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
                  #1=IFCWALL('a',$,$,$,$,$,$,$,$);\n/* never closed\n";
    let image = read(source);
    assert_eq!(image.len(), 1);
    assert!(has_code(&image, DiagCode::UNCLOSED_COMMENT));
}

// ------------------------------------------------------------------- records

#[test]
fn a_space_before_the_parenthesis() {
    let image = read(&ifc("IFC4", "#1 = IFCWALL ('a',$,$,$,$,$,$,$,$) ;\n"));
    assert_eq!(image.len(), 1);
}

#[test]
fn a_record_spread_over_many_lines() {
    let image = read(&ifc(
        "IFC4",
        "#1=IFCWALL(\n  'a',\n  $,\n  $,\n  $,\n  $,\n  $,\n  $,\n  $,\n  $\n);\n\
         #2=IFCSLAB('b',$,$,$,$,$,$,$,$);\n",
    ));
    assert_eq!(image.len(), 2);
    assert_eq!(
        image.entry(2).unwrap().line,
        image.entry(1).unwrap().line + 11
    );
}

#[test]
fn a_missing_semicolon_is_a_warning_not_a_loss() {
    let source = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
                  #1=IFCWALL('a',$,$,$,$,$,$,$,$)\n#2=IFCSLAB('b',$,$,$,$,$,$,$,$);\n\
                  ENDSEC;\nEND-ISO-10303-21;\n";
    let image = read(source);
    assert_eq!(image.len(), 2);
    assert!(has_code(&image, DiagCode::MISSING_SEMICOLON));
}

#[test]
fn an_unknown_class_is_kept_with_its_name() {
    let image = read(&ifc(
        "IFC4",
        "#1=IFCVENDORTHING('a',1,2);\n#2=IFCWALL('b',$,$,$,$,$,$,$,$);\n",
    ));
    assert_eq!(image.len(), 2);
    assert_eq!(image.class_name_of(1), "IFCVENDORTHING");
    assert!(has_code(&image, DiagCode::UNKNOWN_CLASS));
    assert_eq!(image.class_name_of(2), "IfcWall");
}

#[test]
fn a_wrong_argument_count_is_flagged_but_kept() {
    let image = read(&ifc("IFC4", "#1=IFCWALL('a',$,$);\n"));
    assert_eq!(image.len(), 1);
    assert!(has_code(&image, DiagCode::ARITY_MISMATCH));
}

#[test]
fn garbage_between_records_does_not_stop_the_parse() {
    let source = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
                  #1=IFCWALL('a',$,$,$,$,$,$,$,$);\n@@@ nonsense ;\n\
                  #2=IFCSLAB('b',$,$,$,$,$,$,$,$);\nENDSEC;\nEND-ISO-10303-21;\n";
    let image = read(source);
    assert_eq!(image.len(), 2);
    assert!(image.diagnostics.total() > 0);
}

#[test]
fn a_truncated_record_is_reported_and_the_rest_survives() {
    let source = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
                  #1=IFCWALL('a',$,$,$,$,$,$,$,$);\n#2=IFCSLAB('b',$,$,";
    let image = read(source);
    assert_eq!(image.len(), 1);
    assert!(image.diagnostics.has_errors());
}

#[test]
fn truncating_anywhere_never_panics() {
    let full = ifc(
        "IFC4",
        "#1=IFCWALL('Ren\\X2\\00E9\\X0\\',$,'it''s',$,$,#2,$,'T',.SOLIDWALL.);\n\
         #2=IFCLOCALPLACEMENT($,#3);\n#3=IFCAXIS2PLACEMENT3D(#4,$,$);\n\
         #4=IFCCARTESIANPOINT((1.,-2.5E-3,3.));\n\
         #5=(IFCNAMEDUNIT(*,.LENGTHUNIT.)IFCSIUNIT($,.METRE.));\n",
    );
    let bytes = full.as_bytes();
    for cut in 0..bytes.len() {
        let image = read_bytes(&bytes[..cut]);
        // The only requirement is that it returns.
        let _ = image.len();
    }
}

// --------------------------------------------------------- complex instances

#[test]
fn a_complex_instance_keeps_every_leaf() {
    let image = read(&ifc(
        "IFC4",
        "#1=(IFCNAMEDUNIT(*,.LENGTHUNIT.)IFCSIUNIT($,.METRE.));\n",
    ));
    assert_eq!(image.len(), 1);
    let entry = image.entry(1).unwrap();
    assert_ne!(entry.flags & tessifc_step::image::ENTRY_COMPLEX, 0);
    let args = args_of(&image, 1);
    let leaves = args
        .iter()
        .filter(|v| matches!(v, RawValue::Leaf(_)))
        .count();
    assert_eq!(leaves, 2);
    assert!(has_code(&image, DiagCode::COMPLEX_INSTANCE));
}

#[test]
fn a_complex_instance_with_an_unknown_leaf() {
    let image = read(&ifc(
        "IFC4",
        "#1=(IFCNAMEDUNIT(*,.LENGTHUNIT.)IFCVENDORUNIT('x'));\n",
    ));
    assert_eq!(image.len(), 1);
    let args = args_of(&image, 1);
    assert_eq!(
        args.iter()
            .filter(|v| matches!(v, RawValue::Leaf(_)))
            .count(),
        2
    );
}

// --------------------------------------------------------------------- limits

#[test]
fn nesting_deeper_than_the_limit_is_refused_not_overflowed() {
    let mut deep = String::from("#1=IFCWALL(");
    for _ in 0..500 {
        deep.push('(');
    }
    for _ in 0..500 {
        deep.push(')');
    }
    deep.push_str(");\n");
    let image = read(&ifc("IFC4", &deep));
    assert!(has_code(&image, DiagCode::NESTING_TOO_DEEP));
    assert_eq!(image.len(), 0);
}

#[test]
fn nested_typed_values_are_refused_not_overflowed() {
    // A typed value nests without opening a list, so the ceiling must be in the value parser.
    let mut deep = String::from("#1=IFCWALL(");
    for _ in 0..2000 {
        deep.push_str("IFCLABEL(");
    }
    deep.push_str("'x'");
    for _ in 0..2000 {
        deep.push(')');
    }
    deep.push_str(
        ");
",
    );
    let image = read(&ifc("IFC4", &deep));
    assert!(has_code(&image, DiagCode::NESTING_TOO_DEEP));
    assert_eq!(image.len(), 0);
}

#[test]
fn a_very_long_string_hits_the_limit_without_allocating_it_twice() {
    let options = ParseOptions {
        max_string_len: 64,
        ..ParseOptions::default()
    };
    let long = "x".repeat(4096);
    let source = ifc("IFC4", &format!("#1=IFCWALL('{long}',$,$,$,$,$,$,$,$);\n"));
    let image = parse(source.as_bytes(), &options);
    assert_eq!(image.len(), 1);
    assert!(has_code(&image, DiagCode::LIMIT_REACHED));
}

#[test]
fn the_entity_limit_stops_the_parse_cleanly() {
    let options = ParseOptions {
        max_entities: 3,
        ..ParseOptions::default()
    };
    let mut data = String::new();
    for i in 1..=10 {
        data.push_str(&format!("#{i}=IFCWALL('a',$,$,$,$,$,$,$,$);\n"));
    }
    let image = parse(ifc("IFC4", &data).as_bytes(), &options);
    assert_eq!(image.len(), 3);
    assert!(has_code(&image, DiagCode::LIMIT_REACHED));
    let limits = image
        .diagnostics
        .items()
        .iter()
        .filter(|d| d.code == DiagCode::LIMIT_REACHED)
        .count();
    assert_eq!(
        limits, 1,
        "one report for the limit, not one per skipped record"
    );
    assert!(
        !has_code(&image, DiagCode::BAD_HEADER),
        "the tail after the limit is not rescanned"
    );
}

#[test]
fn the_tape_limit_stops_the_parse_with_every_offset_intact() {
    let options = ParseOptions {
        max_tape_bytes: 64,
        ..ParseOptions::default()
    };
    let mut data = String::new();
    for i in 1..=40 {
        data.push_str(&format!("#{i}=IFCCARTESIANPOINT((1.,2.,3.));\n"));
    }
    let image = parse(ifc("IFC4", &data).as_bytes(), &options);
    assert!(image.len() < 40);
    assert!(has_code(&image, DiagCode::LIMIT_REACHED));
    for entry in &image.index {
        let end = entry.tape_off as usize + entry.tape_len as usize;
        assert!(
            end <= image.tape.len(),
            "tape range must be inside the tape"
        );
    }
    let args = args_of(&image, 1);
    assert_eq!(args[1], RawValue::Real(1.0));
    assert_eq!(args[2], RawValue::Real(2.0));
    assert_eq!(args[3], RawValue::Real(3.0));
}

// ------------------------------------------------------------------ hostility

#[test]
fn arbitrary_bytes_never_panic() {
    // A cheap stand-in for the fuzzer, so regressions show up in `cargo test`.
    let seeds: [&[u8]; 12] = [
        b"",
        b"\x00\x01\x02\x03",
        b"ISO-10303-21;",
        b"ISO-10303-21;DATA;#",
        b"ISO-10303-21;DATA;#1=",
        b"ISO-10303-21;DATA;#1=A(",
        b"ISO-10303-21;DATA;#1=A('",
        b"ISO-10303-21;DATA;#1=A(((((((((((((((",
        b"ISO-10303-21;DATA;#1=A(.",
        b"ISO-10303-21;DATA;#1=A(\"",
        b"ISO-10303-21;DATA;#1=A(1.2.3.4.5);",
        b"/*",
    ];
    for seed in seeds {
        let image = read_bytes(seed);
        let _ = image.len();
        let _ = image.diagnostics.total();
    }

    // A crude deterministic pseudo-random sweep, no dependency needed.
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut buffer = vec![0u8; 512];
    for _ in 0..200 {
        for byte in buffer.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = (state >> 24) as u8;
        }
        let image = read_bytes(&buffer);
        let _ = image.len();
    }
}

#[test]
fn every_index_entry_is_internally_consistent() {
    let image = read(&ifc(
        "IFC4",
        "#3=IFCWALL('c',$,$,$,$,$,$,$,$);\n#1=IFCSLAB('a',$,$,$,$,$,$,$,$);\n\
         #2=(IFCNAMEDUNIT(*,.LENGTHUNIT.)IFCSIUNIT($,.METRE.));\n",
    ));
    let mut previous = 0u32;
    for entry in &image.index {
        assert!(
            entry.express_id > previous,
            "index must be sorted and unique"
        );
        previous = entry.express_id;
        let end = entry.tape_off as usize + entry.tape_len as usize;
        assert!(
            end <= image.tape.len(),
            "tape range must be inside the tape"
        );
        assert!(entry.line > 0, "every entry must carry a source line");
    }
}

#[test]
fn diagnostics_carry_a_severity_and_a_stable_code() {
    let image = read(&ifc("IFC4", "#1=IFCNOTACLASS('a');\n"));
    let diagnostic = image
        .diagnostics
        .items()
        .iter()
        .find(|d| d.code == DiagCode::UNKNOWN_CLASS)
        .expect("expected an unknown class diagnostic");
    assert_eq!(diagnostic.severity, Severity::Warning);
    assert_eq!(diagnostic.express_id, Some(1));
    assert!(diagnostic.line > 0);
    assert!(diagnostic.code.as_str().starts_with("W_"));
}

/// A single-schema build must still read every file, mapping the header onto its tables.
#[test]
#[cfg(not(all(
    feature = "schema-ifc2x3",
    feature = "schema-ifc4",
    feature = "schema-ifc4x3"
)))]
fn a_missing_schema_falls_back_to_what_is_compiled_in() {
    for declared in ["IFC2X3", "IFC4", "IFC4X3_ADD2"] {
        let image = read(&ifc(
            declared,
            "#1=IFCWALL('a',$,$,$,$,$,$,$,$);
",
        ));
        assert_eq!(image.len(), 1, "declared {declared}");
        assert_eq!(image.class_name_of(1), "IfcWall", "declared {declared}");
    }
}
