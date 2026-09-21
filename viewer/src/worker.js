// SPDX-License-Identifier: Apache-2.0

//! The geometry worker: parses, tessellates and packs off the main thread.
//! Its body lives in the viewer package; this entry names the checkout's kernel.

import * as glue from "../../bindings/wasm/pkg/tessifc_wasm.js";
import { createEditingSession } from "../../bindings/edit/src/session.js";
import { createScriptEngine, runScript } from "../../bindings/edit/src/script-engine.js";
import { lengthUnitOf, storeysOf } from "../../bindings/edit/src/describe.js";
import { startKernelWorker } from "../../bindings/viewer/src/kernel-worker-core.js";

startKernelWorker({ glue, edit: { createEditingSession, createScriptEngine, runScript, storeysOf, lengthUnitOf } });
