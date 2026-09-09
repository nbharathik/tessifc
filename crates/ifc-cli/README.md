<!-- SPDX-License-Identifier: Apache-2.0 -->
# tessifc-cli

Version 0.1 is a developer preview. Review the [coverage](../../docs/coverage.md)
and [preview contract](../../docs/preview.md) before accepting conversion output.

The `tessifc` command line tool. Build locally with
`cargo build --locked --release -p tessifc-cli`. Registry installation and
prebuilt binaries apply once the preview is published.

## info

```sh
tessifc info model.ifc            # what is in this file
tessifc info model.ifc --json     # the same, machine readable
tessifc info model.ifc --diagnostics --strict
tessifc info model.ifc --schema IFC4    # read with this schema regardless of FILE_SCHEMA
```

`info` reports the schema, entity and product counts per class, parse time
and throughput, model image size, approximate allocated-byte peak and diagnostics. `--json`
output is a stable contract: fields may be added, never renamed or
repurposed. `--strict` exits non-zero if any diagnostic has error severity,
which makes it a CI gate for a set of files.

## convert

```sh
tessifc convert model.ifc -o model.igp           # every core
tessifc convert model.ifc -o checked.igp --strict
tessifc convert model.ifc -o model.igp --jobs 1  # serial, identical output
tessifc convert model.ifc --json --diagnostics   # measure without writing
tessifc convert model.ifc -o model.igp --no-spaces --openings --circle-segments 16
tessifc convert model.ifc -o model.igp --annotations --references
```

`convert` evaluates products across every core by default. The pack is
byte-identical whatever `--jobs` says, apart from the timing statistics in
its index. Family geometry placed many times is written once, with one
transform per placement; the report says how many records that covered.
The output is IGP v0, specified in `docs/igp-format.md`.
Openings, annotations and non-physical references are independent opt-in
groups. Space and zone volumes are included by default and flagged separately
so a viewer can hide them without discarding them.

## edit

```sh
tessifc edit model.ifc --id 219 --attribute Name --value "External wall" -o edited.ifc
tessifc edit model.ifc --id 9001 --argument 2 --raw --value "(1.,2.,3.)" -o edited.ifc
```

Changes one attribute, by schema name or by zero-based argument index, and
writes a new file in which every other byte is identical to the input.
`--raw` passes one complete STEP value instead of text to encode. The CLI
refuses to overwrite its input. `docs/editing.md` has the invariants.

## coverage

```sh
tessifc coverage              # what the evaluator registry handles
tessifc coverage --markdown   # the table in docs/coverage.md
tessifc coverage --json
tessifc coverage --inventory  # every schema entity with its dispatch route
```

## Exit codes

| Code | Meaning |
|---|---|
| 0 | done |
| 1 | Strict validation rejected errors, missing/degraded geometry or recovery diagnostics; or an edit failed structural verification |
| 2 | the file could not be read or written, or the arguments were wrong |

Licensed under Apache-2.0.
