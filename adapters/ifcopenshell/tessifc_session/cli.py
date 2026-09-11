# SPDX-License-Identifier: Apache-2.0
"""Serve the viewer and one IFC file as a live editing session."""

from __future__ import annotations

import argparse
import os
from pathlib import Path

from .assistant import Assistant, AssistantError, create_provider
from .server import create_server
from .session import EditSession


def default_root() -> Path:
    """The checkout root, found from this package or from TESSIFC_ROOT."""
    override = os.environ.get("TESSIFC_ROOT")
    if override:
        return Path(override).resolve()
    return Path(__file__).resolve().parents[3]


def main(argv=None):
    parser = argparse.ArgumentParser(prog="tessifc-session", description=__doc__)
    parser.add_argument("ifc", type=Path, help="The IFC file to follow and edit")
    parser.add_argument("--port", type=int, default=8000)
    parser.add_argument("--root", type=Path, default=None, help="Checkout holding viewer/ and bindings/wasm/pkg")
    parser.add_argument("--assistant", default=None, help="anthropic, fake or none (default: anthropic when a key is set)")
    parser.add_argument("--model", default=None, help="Assistant model id")
    parser.add_argument("--effort", default=None, help="Assistant effort: low, medium, high, xhigh or max")
    args = parser.parse_args(argv)
    if args.ifc.suffix.lower() != ".ifc" or not args.ifc.is_file():
        parser.error("Choose an existing IFC file.")
    root = (args.root or default_root()).resolve()
    if not (root / "viewer" / "index.html").is_file():
        parser.error(f"No viewer found under {root}; pass --root or set TESSIFC_ROOT.")
    session = EditSession(args.ifc)
    try:
        provider = create_provider(args.assistant, args.model, args.effort)
    except AssistantError as error:
        parser.error(str(error))
    assistant = Assistant(session, provider) if provider is not None else None
    if provider is not None and hasattr(provider, "client"):
        try:
            provider.client()
        except AssistantError as error:
            parser.error(str(error))
    # Scripts run in this process; keep the provider key out of their environment.
    for name in ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"):
        os.environ.pop(name, None)
    server = create_server(session, args.port, root=root, assistant=assistant)
    print(f"Following {session.path}", flush=True)
    if session.authoring_available():
        print(f"Scripts run with IfcOpenShell {getattr(session.ifcopenshell, 'version', '')}", flush=True)
    else:
        print("Scripts are unavailable: install IfcOpenShell in this Python environment", flush=True)
    if assistant is not None:
        print(f"Assistant: {provider.name} ({getattr(provider, 'model', '')})", flush=True)
    else:
        print("Assistant: off (set ANTHROPIC_API_KEY or pass --assistant)", flush=True)
    print(f"Open http://127.0.0.1:{server.server_port}/viewer/?session=file", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
