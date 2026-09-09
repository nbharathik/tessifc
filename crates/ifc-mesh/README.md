<!-- SPDX-License-Identifier: Apache-2.0 -->
# tessifc-mesh

Part of [TessIFC](../../README.md), an Apache-2.0 IFC geometry kernel.

Mesh operations with no IFC in them: polygon triangulation, vertex welding,
normals, half-space clipping, structured subtraction and bounded cell booleans. The
geometry crate builds meshes; this crate is where triangles are cut and
tidied.

```rust
use glam::DVec3;
use tessifc_mesh::{Mesh64, face_normals};

let mesh = Mesh64 {
    positions: vec![DVec3::ZERO, DVec3::X, DVec3::Y],
    indices: vec![0, 1, 2],
    closed: Some(false),
};
let normals = face_normals(&mesh);
assert_eq!(normals, vec![DVec3::Z]);
```

## Modules

| Module | What it does |
|---|---|
| `mesh` | `Mesh64`, f64 positions and u32 indices with a `closed` flag; Newell and face normals |
| `triangulate` | Polygons with holes to triangles, ear clipping through `earcutr`; an outline with too few points or no area is refused, and `triangulation_deviates` says when the triangles do not tile the outline |
| `weld` | Hash-grid welding on quantised positions, before booleans and before content hashing for instancing |
| `clip` | Exact clipping of a closed mesh by a plane, per triangle with capped loops triangulated in their plane; convex cutters as sequences of clips; structured subtraction of prisms from prisms |
| `bsp` | Bounded decomposition into convex cells for supported general union and intersection cases |
| `simplify` | Merge coplanar triangles and restore shared boundary vertices |

## Operation limits

Operations have topology and resource conditions: closed inputs, convex
cutters, paired-cap extrusion layouts or a bounded convex-cell decomposition.
An operation outside those conditions returns an error. If the geometry
evaluator retains an uncut operand, it marks that result as degraded; the
retained body is not the requested boolean result. See the
[coverage guide](../../docs/coverage.md) for per-representation limits.

Licensed under Apache-2.0.
