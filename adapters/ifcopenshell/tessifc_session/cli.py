# SPDX-License-Identifier: Apache-2.0
"""Serve the viewer and one IFC file as a live editing session."""

from __future__ import annotations

import argparse
import os
import sys
import threading
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
    parser.add_argument("--mcp", action="store_true", help="Speak MCP on stdin and stdout for an agent; the viewer server keeps running")
    parser.add_argument("--new", action="store_true", help="Create the IFC file first (a project, site, building and storeys)")
    parser.add_argument("--schema", default="IFC4", help="Schema of a new file: IFC2X3, IFC4 or IFC4X3")
    parser.add_argument("--storeys", default=None, help='Storeys of a new file, e.g. "Ground floor:0,Upper floor:3"')
    args = parser.parse_args(argv)
    if args.mcp:
        # Stdout is the protocol from here on; every message goes to stderr.
        sys.stdout = sys.stderr
    if args.new:
        if args.ifc.suffix.lower() != ".ifc":
            parser.error("A new file needs an .ifc name.")
        if not args.ifc.exists():
            from .model import create_model, write_model

            storeys = None
            if args.storeys:
                storeys = []
                for item in args.storeys.split(","):
                    name, _, elevation = item.partition(":")
                    storeys.append({"name": name.strip() or f"Storey {len(storeys) + 1}", "elevation": float(elevation or 0)})
            try:
                write_model(create_model(args.schema, name=args.ifc.stem, storeys=storeys), args.ifc)
            except Exception as error:  # noqa: BLE001 - reported as a usage error
                parser.error(f"The new model could not be written: {error}")
    if args.ifc.suffix.lower() != ".ifc" or not args.ifc.is_file():
        parser.error("Choose an existing IFC file, or pass --new to create it.")
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
    viewer_url = f"http://127.0.0.1:{server.server_port}/viewer/?session=file"
    print(f"Open {viewer_url}", flush=True)
    if args.mcp:
        try:
            from .mcp import run_stdio
        except ImportError:
            parser.error("The MCP transport needs the mcp package: pip install 'tessifc-session[mcp]'")
        import anyio

        thread = threading.Thread(target=server.serve_forever, name="tessifc-viewer", daemon=True)
        thread.start()
        try:
            anyio.run(lambda: run_stdio(session, viewer_url=viewer_url))
        except KeyboardInterrupt:
            pass
        finally:
            server.shutdown()
            server.server_close()
        return
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
