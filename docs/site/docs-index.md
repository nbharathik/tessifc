---
title: Documentation
description: Build with TessIFC. Guides for your first model, browser and native integrations, geometry coverage, and the IGP mesh format.
---
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Documentation

TessIFC turns IFC building models into triangle meshes, with element identity,
placements and diagnostics intact. Start with a local build, then choose the
integration that fits your application.

<div class="tess-doc-start" markdown>

## Your first model

Build the kernel, open the browser viewer and convert an IFC file.
The getting started guide includes working examples for JavaScript, Node.js,
the command line and Rust.

[Get started](getting-started.md){ .md-button .md-button--primary }
[Explore the SDK](sdk.md){ .md-button }

</div>

## Build and integrate

<div class="grid cards" markdown>

-   **SDK and API**

    Packages, geometry settings, workers and memory ownership.

    [Use the API](sdk.md)

-   **Architecture**

    Follow the pipeline from a STEP file to render-ready triangles.

    [Understand the kernel](architecture.md)

-   **Editing**

    Change IFC attributes while preserving the rest of the source file.

    [Work with attributes](editing.md)

-   **Agents and pipelines**

    Run scripts and agents against a model and refresh only what changed.

    [Build on the editing loop](agents.md)

</div>

## Reference

| Guide | What you will find |
| :--- | :--- |
| [IFC coverage](coverage.md) | Supported geometry, representation types and their limits. |
| [IGP format](igp-format.md) | Mesh container layout, element IDs and how to read a pack. |
| [Developer preview](preview.md) | Current scope, validation guidance and the preview contract. |

!!! info "Working with the developer preview"

    Schema recognition is broader than geometry support. Review the conversion
    report before using a mesh downstream. The [preview contract](preview.md)
    explains how to validate your results.
