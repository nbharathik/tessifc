// SPDX-License-Identifier: Apache-2.0

//! Provider adapters for `runAgentTurn`: the chat-completions wire (OpenRouter,
//! Ollama, LM Studio, any compatible server) and the Messages API wire
//! (Anthropic), both plain `fetch`, no streaming, no dependencies.

import { toChatTools, toMessagesTools } from "./agent-tools.js";

/** A failed provider call: the HTTP status and the provider's own detail. */
export class ProviderError extends Error {
  constructor(message, { status = null, detail = null } = {}) {
    super(message);
    this.name = "ProviderError";
    this.status = status;
    this.detail = detail;
  }
}

function trimSlash(url) {
  return String(url ?? "").replace(/\/$/, "");
}

/** The chat-completions body for one turn. */
export function encodeChatRequest({ model, system, tools, messages, maxTokens = null }) {
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
    } else if (message.role === "tool") {
      for (const result of message.results) wire.push({ role: "tool", tool_call_id: result.id, content: result.content });
    }
  }
  const body = { model, messages: wire, tools: toChatTools(tools), tool_choice: "auto" };
  if (maxTokens) body.max_tokens = maxTokens;
  return body;
}

/** A chat-completions reply as the loop's `{ text, toolCalls, stop, usage }`. */
export function decodeChatReply(json) {
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
    stop: choice.finish_reason ?? null,
    usage: { input: json.usage?.prompt_tokens ?? 0, output: json.usage?.completion_tokens ?? 0 },
  };
}

/** The Messages API body for one turn; the stable system block is marked cacheable. */
export function encodeMessagesRequest({ model, system, tools, messages, maxTokens = 16000, thinking = null, effort = null }) {
  const body = {
    model,
    max_tokens: maxTokens,
    system: [{ type: "text", text: system[0], cache_control: { type: "ephemeral" } }, { type: "text", text: system[1] ?? "" }],
    tools: toMessagesTools(tools),
    messages: messages.map((message) => {
      if (message.role === "user") return { role: "user", content: message.text };
      if (message.role === "assistant") {
        // The raw content keeps thinking blocks intact across tool rounds.
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
  if (thinking) body.thinking = thinking;
  if (effort) body.output_config = { effort };
  return body;
}

/** A Messages API reply as the loop's `{ text, toolCalls, raw, stop, usage }`. */
export function decodeMessagesReply(json) {
  const content = Array.isArray(json.content) ? json.content : [];
  return {
    text: content.filter((block) => block.type === "text").map((block) => block.text).join(""),
    toolCalls: content.filter((block) => block.type === "tool_use").map((block) => ({ id: block.id, name: block.name, input: block.input ?? {} })),
    raw: content,
    stop: json.stop_reason ?? null,
    usage: { input: json.usage?.input_tokens ?? 0, output: json.usage?.output_tokens ?? 0 },
  };
}

async function post(url, headers, body, { fetch: fetchImpl, signal }) {
  const response = await fetchImpl(url, { method: "POST", headers, body: JSON.stringify(body), signal });
  const text = await response.text();
  let json = {};
  try {
    json = JSON.parse(text);
  } catch {
    json = {};
  }
  if (!response.ok) {
    const detail = json.error?.message ?? json.message ?? text.slice(0, 200) ?? "";
    throw new ProviderError(`The assistant provider returned ${response.status}${detail ? `: ${detail}` : ""}`, { status: response.status, detail });
  }
  if (json.error) throw new ProviderError(`The assistant provider failed: ${json.error.message ?? JSON.stringify(json.error)}`, { detail: json.error });
  return json;
}

/**
 * A `complete` function over a chat-completions endpoint such as OpenRouter
 * (`https://openrouter.ai/api/v1`), Ollama or LM Studio. `key` becomes a
 * bearer token when given; `headers` are added to every request.
 */
export function chatCompletions({ baseUrl, key = "", model, headers = {}, fetch: fetchImpl = globalThis.fetch, maxTokens = null }) {
  if (!baseUrl) throw new Error("chatCompletions needs a baseUrl");
  if (!model) throw new Error("chatCompletions needs a model");
  const url = `${trimSlash(baseUrl)}/chat/completions`;
  return async ({ system, tools, messages, signal = null }) => {
    /** @type {Record<string, string>} */
    const request = { "content-type": "application/json", ...headers };
    if (key) request.authorization = `Bearer ${key}`;
    return decodeChatReply(await post(url, request, encodeChatRequest({ model, system, tools, messages, maxTokens }), { fetch: fetchImpl, signal }));
  };
}

/**
 * A `complete` function over the Messages API. `browser: true` adds the header
 * a page needs to call the API directly; `thinking` and `effort` are passed
 * through when given.
 */
export function anthropicMessages({ baseUrl = "https://api.anthropic.com", key, model, fetch: fetchImpl = globalThis.fetch, browser = false,
  maxTokens = 16000, headers = {}, thinking = null, effort = null }) {
  if (!key) throw new Error("anthropicMessages needs an API key");
  if (!model) throw new Error("anthropicMessages needs a model");
  const url = `${trimSlash(baseUrl)}/v1/messages`;
  return async ({ system, tools, messages, signal = null }) => {
    const request = { "content-type": "application/json", "x-api-key": key, "anthropic-version": "2023-06-01", ...headers };
    if (browser) request["anthropic-dangerous-direct-browser-access"] = "true";
    const body = encodeMessagesRequest({ model, system, tools, messages, maxTokens, thinking, effort });
    return decodeMessagesReply(await post(url, request, body, { fetch: fetchImpl, signal }));
  };
}
