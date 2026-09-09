# SPDX-License-Identifier: Apache-2.0
"""Build the TessIFC WebAssembly package.

Uses installed Cargo, wasm-bindgen and optionally Binaryen:

    1. cargo build --profile wasm --target wasm32-unknown-unknown -p tessifc-wasm
    2. wasm-bindgen --target web|nodejs --out-dir bindings/wasm/pkg[-node]
    3. wasm-opt -O3, if binaryen happens to be installed

The script installs no tools. Cargo may fetch dependencies pinned in Cargo.lock.
If a tool is missing, it prints the installation command and stops.

Usage:

    python scripts/build-wasm.py                      # both flavours
    python scripts/build-wasm.py --target web         # browsers only
    python scripts/build-wasm.py --target nodejs      # what the smoke test uses
    python scripts/build-wasm.py --profile wasm-release   # smaller, slower

Exit codes:

    0  built
    1  a build step failed, or the artefact is over a ceiling you passed
    2  a prerequisite is missing or is the wrong version
"""

from __future__ import annotations

import argparse
import gzip
import os
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]

CRATE = "tessifc-wasm"
# The crate name with dashes turned into underscores: what cargo actually
# writes, and the stem wasm-bindgen derives its output names from.
CRATE_STEM = "tessifc_wasm"
WASM_TARGET = "wasm32-unknown-unknown"

# Used only if Cargo.lock cannot be read. The lock file is the real source of
# truth, because the CLI and the crate must be the same version exactly.
FALLBACK_BINDGEN_VERSION = "0.2.127"

# -O3 runs the kernel faster and still shrinks the module; -Oz makes it
# measurably slower. The feature flags are ones Rust emits for wasm32 by
# default that older binaryen releases do not understand.
WASM_OPT_FLAGS = [
    "-O3",
    "--enable-bulk-memory",
    "--enable-nontrapping-float-to-int",
    "--enable-mutable-globals",
    "--enable-sign-ext",
    "--enable-reference-types",
    "--enable-multivalue",
]

# Older binaryen releases rewrite the module's exported externref table to
# point at the function table, and the glue then fails at start-up with
# "WebAssembly.Table.grow(): failed to grow table by 4". Distribution packages
# are often that old, so the version is checked and the output is verified.
MIN_WASM_OPT_VERSION = 116

OUT_DIRS = {
    "web": REPO / "bindings" / "wasm" / "pkg",
    "nodejs": REPO / "bindings" / "wasm" / "pkg-node",
}

# wasm-bindgen --target web emits an ES module; --target nodejs emits CommonJS.
# Node decides which is which from the nearest package.json, so pin it inside
# the output directory rather than hoping nothing above it disagrees.
MODULE_TYPES = {"web": "module", "nodejs": "commonjs"}

EXIT_OK = 0
EXIT_FAILED = 1
EXIT_MISSING_TOOL = 2


_step_number = 0


def say(message: str = "") -> None:
    print(message, flush=True)


def step(message: str) -> None:
    global _step_number
    _step_number += 1
    say()
    say(f"[{_step_number}] {message}")


def human(size: int) -> str:
    if size < 1024:
        return f"{size} B"
    if size < 1024 * 1024:
        return f"{size / 1024:.1f} KB ({size} bytes)"
    return f"{size / (1024 * 1024):.2f} MB ({size} bytes)"


def run(command: list[str], cwd: Path) -> int:
    """Run a command, streaming its output. Never uses a shell."""
    say("    $ " + " ".join(str(c) for c in command))
    try:
        return subprocess.run(command, cwd=str(cwd), check=False).returncode
    except OSError as error:
        say(f"    could not run {command[0]}: {error}")
        return 127


def capture(command: list[str]) -> tuple[int, str]:
    try:
        done = subprocess.run(
            command, check=False, capture_output=True, text=True, encoding="utf-8", errors="replace"
        )
    except OSError as error:
        return 127, str(error)
    return done.returncode, (done.stdout or "") + (done.stderr or "")


def find_tool(name: str) -> str | None:
    """Look on PATH, then in CARGO_HOME/bin, which is where cargo install puts things."""
    found = shutil.which(name)
    if found:
        return found
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    for candidate in (cargo_home / "bin" / name, cargo_home / "bin" / f"{name}.exe"):
        if candidate.is_file():
            return str(candidate)
    return None


def version_from_lock(package: str) -> str | None:
    """Read a package version out of the workspace Cargo.lock."""
    lock = REPO / "Cargo.lock"
    if not lock.is_file():
        return None
    current = None
    for raw in lock.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if line.startswith('name = "'):
            current = line[len('name = "') : -1]
        elif line.startswith('version = "') and current == package:
            return line[len('version = "') : -1]
    return None


def install_command(version: str) -> str:
    return f"cargo install wasm-bindgen-cli --version {version}"


def check_cargo() -> str:
    cargo = find_tool("cargo")
    if cargo is None:
        say("ERROR: cargo is not on PATH.")
        say("       Install Rust from https://rustup.rs and try again.")
        sys.exit(EXIT_MISSING_TOOL)
    return cargo


def check_wasm_target() -> None:
    """Warn early rather than letting cargo fail with a longer message."""
    rustup = find_tool("rustup")
    if rustup is None:
        return
    code, output = capture([rustup, "target", "list", "--installed"])
    if code != 0:
        return
    if WASM_TARGET not in output.split():
        say(f"ERROR: the {WASM_TARGET} target is not installed.")
        say(f"       Run: rustup target add {WASM_TARGET}")
        sys.exit(EXIT_MISSING_TOOL)


def check_wasm_bindgen(wanted: str) -> str:
    """Find wasm-bindgen and refuse to continue unless it matches the crate."""
    tool = find_tool("wasm-bindgen")
    if tool is None:
        say("ERROR: wasm-bindgen-cli is not installed.")
        say("       The generated JavaScript glue and the generated wasm must come from")
        say("       the same version, so this script will not guess. Run:")
        say()
        say(f"           {install_command(wanted)}")
        say()
        sys.exit(EXIT_MISSING_TOOL)

    code, output = capture([tool, "--version"])
    found = output.split()[1] if code == 0 and len(output.split()) > 1 else "unknown"
    if found != wanted:
        say(f"ERROR: wasm-bindgen {found} is installed but the wasm-bindgen crate is {wanted}.")
        say("       Mismatched versions produce glue that does not match the module.")
        say("       Run:")
        say()
        say(f"           {install_command(wanted)}")
        say()
        sys.exit(EXIT_MISSING_TOOL)

    say(f"    wasm-bindgen {found}  ({tool})")
    return tool


def cargo_build(cargo: str, profile: str, features: str | None, no_default: bool) -> Path:
    command = [cargo, "build", "--locked", "--target", WASM_TARGET, "-p", CRATE]
    command += ["--release"] if profile == "release" else ["--profile", profile]
    # Feature selection is how the single-schema build is
    # actually produced, and the only way to check that the claim is true:
    #   --no-default-features --features tessifc-schema/schema-ifc4
    if no_default:
        command.append("--no-default-features")
    if features:
        command += ["--features", features]
    if run(command, REPO) != 0:
        say()
        say(f"ERROR: {CRATE} did not build for {WASM_TARGET}.")
        say("       See the Cargo diagnostics above.")
        sys.exit(EXIT_FAILED)

    artefact = REPO / "target" / WASM_TARGET / profile / f"{CRATE_STEM}.wasm"
    if not artefact.is_file():
        say()
        say(f"ERROR: cargo reported success but {artefact} does not exist.")
        say('       Does bindings/wasm/Cargo.toml still declare crate-type = ["cdylib", ...]?')
        sys.exit(EXIT_FAILED)
    say(f"    built {artefact.relative_to(REPO)}  {human(artefact.stat().st_size)}")
    return artefact


def run_bindgen(tool: str, flavour: str, artefact: Path) -> Path:
    out_dir = OUT_DIRS[flavour]
    out_dir.mkdir(parents=True, exist_ok=True)
    command = [tool, "--target", flavour, "--out-dir", str(out_dir), str(artefact)]
    if run(command, REPO) != 0:
        say()
        say(f"ERROR: wasm-bindgen failed for --target {flavour}.")
        sys.exit(EXIT_FAILED)

    produced = out_dir / f"{CRATE_STEM}_bg.wasm"
    if not produced.is_file():
        say()
        say(f"ERROR: wasm-bindgen did not write {produced}.")
        sys.exit(EXIT_FAILED)

    # Pin the module system for this directory so Node reads the glue the way
    # wasm-bindgen wrote it, whatever any package.json above it says.
    marker = out_dir / "package.json"
    marker.write_text(f'{{\n  "type": "{MODULE_TYPES[flavour]}"\n}}\n', encoding="utf-8")

    say(f'    wrote {out_dir.relative_to(REPO)}/  (package.json type: "{MODULE_TYPES[flavour]}")')
    for path in sorted(out_dir.iterdir()):
        say(f"        {path.name}  {human(path.stat().st_size)}")
    return produced


def wasm_opt_version(tool: str) -> int | None:
    """The release number from `wasm-opt --version`, or None if it cannot be read."""
    code, output = capture([tool, "--version"])
    if code != 0:
        return None
    for word in output.split():
        if word.isdigit():
            return int(word)
    return None


def read_leb(data: bytes, offset: int) -> tuple[int, int]:
    value = 0
    shift = 0
    while offset < len(data):
        byte = data[offset]
        offset += 1
        value |= (byte & 0x7F) << shift
        shift += 7
        if not byte & 0x80:
            break
    return value, offset


def table_exports(data: bytes) -> dict[str, tuple[int, int | None]]:
    """Map each exported table name to its (element type, maximum) pair.

    Enough of the binary format to see whether an optimiser has re-pointed an
    export at a different table, which is what breaks wasm-bindgen's start-up.
    """
    tables: list[tuple[int, int | None]] = []
    exports: dict[str, int] = {}
    offset = 8
    while offset < len(data):
        section = data[offset]
        size, offset = read_leb(data, offset + 1)
        body = data[offset : offset + size]
        offset += size
        if section == 2:
            count, at = read_leb(body, 0)
            for _ in range(count):
                length, at = read_leb(body, at)
                at += length
                length, at = read_leb(body, at)
                at += length
                kind = body[at]
                at += 1
                if kind == 0:
                    _, at = read_leb(body, at)
                elif kind == 1:
                    element = body[at]
                    flags = body[at + 1]
                    minimum, at = read_leb(body, at + 2)
                    maximum = None
                    if flags & 1:
                        maximum, at = read_leb(body, at)
                    tables.append((element, maximum))
                elif kind == 2:
                    flags = body[at]
                    _, at = read_leb(body, at + 1)
                    if flags & 1:
                        _, at = read_leb(body, at)
                else:
                    at += 2
        elif section == 4:
            count, at = read_leb(body, 0)
            for _ in range(count):
                element = body[at]
                flags = body[at + 1]
                _, at = read_leb(body, at + 2)
                maximum = None
                if flags & 1:
                    maximum, at = read_leb(body, at)
                tables.append((element, maximum))
        elif section == 7:
            count, at = read_leb(body, 0)
            for _ in range(count):
                length, at = read_leb(body, at)
                name = body[at : at + length].decode("utf-8", errors="replace")
                at += length
                kind = body[at]
                index, at = read_leb(body, at + 1)
                if kind == 1:
                    exports[name] = index
    return {name: tables[index] for name, index in exports.items() if index < len(tables)}


def run_wasm_opt(wasm: Path) -> bool:
    """Optimise in place. Returns False when it was skipped, which is not an error."""
    tool = find_tool("wasm-opt")
    if tool is None:
        say("    WARNING: wasm-opt not found, skipping the -O3 pass.")
        say("             The module is correct but larger and slower than it needs to be.")
        say("             Install binaryen (https://github.com/WebAssembly/binaryen/releases)")
        say("             or, on Windows, `winget install WebAssembly.binaryen`.")
        return False

    version = wasm_opt_version(tool)
    if version is not None and version < MIN_WASM_OPT_VERSION:
        say(f"    WARNING: wasm-opt {version} is too old, skipping the -O3 pass.")
        say(f"             Releases before {MIN_WASM_OPT_VERSION} mis-link the externref table")
        say("             and the module then fails to start. Install a current binaryen")
        say("             from https://github.com/WebAssembly/binaryen/releases.")
        return False
    say(f"    wasm-opt {version if version is not None else '(version unknown)'}  ({tool})")

    before = wasm.stat().st_size
    temporary = wasm.with_suffix(".opt.wasm")
    command = [tool, *WASM_OPT_FLAGS, str(wasm), "-o", str(temporary)]
    if run(command, REPO) != 0 or not temporary.is_file():
        temporary.unlink(missing_ok=True)
        say("    WARNING: wasm-opt failed, keeping the unoptimised module.")
        say("             This is usually an old binaryen that cannot parse a newer")
        say("             wasm feature. The size check below still applies.")
        return False

    # The optimised module must export the same tables as the original. An
    # export that now names a table with a different type or limit would fail
    # in every browser at start-up, so the unoptimised module is kept instead.
    expected = table_exports(wasm.read_bytes())
    produced = table_exports(temporary.read_bytes())
    if produced != expected:
        temporary.unlink(missing_ok=True)
        say("    WARNING: wasm-opt changed the module's table exports, keeping the")
        say("             unoptimised module. This binaryen release is not usable here;")
        say("             install a current one from https://github.com/WebAssembly/binaryen/releases.")
        return False

    after = temporary.stat().st_size
    temporary.replace(wasm)
    saved = before - after
    percent = (saved / before * 100) if before else 0.0
    say(f"    wasm-opt: {human(before)} -> {human(after)}, saved {percent:.1f}%")
    return True


def report_size(wasm: Path, ceiling: int | None) -> bool:
    raw = wasm.stat().st_size
    compressed = len(gzip.compress(wasm.read_bytes(), compresslevel=9, mtime=0))

    say(f"    {wasm.relative_to(REPO)}")
    say(f"        raw      {human(raw)}")
    say(f"        gzipped  {human(compressed)}")
    if ceiling is None:
        return True
    say(f"        ceiling  {human(ceiling)}")
    if compressed > ceiling:
        say(f"        OVER by {human(compressed - ceiling)}")
        return False
    return True


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Build the TessIFC WASM package without wasm-pack.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--target",
        choices=["web", "nodejs", "both"],
        default="both",
        help="web goes to bindings/wasm/pkg, nodejs to bindings/wasm/pkg-node (default: both)",
    )
    parser.add_argument(
        "--profile",
        choices=["wasm", "release", "wasm-release"],
        default="wasm",
        help="cargo profile: wasm is the fast one, wasm-release the size-optimised one (default: wasm)",
    )
    parser.add_argument(
        "--budget-bytes",
        type=int,
        default=None,
        help="fail if the gzipped wasm is larger than this many bytes",
    )
    parser.add_argument("--no-opt", action="store_true", help="skip wasm-opt even if it is present")
    parser.add_argument(
        "--features",
        help="cargo features, comma separated, e.g. tessifc-schema/schema-ifc4",
    )
    parser.add_argument(
        "--no-default-features",
        action="store_true",
        help="drop default features; pair with --features to build one schema",
    )
    parser.add_argument(
        "--skip-build", action="store_true", help="reuse the existing cargo artefact"
    )
    args = parser.parse_args()

    flavours = ["web", "nodejs"] if args.target == "both" else [args.target]

    say(f"TessIFC wasm build   repo: {REPO}")
    say(f"                   target: {WASM_TARGET}")
    say(f"                  profile: {args.profile}")
    say(f"                 flavours: {', '.join(flavours)}")

    step("checking the tools")
    cargo = check_cargo()
    code, output = capture([cargo, "--version"])
    say(f"    {output.strip() if code == 0 else 'cargo (version unknown)'}")
    check_wasm_target()

    wanted = version_from_lock("wasm-bindgen")
    if wanted is None:
        wanted = FALLBACK_BINDGEN_VERSION
        say(f"    WARNING: could not read wasm-bindgen from Cargo.lock, assuming {wanted}")
    else:
        say(f"    wasm-bindgen crate {wanted}  (from Cargo.lock)")
    bindgen = check_wasm_bindgen(wanted)

    step(f"cargo build, profile {args.profile}")
    artefact = REPO / "target" / WASM_TARGET / args.profile / f"{CRATE_STEM}.wasm"
    if args.skip_build:
        if not artefact.is_file():
            say(f"ERROR: --skip-build was passed but {artefact} does not exist.")
            return EXIT_FAILED
        say(f"    skipped, reusing {artefact.relative_to(REPO)}")
    else:
        artefact = cargo_build(cargo, args.profile, args.features, args.no_default_features)

    within_ceiling = True
    for flavour in flavours:
        step(f"wasm-bindgen --target {flavour}")
        produced = run_bindgen(bindgen, flavour, artefact)

        step(f"wasm-opt ({flavour})")
        if args.no_opt:
            say("    skipped (--no-opt)")
        else:
            run_wasm_opt(produced)

        step(f"size ({flavour})")
        within_ceiling = report_size(produced, args.budget_bytes) and within_ceiling

    say()
    if not within_ceiling:
        say("FAILED: the module is over the gzipped size you asked for.")
        say("        Options: build with --profile wasm-release, install binaryen so")
        say("        wasm-opt can run, or drop a schema feature for this build.")
        return EXIT_FAILED

    say("OK: built.")
    return EXIT_OK


if __name__ == "__main__":
    raise SystemExit(main())
