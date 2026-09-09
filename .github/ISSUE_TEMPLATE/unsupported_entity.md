---
name: Unsupported IFC entity
about: TessIFC read the file but did not handle a particular IFC class
title: "Unsupported: Ifc"
labels: unsupported-entity
assignees: ""
---
<!-- SPDX-License-Identifier: Apache-2.0 -->

<!-- Use this when the file loaded and TessIFC told you, politely, that it does
     not know what to do with something. That is a gap in coverage, not a
     crash. If it crashed or produced wrong numbers, use the bug report
     template instead. -->

## IFC class

<!-- The exact class name, as spelled in the schema: IfcAdvancedBrep,
     IfcSweptDiskSolid, IfcTriangulatedFaceSet. If the problem is a specific
     variant rather than the whole class, say which: "IfcExtrudedAreaSolid whose
     SweptArea is an IfcArbitraryProfileDefWithVoids". -->

## Schema

- [ ] IFC2X3
- [ ] IFC4
- [ ] IFC4X3

<!-- Whatever FILE_SCHEMA says. `tessifc info <file>` prints it. -->

## Diagnostic code TessIFC emitted

<!-- Run `tessifc convert <file> --diagnostics --json` and paste the lines about this
     entity. The code is the part that starts with E_, W_ or I_, for example
     W_UNKNOWN_CLASS, W_ARITY_MISMATCH or E_GEOMETRY_LIMIT_REACHED.
     If there was no diagnostic at all, say so: silence about something we
     cannot handle is itself a bug worth fixing. -->

```
paste the diagnostic lines here
```

## A minimal fragment

TessIFC version, effective settings and affected product ID:

<!-- Paste the smallest IFC fragment that shows the problem: one product, its
     placement, its representation and the units. A hand-built fragment is
     ideal. IFC files are not committed to this repository, so a fragment we
     can embed in a test is far more useful than a whole model. -->

```
paste the fragment here
```

## What a viewer shows today

<!-- If another viewer draws this entity correctly, a screenshot helps. Say
     what TessIFC shows instead: nothing, an uncut solid, a bounding box, or
     the wrong shape. -->

**In (viewer name and version):**

**In TessIFC:**

## How common is this

<!-- Optional, but it changes priority. If you know the exporter and version,
     `tessifc info` prints it from the file header. -->
