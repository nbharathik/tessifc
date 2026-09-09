<!-- SPDX-License-Identifier: Apache-2.0 -->
# tessifc-model

Typed views over a parsed model image: entities, attributes by name, and the
inverse relationships IFC does not store directly.

```rust
use tessifc_model::Model;
use tessifc_step::{ParseOptions, parse};

let bytes = std::fs::read("model.ifc")?;
let model = Model::new(parse(&bytes, &ParseOptions::default()));

for wall in model.entities_of_type("IfcWall") {
    let name = wall.attr("Name").as_string().unwrap_or_default();
    println!("#{} {name}: {} openings", wall.id(), model.voids_of(wall.id()).len());
}
# Ok::<(), std::io::Error>(())
```

`Entity::attr` resolves a name every call. In a hot loop, resolve the index once
with `Model::attr_index` and use `Entity::attr_at`.

The inverse index (voids, fills, materials, styles, aggregates, mapped-item
users, spatial containment, type definitions) is built once on first use, in one
pass, as compressed sparse rows.

Licensed under Apache-2.0.
