<!-- SPDX-License-Identifier: Apache-2.0 -->
# tessifc-schema

Generated schema tables for IFC2X3 TC1, IFC4 ADD2 TC1 and IFC4X3 ADD2, plus the
small runtime that reads them. It answers four questions: what classes exist, in
what order does a class serialise its attributes, is A a subtype of B, and what
primitive does a defined type reduce to.

```rust
use tessifc_schema::{Schema, SchemaId};

let schema = Schema::get(SchemaId::Ifc4);
let wall = schema.class_by_name("IfcWall").unwrap();

// STEP argument order is inherited-first: IfcRoot contributes the first four.
assert_eq!(schema.attr_name(wall, 0), Some("GlobalId"));
assert!(schema.is_a(wall, schema.class_by_name("IfcProduct").unwrap()));
```

`is_a` is two integer comparisons: classes are numbered in preorder over the
inheritance forest, so every descendant occupies a contiguous id interval.

The tables under `src/gen/` are generated from the official buildingSMART
EXPRESS files and are committed, so building TessIFC needs neither Python nor
network access. Do not edit them by hand. The EXPRESS
sources are CC BY-ND 4.0 and are never redistributed; see NOTICE.

Features `schema-ifc2x3`, `schema-ifc4` and `schema-ifc4x3` are all on by
default. A WASM build for one schema is smaller.

Licensed under Apache-2.0.
