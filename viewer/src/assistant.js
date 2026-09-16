// SPDX-License-Identifier: Apache-2.0

//! The browser assistant: Ask and Edit over the open model with a provider
//! called directly from the page, through the package's tool loop. Tools run
//! browser scripts in the worker.

import { createAgentTools, runAgentTurn } from "../../bindings/edit/src/agent-tools.js";
import { anthropicMessages, chatCompletions } from "../../bindings/edit/src/providers.js";

const STORAGE_KEY = "tessifc.assistant";
const MAX_HISTORY = 20;

export const PROVIDERS = {
  off: { label: "Off" },
  openrouter: { label: "OpenRouter", baseUrl: "https://openrouter.ai/api/v1", model: "anthropic/claude-opus-5", key: true },
  anthropic: { label: "Anthropic", baseUrl: "https://api.anthropic.com", model: "claude-opus-5", key: true },
  compatible: { label: "OpenAI-compatible URL", baseUrl: "http://127.0.0.1:11434/v1", model: "", key: false },
};

/** Provider settings from the last visit; a blocked store means the assistant is off. */
export function loadAssistantSettings() {
  const settings = { provider: "off", model: "", baseUrl: "", key: "" };
  try {
    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}");
    for (const name of Object.keys(settings)) if (typeof stored?.[name] === "string") settings[name] = stored[name];
  } catch {
    // Nothing stored, or storage is blocked.
  }
  if (!PROVIDERS[settings.provider]) settings.provider = "off";
  return settings;
}

export function saveAssistantSettings(settings) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
  } catch {
    // A blocked storage backend must not break the interface.
  }
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
  let settings = loadAssistantSettings();
  let override = null;

  function configured() {
    const provider = PROVIDERS[settings.provider];
    return Boolean(provider && settings.provider !== "off" && settings.model && (!provider.key || settings.key));
  }

  function describe() {
    return configured() ? { provider: settings.provider, model: settings.model } : null;
  }

  function update(next) {
    settings = { ...settings, ...next };
    if (!PROVIDERS[settings.provider]) settings.provider = "off";
    saveAssistantSettings(settings);
  }

  /** The `complete` function for the configured provider, or the test override. */
  function complete() {
    if (override) return override;
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
    settings: () => ({ ...settings }),
    // Tests replace the network call with a scripted `complete` function.
    useProvider(handler) {
      override = handler ?? null;
    },
  };
}
