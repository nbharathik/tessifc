<!-- SPDX-License-Identifier: Apache-2.0 -->
# Contributing to TessIFC

## Build and test

```sh
cargo build --locked --workspace
cargo test --locked --workspace
```

That is all the Rust core needs. Node is needed for the WASM smoke test and
the viewer tests; Python only for the helper scripts. The Rust version is
pinned in [rust-toolchain.toml](rust-toolchain.toml) and rustup installs it on
first build.

## The rules

### Licensing

Contributions must be compatible with Apache-2.0 distribution. Document the
origin of new algorithms and dependencies so maintainers can review them.

### Write from the specification

Do not copy or adapt source code from any GPL, LGPL, AGPL or MPL licensed
project. Public documentation, issue trackers and papers are fine to read.
Write the implementation from the ISO and buildingSMART specifications.
Every pull request answers this question in its template.

### Sign your commits

We use the [Developer Certificate of Origin](https://developercertificate.org/).
Sign off every commit:

```sh
git commit -s -m "feat(ifc-geom): IfcSweptDiskSolid with bent directrices"
```

### Every source file starts with an SPDX header

```rust
// SPDX-License-Identifier: Apache-2.0
```

Use `#` for Python, TOML and YAML, `/* */` for CSS, and `<!-- -->` for
Markdown and HTML.

## Commit style

Conventional commits, scoped by crate or package:

```
feat(ifc-geom): IfcExtrudedAreaSolid with oblique directions
fix(ifc-step): do not panic on a truncated string escape
test(viewer): fixture for a self-touching profile outline
docs(igp): clarify that instance transforms apply after model_offset
```

Keep commits small and CI green.

## Adding a geometry evaluator

This is the contribution we most want. One evaluator is one file, one
fixture, one test:

1. `crates/ifc-geom/src/eval/<snake_case_class>.rs`: implement
   `SolidEvaluator`, `CurveEvaluator` or `ProfileEvaluator`.
2. A fixture: the smallest IFC fragment that exercises it, embedded in the
   test as a string. A few hundred bytes is ideal. IFC files are never
   committed to the repository.
3. A test asserting something real: vertex count, volume, bounding box, or a
   match against a hand-computed value.
4. Register it in the `Registry` defaults and add a row to
   [docs/coverage.md](docs/coverage.md).

Do not touch the core to add a class. If you have to, that is a bug in the
extension point. Say so in the pull request.

## Testing

```sh
cargo test --locked --workspace
python scripts/build-wasm.py --target both
node bindings/wasm/test/smoke.mjs
node adapters/three/test/build.test.mjs
python -m pip install -r requirements-site.txt
python -m unittest discover -s scripts -p 'test_*.py'
npm ci --prefix viewer
cd viewer
npx playwright install chromium
npm test
```

The Node suites run on IFC fragments embedded in the tests. Set
`TESSIFC_TEST_MODEL` to an IFC file of your own to run them over a whole model
as well. Browser tests generate a first-party pavilion in memory and fail
when required tools or artifacts are missing. To use an installed Chrome,
set `TESSIFC_BROWSER_CHANNEL=chrome`. On Linux, install Playwright system
dependencies with `npx playwright install --with-deps chromium`.

Test first for parser and geometry maths. Every bug fix adds a fixture.

A change to the viewer's depth strategy is accepted on measurement, not on
reasoning. Say in the pull request which models you orbited and what changed
on screen, and a maintainer will run the pixel-level comparison.

## WASM

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <the version in Cargo.lock>
python scripts/build-wasm.py
```

## Schema tables

The tables under `crates/ifc-schema/src/gen/` are generated from the official
EXPRESS schema files and are committed, so building needs neither Python nor
network access. Do not edit them by hand; open an issue if a table is wrong
or a schema version is missing.

## Style

* Doc comments on every public item, short. Module docs of a few lines.
* `//` comments only where the reason is not obvious, one or two lines.
* No em dashes anywhere.
* Prefer boring, readable Rust. Contributors here are AEC developers first.
* Keep `unsafe` inside existing low-level boundaries, with a `// SAFETY:`
  comment. New unsafe operations need an explicit justification and review.
* No panics on input, ever. A panic in the parser is a security bug.

## Before you open a pull request

```sh
cargo fmt --all
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```
