// SPDX-License-Identifier: Apache-2.0

/** Lateral clip planes with a pixel margin; depth clipping stays with the GPU. */
export function viewSidePlanes(matrix, width, height, out = new Float64Array(16)) {
  for (let plane = 0; plane < 4; plane += 1) {
    const axis = plane >> 1;
    const sign = plane % 2 ? -1 : 1;
    const margin = 1 + 2 / Math.max(axis ? height : width, 1);
    for (let column = 0; column < 4; column += 1) {
      out[plane * 4 + column] = margin * matrix[column * 4 + 3] + sign * matrix[column * 4 + axis];
    }
  }
  return out;
}

/** Keep boundary and uncertain boxes so culling cannot remove visible fragments. */
export function boxInView(center, half, planes) {
  if (!center || !half) return true;
  for (let at = 0; at < planes.length; at += 4) {
    const x = planes[at], y = planes[at + 1], z = planes[at + 2];
    const distance = x * center[0] + y * center[1] + z * center[2] + planes[at + 3];
    const support = Math.abs(x) * half[0] + Math.abs(y) * half[1] + Math.abs(z) * half[2];
    const error = 1e-6 * (Math.abs(x * center[0]) + Math.abs(y * center[1]) + Math.abs(z * center[2]) + Math.abs(planes[at + 3]) + support + 1);
    if (distance + support < -error) return false;
  }
  return true;
}
