---
name: Bug report
about: Something TessIFC does that it should not, or does not do that it should
title: ""
labels: bug
assignees: ""
---
<!-- SPDX-License-Identifier: Apache-2.0 -->

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

<!-- If it panicked, paste the panic message and set RUST_BACKTRACE=1 first.
     A panic in the parser is a security bug, not a correctness bug: it goes
     straight to the top of the list. See SECURITY.md if the file came from an
     untrusted source. -->

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
