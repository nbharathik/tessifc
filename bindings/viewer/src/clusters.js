// SPDX-License-Identifier: Apache-2.0

//! Clusters: consecutive runs of a batch's records, in its draw order, that
//! the renderer can leave out of a frame as a unit. A baked cluster is an
//! index range of the batch; an instanced cluster is a run of instances.

/** Vertices a cluster gathers before the next one starts. */
export const CLUSTER_VERTEX_TARGET = 24_576;
/** Clusters one batch may hold; past it the target doubles until it fits. */
export const MAX_CLUSTERS = 8192;

/**
 * @typedef {object} Cluster
 * @property {number} first The first item of the run.
 * @property {number} count Items in the run.
 * @property {number} indexOffset Indices before the run, for a baked batch.
 * @property {number} indexCount Indices in the run, for a baked batch.
 * @property {number} vertices Vertices in the run.
 * @property {number[]} center The run's bounds centre, in render space.
 * @property {number[]} halfExtents Half the run's extent per axis.
 * @property {boolean} bounded False when no item had usable bounds; such a cluster is never culled.
 * @property {WebGLVertexArrayObject | null} [vao] An instanced cluster's own vertex array; the first cluster uses the batch's.
 * @property {number} [overlayOffset] Overlay indices before the run, for a baked batch.
 * @property {number} [overlayCount] Overlay indices in the run.
 */

/**
 * Split items in draw order into clusters of about `vertexTarget` vertices.
 * `vertexOf` and `indexOf` give an item's vertex and index counts, `boundsOf`
 * its render-space bounds (`{ min, max }`) or null.
 * @template T
 * @param {T[]} items
 * @param {(item: T) => number} vertexOf
 * @param {(item: T) => number} indexOf
 * @param {(item: T) => ({ min: number[], max: number[] } | null | undefined)} boundsOf
 * @param {number} [vertexTarget]
 * @param {number} [maxClusters]
 * @returns {Cluster[]}
 */
export function planClusters(items, vertexOf, indexOf, boundsOf, vertexTarget = CLUSTER_VERTEX_TARGET, maxClusters = MAX_CLUSTERS) {
  let target = Math.max(1, vertexTarget);
  let clusters = split(items, vertexOf, indexOf, target);
  // A model of tiny items would otherwise make more clusters than the state arrays budget.
  while (clusters.length > maxClusters) {
    target *= 2;
    clusters = split(items, vertexOf, indexOf, target);
  }
  for (const cluster of clusters) {
    const min = [Infinity, Infinity, Infinity];
    const max = [-Infinity, -Infinity, -Infinity];
    let bounded = false;
    for (let at = cluster.first; at < cluster.first + cluster.count; at += 1) {
      const bounds = boundsOf(items[at]);
      if (!bounds || !Number.isFinite(bounds.min[0]) || !Number.isFinite(bounds.max[0])) continue;
      bounded = true;
      for (let axis = 0; axis < 3; axis += 1) {
        if (bounds.min[axis] < min[axis]) min[axis] = bounds.min[axis];
        if (bounds.max[axis] > max[axis]) max[axis] = bounds.max[axis];
      }
    }
    cluster.bounded = bounded;
    cluster.center = bounded ? [(min[0] + max[0]) / 2, (min[1] + max[1]) / 2, (min[2] + max[2]) / 2] : [0, 0, 0];
    cluster.halfExtents = bounded ? [(max[0] - min[0]) / 2, (max[1] - min[1]) / 2, (max[2] - min[2]) / 2] : [0, 0, 0];
  }
  return clusters;
}

function split(items, vertexOf, indexOf, target) {
  const clusters = [];
  let first = 0;
  let vertices = 0;
  let indexOffset = 0;
  let indexCount = 0;
  for (let at = 0; at < items.length; at += 1) {
    const itemVertices = vertexOf(items[at]);
    const itemIndices = indexOf(items[at]);
    if (at > first && vertices + itemVertices > target) {
      clusters.push({ first, count: at - first, indexOffset, indexCount, vertices, center: null, halfExtents: null, bounded: false, vao: null });
      first = at;
      indexOffset += indexCount;
      vertices = 0;
      indexCount = 0;
    }
    vertices += itemVertices;
    indexCount += itemIndices;
  }
  if (items.length > first) {
    clusters.push({ first, count: items.length - first, indexOffset, indexCount, vertices, center: null, halfExtents: null, bounded: false, vao: null });
  }
  return clusters;
}

/**
 * The runs of consecutive clusters whose state byte is zero, as
 * `[firstCluster, clusterCount]` pairs; what a frame draws.
 * @param {Uint8Array} state
 * @returns {number[][]}
 */
export function visibleRuns(state) {
  const runs = [];
  let start = -1;
  for (let at = 0; at < state.length; at += 1) {
    if (state[at] === 0) {
      if (start < 0) start = at;
    } else if (start >= 0) {
      runs.push([start, at - start]);
      start = -1;
    }
  }
  if (start >= 0) runs.push([start, state.length - start]);
  return runs;
}
