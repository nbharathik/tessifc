---
name: Bug report
about: Something TessIFC does that it should not, or does not do that it should
title: ""
labels: bug
assignees: ""
---
<!-- SPDX-License-Identifier: Apache-2.0 -->

<!-- A panic, hang or runaway memory use on crafted input is a security
     issue. Report it privately through the Security tab ("Report a
     vulnerability"), not in a public issue with the file attached. -->

## What happened

<!-- One or two sentences. -->

## What you expected instead

<!-- If another tool gives a different answer, say which tool and which
     version. -->

## How to reproduce

```
tessifc info path/to/file.ifc
```

<!-- The exact command, including flags. If it needs a file, attach it: see
     "The file" below. -->

## `tessifc info --json` output

<details>
<summary>output of <code>tessifc info path/to/file.ifc --json</code></summary>

```json
paste here
```

</details>

<!-- If it panicked on a file you made or trust, paste the panic message and
     set RUST_BACKTRACE=1 first. If the input could be used against other
     users, report it privately instead (see the note at the top). -->

## The file

- [ ] I can share a file, or a fragment of one, that reproduces this
- [ ] I cannot share it, but I can describe it

<!-- Smaller is better; a hand-built fragment is ideal. If the file is
     confidential, describe it instead: schema, exporter, entity count,
     classes involved. -->

## Environment

- TessIFC version or commit:
- Where it ran: native CLI / WASM in a browser / WASM in Node / Rust API
- Effective geometry settings and affected element IDs:
- Browser and GPU (for viewer issues):
- OS and architecture:
- Exit code:
