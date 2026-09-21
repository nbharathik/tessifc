# SPDX-License-Identifier: Apache-2.0
"""The TessIFC geometry kernel for Python.

One :class:`Kernel` holds any number of open models. Reports come back as
dictionaries, geometry as ``bytes`` in the IGP container, whole or as
streamed chunks; :func:`tessifc.igp.read_igp` reads it without copying.

    import tessifc

    kernel = tessifc.Kernel()
    model = kernel.open_model(open("model.ifc", "rb").read())
    summary = kernel.evaluate_geometry(model)
    pack = tessifc.igp.read_igp(kernel.take_pack(model))
    kernel.close_model(model)

The kernel is not thread-safe: use one per thread. Evaluation releases the
interpreter lock while it runs, so other threads keep going.
"""

from __future__ import annotations

import json
from typing import Any, Optional

from . import igp
from ._core import Kernel as _Kernel
from ._core import KernelError, version

__all__ = ["Kernel", "KernelError", "igp", "version", "__version__"]
__version__ = version()


def _json(text: Optional[str]) -> Any:
    return None if text is None else json.loads(text)


def _dump(value: Any) -> Optional[str]:
    if value is None:
        return None
    if isinstance(value, str):
        return value
    return json.dumps(value)


class Kernel:
    """A TessIFC instance holding open models, each named by an integer id.

    Every ``get_*`` report returns a dictionary, or ``None`` for an id the
    kernel does not hold. Settings are dictionaries with the same fields the
    other bindings take, or the JSON text of one.
    """

    def __init__(self) -> None:
        self._core = _Kernel()

    def open_model(self, data: bytes, options: Optional[dict[str, Any] | str] = None) -> int:
        """Parse an IFC or IFCZIP file and keep it open; returns the model id.

        An unreadable file opens with zero entities and diagnostics rather
        than raising. ``options`` may set ``schemaOverride``, ``maxEntities``
        and ``maxIfczipBytes``; an invalid value raises :class:`KernelError`.
        """
        return self._core.open_model(bytes(data), _dump(options))

    def get_model_info(self, model_id: int) -> Optional[dict[str, Any]]:
        """Schema, entity and product counts per class, the header and diagnostic counts."""
        return _json(self._core.get_model_info(model_id))

    def get_diagnostics(self, model_id: int) -> Optional[list[dict[str, Any]]]:
        """Parse diagnostics; geometry diagnostics travel in the packs."""
        return _json(self._core.get_diagnostics(model_id))

    def get_class_name(self, model_id: int, express_id: int) -> Optional[str]:
        """The IFC class of one instance."""
        return self._core.get_class_name(model_id, express_id)

    def get_product_category(self, model_id: int, express_id: int) -> Optional[str]:
        """``physical``, ``space``, ``opening``, ``annotation`` or ``reference``; ``None`` for a non-product."""
        return self._core.get_product_category(model_id, express_id)

    def get_ids_of_type(self, model_id: int, class_name: str) -> list[int]:
        """Express ids of every instance of a class or its subtypes."""
        return list(self._core.get_ids_of_type(model_id, class_name))

    def get_entity_info(self, model_id: int, express_id: int) -> Optional[dict[str, Any]]:
        """The entity's attributes: name, type, the exact STEP spelling and the decoded value."""
        return _json(self._core.get_entity_info(model_id, express_id))

    def get_spatial_hierarchy(self, model_id: int) -> Optional[dict[str, Any]]:
        """The building tree as a flat node list with ``parentExpressId`` links."""
        return _json(self._core.get_spatial_hierarchy(model_id))

    def get_class_attributes(self, model_id: int, class_name: str) -> Optional[dict[str, Any]]:
        """The schema definition of a class in the model's schema."""
        return _json(self._core.get_class_attributes(model_id, class_name))

    def get_class_supertypes(self, model_id: int, class_name: str) -> Optional[list[str]]:
        """The class and its supertypes up to the root."""
        return _json(self._core.get_class_supertypes(model_id, class_name))

    def get_geometry_capabilities(self, model_id: int) -> Optional[dict[str, Any]]:
        """Every schema entity and the evaluator route it dispatches to."""
        return _json(self._core.get_geometry_capabilities(model_id))

    def evaluate_geometry(self, model_id: int, settings: Optional[dict[str, Any] | str] = None) -> Optional[dict[str, Any]]:
        """Tessellate every product across every core; returns the summary.

        The geometry stays in the kernel for :meth:`take_pack`. Invalid
        settings raise :class:`KernelError`.
        """
        return _json(self._core.evaluate_geometry(model_id, _dump(settings)))

    def get_product_outcomes(self, model_id: int) -> Optional[list[dict[str, Any]]]:
        """What happened to each product: emitted, empty or failed, skipped, filtered."""
        return _json(self._core.get_product_outcomes(model_id))

    def get_pack(self, model_id: int) -> Optional[bytes]:
        """The last evaluation as one IGP pack, the geometry kept in the kernel."""
        return self._core.get_pack(model_id)

    def take_pack(self, model_id: int) -> Optional[bytes]:
        """The last evaluation as one IGP pack, releasing the geometry."""
        return self._core.take_pack(model_id)

    def begin_geometry_stream(self, model_id: int, settings: Optional[dict[str, Any] | str] = None) -> Optional[dict[str, Any]]:
        """Start streaming; follow with :meth:`next_geometry_chunk` until it returns ``None``."""
        return _json(self._core.begin_geometry_stream(model_id, _dump(settings)))

    def next_geometry_chunk(self, model_id: int, budget_ms: float = 0.0, max_products: int = 0, max_triangles: int = 0) -> Optional[bytes]:
        """The next IGP chunk, or ``None`` once the stream is finished.

        The chunk stops after ``budget_ms``, ``max_products`` or
        ``max_triangles`` (zero is no limit), holding at least one product.
        """
        return self._core.next_geometry_chunk(model_id, float(budget_ms), int(max_products), int(max_triangles))

    def stream_progress(self, model_id: int) -> Optional[dict[str, Any]]:
        """How far the stream has come."""
        return _json(self._core.stream_progress(model_id))

    def cancel_geometry_stream(self, model_id: int) -> bool:
        """Stop and release a stream while keeping the model open."""
        return self._core.cancel_geometry_stream(model_id)

    def release_geometry(self, model_id: int) -> bool:
        """Drop the evaluated geometry while keeping the model open."""
        return self._core.release_geometry(model_id)

    def close_model(self, model_id: int) -> bool:
        """Close a model; ``False`` when the id was not open."""
        return self._core.close_model(model_id)

    def close_all(self) -> None:
        """Close every open model."""
        self._core.close_all()

    def model_count(self) -> int:
        """How many models are open."""
        return self._core.model_count()

    def __enter__(self) -> "Kernel":
        return self

    def __exit__(self, *_: object) -> None:
        self.close_all()
