// SPDX-License-Identifier: Apache-2.0
//! Camera navigation in CSS pixels and render-local metres.

const clamp = (value, min, max) => Math.max(min, Math.min(max, value));
const dot = (a, b) => a.reduce((sum, value, i) => sum + value * b[i], 0);

/** Normalize pixel, line and page wheel deltas, including fine trackpad input. */
export function wheelZoomFactor(delta, mode = 0, height = 800) {
  if (!Number.isFinite(delta)) return 1;
  const pixels = delta * (mode === 1 ? 16 : mode === 2 ? Math.max(height, 1) : 1);
  return Math.exp(clamp(pixels, -500, 500) * 0.0012);
}

/** A ray's intersection with the view plane through the current orbit target. */
export function targetPlaneAnchor(camera, ray) {
  const forward = camera.target.map((value, i) => value - camera.position[i]);
  const denominator = dot(ray.direction, forward);
  if (!Number.isFinite(denominator) || Math.abs(denominator) < 1e-12) return camera.target.slice();
  const distance = dot(camera.target.map((value, i) => value - ray.origin[i]), forward) / denominator;
  if (!Number.isFinite(distance) || distance < 0) return camera.target.slice();
  return ray.origin.map((value, i) => value + ray.direction[i] * distance);
}

/** Zoom around an anchor while preserving its screen position and the view direction. */
export function zoomCamera(camera, factor, anchor = camera.target) {
  if (!(factor > 0) || !Number.isFinite(factor) || !anchor.every(Number.isFinite)) return false;
  const distance = Math.hypot(...camera.position.map((value, i) => value - camera.target[i]));
  if (!(distance > 0)) return false;
  const perspective = camera.mode === "perspective";
  const current = perspective ? distance : camera.orthoScale;
  if (!(current > 0)) return false;
  const next = clamp(current * factor, perspective ? 0.002 : 0.001, 1e8);
  const ratio = next / current;
  if (ratio === 1) return false;
  if (perspective) {
    camera.position = camera.position.map((value, i) => anchor[i] + (value - anchor[i]) * ratio);
    camera.target = camera.target.map((value, i) => anchor[i] + (value - anchor[i]) * ratio);
    camera.distance = next;
  } else {
    // An orthographic zoom translates only within the view plane.
    const direction = camera.target.map((value, i) => (value - camera.position[i]) / distance);
    const offset = anchor.map((value, i) => value - camera.target[i]);
    const depth = dot(offset, direction);
    const move = offset.map((value, i) => (value - direction[i] * depth) * (1 - ratio));
    camera.position = camera.position.map((value, i) => value + move[i]);
    camera.target = camera.target.map((value, i) => value + move[i]);
    camera.orthoScale = next;
    camera.distance = distance;
  }
  return true;
}

/** Fit a sphere against both viewport dimensions, including portrait windows. */
export function frameSphere(radius, fov, aspect, margin = 1.18) {
  radius = Math.max(radius, 0.002);
  aspect = Math.max(aspect, 0.01);
  const halfFov = Math.atan(Math.tan(fov / 2) * Math.min(aspect, 1));
  return { distance: radius / Math.sin(halfFov) * margin, orthoScale: radius / Math.min(aspect, 1) * margin };
}
