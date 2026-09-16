// SPDX-License-Identifier: Apache-2.0
//! From shapes to an IGP pack: instance flags, the model offset, and one mesh
//! plus many transforms for a family placed many times.

use crate::{ProductCategory, Shape, ShapePart, codes};
use glam::{DMat4, DVec3};
use std::collections::HashMap;
use tessifc_geom::{PartGeometry, SharedKey};
use tessifc_pack::{
    DiagnosticRecord, INSTANCE_OPENING, INSTANCE_REFERENCE, INSTANCE_SPACE, INSTANCE_TRANSPARENT,
    IgpWriter, Instance, StreamPosition, StreamState,
};
use tessifc_schema::{Schema, SchemaId};
use tessifc_step::Diagnostic;

/// A shared mesh whose own coordinates reach further than this from its
/// origin is baked into world space instead, because f32 would lose it.
const MAX_SHARED_EXTENT_M: f64 = 1.0e4;

/// The `INSTANCE_*` bits a product's record carries, from its class name.
///
/// The name is resolved against the compiled schemas, so a concrete subtype is
/// classified like its supertype. An unknown name counts as physical.
pub fn instance_flags(class: &str, color: [u8; 4]) -> u16 {
    instance_flags_for_category(category_of_class(class), color)
}

/// The classification `product_category` makes, from a class name alone.
fn category_of_class(class: &str) -> ProductCategory {
    for &id in SchemaId::all() {
        let Some(schema) = Schema::try_get(id) else {
            continue;
        };
        let Some(found) = schema.class_by_name(class) else {
            continue;
        };
        let is_a = |name: &str| schema.is_a_name(found, name);
        if is_a("IfcOpeningElement") || is_a("IfcVoidingFeature") {
            return ProductCategory::Opening;
        }
        if is_a("IfcSpace") || is_a("IfcSpatialZone") || is_a("IfcExternalSpatialElement") {
            return ProductCategory::Space;
        }
        if is_a("IfcAnnotation") || is_a("IfcGrid") {
            return ProductCategory::Annotation;
        }
        if is_a("IfcVirtualElement")
            || is_a("IfcStructuralItem")
            || is_a("IfcStructuralActivity")
            || is_a("IfcDistributionPort")
            || is_a("IfcPositioningElement")
        {
            return ProductCategory::Reference;
        }
        return ProductCategory::Physical;
    }
    ProductCategory::Physical
}

/// The `INSTANCE_*` bits for a product that is already classified.
pub fn instance_flags_for_category(category: ProductCategory, color: [u8; 4]) -> u16 {
    let mut flags = match category {
        ProductCategory::Space => INSTANCE_SPACE,
        ProductCategory::Opening => INSTANCE_OPENING,
        ProductCategory::Annotation | ProductCategory::Reference => INSTANCE_REFERENCE,
        ProductCategory::Physical => 0,
    };
    // So a viewer can put it in the transparent pass without unpacking the
    // colour first, which it has to do before it can sort anything.
    if color[3] < 255 {
        flags |= INSTANCE_TRANSPARENT;
    }
    flags
}

/// What a stream carries from one chunk's packer to the next.
#[derive(Clone, Debug, Default)]
pub struct PackState {
    /// The writer's memory of geometry ids.
    pub stream: StreamState,
    /// Shared family meshes already written, by key.
    pub shared: HashMap<SharedKey, u32>,
}

/// Writes shapes into an IGP pack.
pub struct Packer {
    writer: IgpWriter,
    shared: HashMap<SharedKey, u32>,
    offset: DVec3,
    triangles: usize,
    shared_instances: usize,
}

impl Packer {
    /// A packer for a whole model.
    pub fn new(schema: &str, length_scale_to_m: f64, model_offset: DVec3) -> Self {
        Packer::from_writer(
            IgpWriter::new(schema, length_scale_to_m),
            model_offset,
            HashMap::new(),
        )
    }

    /// A packer for the next chunk of a stream.
    pub fn continue_stream(
        schema: &str,
        length_scale_to_m: f64,
        model_offset: DVec3,
        state: PackState,
    ) -> Self {
        Packer::from_writer(
            IgpWriter::continue_stream(schema, length_scale_to_m, state.stream),
            model_offset,
            state.shared,
        )
    }

    fn from_writer(mut writer: IgpWriter, offset: DVec3, shared: HashMap<SharedKey, u32>) -> Self {
        // The offset is recorded, not applied to the source: baking it in
        // would stop two models of the same site lining up.
        writer.set_model_offset(offset.to_array());
        Packer {
            writer,
            shared,
            offset,
            triangles: 0,
            shared_instances: 0,
        }
    }

    /// Mark this pack as one chunk of a stream.
    pub fn set_stream(&mut self, position: StreamPosition) {
        self.writer.set_stream(position);
    }

    /// Record a statistic for the `stats` block.
    pub fn set_stat(&mut self, name: &str, value: f64) {
        self.writer.set_stat(name, value);
    }

    /// Carry the file's map conversion, already serialised as JSON.
    pub fn set_georef(&mut self, json: Option<String>) {
        self.writer.set_georef(json);
    }

    /// Add a shape, one record per coloured part.
    pub fn add_shape(&mut self, shape: Shape) {
        let Shape {
            express_id,
            class,
            category,
            parts,
            ..
        } = shape;
        for part in parts {
            self.add_part(express_id, &class, category, part);
        }
    }

    /// Add a shape without consuming it.
    pub fn add_shape_ref(&mut self, shape: &Shape) {
        for part in &shape.parts {
            let geometry = match &part.geometry {
                PartGeometry::Unique(mesh) => PartGeometry::Unique(mesh.clone()),
                shared => shared.clone(),
            };
            self.add_part(
                shape.express_id,
                &shape.class,
                shape.category,
                ShapePart {
                    geometry,
                    color: part.color,
                    provenance: part.provenance.clone(),
                },
            );
        }
    }

    fn add_part(
        &mut self,
        express_id: u32,
        class: &str,
        category: ProductCategory,
        part: ShapePart,
    ) {
        let flags = instance_flags_for_category(category, part.color);
        let (geometry_id, transform) = match part.geometry {
            PartGeometry::Shared {
                key,
                mesh,
                transform,
            } if shareable(&mesh) => {
                // The offset moves the placement, not the shared mesh.
                let placed = (DMat4::from_translation(-self.offset) * transform)
                    .to_cols_array()
                    .map(|value| value as f32);
                if !finite(&placed) {
                    self.drop_part(express_id);
                    return;
                }
                let id = match self.shared.get(&key).copied() {
                    Some(id) => id,
                    None => {
                        let positions: Vec<f32> = mesh
                            .positions
                            .iter()
                            .flat_map(|point| [point.x as f32, point.y as f32, point.z as f32])
                            .collect();
                        if !finite(&positions) {
                            self.drop_part(express_id);
                            return;
                        }
                        let closed = Some(mesh.closed.unwrap_or_else(|| mesh.is_edge_manifold()));
                        let (positions, indices) =
                            tessifc_mesh::optimize_vertex_locality_f32(&positions, &mesh.indices);
                        let id = self
                            .writer
                            .add_geometry_owned_closed(positions, indices, closed);
                        self.shared.insert(key, id);
                        id
                    }
                };
                self.triangles += mesh.triangle_count();
                self.shared_instances += 1;
                (id, placed)
            }
            geometry => {
                let mesh = geometry.into_world_mesh();
                // f32 out, after the offset. Positions are f64 up to here.
                let positions: Vec<f32> = mesh
                    .positions
                    .iter()
                    .flat_map(|point| {
                        let shifted = *point - self.offset;
                        [shifted.x as f32, shifted.y as f32, shifted.z as f32]
                    })
                    .collect();
                if !finite(&positions) {
                    self.drop_part(express_id);
                    return;
                }
                self.triangles += mesh.triangle_count();
                let closed = Some(mesh.closed.unwrap_or_else(|| mesh.is_edge_manifold()));
                let id = self
                    .writer
                    .add_geometry_owned_closed(positions, mesh.indices, closed);
                (id, IDENTITY)
            }
        };
        self.writer.add_instance(Instance {
            geometry_id,
            express_id,
            class: class.to_string(),
            transform,
            color: part.color,
            flags,
            provenance: tessifc_pack::Provenance {
                representation: part.provenance.representation,
                item: part.provenance.item,
                evaluator: part.provenance.evaluator.clone(),
                boolean: part.provenance.boolean.name(),
            },
        });
    }

    /// Say in the pack why a part was left out of it.
    fn drop_part(&mut self, express_id: u32) {
        self.writer.add_diagnostic(DiagnosticRecord {
            express_id: Some(express_id),
            line: 0,
            severity: "warn".to_string(),
            code: codes::NON_FINITE_GEOMETRY.as_str().to_string(),
            message: "a coordinate does not survive f32; this part was left out".to_string(),
        });
    }

    /// Add diagnostics to the pack's own list.
    pub fn add_diagnostics<'a>(&mut self, diagnostics: impl IntoIterator<Item = &'a Diagnostic>) {
        for diagnostic in diagnostics {
            self.writer.add_diagnostic(DiagnosticRecord {
                express_id: diagnostic.express_id,
                line: diagnostic.line,
                severity: diagnostic.severity.as_str().to_string(),
                code: diagnostic.code.as_str().to_string(),
                message: diagnostic.message.clone(),
            });
        }
    }

    /// Unique meshes written by this packer.
    pub fn geometry_count(&self) -> usize {
        self.writer.geometry_count()
    }

    /// Records written by this packer.
    pub fn instance_count(&self) -> usize {
        self.writer.instance_count()
    }

    /// Triangles across every record, counting a shared mesh once per use.
    pub fn triangles(&self) -> usize {
        self.triangles
    }

    /// Records that refer to a shared family mesh.
    pub fn shared_instances(&self) -> usize {
        self.shared_instances
    }

    /// Serialise a whole pack.
    pub fn finish(self) -> Vec<u8> {
        self.writer.finish()
    }

    /// Serialise one chunk and hand back what the next one needs.
    pub fn finish_chunk(self) -> (Vec<u8>, PackState) {
        let (bytes, stream) = self.writer.finish_chunk();
        (
            bytes,
            PackState {
                stream,
                shared: self.shared,
            },
        )
    }
}

/// Column-major identity, the transform of a mesh already in world space.
const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

/// True when every component can be written as a JSON number.
fn finite(values: &[f32]) -> bool {
    values.iter().all(|value| value.is_finite())
}

/// Whether a family mesh can go into the pack in its own coordinates.
fn shareable(mesh: &tessifc_mesh::Mesh64) -> bool {
    match mesh.bounds() {
        Some((lo, hi)) => {
            lo.abs().max_element() <= MAX_SHARED_EXTENT_M
                && hi.abs().max_element() <= MAX_SHARED_EXTENT_M
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use tessifc_model::Model;
    use tessifc_step::{ParseOptions, parse};

    fn model_of(data: &str) -> Model {
        let source = format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{data}ENDSEC;\n"
        );
        Model::new(parse(source.as_bytes(), &ParseOptions::default()))
    }

    /// A family placed twice, and one wall of its own.
    const TWO_CHAIRS: &str = "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);\n\
         #2=IFCDIRECTION((0.,0.,1.));\n\
         #3=IFCEXTRUDEDAREASOLID(#1,$,#2,1.);\n\
         #4=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));\n\
         #5=IFCCARTESIANPOINT((0.,0.,0.));\n\
         #6=IFCAXIS2PLACEMENT3D(#5,$,$);\n\
         #7=IFCREPRESENTATIONMAP(#6,#4);\n\
         #20=IFCMAPPEDITEM(#7,$);\n\
         #21=IFCSHAPEREPRESENTATION($,'Body','MappedRepresentation',(#20));\n\
         #22=IFCPRODUCTDEFINITIONSHAPE($,$,(#21));\n\
         #30=IFCCARTESIANPOINT((2.,0.,0.));\n\
         #31=IFCAXIS2PLACEMENT3D(#30,$,$);\n\
         #32=IFCLOCALPLACEMENT($,#31);\n\
         #33=IFCFURNISHINGELEMENT('c1',$,$,$,$,#32,#22,$);\n\
         #40=IFCCARTESIANPOINT((4.,0.,0.));\n\
         #41=IFCAXIS2PLACEMENT3D(#40,$,$);\n\
         #42=IFCLOCALPLACEMENT($,#41);\n\
         #43=IFCFURNISHINGELEMENT('c2',$,$,$,$,#42,#22,$);\n\
         #60=IFCEXTRUDEDAREASOLID(#1,$,#2,3.);\n\
         #61=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#60));\n\
         #62=IFCPRODUCTDEFINITIONSHAPE($,$,(#61));\n\
         #63=IFCWALL('w',$,$,$,$,$,#62,$,$);\n";

    fn parse_json(bytes: &[u8]) -> serde_json::Value {
        let json_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        serde_json::from_str(std::str::from_utf8(&bytes[24..24 + json_len]).unwrap()).unwrap()
    }

    #[test]
    fn a_family_is_written_once_and_placed_twice() {
        let model = model_of(TWO_CHAIRS);
        let result = Engine::new().evaluate(&model);
        let mut packer = Packer::new("IFC4", 1.0, result.model_offset);
        for shape in result.shapes {
            packer.add_shape(shape);
        }
        assert_eq!(packer.instance_count(), 3);
        assert_eq!(packer.geometry_count(), 2, "one chair mesh, one wall mesh");
        assert_eq!(packer.shared_instances(), 2);
        assert_eq!(packer.triangles(), 36);
        let json = parse_json(&packer.finish());
        assert_eq!(json["geometries"].as_array().unwrap().len(), 2);
        assert_eq!(json["instances"]["count"], 3);
    }

    #[test]
    fn a_streamed_pack_carries_the_family_across_chunks() {
        let model = model_of(TWO_CHAIRS);
        let engine = Engine::new();
        let mut session = engine.session(&model);
        let offset = session.model_offset();
        let mut state = PackState::default();
        let mut chunks = Vec::new();
        let mut chunk = 0;
        loop {
            let batch = session.next(&model, |progress| progress.products >= 1);
            let mut packer = Packer::continue_stream("IFC4", 1.0, offset, state);
            packer.set_stream(StreamPosition {
                chunk,
                is_final: batch.is_final,
                products_done: session.done(),
                products_total: session.total(),
            });
            for shape in batch.shapes {
                packer.add_shape(shape);
            }
            let (bytes, next) = packer.finish_chunk();
            state = next;
            chunks.push(parse_json(&bytes));
            chunk += 1;
            if batch.is_final {
                break;
            }
        }
        assert_eq!(chunks.len(), 3);
        // The first chair writes the mesh; the second refers to it by id.
        assert_eq!(chunks[0]["geometries"].as_array().unwrap().len(), 1);
        assert_eq!(chunks[1]["geometries"].as_array().unwrap().len(), 0);
        assert_eq!(chunks[1]["instances"]["count"], 1);
        assert_eq!(
            chunks[2]["geometries"][0]["id"], 1,
            "the wall gets the next global id"
        );
        assert_eq!(state.stream.next_geometry_id, 2);
        assert_eq!(state.shared.len(), 1);
    }

    #[test]
    fn flags_follow_category_and_alpha() {
        assert_eq!(instance_flags("IfcWall", [1, 2, 3, 255]), 0);
        assert_eq!(
            instance_flags("IfcSpace", [1, 2, 3, 120]),
            INSTANCE_SPACE | INSTANCE_TRANSPARENT
        );
        assert_eq!(
            instance_flags("IfcOpeningElement", [1, 2, 3, 255]),
            INSTANCE_OPENING
        );
        assert_eq!(
            instance_flags("IfcOpeningStandardCase", [1, 2, 3, 255]),
            INSTANCE_OPENING
        );
        assert_eq!(
            instance_flags("IfcSpatialZone", [1, 2, 3, 255]),
            INSTANCE_SPACE
        );
        assert_eq!(
            instance_flags("IfcGrid", [1, 2, 3, 255]),
            INSTANCE_REFERENCE
        );
    }

    #[test]
    fn a_concrete_subtype_is_classified_like_its_supertype() {
        // Every name the old list held was an abstract one no file can carry.
        for class in [
            "IfcStructuralCurveMember",
            "IfcStructuralSurfaceMember",
            "IfcStructuralPointConnection",
            "IfcStructuralCurveAction",
        ] {
            assert_eq!(
                instance_flags(class, [1, 2, 3, 255]),
                INSTANCE_REFERENCE,
                "{class}"
            );
        }
        assert_eq!(instance_flags("IfcNotAClass", [1, 2, 3, 255]), 0);
    }

    #[test]
    fn a_part_that_does_not_survive_f32_is_left_out_with_a_diagnostic() {
        let mut mesh = tessifc_mesh::Mesh64::new();
        mesh.positions.extend([
            DVec3::ZERO,
            DVec3::new(1.0e39, 0.0, 0.0),
            DVec3::new(0.0, 1.0, 0.0),
        ]);
        mesh.push_triangle(0, 1, 2);
        let color = [1, 2, 3, 255];
        let mut packer = Packer::new("IFC4", 1.0, DVec3::ZERO);
        packer.add_shape(Shape {
            express_id: 7,
            class: "IfcWall".into(),
            category: ProductCategory::Physical,
            parts: vec![ShapePart {
                geometry: PartGeometry::Unique(mesh),
                color,
                provenance: Default::default(),
            }],
            color,
        });

        assert_eq!(packer.instance_count(), 0);
        assert_eq!(packer.geometry_count(), 0);
        let json = parse_json(&packer.finish());
        assert_eq!(json["diagnostics"][0]["id"], 7);
        assert_eq!(json["diagnostics"][0]["code"], "W_NON_FINITE_GEOMETRY");
    }
}
