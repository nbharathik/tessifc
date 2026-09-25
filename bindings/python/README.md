<!-- SPDX-License-Identifier: Apache-2.0 -->
# tessifc for Python

**v0.3 developer preview.** The TessIFC geometry kernel as a native Python
extension: IFC-SPF or IFCZIP in, render-ready IGP meshes out, with the same
reports, settings, diagnostics and streaming as the WebAssembly and CLI
builds. No Python dependencies; NumPy is optional for array views.

Wheels for Linux, macOS and Windows are built for each release and published
with it. From a checkout:

```sh
pip install maturin
cd bindings/python && maturin develop --release
```

```python
import tessifc
from tessifc import igp

kernel = tessifc.Kernel()
model = kernel.open_model(open("model.ifc", "rb").read())
info = kernel.get_model_info(model)
print(info["schema"], info["entities"], "entities")

summary = kernel.evaluate_geometry(model, {"includeOpenings": False})
outcomes = kernel.get_product_outcomes(model)        # read before take_pack
pack = igp.read_igp(kernel.take_pack(model))         # bytes, IGP v0
for record in range(pack.instances.count):
    mesh = pack.geometry_by_id(pack.instances.geometry_ids[record])
    print(pack.class_of(record), pack.instances.express_ids[record], mesh.triangle_count, "triangles")
kernel.close_model(model)
```

## API

`Kernel()` holds any number of open models by integer id. Reports return
dictionaries, or `None` for an id the kernel does not hold; invalid settings
raise `tessifc.KernelError`. Settings and options are dictionaries with the
fields the other bindings take (see the SDK guide), or their JSON text.

| Call | What it does |
|---|---|
| `open_model(data, options=None)` | Parses IFC or IFCZIP bytes; an unreadable file opens with zero entities and diagnostics. Options: `schemaOverride`, `maxEntities`, `maxIfczipBytes`. |
| `get_model_info`, `get_diagnostics`, `get_class_name`, `get_product_category`, `get_ids_of_type`, `get_entity_info`, `get_spatial_hierarchy`, `get_class_attributes`, `get_class_supertypes`, `get_geometry_capabilities` | The model reports, the same JSON shapes as the WebAssembly kernel. |
| `evaluate_geometry(model_id, settings=None)` | Tessellates every product across every core, releasing the interpreter lock meanwhile; returns the summary and keeps the geometry. |
| `get_product_outcomes(model_id)` | Per-product outcomes; read them before the pack is taken. |
| `get_pack(model_id)`, `take_pack(model_id)` | The evaluation as one IGP pack; `take_pack` releases the geometry without copying it. |
| `begin_geometry_stream(model_id, settings=None)`, `next_geometry_chunk(model_id, budget_ms=0, max_products=0, max_triangles=0)`, `stream_progress`, `cancel_geometry_stream` | Geometry in IGP chunks, each holding at least one product and stopping at a budget. |
| `release_geometry`, `close_model`, `close_all`, `model_count` | Lifecycle. `Kernel` is also a context manager that closes everything. |
| `tessifc.version()` | The kernel version. |

`tessifc.igp.read_igp(data)` reads a pack without copying: `pack.index` is
the JSON index, `pack.geometry` a list of meshes with `positions`, `indices`
and optional `uv` as `memoryview`s cast to their element type, and
`pack.instances` the columns (`geometry_ids`, `express_ids`, `class_ids`,
`transforms`, `colors`, `flags`, optional `provenance` and `material`).
`igp.to_numpy(view, columns)` makes a NumPy array of a view. The container
is documented in `docs/igp-format.md`.

The kernel is not thread-safe: use one per thread. Editing is not part of
this package; the `tessifc-session` package in `adapters/ifcopenshell` is the
Python editing path.

## Test from a checkout

```sh
python -m unittest discover -s bindings/python/tests -p 'test_*.py'
```
