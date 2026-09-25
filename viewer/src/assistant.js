// SPDX-License-Identifier: Apache-2.0

//! The browser assistant: Ask and Edit over the open model with a provider
//! called directly from the page, through the package's tool loop. Tools run
//! browser scripts in the worker.

import { createAgentTools, runAgentTurn } from "../../bindings/edit/src/agent-tools.js";
import { anthropicMessages, chatCompletions } from "../../bindings/edit/src/providers.js";

const STORAGE_KEY = "tessifc.assistant";
// Keys live for the tab unless the user asks to remember them on this device.
const TAB_KEYS = "tessifc.assistant.keys";
const MAX_HISTORY = 20;

export const PROVIDERS = {
  off: { label: "Off" },
  openrouter: { label: "OpenRouter", baseUrl: "https://openrouter.ai/api/v1", model: "anthropic/claude-opus-5", key: true },
  anthropic: { label: "Anthropic", baseUrl: "https://api.anthropic.com", model: "claude-opus-5", key: true },
  compatible: { label: "OpenAI-compatible URL", baseUrl: "http://127.0.0.1:11434/v1", model: "", key: false },
};

/**
 * Where a key is kept: the provider and the origin it is sent to, so a key
 * never follows a switch of provider or URL.
 * @param {string} provider
 * @param {string} baseUrl
 */
export function keySlot(provider, baseUrl) {
  let origin = "";
  try {
    origin = new URL(baseUrl).origin;
  } catch {
    // An unfinished URL has no origin yet.
  }
  return origin ? `${provider} ${origin}` : provider;
}

function readKeys(value) {
  const keys = {};
  if (value && typeof value === "object") {
    for (const [slot, key] of Object.entries(value)) if (typeof key === "string" && key) keys[slot] = key;
  }
  return keys;
}

function readJson(storage, name) {
  try {
    return JSON.parse(globalThis[storage]?.getItem(name) ?? "null");
  } catch {
    return null;
  }
}

function writeJson(storage, name, value) {
  try {
    if (value === null) globalThis[storage]?.removeItem(name);
    else globalThis[storage]?.setItem(name, JSON.stringify(value));
  } catch {
    // A blocked storage backend must not break the interface.
  }
}

/**
 * Provider settings from the last visit and the keys by `keySlot`: this
 * tab's, and the device's when the user chose to remember them. A blocked
 * store means the assistant is off.
 */
export function loadAssistantSettings() {
  const settings = { provider: "off", model: "", baseUrl: "", remember: false, keys: {} };
  const stored = readJson("localStorage", STORAGE_KEY);
  for (const name of ["provider", "model", "baseUrl"]) if (typeof stored?.[name] === "string") settings[name] = stored[name];
  if (!PROVIDERS[settings.provider]) settings.provider = "off";
  // A key an earlier version stored was already kept on this device, so it stays remembered.
  const legacy = typeof stored?.key === "string" && stored.key ? stored.key : "";
  settings.remember = stored?.remember === true || Boolean(legacy);
  const remembered = settings.remember ? readKeys(stored?.keys) : {};
  if (legacy) remembered[keySlot(settings.provider, settings.baseUrl)] ??= legacy;
  settings.keys = { ...remembered, ...readKeys(readJson("sessionStorage", TAB_KEYS)) };
  return settings;
}

/** Store the settings: keys in the tab's storage, and on the device only when `remember` is set. */
export function saveAssistantSettings({ provider, model, baseUrl, remember = false, keys = {} }) {
  const kept = readKeys(keys);
  writeJson("localStorage", STORAGE_KEY, remember ? { provider, model, baseUrl, remember: true, keys: kept } : { provider, model, baseUrl });
  writeJson("sessionStorage", TAB_KEYS, Object.keys(kept).length ? kept : null);
}

/** The panel's chat log as the loop's history: alternating user and assistant text, newest last. */
function history(items) {
  const messages = [];
  for (const item of (items ?? []).slice(-MAX_HISTORY)) {
    const role = item?.role === "assistant" ? "assistant" : "user";
    const text = String(item?.content ?? "").trim();
    if (!text) continue;
    if (messages.length && messages[messages.length - 1].role === role) messages[messages.length - 1].text += `\n\n${text}`;
    else messages.push({ role, text });
  }
  while (messages.length && messages[0].role !== "user") messages.shift();
  return messages;
}

/**
 * The assistant over the browser engine. `inspect(code, selection)` runs a
 * read-only script, `execute(script, selection)` runs and publishes one,
 * `undo()` republishes the previous source, `context(selection)` describes
 * the open model and the selection for the prompt.
 */
export function createBrowserAssistant({ inspect, execute, undo = null, context }) {
  let stored = loadAssistantSettings();
  let override = null;

  /** The settings in use, with the key for the current provider and URL only. */
  function current() {
    const { provider, model, baseUrl, remember } = stored;
    return { provider, model, baseUrl, remember, key: stored.keys[keySlot(provider, baseUrl)] ?? "" };
  }

  function configured() {
    const settings = current();
    const provider = PROVIDERS[settings.provider];
    return Boolean(provider && settings.provider !== "off" && settings.model && (!provider.key || settings.key));
  }

  function describe() {
    return configured() ? { provider: stored.provider, model: stored.model } : null;
  }

  /** Change settings; a `key` belongs to the provider and URL in effect after the change. */
  function update(next) {
    const { key, ...rest } = next ?? {};
    stored = { ...stored, ...rest };
    if (!PROVIDERS[stored.provider]) stored.provider = "off";
    stored.remember = Boolean(stored.remember);
    if (typeof key === "string") {
      const keys = { ...stored.keys };
      const slot = keySlot(stored.provider, stored.baseUrl);
      if (key) keys[slot] = key;
      else delete keys[slot];
      stored.keys = keys;
    }
    saveAssistantSettings(stored);
  }

  /** The `complete` function for the configured provider, or the test override. */
  function complete() {
    if (override) return override;
    const settings = current();
    if (settings.provider === "anthropic") {
      return anthropicMessages({ baseUrl: settings.baseUrl, key: settings.key, model: settings.model, browser: true });
    }
    const headers = settings.provider === "openrouter" ? { "x-title": "TessIFC viewer" } : {};
    return chatCompletions({ baseUrl: settings.baseUrl, key: settings.key, model: settings.model, headers });
  }

  /** The worker's script results in the shape the package tools expect. */
  function host(selection) {
    return {
      async runScript(source, _selection, { commit = true } = {}) {
        const result = commit ? await execute(source, selection) : await inspect(source, selection);
        return { report: result, delta: result.impact ? { ...result.impact, impact: result.impact } : null };
      },
      undo: undo ? async () => {
        const result = await undo();
        return { ...result.impact, impact: result.impact };
      } : undefined,
    };
  }

  async function respond({ mode = "ask", prompt, policy = "review", selection = null, history: earlier = [] }) {
    if (!configured()) throw new Error("Configure an assistant provider in Settings first.");
    const text = String(prompt ?? "").trim();
    if (!text) throw new Error("Type a question or an edit request.");
    const edit = mode === "edit";
    const tools = createAgentTools(host(selection), { policy });
    const turn = await runAgentTurn({
      complete: complete(), tools, prompt: text, mode: edit ? "edit" : "ask", context: await context(selection), history: history(earlier),
    });
    const proposal = turn.proposal ? { script: turn.proposal.script, summary: turn.proposal.summary } : null;
    const run = turn.run ? { ...turn.run.report, impact: turn.run.delta?.impact ?? null } : null;
    return { mode: turn.mode, policy, answer: turn.answer, proposal, run, usage: turn.usage, rounds: turn.rounds };
  }

  return {
    respond,
    configured,
    describe,
    update,
    settings: current,
    // Tests replace the network call with a scripted `complete` function.
    useProvider(handler) {
      override = handler ?? null;
    },
  };
}
