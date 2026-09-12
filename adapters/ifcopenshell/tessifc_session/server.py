# SPDX-License-Identifier: Apache-2.0
"""Loopback HTTP server: the viewer, model snapshots, script runs and the assistant."""

from __future__ import annotations

import json
import mimetypes
import secrets
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlsplit

from .assistant import Assistant, AssistantError
from .session import EditSession, SessionBusy, SessionError

MAX_BODY_BYTES = 4 << 20
MAX_WAIT_SECONDS = 25.0
STATIC_SUFFIXES = {".html", ".js", ".css", ".wasm", ".svg"}


def create_server(session: EditSession, port: int = 8000, *, root: Path, assistant: Assistant | None = None,
                  static_roots=("viewer", "bindings/wasm/pkg", "bindings/edit/src", "bindings/viewer/src")):
    """A loopback-only server; `root` is the checkout that holds the viewer and the WASM package."""
    token = secrets.token_urlsafe(24)
    allowed = tuple((Path(root) / name).resolve() for name in static_roots)

    class Handler(BaseHTTPRequestHandler):
        # ------------------------------------------------------ requests

        def do_GET(self):
            if not self.check_origin():
                return
            target = urlsplit(self.path)
            query = parse_qs(target.query)
            if target.path == "/__tessifc/session":
                after = query.get("after", [None])[0]
                timeout = 0.0
                if after is not None:
                    try:
                        timeout = min(float(query.get("timeout", ["20"])[0]), MAX_WAIT_SECONDS)
                    except ValueError:
                        timeout = 0.0
                    session.wait_for_change(after, timeout)
                try:
                    session.snapshot()
                except OSError:
                    self.send_error(503, "IFC snapshot not ready")
                    return
                status = session.describe(assistant)
                status["token"] = token
                self.respond_json(status)
            elif target.path == "/__tessifc/model.ifc":
                try:
                    version, payload = session.snapshot()
                except OSError:
                    self.send_error(503, "IFC snapshot not ready")
                    return
                if query.get("version") != [version]:
                    self.send_error(409, "The IFC snapshot changed; request its current version")
                else:
                    self.respond(payload, "application/octet-stream")
            elif target.path == "/":
                self.send_response(302)
                self.send_header("Location", "/viewer/?session=file")
                self.send_header("Content-Length", "0")
                self.end_headers()
            else:
                self.serve_static(target.path)

        def do_POST(self):
            if not self.check_origin(require_origin=True):
                return
            if self.headers.get("X-Tessifc-Token") != token:
                self.send_error(403, "Missing session token")
                return
            target = urlsplit(self.path)
            try:
                body = self.read_json()
            except ValueError as error:
                self.respond_json({"error": str(error)}, 400)
                return
            try:
                if target.path == "/__tessifc/run":
                    result = session.run_script(str(body.get("script", "")), body.get("selection"))
                elif target.path == "/__tessifc/undo":
                    result = session.undo()
                elif target.path == "/__tessifc/redo":
                    result = session.redo()
                elif target.path == "/__tessifc/assistant":
                    if assistant is None:
                        self.respond_json({"error": "No assistant provider is configured for this session."}, 404)
                        return
                    result = assistant.respond(body)
                else:
                    self.send_error(404)
                    return
            except SessionBusy as error:
                self.respond_json({"error": str(error)}, 409)
                return
            except (SessionError, AssistantError) as error:
                self.respond_json({"error": str(error)}, 400)
                return
            except OSError as error:
                self.respond_json({"error": f"The IFC file could not be read or written: {error}"}, 503)
                return
            result["status"] = session.describe(assistant)
            self.respond_json(result)

        # ------------------------------------------------------- helpers

        def check_origin(self, require_origin: bool = False) -> bool:
            port = self.server.server_port
            hosts = {f"127.0.0.1:{port}", f"localhost:{port}"}
            if self.headers.get("Host") not in hosts:
                self.send_error(403)
                return False
            origin = self.headers.get("Origin")
            if (origin is None and require_origin) or (origin and origin not in {f"http://{host}" for host in hosts}):
                self.send_error(403)
                return False
            return True

        def read_json(self) -> dict:
            try:
                length = int(self.headers.get("Content-Length", "0"))
            except ValueError as error:
                raise ValueError("Invalid Content-Length") from error
            if length <= 0 or length > MAX_BODY_BYTES:
                raise ValueError("The request body is empty or too large.")
            try:
                body = json.loads(self.rfile.read(length).decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise ValueError("The request body must be UTF-8 JSON.") from error
            if not isinstance(body, dict):
                raise ValueError("The request body must be a JSON object.")
            return body

        def serve_static(self, request_path: str):
            try:
                relative = unquote(request_path).lstrip("/")
                path = (Path(root) / relative).resolve(strict=True)
                if not any(path.is_relative_to(base) for base in allowed) or "node_modules" in path.parts:
                    raise OSError("Route unavailable")
                if path.is_dir():
                    path = (path / "index.html").resolve(strict=True)
                if path.suffix not in STATIC_SUFFIXES or not any(path.is_relative_to(base) for base in allowed):
                    raise OSError("Route unavailable")
                mime = {".js": "text/javascript", ".wasm": "application/wasm"}.get(path.suffix)
                self.respond(path.read_bytes(), mime or mimetypes.guess_type(path)[0] or "application/octet-stream")
            except (OSError, ValueError):
                self.send_error(404)

        def respond_json(self, payload, status: int = 200):
            self.respond(json.dumps(payload, default=str).encode("utf-8"), "application/json", status)

        def respond(self, payload: bytes, mime: str, status: int = 200):
            self.send_response(status)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.end_headers()
            try:
                self.wfile.write(payload)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def log_message(self, format, *args):
            if not str(args[1] if len(args) > 1 else "").startswith(("2", "3")):
                super().log_message(format, *args)

    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.daemon_threads = True
    server.session_token = token
    return server
