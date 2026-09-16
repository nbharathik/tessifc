# SPDX-License-Identifier: Apache-2.0
"""One authoritative IFC file, edited in memory and published as immutable snapshots.

The file on disk is the exchange format: every accepted change is written with
an atomic replacement, and the viewer follows content versions. The model is
parsed by an externally installed IfcOpenShell, imported on first use.
"""

from __future__ import annotations

import hashlib
import io
import linecache
import os
import tempfile
import threading
import time
import traceback
from collections import deque
from contextlib import redirect_stdout
from pathlib import Path

from .model import owner_scope

MAX_OUTPUT_CHARS = 64_000
SCRIPT_NAME = "<session script>"


class SessionError(Exception):
    """A request that cannot be served in the current state."""


class SessionBusy(SessionError):
    """Another script is still running against the model."""


def file_stamp(path: Path):
    stat = path.stat()
    return stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns


def data_section(text: str) -> str:
    """The DATA section, so a rewritten header timestamp does not count as a change."""
    start = text.find("DATA;")
    return text[start:] if start >= 0 else text


def clip(text: str, limit: int = MAX_OUTPUT_CHARS) -> str:
    if len(text) <= limit:
        return text
    return text[:limit] + f"\n... {len(text) - limit} more characters"


class BoundedOutput(io.StringIO):
    """Captured print output that stops growing at the limit."""

    def __init__(self, limit: int = MAX_OUTPUT_CHARS):
        super().__init__()
        self.limit = limit
        self.dropped = 0

    def write(self, text):
        room = self.limit - self.tell()
        if room <= 0:
            self.dropped += len(text)
            return len(text)
        if len(text) > room:
            self.dropped += len(text) - room
            text = text[:room]
        return super().write(text)

    def text(self) -> str:
        value = self.getvalue()
        return value + f"\n... {self.dropped} more characters" if self.dropped else value


class EditSession:
    """Retain one immutable snapshot until the file changes, and change it through scripts."""

    def __init__(self, path, *, history_limit: int = 20, history_bytes: int = 256 << 20):
        self.path = Path(path).resolve(strict=True)
        self.lock = threading.RLock()
        self.changed = threading.Condition(self.lock)
        self.model_lock = threading.Lock()
        self.history_limit = history_limit
        self.history_bytes = history_bytes
        self.signature = None
        self.version = None
        self.revision = 0
        self.payload = b""
        self.model = None
        self._baseline = None
        self.undo_stack: deque = deque()
        self.redo_stack: deque = deque()
        self.ifcopenshell = None
        self.authoring_error = None
        self._summary = None
        # Counts the files this session has held; a viewer reopens when it changes.
        self.generation = 1
        self.selection = None
        self.applied = None
        # When a viewer last asked for the status; edits wait for its report only while one is around.
        self.viewer_seen = None
        self.snapshot()

    # ----------------------------------------------------------- snapshots

    def snapshot(self):
        """The current version and bytes, reading the file again after an external write."""
        with self.lock:
            before = file_stamp(self.path)
            if before != self.signature:
                payload = self.path.read_bytes()
                after = file_stamp(self.path)
                if before != after or not payload:
                    raise OSError("The IFC file is still being written.")
                self._adopt(payload, after, external=self.version is not None)
            return self.version, self.payload

    def _adopt(self, payload: bytes, signature, *, external: bool):
        previous = (self.version, self.payload)
        self.payload = payload
        self.signature = signature
        self.version = hashlib.sha256(payload).hexdigest()
        self.model = None
        self._summary = None
        if previous[0] is not None:
            self.revision += 1
            if external:
                self._push(self.undo_stack, previous)
                self.redo_stack.clear()
        self.changed.notify_all()

    def _push(self, stack: deque, entry):
        stack.append(entry)
        while len(stack) > self.history_limit or sum(len(item[1]) for item in stack) > self.history_bytes:
            if len(stack) == 1:
                break
            stack.popleft()

    def wait_for_change(self, after: str | None, timeout: float):
        """Block until the version differs from `after`, polling the file for external writes."""
        deadline = time.monotonic() + max(0.0, timeout)
        with self.changed:
            while True:
                try:
                    version, _ = self.snapshot()
                except OSError:
                    version = self.version
                remaining = deadline - time.monotonic()
                if version != after or remaining <= 0:
                    return version
                self.changed.wait(min(0.5, remaining))

    # ------------------------------------------------------------ authoring

    def authoring_available(self) -> bool:
        try:
            self._ifcopenshell()
            return True
        except SessionError:
            return False

    def _ifcopenshell(self):
        if self.ifcopenshell is None:
            if self.authoring_error:
                raise SessionError(self.authoring_error)
            try:
                import ifcopenshell
                import ifcopenshell.guid
                import ifcopenshell.util.element
            except ImportError as error:
                self.authoring_error = "IfcOpenShell is not installed in this Python environment."
                raise SessionError(self.authoring_error) from error
            self.ifcopenshell = ifcopenshell
        return self.ifcopenshell

    def _ensure_model(self):
        """The parsed model for the committed bytes; parsed once per version."""
        ifcopenshell = self._ifcopenshell()
        with self.lock:
            self.snapshot()
            if self.model is None:
                self.model = ifcopenshell.open(str(self.path))
                self.model.set_history_size(1)
                # Comparing against this, not the file bytes, keeps a foreign
                # serialization from counting as a change on the first script.
                self._baseline = data_section(self.model.to_string())
            return self.model

    def _namespace(self, model, selection):
        ifcopenshell = self._ifcopenshell()
        try:
            import ifcopenshell.api as api
        except ImportError:
            api = None
        elements = self.resolve(model, selection)
        return {
            "__name__": "__tessifc_script__",
            "model": model,
            "ifcopenshell": ifcopenshell,
            "api": api,
            "element": ifcopenshell.util.element,
            "guid": ifcopenshell.guid,
            "selection": elements,
            "selected": elements[0] if elements else None,
        }

    @staticmethod
    def resolve(model, selection):
        """Entity instances for the viewer's selected GlobalIds or Express IDs."""
        found = []
        for value in (selection or {}).get("guids") or []:
            try:
                found.append(model.by_guid(str(value)))
            except (RuntimeError, KeyError, TypeError):
                pass
        if not found:
            for value in (selection or {}).get("ids") or []:
                try:
                    found.append(model.by_id(int(value)))
                except (RuntimeError, KeyError, TypeError, ValueError):
                    pass
        return found

    def run_script(self, source: str, selection=None, *, commit: bool = True, label: str = "script"):
        """Execute Python against the model. A failure or `commit=False` leaves the model unchanged."""
        if not isinstance(source, str):
            raise SessionError("The script must be a string.")
        if not self.model_lock.acquire(blocking=False):
            raise SessionBusy("A script is still running; wait for it to finish.")
        started = time.perf_counter()
        output = BoundedOutput()
        try:
            model = self._ensure_model()
            namespace = self._namespace(model, selection)
            try:
                code = compile(source, SCRIPT_NAME, "exec")
            except SyntaxError as error:
                return self._result(False, output, started, error=error, label=label)
            # Tracebacks quote the script's own lines.
            linecache.cache[SCRIPT_NAME] = (len(source), None, source.splitlines(True), SCRIPT_NAME)
            model.begin_transaction()
            try:
                with redirect_stdout(output), owner_scope(model):
                    exec(code, namespace)
            except Exception as error:
                operations = list(model.transaction.operations) if model.transaction else []
                model.discard_transaction()
                if operations:
                    self._reload_model()
                return self._result(False, output, started, error=error, label=label)
            operations = list(model.transaction.operations) if model.transaction else []
            if not commit:
                model.discard_transaction()
                if operations:
                    self._reload_model()
                return self._result(True, output, started, operations=operations, label=label, changed=False)
            model.end_transaction()
            model.history.clear()
            changed = self._publish(model)
            return self._result(True, output, started, operations=operations, label=label, changed=changed)
        finally:
            self.model_lock.release()

    def _reload_model(self):
        with self.lock:
            self.model = None

    def _result(self, ok, output, started, *, error=None, operations=(), label, changed=False):
        counts = {"created": 0, "modified": 0, "deleted": 0}
        for operation in operations:
            action = operation.get("action")
            key = {"create": "created", "edit": "modified", "delete": "deleted"}.get(action)
            if key:
                counts[key] += 1
        result = {
            "ok": ok,
            "label": label,
            "changed": bool(changed),
            "version": self.version,
            "revision": self.revision,
            "stdout": output.text(),
            "operations": counts,
            "elapsedMs": round((time.perf_counter() - started) * 1000, 1),
        }
        if error is not None:
            result["error"] = f"{type(error).__name__}: {error}"
            if isinstance(error, SyntaxError):
                lines = [f"line {error.lineno}: {(error.text or '').rstrip()}"]
            else:
                frames = traceback.extract_tb(error.__traceback__)
                lines = [f"line {frame.lineno}: {frame.line or ''}".rstrip() for frame in frames if frame.filename == SCRIPT_NAME]
            result["traceback"] = clip("\n".join(lines), 4000)
        return result

    def _publish(self, model) -> bool:
        """Serialize, and replace the file only when the DATA section differs."""
        text = model.to_string()
        data = data_section(text)
        if data == self._baseline:
            return False
        payload = text.encode("utf-8")
        fd, name = tempfile.mkstemp(prefix=".tessifc-", suffix=".ifc", dir=self.path.parent)
        temporary = Path(name)
        try:
            with os.fdopen(fd, "wb") as handle:
                handle.write(payload)
            with self.lock:
                previous = (self.version, self.payload)
                temporary.replace(self.path)
                self._adopt(payload, file_stamp(self.path), external=False)
                self._push(self.undo_stack, previous)
                self.redo_stack.clear()
                self.model = model
                self._baseline = data
                return True
        finally:
            temporary.unlink(missing_ok=True)

    def _restore(self, source: deque, target: deque, label: str):
        if not self.model_lock.acquire(blocking=False):
            raise SessionBusy("A script is still running; wait for it to finish.")
        started = time.perf_counter()
        try:
            with self.lock:
                self.snapshot()
                if not source:
                    raise SessionError(f"Nothing to {label}.")
                version, payload = source.pop()
                current = (self.version, self.payload)
                fd, name = tempfile.mkstemp(prefix=".tessifc-", suffix=".ifc", dir=self.path.parent)
                with os.fdopen(fd, "wb") as handle:
                    handle.write(payload)
                Path(name).replace(self.path)
                self._adopt(payload, file_stamp(self.path), external=False)
                self._push(target, current)
                return self._result(True, BoundedOutput(), started, label=label, changed=True)
        finally:
            self.model_lock.release()

    def undo(self):
        """Restore the previous content as a new version."""
        return self._restore(self.undo_stack, self.redo_stack, "undo")

    def redo(self):
        """Reapply the content that the last undo removed."""
        return self._restore(self.redo_stack, self.undo_stack, "redo")

    # ----------------------------------------------------------- reporting

    def describe(self, assistant=None) -> dict:
        with self.lock:
            authoring = self.authoring_available()
            version = None
            if authoring:
                version = getattr(self.ifcopenshell, "version", None)
            return {
                "name": self.path.name,
                "version": self.version,
                "revision": self.revision,
                "generation": self.generation,
                "busy": self.model_lock.locked(),
                "undo": len(self.undo_stack),
                "redo": len(self.redo_stack),
                "capabilities": {
                    "authoring": "python" if authoring else False,
                    "authoringError": None if authoring else self.authoring_error,
                    "ifcopenshell": version,
                    "assistant": assistant.describe() if assistant is not None else None,
                    "selection": True,
                    "applied": True,
                },
                "examples": [],
            }

    def switch_to(self, path):
        """Follow another file; the viewer sees a new generation and reopens."""
        with self.lock:
            self.path = Path(path).resolve(strict=True)
            self.signature = None
            self.version = None
            self.revision = 0
            self.payload = b""
            self.model = None
            self._baseline = None
            self._summary = None
            self.undo_stack.clear()
            self.redo_stack.clear()
            self.selection = None
            self.applied = None
            self.generation += 1
            self.snapshot()
            self.changed.notify_all()

    def report_selection(self, body: dict):
        """What the viewer has selected, as the page reported it."""
        with self.lock:
            ids = body.get("ids") or []
            guids = body.get("guids") or []
            self.selection = {"ids": ids, "guids": guids, "className": body.get("className"), "name": body.get("name"),
                              "reportedAt": time.time()} if (ids or guids) else None

    def report_applied(self, body: dict):
        """The viewer's report of the version it displayed and what the kernel rebuilt."""
        with self.lock:
            self.applied = {"version": body.get("version"), "revision": body.get("revision"),
                            "affectedProducts": body.get("affectedProducts") or [], "removedProducts": body.get("removedProducts") or [],
                            "fullRebuild": bool(body.get("fullRebuild")), "reportedAt": time.time()}
            self.changed.notify_all()

    def wait_for_applied(self, version: str, timeout: float):
        """Block until the viewer reports `version` applied, or the timeout passes; returns the report or None."""
        deadline = time.monotonic() + max(0.0, timeout)
        with self.lock:
            while True:
                if self.applied and self.applied.get("version") == version:
                    return dict(self.applied)
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return None
                self.changed.wait(min(remaining, 0.5))

    def summary(self) -> dict:
        """Schema, units, product counts and storeys, computed once per version."""
        with self.lock:
            if self._summary is not None:
                return self._summary
        with self.model_lock:
            model = self._ensure_model()
            counts = {}
            for product in model.by_type("IfcProduct"):
                counts[product.is_a()] = counts.get(product.is_a(), 0) + 1
            storeys = []
            for storey in model.by_type("IfcBuildingStorey"):
                storeys.append({"name": storey.Name, "elevation": storey.Elevation, "id": storey.id()})
            unit = None
            for assignment in model.by_type("IfcUnitAssignment"):
                for item in assignment.Units or ():
                    if getattr(item, "UnitType", None) == "LENGTHUNIT":
                        prefix = getattr(item, "Prefix", None) or ""
                        unit = f"{prefix}{getattr(item, 'Name', '')}".strip() or None
            summary = {
                "name": self.path.name,
                "schema": model.schema,
                "lengthUnit": unit,
                "products": dict(sorted(counts.items(), key=lambda item: (-item[1], item[0]))),
                "storeys": storeys,
                "revision": self.revision,
            }
            with self.lock:
                self._summary = summary
            return summary

    def describe_selection(self, selection) -> list:
        """Attributes, container and property sets of the selected elements, bounded for a prompt."""
        with self.model_lock:
            model = self._ensure_model()
            element = self.ifcopenshell.util.element
            described = []
            for entity in self.resolve(model, selection)[:4]:
                info = {"id": entity.id(), "class": entity.is_a()}
                for name in ("GlobalId", "Name", "Description", "ObjectType", "Tag", "PredefinedType"):
                    try:
                        value = getattr(entity, name)
                    except AttributeError:
                        continue
                    if value is not None:
                        info[name] = value
                try:
                    container = element.get_container(entity)
                    if container is not None:
                        info["container"] = f"{container.is_a()} #{container.id()} {container.Name or ''}".strip()
                except Exception:
                    pass
                try:
                    representation = getattr(entity, "Representation", None)
                    if representation is not None:
                        info["representations"] = [
                            f"#{shape.id()} {shape.RepresentationIdentifier or ''}/{shape.RepresentationType or ''}: "
                            + ", ".join(f"#{item.id()} {item.is_a()}" for item in (shape.Items or ())[:6])
                            for shape in (representation.Representations or ())[:6]
                        ]
                    placement = getattr(entity, "ObjectPlacement", None)
                    if placement is not None:
                        info["placement"] = f"#{placement.id()} {placement.is_a()}"
                except Exception:
                    pass
                try:
                    psets = element.get_psets(entity)
                    trimmed = {}
                    for pset_name, properties in list(psets.items())[:12]:
                        trimmed[pset_name] = {key: value for key, value in list(properties.items())[:20] if key != "id"}
                    if trimmed:
                        info["propertySets"] = trimmed
                except Exception:
                    pass
                described.append(info)
            return described
