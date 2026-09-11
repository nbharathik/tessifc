# SPDX-License-Identifier: Apache-2.0
"""The Claude provider's request shape, checked against a local stand-in for the API."""

from __future__ import annotations

import json
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

PACKAGE_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PACKAGE_ROOT))

from tessifc_session.assistant import AnthropicProvider, Assistant  # noqa: E402

try:
    import anthropic  # noqa: F401
except ImportError:
    anthropic = None


class RecordingApi:
    """Answers one tool call, then a final text, and keeps every request body."""

    def __init__(self):
        self.requests = []
        self.headers = []
        server = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0"))
                body = json.loads(self.rfile.read(length))
                server.requests.append(body)
                server.headers.append(dict(self.headers))
                if len(server.requests) == 1:
                    content = [{"type": "tool_use", "id": "toolu_1", "name": "inspect_model",
                                "input": {"code": "print('hello')"}}]
                    stop = "tool_use"
                else:
                    content = [{"type": "text", "text": "Two walls."}]
                    stop = "end_turn"
                payload = json.dumps({
                    "id": "msg_1", "type": "message", "role": "assistant", "model": body["model"],
                    "content": content, "stop_reason": stop, "stop_sequence": None,
                    "usage": {"input_tokens": 10, "output_tokens": 5},
                }).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *args):
                pass

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


class StubSession:
    """Enough of EditSession for the loop: a context and a scripted inspect result."""

    def summary(self):
        return {"name": "stub.ifc", "schema": "IFC4", "lengthUnit": "METRE", "products": {"IfcWall": 2},
                "storeys": [], "revision": 0}

    def describe_selection(self, selection):
        return []

    def run_script(self, source, selection=None, *, commit=True, label="script"):
        return {"ok": True, "changed": False, "stdout": "hello\n", "operations": {}, "error": None}


@unittest.skipIf(anthropic is None, "the anthropic package is not installed")
class AnthropicRequestTests(unittest.TestCase):
    def setUp(self):
        self.api = RecordingApi()

    def tearDown(self):
        self.api.close()

    def test_requests_carry_model_thinking_effort_fallbacks_and_tools(self):
        provider = AnthropicProvider("claude-opus-5", "low")
        provider._client = anthropic.Anthropic(api_key="test-key", base_url=self.api.url, max_retries=0)
        outcome = Assistant(StubSession(), provider).respond({"mode": "ask", "prompt": "How many walls?"})
        self.assertEqual(outcome["answer"], "Two walls.")
        self.assertEqual(outcome["rounds"], 2)
        self.assertEqual(outcome["usage"], {"inputTokens": 20, "outputTokens": 10})
        first, second = self.api.requests
        self.assertEqual(first["model"], "claude-opus-5")
        self.assertEqual(first["thinking"], {"type": "adaptive"})
        self.assertEqual(first["output_config"], {"effort": "low"})
        self.assertEqual(first["fallbacks"], "default")
        self.assertEqual([tool["name"] for tool in first["tools"]], ["inspect_model"])
        self.assertEqual(first["system"][0]["cache_control"], {"type": "ephemeral"})
        self.assertIn("IfcWall 2", first["system"][1]["text"])
        self.assertIn("server-side-fallback-2026-07-01", self.api.headers[0].get("anthropic-beta", ""))
        self.assertEqual(second["messages"][1]["role"], "assistant")
        self.assertEqual(second["messages"][1]["content"][0]["type"], "tool_use")
        result = second["messages"][2]["content"][0]
        self.assertEqual((result["type"], result["tool_use_id"], result["content"]), ("tool_result", "toolu_1", "hello\n"))


if __name__ == "__main__":
    unittest.main()
