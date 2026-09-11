// SPDX-License-Identifier: Apache-2.0

//! The browser assistant: Ask and Edit over the open model with a provider
//! called directly from the page. Tools run browser scripts in the worker.

import { SYSTEM_PROMPT, toChatTools, toMessagesTools, toolsFor } from "../../bindings/edit/src/agent-tools.js";

const STORAGE_KEY = "tessifc.assistant";
const MAX_ROUNDS = 8;
const MAX_HISTORY = 20;
const TOOL_OUTPUT_CHARS = 12_000;

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

function clip(text, limit) {
  const value = String(text ?? "");
  return value.length > limit ? `${value.slice(0, limit)}\n... ${value.length - limit} more characters` : value;
}

/** Messages API request shape, used for Anthropic. */
function anthropicRequest({ settings, system, tools, messages }) {
  const body = {
    model: settings.model,
    max_tokens: 16000,
    system: [{ type: "text", text: system[0], cache_control: { type: "ephemeral" } }, { type: "text", text: system[1] }],
    tools: toMessagesTools(tools),
    messages: messages.map((message) => {
      if (message.role === "user") return { role: "user", content: message.text };
      if (message.role === "assistant") {
        if (message.raw) return { role: "assistant", content: message.raw };
        const content = [];
        if (message.text) content.push({ type: "text", text: message.text });
        for (const call of message.toolCalls ?? []) content.push({ type: "tool_use", id: call.id, name: call.name, input: call.input });
        return { role: "assistant", content: content.length ? content : message.text || "." };
      }
      return { role: "user", content: message.results.map((result) => ({
        type: "tool_result", tool_use_id: result.id, content: result.content, is_error: result.isError })) };
    }),
  };
  return {
    url: `${settings.baseUrl.replace(/\/$/, "")}/v1/messages`,
    headers: {
      "content-type": "application/json",
      "x-api-key": settings.key,
      "anthropic-version": "2023-06-01",
      "anthropic-dangerous-direct-browser-access": "true",
    },
    body,
    parse(json) {
      const content = Array.isArray(json.content) ? json.content : [];
      return {
        text: content.filter((block) => block.type === "text").map((block) => block.text).join(""),
        toolCalls: content.filter((block) => block.type === "tool_use").map((block) => ({ id: block.id, name: block.name, input: block.input ?? {} })),
        raw: content,
        stop: json.stop_reason,
        usage: { input: json.usage?.input_tokens ?? 0, output: json.usage?.output_tokens ?? 0 },
      };
    },
  };
}

/** Chat completions request shape, used for OpenRouter and any compatible server. */
function chatRequest({ settings, system, tools, messages }) {
  const wire = [{ role: "system", content: system.join("\n\n") }];
  for (const message of messages) {
    if (message.role === "user") wire.push({ role: "user", content: message.text });
    else if (message.role === "assistant") {
      const entry = { role: "assistant", content: message.text || null };
      if (message.toolCalls?.length) {
        entry.tool_calls = message.toolCalls.map((call) => ({
          id: call.id, type: "function", function: { name: call.name, arguments: JSON.stringify(call.input ?? {}) },
        }));
      }
      wire.push(entry);
    } else {
      for (const result of message.results) wire.push({ role: "tool", tool_call_id: result.id, content: result.content });
    }
  }
  const headers = { "content-type": "application/json" };
  if (settings.key) headers.authorization = `Bearer ${settings.key}`;
  if (settings.provider === "openrouter") headers["x-title"] = "TessIFC viewer";
  return {
    url: `${settings.baseUrl.replace(/\/$/, "")}/chat/completions`,
    headers,
    body: {
      model: settings.model,
      messages: wire,
      tools: toChatTools(tools),
      tool_choice: "auto",
    },
    parse(json) {
      const choice = json.choices?.[0] ?? {};
      const message = choice.message ?? {};
      const toolCalls = (message.tool_calls ?? []).map((call, index) => {
        let input = {};
        try {
          input = JSON.parse(call.function?.arguments || "{}");
        } catch {
          input = {};
        }
        return { id: call.id ?? `call_${index}`, name: call.function?.name ?? "", input };
      });
      return {
        text: typeof message.content === "string" ? message.content : "",
        toolCalls,
        stop: choice.finish_reason,
        usage: { input: json.usage?.prompt_tokens ?? 0, output: json.usage?.completion_tokens ?? 0 },
      };
    },
  };
}

/**
 * The assistant over the browser engine. `inspect(code)` runs a read-only
 * script, `execute(script)` runs and publishes one, `context(selection)`
 * describes the open model and the selection for the prompt.
 */
export function createBrowserAssistant({ inspect, execute, undo = null, context }) {
  let settings = loadAssistantSettings();
  let complete = defaultComplete;

  async function defaultComplete(request) {
    const response = await fetch(request.url, { method: "POST", headers: request.headers, body: JSON.stringify(request.body) });
    const text = await response.text();
    let json = {};
    try {
      json = JSON.parse(text);
    } catch {
      json = {};
    }
    if (!response.ok) {
      const detail = json.error?.message ?? json.message ?? text.slice(0, 200) ?? "";
      throw new Error(`The assistant provider returned ${response.status}${detail ? `: ${detail}` : ""}`);
    }
    if (json.error) throw new Error(`The assistant provider failed: ${json.error.message ?? JSON.stringify(json.error)}`);
    return request.parse(json);
  }

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

  function request(system, tools, messages) {
    const build = settings.provider === "anthropic" ? anthropicRequest : chatRequest;
    return build({ settings, system, tools, messages });
  }

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

  async function respond({ mode = "ask", prompt, policy = "review", selection = null, history: earlier = [] }) {
    if (!configured()) throw new Error("Configure an assistant provider in Settings first.");
    const text = String(prompt ?? "").trim();
    if (!text) throw new Error("Type a question or an edit request.");
    const edit = mode === "edit";
    const tools = toolsFor(edit ? "edit" : "ask");
    const system = [SYSTEM_PROMPT, `Mode: ${edit ? "edit" : "ask"}. Edit policy: ${policy}.\n${await context(selection)}`];
    const messages = [...history(earlier), { role: "user", text }];
    const outcome = { mode: edit ? "edit" : "ask", policy, answer: "", proposal: null, run: null, usage: { inputTokens: 0, outputTokens: 0 }, rounds: 0 };
    for (let round = 0; round < MAX_ROUNDS; round += 1) {
      const reply = await complete(request(system, tools, messages));
      outcome.rounds += 1;
      outcome.usage.inputTokens += reply.usage?.input ?? 0;
      outcome.usage.outputTokens += reply.usage?.output ?? 0;
      if (reply.stop === "refusal") {
        outcome.answer = reply.text || "The assistant declined this request.";
        return outcome;
      }
      if (!reply.toolCalls.length) {
        outcome.answer = reply.text || (reply.stop === "max_tokens" || reply.stop === "length" ? "The response was cut short." : "");
        return outcome;
      }
      messages.push({ role: "assistant", text: reply.text, toolCalls: reply.toolCalls, raw: reply.raw });
      const results = [];
      for (const call of reply.toolCalls) {
        const { content, isError } = await runTool(call, selection, policy, outcome);
        results.push({ id: call.id, name: call.name, content, isError });
      }
      messages.push({ role: "tool", results });
    }
    outcome.answer ||= "The assistant stopped after too many tool calls.";
    return outcome;
  }

  async function runTool(call, selection, policy, outcome) {
    if (call.name === "inspect_model") {
      try {
        const result = await inspect(String(call.input?.code ?? ""), selection);
        if (!result.ok) return { content: clip(`${result.error}\n${result.traceback ?? ""}\n${result.stdout ?? ""}`, TOOL_OUTPUT_CHARS), isError: true };
        return { content: clip(result.stdout || "(no output)", TOOL_OUTPUT_CHARS), isError: false };
      } catch (error) {
        return { content: String(error.message), isError: true };
      }
    }
    if (call.name === "propose_edit") {
      const script = String(call.input?.script ?? "");
      const summary = String(call.input?.summary ?? "");
      if (!script.trim()) return { content: "The script is empty.", isError: true };
      if (outcome.proposal) return { content: "A script was already proposed; explain it to the user instead.", isError: true };
      outcome.proposal = { script, summary };
      if (policy !== "auto") return { content: "Recorded. The user reviews and runs it from the viewer; describe the change briefly.", isError: false };
      try {
        const run = await execute(script, selection);
        outcome.run = run;
        const report = { ok: run.ok, changed: run.changed, error: run.error ?? null, traceback: run.traceback ?? null, stdout: run.stdout,
          operations: run.operations, affectedProducts: run.impact?.affectedProducts?.length ?? null };
        return { content: clip(JSON.stringify(report), TOOL_OUTPUT_CHARS), isError: !run.ok };
      } catch (error) {
        outcome.run = { ok: false, changed: false, error: String(error.message), stdout: "" };
        return { content: String(error.message), isError: true };
      }
    }
    if (call.name === "undo_edit") {
      if (!undo) return { content: "Undo is not available here.", isError: true };
      try {
        const result = await undo();
        outcome.run = result;
        return { content: JSON.stringify({ revision: result.impact?.revision ?? null, affectedProducts: result.impact?.affectedProducts ?? null }), isError: false };
      } catch (error) {
        return { content: String(error.message), isError: true };
      }
    }
    return { content: `Unknown tool ${call.name}.`, isError: true };
  }

  return {
    respond,
    configured,
    describe,
    update,
    settings: () => ({ ...settings }),
    // Tests replace the network call with a scripted provider.
    useProvider(handler) {
      complete = handler ?? defaultComplete;
    },
  };
}
