// SPDX-License-Identifier: Apache-2.0

//! @tessifc/edit: the editing session over a TessIFC kernel, the browser
//! script engine, the IGP reader and provider-neutral agent tools.

export { createEditingSession } from "./session.js";
export { createModel, createModelText } from "./create-model.js";
export { API_REFERENCE, Enum, Int, Ref, Typed, createScriptEngine, decodeStepString, encodeStepString, formatValue, newGuid, parseValue, runScript, splitArguments } from "./script-engine.js";
export { DEFAULT_HIDDEN_INSTANCE_FLAGS, INSTANCE_OPENING, INSTANCE_REFERENCE, INSTANCE_SPACE, classLabelColor, defaultHiddenClassIds, humanizeIfcClass, readIgp } from "./igp.js";
export { SYSTEM_PROMPT, TOOLS, createAgentTools, normalizeHistory, runAgentTurn, toChatTools, toMessagesTools, toolsFor } from "./agent-tools.js";
export { ProviderError, anthropicMessages, chatCompletions, decodeChatReply, decodeMessagesReply, encodeChatRequest, encodeMessagesRequest } from "./providers.js";
export { describeModel, describeModelInfo, describeSelection, lengthUnitOf, storeysOf } from "./describe.js";
export { createSceneMirror, evaluateScene, normalizeScene, verifyRevision } from "./verify.js";
export { JAVASCRIPT_EXAMPLES } from "./examples.js";

/** @typedef {import("./types.js").Kernel} Kernel */
/** @typedef {import("./types.js").GeometrySettings} GeometrySettings */
/** @typedef {import("./types.js").Diagnostic} Diagnostic */
/** @typedef {import("./types.js").Pack} Pack */
/** @typedef {import("./types.js").PackIndex} PackIndex */
/** @typedef {import("./types.js").PackGeometry} PackGeometry */
/** @typedef {import("./types.js").PackInstances} PackInstances */
/** @typedef {import("./types.js").Selection} Selection */
/** @typedef {import("./types.js").ScriptReport} ScriptReport */
/** @typedef {import("./types.js").RevisionImpact} RevisionImpact */
/** @typedef {import("./types.js").Delta} Delta */
/** @typedef {import("./types.js").SessionError} SessionError */
/** @typedef {import("./types.js").ScriptRunner} ScriptRunner */
/** @typedef {import("./session.js").Session} Session */
/** @typedef {import("./script-engine.js").ScriptEngine} ScriptEngine */
/** @typedef {import("./agent-tools.js").AgentTools} AgentTools */
/** @typedef {import("./agent-tools.js").ScriptHost} ScriptHost */
/** @typedef {import("./verify.js").SceneMirror} SceneMirror */
/** @typedef {import("./create-model.js").ModelOptions} ModelOptions */
