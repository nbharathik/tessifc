// SPDX-License-Identifier: Apache-2.0

//! @tessifc/mcp: an MCP server over the TessIFC kernel, the model host behind
//! it, and the loopback viewer server that lets the reference viewer follow.

export { BUILD_PROMPT, createTessifcServer, runRecord } from "./server.js";
export { GEOMETRY_SETTINGS, SessionBusy, createModelHost } from "./session-host.js";
export { createViewerServer } from "./viewer-server.js";

/** @typedef {import("./session-host.js").ModelHost} ModelHost */
/** @typedef {import("./viewer-server.js").ViewerServer} ViewerServer */
