<!-- SPDX-License-Identifier: Apache-2.0 -->
# Editing IFC without rewriting it

TessIFC edits IFC-SPF at the source boundary, not by serializing the geometry
model back into a new file. This matters because a conventional rewrite loses
or normalizes information the geometry engine does not understand: comments,
whitespace, record order, vendor classes, newer-schema attributes, and the
authoring application's exact spelling of unchanged values.

## Invariants

An accepted edit has these properties:

1. Only the requested top-level argument byte span changes.
2. Every untouched source byte remains identical.
3. Strings are escaped by TessIFC, including apostrophes and UTF-16 STEP
   escapes. Raw values must parse as exactly one STEP value.
4. All edit spans are resolved before any replacement and applied back-to-front.
5. The source length and per-record hash must match the parsed image.
6. The edited file is reparsed before it replaces the open model or is written.
7. Entity count and every edited express id must survive verification.

Unknown simple classes remain editable by zero-based argument index. Complex
instances are edited by named schema leaf and leaf-local argument index; a flat
numeric edit is refused because it would be ambiguous.

## CLI

Text values are encoded safely:

```sh
tessifc edit model.ifc --id 219 --attribute Name --value "External wall" -o edited.ifc
```

Lists, enumerations, references and typed values use validated raw STEP syntax:

```sh
tessifc edit model.ifc --id 9001 --argument 2 --raw --value "(1.,2.,3.)" -o edited.ifc
```

The CLI refuses to overwrite the input path. Write a new file, inspect it, and
replace the original through the user's normal version-control workflow.

## WASM and viewer

`Kernel.getEntityInfo` exposes schema names, exact raw spelling and decoded text.
`Kernel.setAttributes` applies a form save as one transaction and one reparse.
`Kernel.exportModel` returns the current source. The viewer keeps the model in a
worker, edits scalar text fields, and downloads an `.edited.ifc` revision with
**Save IFC** or `Ctrl+S`.

Edits that change geometry are supported by the raw API, but the current viewer
intentionally exposes only text fields.

After a save the viewer asks the kernel to re-evaluate the edited product alone
(`Kernel.evaluateProducts`), receives it as one self-contained IGP chunk, and
swaps that product's records in place. When the triangles are byte-identical,
which the supported metadata edits preserve, nothing on the GPU is touched.
The v0.1 viewer does not expose geometry edits. A host changing a placement,
opening, profile or shared representation must re-evaluate every affected
product, or re-evaluate the whole model; dependency invalidation is not automatic.
