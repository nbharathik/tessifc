// SPDX-License-Identifier: Apache-2.0

//! The MCP server: tools an agent calls to inspect, edit, create, undo,
//! export and verify the model held by the host, plus the script API as a
//! resource and a prompt that builds a small building step by step.

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";
import { API_REFERENCE } from "../../edit/src/script-engine.js";
import { SessionBusy } from "./session-host.js";

const OUTPUT_LIMIT = 12_000;
const LIST_LIMIT = 200;

function clip(text, limit = OUTPUT_LIMIT) {
  const value = String(text ?? "");
  return value.length > limit ? `${value.slice(0, limit)}\n... ${value.length - limit} more characters` : value;
}

function result(payload, { isError = false, structured = true } = {}) {
  const response = { content: [{ type: "text", text: clip(JSON.stringify(payload, null, 2)) }], isError };
  if (structured) response.structuredContent = payload;
  return response;
}

/** An error result: text only, so it never has to fit a tool's output schema. */
function failure(error) {
  const payload = { error: error?.message ?? String(error) };
  if (error?.impact) payload.impact = { diagnostics: (error.impact.diagnostics ?? []).slice(0, 20), productOutcomes: (error.impact.productOutcomes ?? []).slice(0, 20) };
  if (error?.committed) payload.committed = true;
  if (error instanceof SessionBusy) payload.busy = true;
  return result(payload, { isError: true, structured: false });
}

/** The record a run returns: the kernel's numbers about the revision, never the script's claims. */
export function runRecord(outcome, host) {
  const { report, delta } = outcome;
  const impact = delta?.impact ?? null;
  return {
    ok: report.ok,
    changed: Boolean(delta),
    revision: outcome.revision ?? host.session?.revision ?? null,
    version: outcome.version ?? host.version ?? null,
    stdout: report.stdout ?? "",
    error: report.error ?? null,
    traceback: report.traceback ?? null,
    operations: report.operations ?? { created: 0, modified: 0, deleted: 0 },
    affectedProducts: delta?.affectedProducts ?? [],
    removedProducts: delta?.removedProducts ?? [],
    metadataProducts: delta?.metadataProducts ?? [],
    fullRebuild: delta?.fullRebuild ?? false,
    kind: delta?.kind ?? null,
    reasons: impact?.reasons?.slice(0, 20) ?? [],
    diagnostics: (delta?.pack?.index?.diagnostics ?? impact?.diagnostics ?? []).slice(0, 20),
    history: host.session ? host.session.history : { undo: 0, redo: 0 },
    saved: Boolean(outcome.saved),
    label: report.label ?? null,
    timings: delta?.timings ? { totalMs: delta.timings.totalMs, prepareMs: delta.timings.prepareMs, geometryMs: delta.timings.geometryMs } : null,
  };
}

const RUN_SHAPE = {
  ok: z.boolean(), changed: z.boolean(), revision: z.string().nullable(), version: z.string().nullable(), stdout: z.string(),
  error: z.string().nullable(), traceback: z.string().nullable(), operations: z.object({ created: z.number(), modified: z.number(), deleted: z.number() }),
  affectedProducts: z.array(z.number()), removedProducts: z.array(z.number()), metadataProducts: z.array(z.number()), fullRebuild: z.boolean(),
  kind: z.string().nullable(), reasons: z.array(z.any()), diagnostics: z.array(z.any()), history: z.object({ undo: z.number(), redo: z.number() }),
  saved: z.boolean(), label: z.string().nullable(), timings: z.any().nullable(),
};

export const BUILD_PROMPT = ({ storeys = "2", footprint = "10 x 8 m", style = "house" }) => `Build a small ${style} in the open IFC model: ${storeys} storeys, footprint about ${footprint}.
Call describe_model first. Then call edit_model once per step, in this order: exterior walls of the ground floor, the base slab, interior walls, doors, windows, the upper storey's slab and walls with windows, the roof slab, a few columns and a beam, property sets (Pset_WallCommon with IsExternal and LoadBearing), and colours.
Use the building helpers (ifc.addWall, ifc.addSlab, ifc.addDoor, ifc.addWindow, ifc.addColumn, ifc.addBeam, ifc.addProperties, ifc.setColor); read tessifc://script-api when unsure. After every step check affectedProducts in the result and fix any error before continuing.
Finish with verify_revision and, when it reports ok, the words BUILDING COMPLETE with the product counts from describe_model.`;

/**
 * Create the MCP server over a model host. `viewerUrl` is mentioned in the
 * instructions so an agent can tell the user where the model is shown.
 */
export function createTessifcServer(host, { viewerUrl = null, version = "0.0.0" } = {}) {
  const instructions = [
    "tessifc edits one IFC model with JavaScript scripts against a schema-aware API and reports which products the kernel rebuilt.",
    "Start with describe_model (or new_model / open_model when none is open), inspect with inspect_model, change with edit_model, one step per call.",
    "The result of every edit carries the kernel's revision, affected and removed products and diagnostics; report those, not estimates.",
    "Read the resource tessifc://script-api for the script names.",
    viewerUrl ? `A viewer follows every change at ${viewerUrl}; get_selection returns what the user clicked there.` : "",
  ].filter(Boolean).join(" ");
  const server = new McpServer({ name: "tessifc", version }, { instructions });
  const busyOr = async (work) => {
    try {
      return await work();
    } catch (error) {
      return failure(error);
    }
  };

  server.registerTool("describe_model", {
    title: "Describe the model",
    description: "The open model: file, schema, revision, length unit, product counts by class, storeys, undo depth and the viewer's state.",
    inputSchema: {},
  }, async () => busyOr(async () => {
    if (!host.session) return result({ open: false, text: "No model is open; call new_model or open_model." });
    const status = host.describe();
    const facts = host.facts();
    const info = host.session.info();
    return result({
      open: true, name: status.name, path: status.path, schema: info.schema ?? null, revision: status.revision, version: status.version,
      generation: status.generation, lengthUnit: facts.lengthUnit, entities: info.entities, products: info.products ?? {},
      storeys: facts.storeys, history: { undo: status.undo, redo: status.redo }, saved: status.saved,
      viewer: viewerUrl ? { url: viewerUrl, selection: host.selection, applied: host.applied } : null,
      text: host.context(),
    });
  }));

  server.registerTool("find_products", {
    title: "Find products",
    description: "Products of a class (and its subtypes), optionally filtered by name or storey, with ids, names and GlobalIds.",
    inputSchema: {
      class: z.string().default("IfcProduct").describe("IFC class, e.g. IfcWall"),
      name: z.string().optional().describe("Case-insensitive substring of the Name"),
      storey: z.string().optional().describe("Name of the containing storey"),
      limit: z.number().int().min(1).max(LIST_LIMIT).default(50),
    },
  }, async ({ class: className = "IfcProduct", name, storey, limit = 50 }) => busyOr(async () => {
    if (!host.session) return failure(new Error("No model is open."));
    const ifc = host.engine();
    const needle = name ? String(name).toLowerCase() : null;
    const products = [];
    let total = 0;
    for (const product of ifc.byType(className)) {
      const container = ifc.container(product);
      if (needle && !String(product.Name ?? "").toLowerCase().includes(needle)) continue;
      if (storey && container?.Name !== storey) continue;
      total += 1;
      if (products.length < limit) products.push({ id: product.id, class: product.type, name: product.Name ?? null, guid: product.GlobalId ?? null, storey: container?.Name ?? null });
    }
    return result({ products, total });
  }));

  server.registerTool("product_info", {
    title: "Product details",
    description: "One entity by express id or GlobalId: attributes, container, property sets, representation items and placement.",
    inputSchema: { id: z.number().int().optional(), guid: z.string().optional() },
  }, async ({ id, guid }) => busyOr(async () => {
    if (!host.session) return failure(new Error("No model is open."));
    const ifc = host.engine();
    const entity = guid ? ifc.byGuid(guid) : id != null ? ifc.get(id) : null;
    if (!entity) return failure(new Error("Give an express id or a GlobalId of an existing entity."));
    const attributes = {};
    for (const [key, value] of Object.entries(entity.attributes())) attributes[key] = plain(value);
    const psets = ifc.inverses(entity, "IfcRelDefinesByProperties").map((rel) => rel.RelatingPropertyDefinition).filter(Boolean)
      .map((pset) => ({ name: pset.Name ?? null, properties: (pset.HasProperties ?? []).map((property) => ({ name: property.Name ?? null, value: plain(property.NominalValue) })) }));
    const items = entity.Representation?.Representations?.flatMap((rep) => (rep.Items ?? []).map((item) => ({ id: item.id, class: item.type, representation: rep.RepresentationIdentifier ?? null }))) ?? [];
    const location = entity.ObjectPlacement?.RelativePlacement?.Location?.Coordinates ?? null;
    return result({ id: entity.id, class: entity.type, guid: entity.GlobalId ?? null, name: entity.Name ?? null, attributes,
      container: ifc.container(entity)?.Name ?? null, propertySets: psets, representations: items, placement: location ? { location } : null });
  }));

  server.registerTool("inspect_model", {
    title: "Inspect the model",
    description: "Run read-only JavaScript against the model and return what it prints; every modification is discarded. Same names as edit_model scripts.",
    inputSchema: { code: z.string().describe("JavaScript that prints what you need to know") },
  }, async ({ code }) => busyOr(async () => {
    const outcome = await host.run(code, null, { commit: false, label: "inspect" });
    const { report } = outcome;
    if (!report.ok) return result({ ok: false, error: report.error, traceback: report.traceback ?? null, stdout: report.stdout ?? "" }, { isError: true, structured: false });
    return result({ ok: true, stdout: report.stdout ?? "", error: null, traceback: null });
  }));

  server.registerTool("edit_model", {
    title: "Edit the model",
    description: "Run a JavaScript script that changes the model; its edits become one revision and the viewer follows. The result carries the kernel's revision, affected and removed products, diagnostics and whether the file was saved.",
    inputSchema: { script: z.string().describe("Complete script using ifc, selected, selection and print"), summary: z.string().optional().describe("One sentence about the change") },
    outputSchema: RUN_SHAPE,
  }, async ({ script, summary }) => busyOr(async () => {
    const outcome = await host.run(script, null, { label: summary ? clip(summary, 200) : "edit" });
    return result(runRecord(outcome, host), { isError: !outcome.report.ok });
  }));

  server.registerTool("undo", { title: "Undo", description: "Undo the last change as a new revision.", inputSchema: {}, outputSchema: RUN_SHAPE },
    async () => busyOr(async () => result(runRecord(await host.undo(), host))));
  server.registerTool("redo", { title: "Redo", description: "Redo the change the last undo removed, as a new revision.", inputSchema: {}, outputSchema: RUN_SHAPE },
    async () => busyOr(async () => result(runRecord(await host.redo(), host))));

  server.registerTool("export_model", {
    title: "Export the model",
    description: "Write the committed IFC to a path (default: the followed file).",
    inputSchema: { path: z.string().optional().describe("Destination .ifc path") },
  }, async ({ path }) => busyOr(async () => {
    const target = path ?? host.path;
    if (!target) return failure(new Error("Give a path; this model is not backed by a file."));
    if (!/\.ifc$/i.test(target)) return failure(new Error("The path must end with .ifc"));
    return result(await host.exportTo(target));
  }));

  server.registerTool("new_model", {
    title: "New model",
    description: "Start a new IFC model with a project, site, building and storeys, optionally saved to a path.",
    inputSchema: {
      schema: z.enum(["IFC2X3", "IFC4", "IFC4X3"]).default("IFC4"),
      name: z.string().default("New project"),
      units: z.enum(["m", "mm"]).default("m"),
      site: z.string().optional(),
      building: z.string().optional(),
      storeys: z.array(z.object({ name: z.string(), elevation: z.number().default(0) })).optional(),
      path: z.string().optional().describe("Where every commit is saved"),
      force: z.boolean().default(false).describe("Drop an unsaved current model"),
    },
  }, async ({ path, force = false, ...options }) => busyOr(async () => {
    const opened = await host.newModel(options, { path, force });
    return result({ revision: opened.revision, version: opened.version, generation: opened.generation, storeys: opened.storeys, products: opened.products, path: host.path });
  }));

  server.registerTool("open_model", {
    title: "Open a model",
    description: "Open an IFC file; it becomes the followed file that commits are saved to.",
    inputSchema: { path: z.string() },
  }, async ({ path }) => busyOr(async () => {
    const opened = await host.openFile(path);
    return result({ revision: opened.revision, version: opened.version, generation: opened.generation, products: opened.products, entities: opened.info.entities, schema: opened.info.schema ?? null, path: host.path });
  }));

  server.registerTool("get_selection", {
    title: "The viewer's selection",
    description: "What the user selected in the viewer that follows this session, if any.",
    inputSchema: {},
  }, async () => result(host.selection ?? { ids: [], guids: [], className: null, name: null, reportedAt: null }));

  server.registerTool("verify_revision", {
    title: "Verify the revision",
    description: "Evaluate the exported file from scratch and compare it product by product with the scene built from the deltas.",
    inputSchema: {},
  }, async () => busyOr(async () => result(host.verify())));

  server.registerTool("list_examples", {
    title: "Example scripts",
    description: "Ready-made scripts: a door, a wall raise, a column, a small house, and more.",
    inputSchema: {},
  }, async () => result({ examples: host.describe().examples }));

  server.registerResource("script-api", "tessifc://script-api", { title: "Script API", description: "The names available to inspect_model and edit_model scripts.", mimeType: "text/plain" },
    async (uri) => ({ contents: [{ uri: uri.href, mimeType: "text/plain", text: API_REFERENCE }] }));
  server.registerResource("model-summary", "tessifc://model/summary", { title: "Model summary", description: "The open model in a few lines.", mimeType: "text/plain" },
    async (uri) => ({ contents: [{ uri: uri.href, mimeType: "text/plain", text: host.context() }] }));

  server.registerPrompt("build-a-building", {
    title: "Build a building",
    description: "Step-by-step brief for building a small building in the open model.",
    argsSchema: { storeys: z.string().optional(), footprint: z.string().optional(), style: z.string().optional() },
  }, ({ storeys, footprint, style }) => ({ messages: [{ role: "user", content: { type: "text", text: BUILD_PROMPT({ storeys, footprint, style }) } }] }));

  return server;
}

/** Entity proxies and typed values as plain JSON. */
function plain(value) {
  if (value === null || value === undefined) return null;
  if (Array.isArray(value)) return value.map(plain);
  if (typeof value === "object") {
    if (typeof value.id === "number" && typeof value.type === "string") return { id: value.id, class: value.type };
    if ("type" in value && "value" in value) return { type: value.type, value: plain(value.value) };
    if ("value" in value) return plain(value.value);
    return String(value);
  }
  return value;
}
