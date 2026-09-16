// SPDX-License-Identifier: Apache-2.0

//! What an assistant learns about the open model before a turn: file, schema,
//! revision, unit, product counts, storeys and the selection. One formatter
//! for the viewer, the Node loop and the MCP server.

const LIMITS = { classes: 40, storeys: 20, fields: 40 };

/** The length unit of a session's model, read from its unit assignment. */
export function lengthUnitOf(session) {
  const named = (id) => session.entity(id)?.fields ?? [];
  const field = (fields, name) => fields.find((item) => item.name === name)?.value ?? null;
  for (const id of session.idsOfType("IfcSIUnit")) {
    const fields = named(id);
    if (field(fields, "UnitType") !== "LENGTHUNIT") continue;
    const prefix = field(fields, "Prefix");
    const name = field(fields, "Name");
    if (name === "METRE") return prefix === "MILLI" ? "mm" : prefix === "CENTI" ? "cm" : prefix ? `${String(prefix).toLowerCase()}m` : "m";
    return `${prefix ? String(prefix).toLowerCase() : ""}${String(name).toLowerCase()}`;
  }
  for (const id of session.idsOfType("IfcConversionBasedUnit")) {
    const fields = named(id);
    if (field(fields, "UnitType") === "LENGTHUNIT") return field(fields, "Name") ?? "unknown";
  }
  return "unknown";
}

/** The storeys of a session's model by class, lowest first; the hierarchy is empty until geometry exists. */
export function storeysOf(session) {
  return session.idsOfType("IfcBuildingStorey").map((id) => {
    const fields = session.entity(id)?.fields ?? [];
    const value = (name) => fields.find((item) => item.name === name)?.value ?? null;
    return { expressId: id, name: value("Name"), elevation: value("Elevation") == null ? null : Number(value("Elevation")) };
  }).sort((a, b) => (a.elevation ?? Number.POSITIVE_INFINITY) - (b.elevation ?? Number.POSITIVE_INFINITY));
}

/** The selected entity of a session as `{ expressId, className, fields }`, or null. */
export function describeSelection(session, selection) {
  const id = Number(selection?.ids?.[0]);
  if (!Number.isInteger(id)) return null;
  const entity = session.entity(id);
  if (!entity) return null;
  return { expressId: id, className: entity.class, fields: entity.fields ?? [] };
}

/**
 * Format the context lines from plain data. `products` is `{ className: count }`,
 * `storeys` a list of `{ expressId, name }`, `selection` a `describeSelection` result.
 */
export function describeModelInfo({ name = "model.ifc", schema = null, revision = "0", lengthUnit = null, products = {}, storeys = [], selection = null } = {}, limits = {}) {
  const max = { ...LIMITS, ...limits };
  const classes = Object.entries(products).sort((left, right) => right[1] - left[1]).slice(0, max.classes)
    .map(([className, total]) => `${className} ${total}`).join(", ");
  const storeyList = storeys.slice(0, max.storeys).map((storey) => `#${storey.expressId} ${storey.name ?? "unnamed"}`).join("; ");
  const unit = lengthUnit && lengthUnit !== "unknown" ? `lengths in ${lengthUnit}` : "lengths in the model's length unit";
  const lines = [
    `File: ${name} (${schema ?? "unknown schema"}), revision ${revision}, ${unit}.`,
    `Products by class: ${classes || "none"}.`,
  ];
  if (storeyList) lines.push(`Storeys: ${storeyList}.`);
  if (selection) {
    const fields = (selection.fields ?? []).filter((field) => field.raw && field.raw !== "$").slice(0, max.fields)
      .map((field) => `${field.name}=${field.raw}`).join(", ");
    lines.push(`Selected in the viewer (the \`selected\` entity): #${selection.expressId} ${selection.className}${fields ? ` with ${fields}` : ""}.`);
  } else {
    lines.push("Nothing is selected in the viewer.");
  }
  return lines.join("\n");
}

/** The context of an editing session: its model, revision, storeys and the given selection. */
export function describeModel(session, { selection = null, limits = {} } = {}) {
  const info = session.info();
  return describeModelInfo({
    name: session.name ?? "model.ifc",
    schema: info.schema ?? null,
    revision: session.revision,
    lengthUnit: lengthUnitOf(session),
    products: info.products ?? {},
    storeys: storeysOf(session),
    selection: describeSelection(session, selection),
  }, limits);
}
