<!-- SPDX-License-Identifier: Apache-2.0 -->
# tessifc-step

STEP-21 (ISO 10303-21) reader for IFC files. Turns bytes into a flat, immutable
model image: a value tape, an instance index, an interned string arena and
diagnostics. It knows about records and values, not about geometry.

Recoverable input errors become diagnostics. A panic on crafted input is a
bug to report; the developer preview is not a proof against every malformed file.
Malformed, truncated or hostile input yields whatever was readable plus
diagnostics saying what went wrong.

```rust
use tessifc_step::{ParseOptions, parse};

let bytes = std::fs::read("model.ifc")?;
let image = parse(&bytes, &ParseOptions::default());

println!("{} instances, schema {}", image.len(), image.schema);
for d in image.diagnostics.items() {
    println!("{d}");
}
# Ok::<(), std::io::Error>(())
```

What it handles: complex instances, the string escapes `''`, `\X\`, `\X2\`,
`\X4\`, `\S\`, `\N\`, `\F\` and `\T\`; `\P\` is accepted and consumed, and its
ISO 8859 page is approximated by Latin-1. Also `$` and `*`, typed values, nested
lists, comments anywhere, CRLF, BOM, unknown class names, duplicate and
out-of-range instance names, and truncation at any byte.

`open` reads an IFCZIP archive as well as plain text: one stored or deflated
`.ifc` entry, walked through the central directory without a zip crate,
inflated within `ParseOptions::max_ifczip_bytes` and checked against its
CRC-32. ZIP64, encryption and other methods are refused with a diagnostic.
The `ifczip` feature, on by default, carries the inflate dependency.

Licensed under Apache-2.0.
