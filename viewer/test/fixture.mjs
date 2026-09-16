// SPDX-License-Identifier: Apache-2.0
/**
 * A first-party pavilion generated in memory. `duplicateGuids` gives every column
 * the same GlobalId; `family` adds two seats sharing one mapped representation.
 */
export function pavilionIfc({ duplicateGuids = false, family = false } = {}) {
  const lines = [];
  const entity = (text) => { lines.push(`#${lines.length + 1}=${text};`); return `#${lines.length}`; };
  let serial = 0;
  const guid = () => `'0${String(++serial).padStart(21, "0")}'`;
  const columnGuid = duplicateGuids ? guid() : null;
  const real = (value) => Number.isInteger(value) ? `${value}.` : String(value);
  const point = (x, y, z) => entity(`IFCCARTESIANPOINT((${[x, y, z].map(real).join(",")}))`);
  const origin = point(0, 0, 0);
  const up = entity("IFCDIRECTION((0.,0.,1.))");
  const axis = entity(`IFCAXIS2PLACEMENT3D(${origin},$,$)`);
  const placement = entity(`IFCLOCALPLACEMENT($,${axis})`);
  const context = entity(`IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,0.00001,${axis},$)`);
  const metre = entity("IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.)");
  const units = entity(`IFCUNITASSIGNMENT((${metre}))`);
  const project = entity(`IFCPROJECT(${guid()},$,'TessIFC Pavilion',$,$,$,$,(${context}),${units})`);
  const building = entity(`IFCBUILDING(${guid()},$,'Pavilion',$,$,${placement},$,$,.ELEMENT.,$,$,$)`);
  const storey = entity(`IFCBUILDINGSTOREY(${guid()},$,'Ground floor',$,$,${placement},$,$,.ELEMENT.,0.)`);
  entity(`IFCRELAGGREGATES(${guid()},$,$,$,${project},(${building}))`);
  entity(`IFCRELAGGREGATES(${guid()},$,$,$,${building},(${storey}))`);
  const products = [];
  const box = (type, name, x, y, z, width, depth, height, color = [0.78, 0.8, 0.83], transparency = 0) => {
    const position = entity(`IFCAXIS2PLACEMENT3D(${point(x, y, z)},$,$)`);
    const local = entity(`IFCLOCALPLACEMENT($,${position})`);
    const profile = entity(`IFCRECTANGLEPROFILEDEF(.AREA.,$,$,${real(width)},${real(depth)})`);
    const solid = entity(`IFCEXTRUDEDAREASOLID(${profile},${axis},${up},${real(height)})`);
    const rgb = entity(`IFCCOLOURRGB($,${color.map(real).join(",")})`);
    const rendering = entity(`IFCSURFACESTYLERENDERING(${rgb},${real(transparency)},$,$,$,$,$,$,.NOTDEFINED.)`);
    const style = entity(`IFCSURFACESTYLE($,.BOTH.,(${rendering}))`);
    entity(`IFCSTYLEDITEM(${solid},(${style}),$)`);
    const shape = entity(`IFCSHAPEREPRESENTATION(${context},'Body','SweptSolid',(${solid}))`);
    const representation = entity(`IFCPRODUCTDEFINITIONSHAPE($,$,(${shape}))`);
    const predefined = type === "IFCFURNISHINGELEMENT" ? "" : ",.NOTDEFINED.";
    const id = duplicateGuids && type === "IFCCOLUMN" ? columnGuid : guid();
    const product = entity(`${type}(${id},$,'${name}',$,$,${local},${representation},$${predefined})`);
    products.push(product);
    return product;
  };
  box("IFCSLAB", "Foundation", 0, 0, -0.25, 12, 8, 0.25, [0.47, 0.52, 0.59]);
  box("IFCSLAB", "Roof", 0, 0, 3.4, 12, 8, 0.22, [0.88, 0.9, 0.93]);
  const wall = box("IFCWALL", "Gallery wall", 0, 3.2, 0, 10.8, 0.25, 3.4);
  const opening = box("IFCOPENINGELEMENT", "Gallery window opening", -2.5, 3.2, 1, 2.6, 0.5, 1.8);
  entity(`IFCRELVOIDSELEMENT(${guid()},$,$,$,${wall},${opening})`);
  box("IFCWALL", "Service wall", -5.3, 0, 0, 0.25, 6.4, 3.4);
  for (const x of [-4.8, 0, 4.8]) {
    for (const y of [-3, 3]) box("IFCCOLUMN", "Steel column", x, y, 0, 0.18, 0.18, 3.4, [0.19, 0.24, 0.31]);
  }
  for (let x = -4; x <= 4; x += 2) {
    box("IFCPLATE", "Glazed facade", x, -3.1, 0.15, 1.85, 0.06, 3.1, [0.35, 0.64, 0.75], 0.58);
  }
  for (const x of [-2.5, 2.5]) box("IFCFURNISHINGELEMENT", "Exhibit plinth", x, 0.4, 0, 1.6, 1.2, 0.7, [0.82, 0.64, 0.4]);
  if (family) {
    const profile = entity("IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.6,0.6)");
    const solid = entity(`IFCEXTRUDEDAREASOLID(${profile},${axis},${up},0.9)`);
    const shape = entity(`IFCSHAPEREPRESENTATION(${context},'Body','SweptSolid',(${solid}))`);
    const map = entity(`IFCREPRESENTATIONMAP(${axis},${shape})`);
    for (const [name, x] of [["Mapped seat A", -3.5], ["Mapped seat B", 3.5]]) {
      const position = entity(`IFCAXIS2PLACEMENT3D(${point(x, -1.5, 0)},$,$)`);
      const local = entity(`IFCLOCALPLACEMENT($,${position})`);
      const operator = entity(`IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,${origin},1.,$)`);
      const mapped = entity(`IFCMAPPEDITEM(${map},${operator})`);
      const mappedShape = entity(`IFCSHAPEREPRESENTATION(${context},'Body','MappedRepresentation',(${mapped}))`);
      const representation = entity(`IFCPRODUCTDEFINITIONSHAPE($,$,(${mappedShape}))`);
      products.push(entity(`IFCFURNISHINGELEMENT(${guid()},$,'${name}',$,$,${local},${representation},$)`));
    }
  }
  entity(`IFCRELCONTAINEDINSPATIALSTRUCTURE(${guid()},$,$,$,(${products.filter((id) => id !== opening).join(",")}),${storey})`);
  return `ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('ViewDefinition [ReferenceView_V1.2]'),'2;1');\nFILE_NAME('pavilion.ifc','2026-01-01T00:00:00',('TessIFC'),('TessIFC'),'TessIFC','TessIFC','');\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n${lines.join("\n")}\nENDSEC;\nEND-ISO-10303-21;\n`;
}

export const pavilionFile = () => ({ name: "pavilion.ifc", mimeType: "application/octet-stream", buffer: Buffer.from(pavilionIfc()) });
