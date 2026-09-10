<!-- SPDX-License-Identifier: Apache-2.0 -->
# tessifc-engine

Part of [TessIFC](../../README.md).

Orchestration: from a parsed model to placed geometry. The engine decides
which products to evaluate, evaluates them, in batches or all at once and on
one core or every core, and collects shapes and diagnostics in a fixed order
so geometry order is reproducible. Timing metadata and stream boundaries can differ.

```rust
use tessifc_engine::Engine;
use tessifc_model::Model;
use tessifc_step::{ParseOptions, parse};

let bytes = std::fs::read("model.ifc")?;
let model = Model::new(parse(&bytes, &ParseOptions::default()));

let result = Engine::new().evaluate(&model);
println!("{} shapes, {} triangles, offset {:?}",
    result.shapes.len(), result.triangles(), result.model_offset);
for diagnostic in &result.diagnostics {
    println!("{diagnostic}");
}
# Ok::<(), std::io::Error>(())
```

## Streaming

A `Session` evaluates in batches and keeps its caches between them, so a
family tessellated for the first batch is not tessellated again for the
second and a diagnostic reported once is not reported twice. The stop
predicate sees how far the batch has come, in products and triangles, and
the caller brings its own clock:

```rust
use tessifc_engine::Engine;
# use tessifc_model::Model;
# use tessifc_step::{ParseOptions, parse};
# let model = Model::new(parse(b"", &ParseOptions::default()));
let engine = Engine::new();
let mut session = engine.session(&model);
while !session.is_finished() {
    let batch = session.next(&model, |progress| progress.triangles >= 200_000);
    // batch.shapes are ready to pack or draw; batch.is_final says when to stop
    let _ = batch;
}
```

The session owns no reference to the model, so a host that keeps models in a
table can keep sessions beside them. The model offset is decided from product
placements before any geometry exists, so a stream and a whole evaluation
agree on it.

## Packing

The `pack` module turns shapes into IGP: `Packer::new(schema, length_to_m,
model_offset)`, then `add_shape` per shape, `add_diagnostics`, `set_stat`,
and `finish()` for the bytes. A `PackState` carries geometry ids and the
shared-family table across the chunks of a stream.

## Parallelism

With the `parallel` feature, on by default in the CLI, `Engine::evaluate`
maps products over the rayon pool, one product per unit of work, and collects
results in express-id order. Packs are identical to the serial ones apart
from timing statistics. The WASM build is single-threaded and streams
instead.

Licensed under Apache-2.0.
