// SPDX-License-Identifier: Apache-2.0
// The provider adapters against a fake fetch: request bodies on both wires,
// tool-call decoding, usage, headers and error mapping. No network, no kernel.
import assert from "node:assert/strict";
import { ProviderError, anthropicMessages, chatCompletions, decodeChatReply, decodeMessagesReply, encodeChatRequest, encodeMessagesRequest } from "../src/providers.js";
import { toolsFor } from "../src/agent-tools.js";

let passed = 0;
function ok(condition, label) {
  assert.ok(condition, label);
  passed += 1;
  console.log(`ok    ${label}`);
}

const system = ["You are the assistant.", "Mode: edit. Edit policy: auto.\nFile: x.ifc"];
const messages = [
  { role: "user", text: "Add a wall" },
  { role: "assistant", text: "", toolCalls: [{ id: "c1", name: "inspect_model", input: { code: "print(1)" } }], raw: [{ type: "tool_use", id: "c1", name: "inspect_model", input: { code: "print(1)" } }] },
  { role: "tool", results: [{ id: "c1", name: "inspect_model", content: "1", isError: false }] },
];

const chat = encodeChatRequest({ model: "m", system, tools: toolsFor("edit"), messages });
ok(chat.messages[0].role === "system" && chat.messages[0].content.includes("Mode: edit") && chat.tools.length === 3 && chat.tool_choice === "auto",
  "the chat wire carries the joined system prompt and every tool");
ok(chat.messages[2].tool_calls[0].function.arguments === '{"code":"print(1)"}' && chat.messages[3].role === "tool" && chat.messages[3].tool_call_id === "c1",
  "assistant tool calls and tool results map to chat-completions turns");
const chatReply = decodeChatReply({ choices: [{ message: { content: null, tool_calls: [{ id: "x", function: { name: "propose_edit", arguments: "{\"script\":\"a\",\"summary\":\"b\"}" } }] }, finish_reason: "tool_calls" }], usage: { prompt_tokens: 10, completion_tokens: 5 } });
ok(chatReply.toolCalls[0].input.script === "a" && chatReply.stop === "tool_calls" && chatReply.usage.input === 10, "chat replies decode tool calls, stop and usage");
ok(decodeChatReply({ choices: [{ message: { tool_calls: [{ function: { name: "x", arguments: "{bad" } }] } }] }).toolCalls[0].input.code === undefined, "malformed tool arguments decode to an empty input");

const anthropic = encodeMessagesRequest({ model: "claude", system, tools: toolsFor("ask"), messages, thinking: { type: "adaptive" }, effort: "medium" });
ok(anthropic.system[0].cache_control?.type === "ephemeral" && anthropic.tools.length === 1 && anthropic.tools[0].strict === true, "the Messages wire caches the stable block and marks tools strict");
ok(anthropic.messages[1].content[0].type === "tool_use" && anthropic.messages[2].content[0].type === "tool_result" && anthropic.messages[2].content[0].tool_use_id === "c1",
  "assistant raw content and tool results map to Messages turns");
ok(anthropic.thinking.type === "adaptive" && anthropic.output_config.effort === "medium", "thinking and effort pass through when given");
const messagesReply = decodeMessagesReply({ content: [{ type: "text", text: "Hi" }, { type: "tool_use", id: "t", name: "inspect_model", input: { code: "1" } }], stop_reason: "tool_use", usage: { input_tokens: 3, output_tokens: 4 } });
ok(messagesReply.text === "Hi" && messagesReply.toolCalls[0].id === "t" && messagesReply.raw.length === 2 && messagesReply.usage.output === 4, "Messages replies decode text, tool calls, raw content and usage");

const calls = [];
const fakeFetch = (status, body) => async (url, init) => {
  calls.push({ url, init });
  return { ok: status < 400, status, text: async () => JSON.stringify(body) };
};
const complete = chatCompletions({ baseUrl: "https://openrouter.ai/api/v1/", key: "k", model: "m", headers: { "x-title": "t" },
  fetch: fakeFetch(200, { choices: [{ message: { content: "done" }, finish_reason: "stop" }], usage: { prompt_tokens: 1, completion_tokens: 2 } }) });
const reply = await complete({ system, tools: toolsFor("ask"), messages: [{ role: "user", text: "hi" }] });
ok(calls[0].url === "https://openrouter.ai/api/v1/chat/completions" && calls[0].init.headers.authorization === "Bearer k" && calls[0].init.headers["x-title"] === "t",
  "chatCompletions posts to the endpoint with the bearer key and extra headers");
ok(reply.text === "done" && reply.stop === "stop" && JSON.parse(calls[0].init.body).model === "m", "the reply is decoded and the model named");

const anthropicComplete = anthropicMessages({ key: "sk", model: "claude", browser: true,
  fetch: fakeFetch(200, { content: [{ type: "text", text: "ok" }], stop_reason: "end_turn", usage: {} }) });
await anthropicComplete({ system, tools: toolsFor("ask"), messages: [{ role: "user", text: "hi" }] });
const last = calls[calls.length - 1];
ok(last.url === "https://api.anthropic.com/v1/messages" && last.init.headers["x-api-key"] === "sk" && last.init.headers["anthropic-version"] === "2023-06-01"
  && last.init.headers["anthropic-dangerous-direct-browser-access"] === "true", "anthropicMessages posts with the key, the version and the browser header");
const server = anthropicMessages({ key: "sk", model: "claude", fetch: fakeFetch(200, { content: [], stop_reason: "end_turn" }) });
await server({ system, tools: toolsFor("ask"), messages: [{ role: "user", text: "hi" }] });
ok(calls[calls.length - 1].init.headers["anthropic-dangerous-direct-browser-access"] === undefined, "outside a browser the direct-access header is absent");

const failing = chatCompletions({ baseUrl: "http://x", model: "m", fetch: fakeFetch(429, { error: { message: "slow down" } }) });
await assert.rejects(() => failing({ system, tools: [], messages: [] }), (error) => error instanceof ProviderError && error.status === 429 && /429: slow down/.test(error.message));
ok(true, "HTTP failures become ProviderError with the status and detail");
assert.throws(() => chatCompletions({ baseUrl: "http://x" }), /needs a model/);
assert.throws(() => anthropicMessages({ model: "m" }), /needs an API key/);
ok(true, "missing configuration is refused up front");

console.log(`PASS ${passed} checks`);
