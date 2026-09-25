<!-- SPDX-License-Identifier: Apache-2.0 -->
# tessifc-pack

Part of [TessIFC](../../README.md).

The IGP (IFC Geometry Pack) v0 writer. IGP is a GLB-shaped
container: a 24-byte header, a JSON index padded to eight bytes, and a binary
chunk of 8-byte-aligned sections holding deduplicated meshes and one column
per instance field. The layout is normative and specified in
[`docs/igp-format.md`](../../docs/igp-format.md); a reader needs no library,
and the reference one is [`viewer/src/igp.js`](../../viewer/src/igp.js).

```rust
use tessifc_pack::{IgpWriter, Instance, Provenance};

let mut writer = IgpWriter::new("IFC4", 0.001);
writer.set_model_offset([420_000.0, 5_900_000.0, 12.0]);
let geometry = writer.add_geometry(
    &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
    &[0, 1, 2],
);
writer.add_instance(Instance {
    geometry_id: geometry,
    express_id: 42,
    class: "IfcWall".into(),
    transform: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
    color: [200, 200, 200, 255],
    flags: 0,
    provenance: Provenance::default(),
    material: None,
});
let bytes = writer.finish();
assert_eq!(&bytes[0..4], b"IGP\0");
```

Instance flags are the `INSTANCE_*` constants: transparent, opening, space,
has-diagnostic and non-physical reference geometry. Diagnostics go in through `add_diagnostic` as
`DiagnosticRecord`s and statistics through `set_stat`.

## Streaming

`finish_chunk` closes a chunk and returns the `StreamState` that lets
`continue_stream` open the next one with global geometry ids intact.
`set_stream` records the chunk's position for the reader, and
`next_geometry_id` says where a patch chunk may start. Every chunk is itself
a valid pack.

## Determinism

The writer sorts instances by express id and class names alphabetically.
Geometries are written in first-use order and diagnostics in the order they
were raised, both of which are stable because product evaluation order is.
It writes no timestamp, path or hostname. Same input, same bytes: that is
what makes the regression suite a hash comparison.

Licensed under Apache-2.0.
