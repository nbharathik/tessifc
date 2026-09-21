// SPDX-License-Identifier: Apache-2.0

//! A minimal IFC file from nothing: project, units, contexts, site, building
//! and storeys, so a session can start empty and scripts add the products.

import { encodeStepString, formatValue, newGuid as randomGuid } from "./script-engine.js";

const SCHEMAS = {
  IFC2X3: { description: "CoordinationView_V2.0", ownerHistory: true },
  IFC4: { description: "ViewDefinition [ReferenceView_V1.2]", ownerHistory: false },
  IFC4X3: { description: "ViewDefinition [ReferenceView]", ownerHistory: false },
};

const real = (value) => formatValue(Number(value));

/**
 * @typedef {object} ModelOptions
 * @property {"IFC2X3" | "IFC4" | "IFC4X3" | string} [schema]
 * @property {string} [name]
 * @property {"m" | "mm" | string} [units]
 * @property {string} [site]
 * @property {string} [building]
 * @property {Array<{ name: string, elevation: number }>} [storeys]
 * @property {string} [author]
 * @property {string} [organisation]
 * @property {string} [producer]
 * @property {string | null} [timestamp]
 * @property {() => string} [guid]
 */

/**
 * The text of a new IFC file. Lengths in `units` ("m" or "mm"); every storey
 * is `{ name, elevation }`. `guid` and `timestamp` are injectable so tests
 * get deterministic output.
 * @param {ModelOptions} [options]
 */
export function createModelText({
  schema = "IFC4",
  name = "New project",
  units = "m",
  site = "Site",
  building = "Building",
  storeys = [{ name: "Ground floor", elevation: 0 }],
  author = "",
  organisation = "",
  producer = "tessifc",
  timestamp = null,
  guid = randomGuid,
} = {}) {
  const schemaName = String(schema).toUpperCase();
  const variant = SCHEMAS[schemaName];
  if (!variant) throw new Error(`Unsupported schema ${schema}; use IFC2X3, IFC4 or IFC4X3`);
  if (units !== "m" && units !== "mm") throw new Error(`Unsupported length unit ${units}; use "m" or "mm"`);
  if (!Array.isArray(storeys) || !storeys.length) throw new Error("At least one storey is needed");
  const stamp = (timestamp ?? new Date().toISOString()).replace(/\.\d+Z?$/, "").replace(/Z$/, "");
  const lines = [];
  const entity = (text) => {
    lines.push(`#${lines.length + 1}=${text};`);
    return `#${lines.length}`;
  };
  const label = (text) => encodeStepString(text);
  const id = () => label(guid());

  // IFC2X3 requires an owner history on every rooted entity.
  let history = "$";
  if (variant.ownerHistory) {
    const person = entity(`IFCPERSON($,$,${author ? label(author) : "$"},$,$,$,$,$)`);
    const org = entity(`IFCORGANIZATION($,${label(organisation || producer)},$,$,$)`);
    const owner = entity(`IFCPERSONANDORGANIZATION(${person},${org},$)`);
    const application = entity(`IFCAPPLICATION(${org},${label("0.2")},${label(producer)},${label(producer)})`);
    const seconds = Math.floor(Date.parse(`${stamp}Z`) / 1000);
    history = entity(`IFCOWNERHISTORY(${owner},${application},$,.ADDED.,$,$,$,${Number.isFinite(seconds) ? seconds : 0})`);
  }

  const origin = entity("IFCCARTESIANPOINT((0.,0.,0.))");
  const up = entity("IFCDIRECTION((0.,0.,1.))");
  const east = entity("IFCDIRECTION((1.,0.,0.))");
  const axes = entity(`IFCAXIS2PLACEMENT3D(${origin},${up},${east})`);
  const context = entity(`IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-05,${axes},$)`);
  entity(`IFCGEOMETRICREPRESENTATIONSUBCONTEXT('Body','Model',*,*,*,*,${context},$,.MODEL_VIEW.,$)`);
  const length = entity(units === "mm" ? "IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.)" : "IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.)");
  const area = entity("IFCSIUNIT(*,.AREAUNIT.,$,.SQUARE_METRE.)");
  const volume = entity("IFCSIUNIT(*,.VOLUMEUNIT.,$,.CUBIC_METRE.)");
  const angle = entity("IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.)");
  const unitAssignment = entity(`IFCUNITASSIGNMENT((${length},${area},${volume},${angle}))`);
  const project = entity(`IFCPROJECT(${id()},${history},${label(name)},$,$,$,$,(${context}),${unitAssignment})`);
  const sitePlacement = entity(`IFCLOCALPLACEMENT($,${axes})`);
  const siteEntity = entity(`IFCSITE(${id()},${history},${label(site)},$,$,${sitePlacement},$,$,.ELEMENT.,$,$,$,$,$)`);
  const buildingPlacement = entity(`IFCLOCALPLACEMENT(${sitePlacement},${axes})`);
  const buildingEntity = entity(`IFCBUILDING(${id()},${history},${label(building)},$,$,${buildingPlacement},$,$,.ELEMENT.,$,$,$)`);
  entity(`IFCRELAGGREGATES(${id()},${history},$,$,${project},(${siteEntity}))`);
  entity(`IFCRELAGGREGATES(${id()},${history},$,$,${siteEntity},(${buildingEntity}))`);
  const storeyEntities = storeys.map((storey, index) => {
    const elevation = Number(storey?.elevation ?? 0);
    const point = entity(`IFCCARTESIANPOINT((0.,0.,${real(elevation)}))`);
    const placement = entity(`IFCAXIS2PLACEMENT3D(${point},$,$)`);
    const local = entity(`IFCLOCALPLACEMENT(${buildingPlacement},${placement})`);
    return entity(`IFCBUILDINGSTOREY(${id()},${history},${label(storey?.name ?? `Storey ${index + 1}`)},$,$,${local},$,$,.ELEMENT.,${real(elevation)})`);
  });
  entity(`IFCRELAGGREGATES(${id()},${history},$,$,${buildingEntity},(${storeyEntities.join(",")}))`);

  const header = [
    "ISO-10303-21;",
    "HEADER;",
    `FILE_DESCRIPTION((${label(variant.description)}),'2;1');`,
    `FILE_NAME(${label(`${name}.ifc`)},${label(stamp)},(${label(author)}),(${label(organisation)}),${label(producer)},${label(producer)},'');`,
    `FILE_SCHEMA((${label(schemaName)}));`,
    "ENDSEC;",
    "DATA;",
  ];
  return `${header.join("\n")}\n${lines.join("\n")}\nENDSEC;\nEND-ISO-10303-21;\n`;
}

/**
 * The bytes of a new IFC file; see `createModelText` for the options.
 * @param {ModelOptions} [options]
 */
export function createModel(options = {}) {
  return new TextEncoder().encode(createModelText(options));
}
