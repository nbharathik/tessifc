// SPDX-License-Identifier: Apache-2.0

//! Let a language model build the demo house through the agent tools; every
//! proposal runs at once and the kernel's report goes back to the model.
//! Set OPENROUTER_API_KEY (the default provider) or ANTHROPIC_API_KEY.
//!
//!   node examples/agent-building/agent.mjs [house.ifc] [--provider openrouter|anthropic] [--model <id>]
//!        [--serve] [--port 8000] [--max-turns 24] [--out transcript.json] [--prompt "..."]

import { createRequire } from "node:module";
import { existsSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { createAgentTools, runAgentTurn } from "../../bindings/edit/src/agent-tools.js";
import { anthropicMessages, chatCompletions } from "../../bindings/edit/src/providers.js";
import { createModelHost } from "../../bindings/mcp/src/session-host.js";
import { createViewerServer } from "../../bindings/mcp/src/viewer-server.js";
import { MODEL, EXPECTED } from "./steps.mjs";

const root = resolve(fileURLToPath(new URL("../../", import.meta.url)));
const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    provider: { type: "string", default: process.env.ANTHROPIC_API_KEY && !process.env.OPENROUTER_API_KEY ? "anthropic" : "openrouter" },
    model: { type: "string" },
    serve: { type: "boolean", default: false },
    port: { type: "string", default: "8000" },
    "max-turns": { type: "string", default: "24" },
    out: { type: "string", default: "transcript.json" },
    prompt: { type: "string" },
  },
});
const kernelModule = resolve(root, "bindings/wasm/pkg-node/tessifc_wasm.js");
if (!existsSync(kernelModule)) {
  console.error("Build the Node kernel first: python scripts/build-wasm.py --target both");
  process.exit(1);
}
const { Kernel } = createRequire(import.meta.url)(kernelModule);

function provider() {
  if (values.provider === "anthropic") {
    const key = process.env.ANTHROPIC_API_KEY;
    if (!key) throw new Error("Set ANTHROPIC_API_KEY");
    return { complete: anthropicMessages({ key, model: values.model ?? "claude-opus-5" }), model: values.model ?? "claude-opus-5" };
  }
  const key = process.env.OPENROUTER_API_KEY;
  if (!key) throw new Error("Set OPENROUTER_API_KEY (or ANTHROPIC_API_KEY with --provider anthropic)");
  const model = values.model ?? "anthropic/claude-opus-5";
  return { complete: chatCompletions({ baseUrl: "https://openrouter.ai/api/v1", key, model, headers: { "x-title": "tessifc agent demo" } }), model };
}

const { complete, model } = provider();
const file = resolve(positionals[0] ?? "house.ifc");
const host = createModelHost({ Kernel, log: () => {} });
await host.newModel(MODEL, { path: file, force: true });
let viewer = null;
if (values.serve) {
  viewer = createViewerServer(host, { root, port: Number(values.port) || 0 });
  await viewer.listen();
  console.log(`Open ${viewer.viewerUrl} to watch`);
}

const transcript = { provider: values.provider, model, file, startedAt: new Date().toISOString(), turns: [] };
// The tools see the host as a session: scripts run through it and undo yields the delta.
const tools = createAgentTools({
  runScript: (source, selection, options) => host.run(source, selection, options),
  undo: async () => (await host.undo()).delta,
}, {
  policy: "auto",
  onDelta: (delta) => console.log(`   revision ${delta.revision}: ${delta.affectedProducts.length} rebuilt, ${delta.removedProducts.length} removed`),
});
const maxTurns = Number(values["max-turns"]) || 24;
// One script per turn: the tools accept a single proposal, so the brief asks for steps.
const BRIEF = `Build a small house in the open IFC model: two storeys (Ground floor at 0, Upper floor at 3), footprint about 10 x 8 m.
Propose exactly one script per turn, in this order: exterior walls of the ground floor, the base slab, interior walls, doors, windows,
the upper floor's slab and walls with windows, the roof slab, two columns and a beam, property sets (Pset_WallCommon with IsExternal
and LoadBearing), then colours. Use the building helpers: ifc.addWall, ifc.addSlab, ifc.addDoor, ifc.addWindow, ifc.addColumn,
ifc.addBeam, ifc.addProperties, ifc.setColor, ifc.byName, ifc.storeys. After each result check affectedProducts and fix any error in
the next turn. When every step is done and nothing is left, answer with the words BUILDING COMPLETE and the counts you know.`;
let prompt = values.prompt ?? BRIEF;
const history = [];
let finished = false;
for (let turn = 1; turn <= maxTurns && !finished; turn += 1) {
  console.log(`\nTurn ${turn}: ${prompt.slice(0, 80)}${prompt.length > 80 ? "..." : ""}`);
  const outcome = await runAgentTurn({
    complete, tools, prompt, mode: "edit", context: host.context(), history,
    onEvent: (event) => {
      if (event.type === "tool" && event.phase === "start") console.log(`   ${event.name}${event.name === "propose_edit" ? `: ${event.input?.summary ?? ""}` : ""}`);
      if (event.type === "tool" && event.phase === "end" && event.isError) console.log(`   error: ${String(event.content).slice(0, 200)}`);
    },
  });
  console.log(`   ${outcome.answer.slice(0, 300)}`);
  transcript.turns.push({
    prompt, answer: outcome.answer, usage: outcome.usage, rounds: outcome.rounds, stop: outcome.stop,
    proposals: outcome.proposals.map((proposal) => ({
      summary: proposal.summary, script: proposal.script,
      report: proposal.run ? { ok: proposal.run.report.ok, error: proposal.run.report.error ?? null, revision: proposal.run.delta?.revision ?? null,
        affectedProducts: proposal.run.delta?.affectedProducts.length ?? 0, removedProducts: proposal.run.delta?.removedProducts.length ?? 0 } : null,
    })),
  });
  history.push({ role: "user", text: prompt }, { role: "assistant", text: outcome.answer });
  finished = /BUILDING COMPLETE/i.test(outcome.answer);
  prompt = "Continue with the next step. When every step is done, say BUILDING COMPLETE.";
}

const verified = host.verify();
const info = host.session.info();
transcript.final = { complete: finished, revision: host.session.revision, products: info.products, verified,
  expected: EXPECTED, endedAt: new Date().toISOString() };
writeFileSync(resolve(values.out), JSON.stringify(transcript, null, 2));
console.log(`\n${finished ? "The model reported the building complete" : "Stopped after the turn limit"}; revision ${host.session.revision}, ` +
  `${verified.products} placed products, verification ${verified.ok ? "ok" : `${verified.mismatches.length} mismatches`}. Transcript: ${resolve(values.out)}`);
if (viewer) {
  console.log("The viewer keeps following; press Ctrl+C to stop.");
  await new Promise((done) => process.once("SIGINT", done));
  await viewer.close();
}
host.dispose();
