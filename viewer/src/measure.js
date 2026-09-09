// SPDX-License-Identifier: Apache-2.0

//! Measurement maths: snapping a picked surface point to the corner or edge
//! under the pointer, the numbers a measurement reports, and the text it
//! copies. No DOM and no WebGL, so every rule here has a unit test.

import { coordinate, distance as formatDistance } from "./format.js";

/** Snap radius in CSS pixels. Wide enough for a trackpad, narrow enough to trust. */
export const SNAP_PIXELS = 12;

/** CSS-pixel position of a pack-space point, and whether it is in front of the camera. */
export function projectPoint(viewProjection, renderOrigin, point, width, height) {
  const x = point[0] - renderOrigin[0];
  const y = point[1] - renderOrigin[1];
  const z = point[2] - renderOrigin[2];
  const m = viewProjection;
  const clipX = m[0] * x + m[4] * y + m[8] * z + m[12];
  const clipY = m[1] * x + m[5] * y + m[9] * z + m[13];
  const clipW = m[3] * x + m[7] * y + m[11] * z + m[15];
  if (!(clipW > 1e-9)) return { x: 0, y: 0, w: 0, visible: false };
  return {
    x: ((clipX / clipW + 1) / 2) * width,
    y: ((1 - clipY / clipW) / 2) * height,
    w: clipW,
    visible: true,
  };
}

/** The 3D parameter that lands where screen parameter `t` does; the perspective divide is not linear. */
function edgeParameter(t, nearW, farW) {
  if (!Number.isFinite(nearW) || !Number.isFinite(farW)) return t;
  const denominator = farW + t * (nearW - farW);
  return Math.abs(denominator) > 1e-12 ? (t * nearW) / denominator : t;
}

/** Snap a hit to a triangle corner within `snapPixels`, else the nearest edge, else the surface point. */
export function snapToTriangle(hit, pointer, project, snapPixels = SNAP_PIXELS) {
  if (!hit?.point) return null;
  const triangle = hit.triangle;
  if (!Array.isArray(triangle) || triangle.length !== 3) return { point: hit.point, kind: "face" };

  const corners = triangle.map((corner) => ({ corner, screen: project(corner) }));
  let best = null;
  for (const { corner, screen } of corners) {
    if (!screen?.visible) continue;
    const gap = Math.hypot(screen.x - pointer.x, screen.y - pointer.y);
    if (gap <= snapPixels && (!best || gap < best.gap)) best = { point: corner.slice(), kind: "vertex", gap };
  }
  if (best) return best;

  for (let index = 0; index < 3; index += 1) {
    const start = corners[index];
    const end = corners[(index + 1) % 3];
    if (!start.screen?.visible || !end.screen?.visible) continue;
    const dx = end.screen.x - start.screen.x;
    const dy = end.screen.y - start.screen.y;
    const lengthSquared = dx * dx + dy * dy;
    if (lengthSquared < 1e-6) continue;
    const t = clamp(
      ((pointer.x - start.screen.x) * dx + (pointer.y - start.screen.y) * dy) / lengthSquared,
      0,
      1,
    );
    const footX = start.screen.x + dx * t;
    const footY = start.screen.y + dy * t;
    const gap = Math.hypot(footX - pointer.x, footY - pointer.y);
    if (gap <= snapPixels && (!best || gap < best.gap)) {
      const along = edgeParameter(t, start.screen.w, end.screen.w);
      const point = start.corner.map((value, axis) => value + (end.corner[axis] - value) * along);
      best = { point, kind: "edge", gap };
    }
  }
  return best ?? { point: hit.point, kind: "face", gap: 0 };
}

/** What two points measure: straight length, axis deltas and the plan length. */
export function measurementBetween(a, b) {
  const delta = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
  return {
    a: a.slice(),
    b: b.slice(),
    delta,
    length: Math.hypot(delta[0], delta[1], delta[2]),
    horizontal: Math.hypot(delta[0], delta[1]),
    vertical: Math.abs(delta[2]),
  };
}

/** The one-line readout of a measurement. */
export function measurementLabel(measurement) {
  return formatDistance(measurement.length);
}

/** The second line: axis deltas, each as an absolute length. */
export function measurementDetail(measurement) {
  const [dx, dy, dz] = measurement.delta.map((value) => formatDistance(Math.abs(value)));
  return `dX ${dx}  dY ${dy}  dZ ${dz}`;
}

/** Measurements as tab-separated text in IFC coordinates; `offset` is the pack's model offset. */
export function measurementText(measurements, offset = [0, 0, 0]) {
  const lines = ["#\tlength\tdX\tdY\tdZ\tplan\tfrom\tto"];
  measurements.forEach((measurement, index) => {
    const from = measurement.a.map((value, axis) => coordinate(value + offset[axis])).join(" ");
    const to = measurement.b.map((value, axis) => coordinate(value + offset[axis])).join(" ");
    lines.push(
      [
        index + 1,
        formatDistance(measurement.length),
        ...measurement.delta.map((value) => formatDistance(Math.abs(value))),
        formatDistance(measurement.horizontal),
        from,
        to,
      ].join("\t"),
    );
  });
  return lines.join("\n");
}

function clamp(value, minimum, maximum) {
  return Math.max(minimum, Math.min(maximum, value));
}
