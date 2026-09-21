// SPDX-License-Identifier: Apache-2.0

//! The texture cache and its policy live in the viewer package; this shim keeps
//! the app importing from its own tree.

export * from "../../bindings/viewer/src/textures.js";
