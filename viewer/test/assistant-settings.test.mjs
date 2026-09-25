// SPDX-License-Identifier: Apache-2.0
// The browser assistant's provider settings without a browser: a key stays with
// the tab unless the user asks to remember it, each provider and origin keeps
// its own key, and a key stored by an earlier version keeps working.
import assert from "node:assert/strict";
import { PROVIDERS, createBrowserAssistant, keySlot, loadAssistantSettings } from "../src/assistant.js";

class MemoryStorage {
  #items = new Map();
  getItem(name) {
    return this.#items.has(name) ? this.#items.get(name) : null;
  }
  setItem(name, value) {
    this.#items.set(name, String(value));
  }
  removeItem(name) {
    this.#items.delete(name);
  }
  clear() {
    this.#items.clear();
  }
  text() {
    return [...this.#items.values()].join("\n");
  }
}

const device = new MemoryStorage();
const tab = new MemoryStorage();
const install = (name, value) => Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
install("localStorage", device);
install("sessionStorage", tab);

// Every provider call lands here: which URL, with which credentials.
const requests = [];
install("fetch", async (url, init) => {
  requests.push({ url: String(url), headers: { ...init.headers } });
  const body = String(url).endsWith("/v1/messages")
    ? { content: [{ type: "text", text: "hi" }], stop_reason: "end_turn", usage: {} }
    : { choices: [{ message: { role: "assistant", content: "hi" }, finish_reason: "stop" }], usage: {} };
  return { ok: true, status: 200, text: async () => JSON.stringify(body) };
});

const host = { inspect: async () => ({ ok: true, stdout: "" }), execute: async () => ({ ok: true, stdout: "" }), context: async () => "No model is open." };
const choose = (assistant, provider) => assistant.update({ provider, model: PROVIDERS[provider].model || "local-model", baseUrl: PROVIDERS[provider].baseUrl });
const ask = async (assistant) => {
  const before = requests.length;
  await assistant.respond({ mode: "ask", prompt: "How many walls?" });
  return requests.slice(before);
};

assert.equal(keySlot("compatible", "http://127.0.0.1:11434/v1"), "compatible http://127.0.0.1:11434");
assert.equal(keySlot("compatible", "http://127.0.0.1:8080/v1"), "compatible http://127.0.0.1:8080", "another port is another slot");
assert.equal(keySlot("compatible", "127.0.0.1"), "compatible", "an unfinished URL has no origin yet");

{
  const assistant = createBrowserAssistant(host);
  assert.deepEqual(assistant.settings(), { provider: "off", model: "", baseUrl: "", remember: false, key: "" });
  choose(assistant, "openrouter");
  assistant.update({ key: "sk-or-secret" });
  assert.equal(assistant.settings().key, "sk-or-secret");
  assert.ok(!device.text().includes("sk-or-secret"), "a key is not written to the device by default");
  assert.ok(tab.text().includes("sk-or-secret"), "the key lives with the tab");
  const [openrouter] = await ask(assistant);
  assert.equal(openrouter.headers.authorization, "Bearer sk-or-secret");

  choose(assistant, "anthropic");
  assert.equal(assistant.settings().key, "", "another provider does not inherit the key");
  assert.equal(assistant.configured(), false);
  assistant.update({ key: "sk-ant-secret" });
  const [anthropic] = await ask(assistant);
  assert.equal(anthropic.headers["x-api-key"], "sk-ant-secret");

  choose(assistant, "compatible");
  assert.equal(assistant.settings().key, "");
  const [local] = await ask(assistant);
  assert.equal(local.headers.authorization, undefined, "no key is sent to a local URL that was never given one");
  assistant.update({ key: "local-key" });
  assistant.update({ baseUrl: "http://127.0.0.1:8080/v1" });
  assert.equal(assistant.settings().key, "", "a key stays with the origin it was typed for");
  assistant.update({ baseUrl: PROVIDERS.compatible.baseUrl });
  assert.equal(assistant.settings().key, "local-key");

  choose(assistant, "openrouter");
  assert.equal(assistant.settings().key, "sk-or-secret", "switching back finds the provider's own key");
  console.log("ok    each provider and origin keeps its own key, and only that key is sent");

  // A reload in the same tab keeps the keys; a new tab starts without them.
  assert.equal(createBrowserAssistant(host).settings().key, "sk-or-secret");
  tab.clear();
  assert.equal(createBrowserAssistant(host).settings().key, "", "a closed tab forgets the key");
  assert.equal(createBrowserAssistant(host).settings().provider, "openrouter", "the provider choice itself is kept");
  console.log("ok    keys last for the tab unless remembered");
}

{
  device.clear();
  tab.clear();
  const assistant = createBrowserAssistant(host);
  choose(assistant, "openrouter");
  assistant.update({ key: "sk-or-kept", remember: true });
  assert.ok(device.text().includes("sk-or-kept"), "remembering writes the key to the device");
  tab.clear();
  const reopened = createBrowserAssistant(host);
  assert.deepEqual(reopened.settings(), { provider: "openrouter", model: PROVIDERS.openrouter.model, baseUrl: PROVIDERS.openrouter.baseUrl, remember: true, key: "sk-or-kept" });
  reopened.update({ remember: false });
  assert.ok(!device.text().includes("sk-or-kept"), "unticking removes the key from the device");
  assert.equal(reopened.settings().key, "sk-or-kept", "the open tab keeps using it");
  tab.clear();
  assert.equal(createBrowserAssistant(host).settings().key, "");
  console.log("ok    remembering a key is an explicit choice and can be undone");
}

{
  device.clear();
  tab.clear();
  device.setItem("tessifc.assistant", JSON.stringify({ provider: "compatible", model: "fake-model", baseUrl: "http://127.0.0.1:9/v1", key: "test-key" }));
  const assistant = createBrowserAssistant(host);
  assert.deepEqual(assistant.settings(), { provider: "compatible", model: "fake-model", baseUrl: "http://127.0.0.1:9/v1", remember: true, key: "test-key" });
  assert.equal(assistant.configured(), true);
  const [migrated] = await ask(assistant);
  assert.equal(migrated.url, "http://127.0.0.1:9/v1/chat/completions");
  assert.equal(migrated.headers.authorization, "Bearer test-key");
  assistant.update({ model: "other-model" });
  const stored = JSON.parse(device.getItem("tessifc.assistant"));
  assert.equal(stored.key, undefined, "the old plain field is dropped on the next save");
  assert.deepEqual(stored.keys, { "compatible http://127.0.0.1:9": "test-key" });
  console.log("ok    a key stored by an earlier version keeps working and shows as remembered");
}

{
  const blocked = { getItem() { throw new Error("blocked"); }, setItem() { throw new Error("blocked"); }, removeItem() { throw new Error("blocked"); } };
  install("localStorage", blocked);
  install("sessionStorage", blocked);
  assert.equal(loadAssistantSettings().provider, "off", "a blocked store means the assistant is off");
  const assistant = createBrowserAssistant(host);
  choose(assistant, "openrouter");
  assistant.update({ key: "memory-only" });
  assert.equal(assistant.settings().key, "memory-only", "without storage the key still works for the page");
  console.log("ok    blocked storage keeps the key in memory");
}
