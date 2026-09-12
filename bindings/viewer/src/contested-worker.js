// SPDX-License-Identifier: Apache-2.0

//! A worker that runs the coincident-plane analysis for `createViewer`, so a
//! large model never stalls the page while its overlay is refined.

import { findContestedTriangles } from "./depth-planes.js";

self.addEventListener("message", ({ data }) => {
  const { requestId, instances, geometries } = data;
  try {
    const byId = new Map(geometries.map((geometry) => [geometry.id, geometry]));
    const result = findContestedTriangles({ instances }, byId);
    self.postMessage(
      { requestId, records: result.records, offsets: result.offsets, triangles: result.triangles, pairs: result.pairs },
      [result.records.buffer, result.offsets.buffer, result.triangles.buffer],
    );
  } catch (error) {
    self.postMessage({ requestId, error: String(error?.message ?? error) });
  }
});
