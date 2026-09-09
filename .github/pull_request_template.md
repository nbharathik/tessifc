<!-- SPDX-License-Identifier: Apache-2.0 -->

## What changed

<!-- One paragraph. What does this do that the previous commit did not? If it
     fixes a bug, say what the bug did. -->

## Which fixture proves it

<!-- Name the file and the test. Every bug fix adds a fixture. If nothing here
     is testable, say why in one line. Docs, CI config and renames are
     legitimate answers. -->

## License provenance

**Did you copy or adapt code from any GPL, LGPL, AGPL or MPL licensed project
while writing this?**

<!-- Answer yes or no. An undeclared yes puts the project's Apache-2.0 license
     at risk. Public documentation and papers are fine to use. -->

Answer:

## Checklist

- [ ] Every new file starts with the SPDX header
      (`// SPDX-License-Identifier: Apache-2.0`, `#` for Python, TOML and YAML,
      `<!-- -->` for Markdown)
- [ ] Every commit is signed off (`git commit -s`, DCO)
- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --locked --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --locked --workspace` passes
- [ ] Relevant browser, adapter or script tests pass
- [ ] New dependency origins and licensing are documented for review
