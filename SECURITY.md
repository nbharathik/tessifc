<!-- SPDX-License-Identifier: Apache-2.0 -->
# Security policy

## v0.1 threat model

IFC files are untrusted input. The parser and evaluators report recoverable
problems through diagnostics. A panic, stack overflow or unexpectedly large
resource use on crafted input is a bug to report, not an accepted way to
reject a file.

The core does not initiate network connections or execute file-provided code.
The CLI reads and writes paths supplied by its caller. The viewer runs the
kernel in a Web Worker and does not upload model data.

Parser options bound entity counts, string sizes and nesting. Geometry has
depth, tessellation and operation-specific budgets. These are local limits;
the preview does not provide a complete process-wide memory or time limit,
and total work can grow faster than input size. Allocation failure or a host
WASM trap can still terminate a conversion.

Services should validate input size before loading, isolate conversion in a
worker process and enforce memory and elapsed-time limits. A browser host
should terminate and replace its worker when cancelling a long operation.
Do not equate a partial mesh or a successful process exit with valid geometry.

Unsafe code is limited to documented low-level operations, including CLI
allocation accounting. The test suites exercise parser recovery and geometry
limits with first-party fragments. Tests are regression evidence, not a proof
against every malformed input.

## Reporting a vulnerability

Report privately using the repository's **Security → Report a vulnerability**
feature when enabled, or email **bharathikannanhs@gmail.com**.

Include a minimal input or generator, version, effective settings, platform,
observed behaviour and reproduction command. Share only models you are
authorised to disclose. We aim to acknowledge reports within five working
days and provide a fix or mitigation within 30 days for confirmed issues.

Please test locally and do not run denial-of-service tests against services
operated by others.

## Supported versions

During the preview, fixes target the latest release. Keep package versions
aligned and retest your model set when upgrading. Confirmed failures should
be reduced to regression tests wherever possible.
