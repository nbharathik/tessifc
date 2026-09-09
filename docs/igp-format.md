<!-- SPDX-License-Identifier: Apache-2.0 -->
# IGP v0 - the IFC Geometry Pack

**Normative.** This document defines the container TessIFC writes. Changing
anything here requires a version bump. A reader written against this document must keep working.

> Status: the format is **specified** and frozen for v0. The writer lives in
> `crates/ifc-pack`.

## Why not glTF

glTF can carry triangles, and TessIFC will export it later. It cannot carry
what this format exists for: an `expressId` on every instance and a
diagnostics table. Putting those in `extras` works but bloats every node, and
glTF forces a node tree, an accessor model and a material model that a BIM
viewer then has to undo.

IGP is a GLB-shaped container because that shape is proven and because reading
it needs no library: a `DataView` in JavaScript, `numpy.frombuffer` in Python,
a slice in Rust.

## Header

24 bytes, little-endian.

| Offset | Size | Field | Value |
|---|---|---|---|
| 0 | 4 | `magic` | `0x00504749`, the bytes `I`, `G`, `P`, `0x00` |
| 4 | 4 | `version` | `0` |
| 8 | 4 | `json_len` | byte length of the JSON chunk, before padding |
| 12 | 8 | `bin_len` | byte length of the BIN chunk (`u32` low, then `u32` high) |
| 20 | 4 | `flags` | bit 0 streaming chunk (partial), bit 1 f64 positions (reserved; the v0 writer never sets it), bits 2-31 reserved, must be 0 |

A reader must reject a file whose `magic` does not match, and must refuse a
`version` it does not know rather than guessing.

## Chunks

```
[ 24-byte header ][ JSON chunk, padded to 8 bytes with 0x20 ][ BIN chunk ]
```

The JSON chunk is UTF-8, exactly `json_len` bytes, padded with spaces (`0x20`)
to the next multiple of 8. The BIN chunk starts immediately after the padding,
so its first byte is 8-byte aligned, and **every offset in the JSON is relative
to the first byte of the BIN chunk**.

Every section inside BIN is itself 8-byte aligned. That is not decoration: it
lets a JavaScript reader construct a `Float64Array` view directly over the
buffer instead of copying.

## JSON index

```json
{
  "igp": 0,
  "generator": "tessifc 0.1.0",
  "schema": "IFC4",
  "units": { "length_scale_to_m": 0.001 },
  "model_offset": [420000.0, 5900000.0, 12.0],
  "georef": { "map_conversion": { } },

  "geometries": [
    {
      "id": 0,
      "positions": { "off": 0, "count": 1200 },
      "indices":   { "off": 14400, "count": 1800, "type": "u16" },
      "bbox": [0.0, 0.0, 0.0, 1.0, 2.0, 3.0],
      "primitive": "triangles",
      "closed": true
    }
  ],

  "instances": {
    "count": 5321,
    "geometry_id": { "off": 0,      "type": "u32" },
    "express_id":  { "off": 21284,  "type": "u32" },
    "class_id":    { "off": 42568,  "type": "u16" },
    "transform":   { "off": 53216,  "type": "f32x16" },
    "color":       { "off": 393760, "type": "u8x4" },
    "flags":       { "off": 415044, "type": "u16" },
    "provenance":  { "off": 425688, "type": "u32" }
  },

  "classes": ["IfcWall", "IfcDoor"],
  "provenance": [
    { "rep": 3021, "item": 3025, "evaluator": "IfcExtrudedAreaSolid", "fallback": null, "boolean": "exact" }
  ],
  "diagnostics": [
    { "id": 3025, "line": 88213, "sev": "warn",
      "code": "W_PROFILE_SELF_INTERSECTING", "msg": "..." }
  ],
  "stats": {
    "products": 4210,
    "triangles": 2310004,
    "parse_ms": 410,
    "geometry_ms": 6120
  }
}
```

Unknown members must be ignored by a reader, so that a later minor addition
does not break it.

### Fields

| Field | Required | Meaning |
|---|---|---|
| `igp` | yes | format version, mirrors the header |
| `generator` | yes | tool and version that wrote the file |
| `schema` | yes | `IFC2X3`, `IFC4` or `IFC4X3` |
| `units.length_scale_to_m` | yes | the model unit scale that was applied; positions are already in metres |
| `model_offset` | yes | f64 metres, **already subtracted** from every position. Add it back to recover world coordinates |
| `georef` | no | Present when the file declares an `IfcMapConversion`. `map_conversion` carries `eastings`, `northings` and `orthogonal_height` in metres, an optional `x_axis` direction and `scale`; `projected_crs` repeats the target CRS attributes by name. Metadata only: no position is moved by it |
| `geometries` | yes | one entry per unique mesh |
| `instances` | yes | columnar table, one row per placed geometry |
| `classes` | yes | class names; `class_id` indexes this array, and it is **not** a schema class id |
| `provenance` | yes | one entry per distinct origin; the `provenance` instance column indexes this array |
| `stream` | no | present on every chunk of a stream; see Streaming |
| `diagnostics` | yes | the diagnostics raised while this chunk, or this whole pack, was evaluated; an empty array when there were none |
| `stats` | yes | a flat object of name to number, whichever the caller recorded; empty except on the final chunk of a stream |

### Geometry entries

`positions` is `count` vertices of 3 floats: f32 by default, f64 when header
flag bit 1 is set. `count` is the vertex count, not the float count.

There is no `normals` section in v0. A consumer derives normals from the
positions and indices, which is what a renderer that welds or flat-shades does
anyway.

`indices.type` is `u16` when the geometry has fewer than 65536 vertices, `u32`
otherwise. `count` is the index count, so a triangle count is `count / 3`.

`bbox` is `[minx, miny, minz, maxx, maxy, maxz]` in the same space as the
positions, that is after the model offset.

`primitive` is `triangles`. The value `lines`, for which `indices` would hold
vertex pairs, is reserved: the v0 writer emits only `triangles`, so a reader
may treat any other value as a file it does not understand.

`closed` is optional and says whether the mesh is a closed solid, by the test
that every edge is shared by exactly two triangles. A reader needs it to know
what may be filled in at a section plane, and what a volume computed from the
geometry would mean. It is absent when nobody checked.

### Instance columns

Every column is a contiguous array of `count` elements at its own 8-byte
aligned offset. Reading is `new Uint32Array(bin, off, count)` and nothing else.

`transform` is a **column-major 4x4** matrix of f32, 16 per instance, applied
after the model offset. Column-major is what OpenGL, three.js and glam all use;
writing it row-major is the classic way to make every object appear
transposed.

`express_id` is **not** unique. One IFC product is one instance per colour it is
drawn in, so a window whose frame and pane are styled differently is two rows
with the same express id. A viewer that turns a pick into an element
must expect a set of rows, not a row.

`provenance` indexes the `provenance` array. Each entry says where the mesh
came from: `rep` is the `IfcShapeRepresentation`, `item` the representation
item the evaluator was given, `evaluator` that item's IFC class, `fallback` the
code of the first diagnostic raised against the item or `null`, and `boolean`
one of `none`, `exact`, `surface` or `refused`. Identical entries collapse, so a
family placed a thousand times costs one row.

`flags` bits: 0 transparent, 1 opening or void, 2 space or spatial zone,
3 has-diagnostic, 4 non-physical reference geometry. Bit 3 is set on an
instance whose own express id appears in `diagnostics`; a diagnostic raised
against a representation item rather than the product it belongs to does not
set it. Reference geometry
includes annotations, grids, virtual elements, ports and structural analysis
items. The bits let a viewer control helper geometry without guessing from
class names, including IFC subclasses such as `IfcOpeningStandardCase`. Bits
5 through 15 are reserved and must be written as 0.

## Streaming

The engine emits a sequence of IGP chunks. Every chunk except the last sets
header flag bit 0 and carries only the `geometries` and `instances` that are
new. Geometry ids are global across chunks, so a consumer keeps a
`Map<geometryId, BufferGeometry>` and appends instances as they arrive. A mesh
that an earlier chunk wrote is referred to by id and not written again. The
final chunk clears bit 0 and carries `stats`; every chunk carries the
`diagnostics` raised while it was evaluated.

Every chunk of a stream also carries a `stream` member in its index:

```json
"stream": { "chunk": 3, "final": false, "products_done": 812, "products_total": 2038 }
```

| Field | Meaning |
|---|---|
| `chunk` | zero-based index of this chunk |
| `final` | true for the last chunk, which also clears header flag bit 0 |
| `products_done` | products evaluated up to and including this chunk |
| `products_total` | products the stream will evaluate |

A whole pack has no `stream` member; a reader treats it as one final chunk.
Two things are per chunk rather than global: the `classes` table, so `class_id`
must be remapped when chunks are merged, and `model_offset`, which every chunk
repeats and which never changes within a stream. The offset is decided before
the first chunk from the product placements, so the first chunk and the last
agree on it.

Chunk policy is the caller's: the WASM `nextGeometryChunk` takes a time
budget, a product cap and a triangle cap and stops at whichever comes first,
always after at least one product. The viewer asks for a 45 ms first chunk
and 220 ms chunks after that.

A chunk with `final: true` and `chunk: 0` is also how a re-evaluated subset of
products travels after an edit (`evaluateProducts`): it is self-contained, its
records are baked into world space, and its geometry ids start where the
caller says so they cannot collide with the pack being patched.

## Determinism

The same file with the same settings must produce **byte-identical** output,
apart from any timing figure a caller records in `stats`. That is what makes
regression testing a hash comparison instead of a tolerance argument.
Concretely, the writer must:

* deduplicate identical meshes by content hash, and order geometries by the
  order in which each distinct mesh first appears;
* order instances by express id, and within one express id by the order the
  colours appear in the file;
* order classes by name;
* keep diagnostics in the order they were raised;
* never write a timestamp, a hostname, a path, or a random number;
* write `generator` as the version string only, with no build metadata.

This is the writer's ordering contract. Cross-target geometry is also
compared numerically; timing statistics and streaming chunk boundaries can
differ between runs. See the [preview contract](preview.md).

## Reading it in twenty lines

```js
function readIgp(buffer) {
  if (buffer.byteLength < 24) throw new Error("not an IGP file");
  const view = new DataView(buffer);
  if (view.getUint32(0, true) !== 0x00504749) throw new Error("not an IGP file");
  if (view.getUint32(4, true) !== 0) throw new Error("unsupported IGP version");

  const jsonLen = view.getUint32(8, true);
  const binStart = 24 + Math.ceil(jsonLen / 8) * 8;
  const index = JSON.parse(new TextDecoder().decode(new Uint8Array(buffer, 24, jsonLen)));

  const bin = buffer.slice(binStart);
  const width = { f32x16: 16, u8x4: 4 };
  const column = (c, Type) => new Type(bin, c.off, index.instances.count * (width[c.type] ?? 1));

  return { index, bin, column };
}
```

```python
import json, struct
import numpy as np

def read_igp(path):
    raw = open(path, "rb").read()
    magic, version, json_len, lo, hi, flags = struct.unpack_from("<IIIIII", raw, 0)
    assert magic == 0x00504749 and version == 0
    bin_start = 24 + -(-json_len // 8) * 8
    index = json.loads(raw[24 : 24 + json_len].decode("utf-8"))
    blob = raw[bin_start:]
    return index, blob

def positions(index, blob, geometry):
    section = geometry["positions"]
    return np.frombuffer(
        blob, dtype=np.float32, count=section["count"] * 3, offset=section["off"]
    ).reshape(-1, 3)
```

Source provenance is preserved across same-colour representation items. Consumers
must not assume that equal colours imply one source item or one instance record.
A boolean `refused` outcome survives subsequent cuts; `surface` outcomes remain
distinct from a closed-solid result.
