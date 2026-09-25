---
title: IFC geometry for your application
description: An open-source IFC geometry kernel in Rust. Turn building models into render-ready meshes in the browser, Node.js and native applications.
template: home.html
hide:
  - navigation
  - toc
  - footer
---
<!-- SPDX-License-Identifier: Apache-2.0 -->

## Start with a model, build from there

Use the browser API in your application or convert files from the command line.
Build from the checkout for the v0.3 developer preview.

=== "JavaScript"

    ```javascript title="Browser API"
    import init, { Kernel } from "./bindings/wasm/pkg/tessifc_wasm.js";

    await init();
    const kernel = new Kernel();
    const id = kernel.openModel(new Uint8Array(await file.arrayBuffer()));

    try {
      const summary = JSON.parse(kernel.evaluateGeometry(id, "{}"));
      console.log(summary, JSON.parse(kernel.getProductOutcomes(id)));
      const pack = kernel.takePack(id); // Owned IGP mesh data
    } finally {
      kernel.closeModel(id);
      kernel.free();
    }
    ```

=== "Command line"

    ```bash title="Build and convert"
    cargo build --locked --release -p tessifc-cli

    # Inspect a model
    target/release/tessifc info model.ifc --json

    # Convert supported geometry into an IGP mesh pack
    target/release/tessifc convert model.ifc -o model.igp

    # Reject missing, degraded or repaired geometry
    target/release/tessifc convert model.ifc -o checked.igp --strict
    ```

=== "Rust"

    ```rust title="Evaluate geometry"
    use tessifc_engine::Engine;
    use tessifc_model::Model;
    use tessifc_step::{ParseOptions, parse};

    let bytes = std::fs::read("model.ifc")?;
    let model = Model::new(parse(&bytes, &ParseOptions::default()));
    let result = Engine::new().evaluate(&model);

    println!("{} triangles", result.triangles());
    for diagnostic in &result.diagnostics {
        println!("{diagnostic}");
    }
    ```

[Build instructions and complete examples](docs/getting-started.md){ .tess-text-link }
