# SPDX-License-Identifier: Apache-2.0
"""A reader for IGP v0, the container the kernel writes geometry in.

Everything is a view over the pack's own bytes: positions, indices and the
instance columns are ``memoryview`` objects cast to their element type, so
reading a pack copies nothing. :func:`to_numpy` turns a view into an array
when NumPy is installed; the reader itself needs nothing beyond the
standard library.
"""

from __future__ import annotations

import json
import struct
from dataclasses import dataclass, field
from typing import Any, Optional

IGP_MAGIC = 0x00504749
IGP_VERSION = 0
HEADER_BYTES = 24
INSTANCE_OPENING = 1 << 1
INSTANCE_SPACE = 1 << 2
INSTANCE_REFERENCE = 1 << 4
NO_MATERIAL = 0xFFFFFFFF


class PackError(ValueError):
    """The bytes are not a pack this reader understands."""


@dataclass
class Geometry:
    """One mesh: positions three per vertex, indices three per triangle."""

    id: int
    primitive: str
    bbox: list[float]
    closed: Optional[bool]
    positions: memoryview
    indices: memoryview
    uv: Optional[memoryview] = None

    @property
    def vertex_count(self) -> int:
        return len(self.positions) // 3

    @property
    def triangle_count(self) -> int:
        return len(self.indices) // 3


@dataclass
class Instances:
    """The placed instances, one column per attribute, ``count`` rows."""

    count: int
    geometry_ids: memoryview
    express_ids: memoryview
    class_ids: memoryview
    transforms: memoryview
    colors: memoryview
    flags: memoryview
    provenance: Optional[memoryview] = None
    material: Optional[memoryview] = None

    def transform(self, record: int) -> list[float]:
        """The column-major 4x4 matrix of one record."""
        return list(self.transforms[record * 16 : record * 16 + 16])

    def color(self, record: int) -> tuple[int, int, int, int]:
        """RGBA 0..255 of one record."""
        r, g, b, a = self.colors[record * 4 : record * 4 + 4]
        return (r, g, b, a)


@dataclass
class Texture:
    """One entry of the optional ``textures`` table, its bytes viewed when embedded."""

    id: int
    mime: Optional[str]
    repeat: list[bool]
    transform: Optional[list[float]]
    uri: Optional[str] = None
    blob: Optional[memoryview] = None
    pixels: Optional[dict[str, Any]] = None
    omitted: bool = False


@dataclass
class Pack:
    """A parsed pack: the JSON index and views over the binary chunk."""

    index: dict[str, Any]
    geometry: list[Geometry]
    instances: Instances
    flags: int
    stream: dict[str, Any]
    materials: list[dict[str, Any]] = field(default_factory=list)
    textures: list[Texture] = field(default_factory=list)

    @property
    def classes(self) -> list[str]:
        return list(self.index.get("classes", []))

    def class_of(self, record: int) -> str:
        """The IFC class of one instance."""
        return self.classes[self.instances.class_ids[record]]

    def geometry_by_id(self, geometry_id: int) -> Optional[Geometry]:
        for mesh in self.geometry:
            if mesh.id == geometry_id:
                return mesh
        return None


def read_igp(data: bytes | bytearray | memoryview) -> Pack:
    """Parse a pack; the returned views keep ``data`` alive."""
    buffer = memoryview(data).cast("B")
    if len(buffer) < HEADER_BYTES:
        raise PackError("the geometry pack is shorter than its 24-byte header")
    magic, version, json_length = struct.unpack_from("<III", buffer, 0)
    (binary_length,) = struct.unpack_from("<Q", buffer, 12)
    (flags,) = struct.unpack_from("<I", buffer, 20)
    if magic != IGP_MAGIC:
        raise PackError("the bytes are not IGP geometry")
    if version != IGP_VERSION:
        raise PackError(f"IGP version {version} is not supported")
    binary_start = HEADER_BYTES + ((json_length + 7) // 8) * 8
    if binary_start > len(buffer) or binary_length > len(buffer) - binary_start:
        raise PackError("the geometry pack is truncated")
    try:
        index = json.loads(bytes(buffer[HEADER_BYTES : HEADER_BYTES + json_length]).decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise PackError(f"the IGP index is not valid JSON: {error}") from error
    if (
        index.get("igp") != IGP_VERSION
        or not isinstance(index.get("geometries"), list)
        or not isinstance(index.get("instances"), dict)
        or not isinstance(index.get("classes"), list)
    ):
        raise PackError("the geometry pack index is missing required IGP v0 fields")
    binary = buffer[binary_start : binary_start + binary_length]

    def view(section: Any, fmt: str, count: int) -> memoryview:
        if not isinstance(section, dict) or not _count(section.get("off")) or not _count(count):
            raise PackError("an IGP section is invalid")
        size = struct.calcsize(fmt) * count
        off = section["off"]
        if off + size > len(binary):
            raise PackError("an IGP section reaches past the binary chunk")
        return binary[off : off + size].cast(fmt)

    position_format = "d" if flags & 2 else "f"
    geometry = []
    for entry in index["geometries"]:
        if not isinstance(entry, dict) or "positions" not in entry or "indices" not in entry:
            raise PackError("the geometry pack index is missing required IGP v0 fields")
        positions = entry["positions"]
        indices = entry["indices"]
        if not _count(positions.get("count")) or not _count(indices.get("count")):
            raise PackError("an IGP geometry entry declares an invalid element count")
        index_format = "I" if indices.get("type") == "u32" else "H"
        uv = None
        if isinstance(entry.get("uv"), dict) and entry["uv"].get("count") == positions["count"]:
            uv = view(entry["uv"], "f", positions["count"] * 2)
        geometry.append(
            Geometry(
                id=entry.get("id"),
                primitive=entry.get("primitive", "triangles"),
                bbox=list(entry.get("bbox", [])),
                closed=entry["closed"] if isinstance(entry.get("closed"), bool) else None,
                positions=view(positions, position_format, positions["count"] * 3),
                indices=view(indices, index_format, indices["count"]),
                uv=uv,
            )
        )

    columns = index["instances"]
    count = columns.get("count")
    if not _count(count):
        raise PackError("the IGP instance count is invalid")
    instances = Instances(
        count=count,
        geometry_ids=view(columns.get("geometry_id"), "I", count),
        express_ids=view(columns.get("express_id"), "I", count),
        class_ids=view(columns.get("class_id"), "H", count),
        transforms=view(columns.get("transform"), "f", count * 16),
        colors=view(columns.get("color"), "B", count * 4),
        flags=view(columns.get("flags"), "H", count),
        provenance=view(columns["provenance"], "I", count) if "provenance" in columns else None,
        material=view(columns["material"], "I", count) if "material" in columns else None,
    )

    stream = index.get("stream")
    if stream is None:
        stream = {"chunk": 0, "final": True, "products_done": count, "products_total": count}
    elif not isinstance(stream, dict) or not isinstance(stream.get("final"), bool):
        raise PackError("the IGP stream position is invalid")

    textures = []
    for texture in index.get("textures", []) or []:
        if not isinstance(texture, dict) or not _count(texture.get("id")):
            raise PackError("an IGP texture entry is invalid")
        record = Texture(
            id=texture["id"],
            mime=texture.get("mime") if isinstance(texture.get("mime"), str) else None,
            repeat=[bool(value) for value in texture.get("repeat", [True, True])],
            transform=list(texture["transform"]) if isinstance(texture.get("transform"), list) else None,
        )
        if isinstance(texture.get("uri"), str):
            record.uri = texture["uri"]
        elif isinstance(texture.get("blob"), dict):
            record.blob = view(texture["blob"], "B", texture["blob"].get("len", -1))
        elif isinstance(texture.get("pixels"), dict):
            pixels = texture["pixels"]
            record.pixels = {
                "width": pixels.get("width"),
                "height": pixels.get("height"),
                "components": pixels.get("components"),
                "bytes": view(pixels, "B", pixels.get("len", -1)),
            }
        else:
            record.omitted = True
        textures.append(record)

    return Pack(
        index=index,
        geometry=geometry,
        instances=instances,
        flags=flags,
        stream=stream,
        materials=list(index.get("materials", []) or []),
        textures=textures,
    )


def to_numpy(view: memoryview, columns: int = 1):
    """A NumPy array over a view, reshaped to ``columns`` per row; needs NumPy."""
    import numpy  # noqa: PLC0415  (optional dependency)

    array = numpy.asarray(view)
    return array.reshape(-1, columns) if columns > 1 else array


def _count(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0
