// SPDX-License-Identifier: Apache-2.0

//! Building helpers for scripts: storeys, walls, slabs, openings with doors
//! and windows, columns, beams, property sets and colours, all made of the
//! schema-driven `ifc.add` so they work in IFC2X3, IFC4 and IFC4X3.

const EPSILON = 1e-9;

function vector3(value, name) {
  if (!Array.isArray(value) || value.length < 2 || value.some((item) => !Number.isFinite(Number(item)))) {
    throw new Error(`${name} needs [x, y] or [x, y, z] numbers`);
  }
  return [Number(value[0]), Number(value[1]), Number(value[2] ?? 0)];
}

function unit(vector, name) {
  const length = Math.hypot(...vector);
  if (length < EPSILON) throw new Error(`${name} has no direction`);
  return vector.map((item) => item / length);
}

/** Helpers over the engine's core: `add`, queries and the relationship makers. */
export function createHelpers(core) {
  const { add, byType, inverses, contain, aggregate, container, context, typed, classDefinition } = core;

  function required(className, attribute) {
    const definition = classDefinition(className);
    const found = definition.attributes.find((item) => item.name.toUpperCase() === attribute.toUpperCase());
    return Boolean(found) && !found.optional;
  }

  function direction3(vector) {
    return add("IfcDirection", { DirectionRatios: vector.map((item) => Number(item)) });
  }

  /** A 3D placement; `direction` and `axis` are optional unit vectors, 2D or 3D. */
  function placement3d(at, { direction = null, axis = null, rotation = null } = {}) {
    const location = add("IfcCartesianPoint", { Coordinates: vector3(at, "at") });
    let ref = direction ? vector3(direction, "direction") : null;
    if (rotation != null) {
      const radians = (Number(rotation) * Math.PI) / 180;
      ref = [Math.cos(radians), Math.sin(radians), 0];
    }
    const attributes = { Location: location };
    if (axis) attributes.Axis = direction3(unit(vector3(axis, "axis"), "axis"));
    if (ref) attributes.RefDirection = direction3(unit(ref, "direction"));
    return add("IfcAxis2Placement3D", attributes);
  }

  function placement2d() {
    return add("IfcAxis2Placement2D", { Location: add("IfcCartesianPoint", { Coordinates: [0, 0] }) });
  }

  function rectangle(width, depth) {
    const attributes = { ProfileType: "AREA", XDim: Number(width), YDim: Number(depth) };
    if (required("IfcRectangleProfileDef", "Position")) attributes.Position = placement2d();
    return add("IfcRectangleProfileDef", attributes);
  }

  function polygonProfile(points) {
    if (!Array.isArray(points) || points.length < 3) throw new Error("polygon needs at least three [x, y] points");
    const corners = points.map((point) => {
      const [x, y] = vector3(point, "polygon point");
      return add("IfcCartesianPoint", { Coordinates: [x, y] });
    });
    const first = corners[0];
    const polyline = add("IfcPolyline", { Points: [...corners, first] });
    return add("IfcArbitraryClosedProfileDef", { ProfileType: "AREA", OuterCurve: polyline });
  }

  function extrusion(profile, depth) {
    const attributes = { SweptArea: profile, ExtrudedDirection: direction3([0, 0, 1]), Depth: Number(depth) };
    if (required("IfcExtrudedAreaSolid", "Position")) attributes.Position = placement3d([0, 0, 0]);
    return add("IfcExtrudedAreaSolid", attributes);
  }

  function bodyShape(items) {
    const shape = add("IfcShapeRepresentation", { ContextOfItems: context(), RepresentationIdentifier: "Body", RepresentationType: "SweptSolid", Items: items });
    return add("IfcProductDefinitionShape", { Representations: [shape] });
  }

  /** A placed product with one extruded body; `relativeTo` is a product or spatial element. */
  function placedProduct(className, name, { at, relativeTo = null, direction = null, axis = null, rotation = null, attributes = {} }, profile, depth) {
    const axes = placement3d(at, { direction, axis, rotation });
    const placement = add("IfcLocalPlacement", { PlacementRelTo: relativeTo ? relativeTo.ObjectPlacement : null, RelativePlacement: axes });
    const solid = extrusion(profile, depth);
    return add(className, { Name: name, ObjectPlacement: placement, Representation: bodyShape([solid]), ...attributes });
  }

  /** A rectangular extrusion centred on x/y at `at`, rising from its z along the placement's axis. */
  function addBox(className, name, { at = [0, 0, 0], size = [1, 1, 1], relativeTo = null, direction = null, axis = null, rotation = null, attributes = {} } = {}) {
    const [width, depth, height] = size.map(Number);
    return placedProduct(className, name, { at, relativeTo, direction, axis, rotation, attributes }, rectangle(width, depth), height);
  }

  function storeys() {
    return byType("IfcBuildingStorey").sort((a, b) => (a.Elevation ?? Number.POSITIVE_INFINITY) - (b.Elevation ?? Number.POSITIVE_INFINITY));
  }

  function byName(className, name) {
    return byType(className).find((item) => item.Name === name) ?? null;
  }

  /** A storey by entity, by name, or the lowest one. */
  function resolveStorey(storey) {
    if (storey && typeof storey === "object") return storey;
    if (typeof storey === "string") {
      const found = byName("IfcBuildingStorey", storey);
      if (!found) throw new Error(`No storey named ${JSON.stringify(storey)}`);
      return found;
    }
    const lowest = storeys()[0];
    if (!lowest) throw new Error("The model has no storey; add one with ifc.addStorey({ name, elevation })");
    return lowest;
  }

  function addStorey({ name = "Storey", elevation = 0, building = null } = {}) {
    const parent = building ?? byType("IfcBuilding")[0];
    if (!parent) throw new Error("The model has no IfcBuilding to add a storey to");
    const axes = placement3d([0, 0, Number(elevation)]);
    const placement = add("IfcLocalPlacement", { PlacementRelTo: parent.ObjectPlacement ?? null, RelativePlacement: axes });
    const storey = add("IfcBuildingStorey", { Name: name, ObjectPlacement: placement, CompositionType: "ELEMENT", Elevation: Number(elevation) });
    aggregate(parent, storey);
    return storey;
  }

  function addWall({ from, to, height = 3, thickness = 0.2, storey = null, name = "Wall", attributes = {} } = {}) {
    const [x1, y1, z1] = vector3(from, "from");
    const [x2, y2] = vector3(to, "to");
    const length = Math.hypot(x2 - x1, y2 - y1);
    if (length < EPSILON) throw new Error("addWall needs two different points");
    const target = resolveStorey(storey);
    const wall = addBox("IfcWall", name, {
      at: [(x1 + x2) / 2, (y1 + y2) / 2, z1], size: [length, Number(thickness), Number(height)],
      direction: [(x2 - x1) / length, (y2 - y1) / length], relativeTo: target, attributes,
    });
    contain(wall, target);
    return wall;
  }

  function addSlab({ polygon = null, size = null, at = [0, 0, 0], thickness = 0.2, type = "FLOOR", storey = null, name = "Slab", attributes = {} } = {}) {
    if (!polygon && !size) throw new Error("addSlab needs a polygon of [x, y] points or a size [width, depth]");
    const target = resolveStorey(storey);
    const profile = polygon ? polygonProfile(polygon) : rectangle(Number(size[0]), Number(size[1]));
    const slab = placedProduct("IfcSlab", name, { at, relativeTo: target, attributes: { PredefinedType: String(type).toUpperCase(), ...attributes } }, profile, thickness);
    contain(slab, target);
    return slab;
  }

  function hostThickness(host) {
    const items = host.Representation?.Representations?.flatMap((item) => item.Items ?? []) ?? [];
    const solid = items.find((item) => item?.is?.("IfcExtrudedAreaSolid") && item.SweptArea?.is?.("IfcRectangleProfileDef"));
    return solid ? Number(solid.SweptArea.YDim) : null;
  }

  /** An opening through `host`, at [x, y, sill] in the host's placement; `size` is [width, height]. */
  function addOpening({ in: host, at = [0, 0, 0], size = [1, 1], depth = null, name = "Opening" } = {}) {
    if (!host) throw new Error("addOpening needs a host element in `in`");
    const [x, y, z] = vector3(at, "at");
    const [width, height] = size.map(Number);
    // Through the host by default: its rectangular thickness plus a margin on each side.
    const thickness = hostThickness(host);
    if (depth == null && thickness == null) throw new Error("addOpening needs `depth` when the host has no rectangular extrusion");
    const through = depth != null ? Number(depth) : thickness + 0.1;
    const opening = addBox("IfcOpeningElement", name, { at: [x, y, z], size: [width, through, height], relativeTo: host });
    core.void(host, opening);
    return opening;
  }

  function filling(className, { in: host, at = [0, 0, 0], size, name, attributes = {} }, sizeDefault) {
    const [width, height] = (size ?? sizeDefault).map(Number);
    const [x, y, z] = vector3(at, "at");
    const opening = addOpening({ in: host, at: [x, y, z], size: [width, height], name: `${name} opening` });
    const element = addBox(className, name, {
      at: [x, y, z], size: [width, 0.05, height], relativeTo: host,
      attributes: { OverallHeight: height, OverallWidth: width, ...attributes },
    });
    core.fill(opening, element);
    const storey = container(host) ?? storeys()[0] ?? null;
    if (storey) contain(element, storey);
    return element;
  }

  function addDoor(options = {}) {
    return filling("IfcDoor", { name: "Door", ...options }, [0.9, 2.1]);
  }

  function addWindow(options = {}) {
    const at = options.at ?? [0, 0, 0.9];
    return filling("IfcWindow", { name: "Window", ...options, at }, [1.2, 1.2]);
  }

  function addColumn({ at = [0, 0], size = [0.3, 0.3], height = 3, storey = null, name = "Column", rotation = null, attributes = {} } = {}) {
    const target = resolveStorey(storey);
    const [x, y, z] = vector3(at, "at");
    const column = addBox("IfcColumn", name, { at: [x, y, z], size: [Number(size[0]), Number(size[1]), Number(height)], relativeTo: target, rotation, attributes });
    contain(column, target);
    return column;
  }

  /** A beam from one point to another; `size` is [depth, width] of its section. */
  function addBeam({ from, to, size = [0.3, 0.2], storey = null, name = "Beam", attributes = {} } = {}) {
    const start = vector3(from, "from");
    const end = vector3(to, "to");
    const along = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
    const length = Math.hypot(...along);
    if (length < EPSILON) throw new Error("addBeam needs two different points");
    const axis = along.map((item) => item / length);
    // The profile's x follows world up unless the beam is vertical.
    const direction = Math.abs(axis[2]) > 1 - 1e-6 ? [1, 0, 0] : [0, 0, 1];
    const target = resolveStorey(storey);
    const beam = addBox("IfcBeam", name, { at: start, size: [Number(size[0]), Number(size[1]), length], axis, direction, relativeTo: target, attributes });
    contain(beam, target);
    return beam;
  }

  function propertyValue(value) {
    if (core.isTyped(value)) return value;
    if (core.isInt(value)) return typed("IFCINTEGER", value);
    if (typeof value === "boolean") return typed("IFCBOOLEAN", value);
    if (typeof value === "number") return typed("IFCREAL", value);
    return typed("IFCLABEL", String(value));
  }

  /** Add or update single values in a property set of `entity`, creating the set when needed. */
  function addProperties(entity, psetName, values = {}) {
    if (!entity) throw new Error("addProperties needs an entity");
    const relation = inverses(entity, "IfcRelDefinesByProperties").find((rel) => rel.RelatingPropertyDefinition?.is?.("IfcPropertySet")
      && rel.RelatingPropertyDefinition.Name === psetName);
    let pset = relation?.RelatingPropertyDefinition ?? null;
    const properties = pset ? [...(pset.HasProperties ?? [])] : [];
    for (const [key, value] of Object.entries(values)) {
      const existing = properties.find((item) => item?.is?.("IfcPropertySingleValue") && item.Name === key);
      if (existing) existing.NominalValue = propertyValue(value);
      else properties.push(add("IfcPropertySingleValue", { Name: key, NominalValue: propertyValue(value) }));
    }
    if (pset) {
      pset.HasProperties = properties;
      return pset;
    }
    pset = add("IfcPropertySet", { Name: psetName, HasProperties: properties });
    add("IfcRelDefinesByProperties", { RelatedObjects: [entity], RelatingPropertyDefinition: pset });
    return pset;
  }

  function surfaceStyle([r, g, b, a = 1]) {
    const rgb = add("IfcColourRgb", { Red: Number(r), Green: Number(g), Blue: Number(b) });
    const rendering = add("IfcSurfaceStyleRendering", { SurfaceColour: rgb, Transparency: 1 - Number(a), ReflectanceMethod: "NOTDEFINED" });
    const style = add("IfcSurfaceStyle", { Side: "BOTH", Styles: [rendering] });
    // IFC2X3 styled items take a style assignment; later schemas take the style itself.
    return core.schema.toUpperCase() === "IFC2X3" ? add("IfcPresentationStyleAssignment", { Styles: [style] }) : style;
  }

  /** Colour every body item of a product; components are 0 to 1, alpha optional. */
  function setColor(entity, color) {
    if (!Array.isArray(color) || color.length < 3) throw new Error("setColor needs [r, g, b] or [r, g, b, a] in 0..1");
    const items = entity?.Representation?.Representations?.flatMap((rep) => rep.Items ?? []) ?? [];
    if (!items.length) throw new Error(`#${entity?.id} has no representation items to colour`);
    const style = surfaceStyle(color);
    for (const item of items) {
      const styled = inverses(item, "IfcStyledItem")[0];
      if (styled) styled.Styles = [style];
      else add("IfcStyledItem", { Item: item, Styles: [style] });
    }
    return style;
  }

  function lengthUnit() {
    const units = byType("IfcSIUnit").find((item) => item.UnitType === "LENGTHUNIT");
    if (units) return `${units.Prefix ? String(units.Prefix).toLowerCase() : ""}${units.Name === "METRE" ? "m" : String(units.Name).toLowerCase()}`.replace("millim", "mm").replace("centim", "cm");
    const conversion = byType("IfcConversionBasedUnit").find((item) => item.UnitType === "LENGTHUNIT");
    return conversion?.Name ?? "unknown";
  }

  function describe() {
    const info = core.modelInfo();
    return {
      schema: core.schema,
      lengthUnit: lengthUnit(),
      entities: info.entities,
      products: info.products ?? {},
      storeys: storeys().map((storey) => ({ id: storey.id, name: storey.Name ?? null, elevation: storey.Elevation ?? null })),
    };
  }

  return Object.freeze({
    addBox, addStorey, addWall, addSlab, addOpening, addDoor, addWindow, addColumn, addBeam, addProperties, setColor, storeys, byName, describe,
  });
}
