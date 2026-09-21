// SPDX-License-Identifier: Apache-2.0
//! From shapes to an IGP pack: instance flags, the model offset, and one mesh
//! plus many transforms for a family placed many times.

use crate::{ProductCategory, Shape, ShapePart, codes};
use glam::{DMat4, DVec3};
use std::collections::HashMap;
use tessifc_geom::TextureSource;
use tessifc_geom::{PartGeometry, SharedKey};
use tessifc_pack::{
    DiagnosticRecord, INSTANCE_OPENING, INSTANCE_REFERENCE, INSTANCE_SPACE, INSTANCE_TRANSPARENT,
    IgpWriter, Instance, MaterialRecord, StreamPosition, StreamState, TextureData, TextureRecord,
};
use tessifc_schema::{Schema, SchemaId};
use tessifc_step::Diagnostic;

/// A shared mesh whose own coordinates reach further than this from its
/// origin is baked into world space instead, because f32 would lose it.
const MAX_SHARED_EXTENT_M: f64 = 1.0e4;

/// Texture bytes one pack may embed; past it a texture is written without
/// its pixels and said so.
const MAX_EMBEDDED_TEXTURE_BYTES: usize = 64 << 20;

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
    embedded_texture_bytes: usize,
    /// Coarse levels to write per large mesh, and the chord tolerance they start from.
    lod: Option<(u8, f64)>,
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
            embedded_texture_bytes: 0,
            lod: None,
        }
    }

    /// Write `levels` coarse levels (0 to 2) for every mesh large enough,
    /// with tolerances derived from `chord_tolerance_m` and the mesh's size.
    pub fn set_lod_levels(&mut self, levels: u8, chord_tolerance_m: f64) {
        self.lod = (levels > 0).then_some((levels.min(2), chord_tolerance_m));
    }

    /// Add the coarse levels of a geometry this pack just stored.
    fn add_levels(&mut self, id: u32) {
        let Some((levels, chord)) = self.lod else {
            return;
        };
        let Some(mut indices) = lod_indices(&self.writer, id, chord, levels) else {
            return;
        };
        for (level, coarse) in indices.drain(..).enumerate() {
            self.writer.add_geometry_lod(id, coarse, level as u8 + 1);
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
                    material: part.material.clone(),
                },
            );
        }
    }

    /// The pack's material index for a part's material, writing its texture
    /// once per stream.
    fn material_id(&mut self, express_id: u32, material: &tessifc_geom::Material) -> u32 {
        let texture = material.texture.as_ref().map(|texture| {
            if !self.writer.has_texture(texture.id) {
                let (mime, data) = match &texture.source {
                    TextureSource::Url(url) => (mime_of(url), TextureData::Uri(url.clone())),
                    TextureSource::Blob { format, bytes } => {
                        (mime_of(format), TextureData::Blob(bytes.clone()))
                    }
                    TextureSource::Pixels {
                        width,
                        height,
                        components,
                        bytes,
                    } => (
                        None,
                        TextureData::Pixels {
                            width: *width,
                            height: *height,
                            components: *components,
                            bytes: bytes.clone(),
                        },
                    ),
                };
                let size = match &data {
                    TextureData::Blob(bytes) => bytes.len(),
                    TextureData::Pixels { bytes, .. } => bytes.len(),
                    _ => 0,
                };
                let data = if self.embedded_texture_bytes + size > MAX_EMBEDDED_TEXTURE_BYTES {
                    self.writer.add_diagnostic(DiagnosticRecord {
                        express_id: Some(express_id),
                        line: 0,
                        severity: "warn".to_string(),
                        code: codes::TEXTURE_OMITTED.as_str().to_string(),
                        message: format!(
                            "texture #{} left out: the pack's embedded textures would exceed their limit",
                            texture.id
                        ),
                    });
                    TextureData::Omitted
                } else {
                    self.embedded_texture_bytes += size;
                    data
                };
                self.writer.add_texture(TextureRecord {
                    id: texture.id,
                    mime,
                    repeat: texture.repeat,
                    transform: texture.transform,
                    data,
                });
            }
            texture.id
        });
        self.writer.add_material(MaterialRecord {
            color: material.colour.0,
            diffuse: material.diffuse,
            specular: material.specular,
            shininess: material.shininess,
            roughness: material.roughness,
            reflectance: material.reflectance.clone(),
            texture,
            source: material.style,
        })
    }

    fn add_part(
        &mut self,
        express_id: u32,
        class: &str,
        category: ProductCategory,
        part: ShapePart,
    ) {
        let flags = instance_flags_for_category(category, part.color);
        let material = part
            .material
            .as_ref()
            .map(|material| self.material_id(express_id, material));
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
                        // The locality pass permutes vertices, so the shared
                        // mesh is reordered once as a whole and narrowed after.
                        let mut ordered = (*mesh).clone();
                        tessifc_mesh::optimize_vertex_locality(&mut ordered);
                        let positions: Vec<f32> = ordered
                            .positions
                            .iter()
                            .flat_map(|point| [point.x as f32, point.y as f32, point.z as f32])
                            .collect();
                        let uvs = flat_uvs(&ordered);
                        let before = self.writer.geometry_count();
                        let id = self.writer.add_geometry_owned_closed_uv(
                            positions,
                            ordered.indices,
                            closed,
                            uvs,
                        );
                        if self.writer.geometry_count() > before {
                            self.add_levels(id);
                        }
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
                let uvs = flat_uvs(&mesh);
                let before = self.writer.geometry_count();
                let id =
                    self.writer
                        .add_geometry_owned_closed_uv(positions, mesh.indices, closed, uvs);
                if self.writer.geometry_count() > before {
                    self.add_levels(id);
                }
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
            material,
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

/// The coarse index lists of a stored geometry, one per level, or `None`
/// when the mesh is too small, refused, or when the first level would not
/// remove a quarter of its triangles. Level 2 simplifies level 1 again at
/// four times the tolerance.
pub fn lod_indices(
    writer: &IgpWriter,
    id: u32,
    chord_tolerance_m: f64,
    levels: u8,
) -> Option<Vec<Vec<u32>>> {
    let (positions, indices) = writer.geometry_data(id)?;
    if indices.len() < tessifc_mesh::LOD_MIN_TRIANGLES * 3 {
        return None;
    }
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for vertex in positions.chunks_exact(3) {
        for axis in 0..3 {
            lo[axis] = lo[axis].min(vertex[axis]);
            hi[axis] = hi[axis].max(vertex[axis]);
        }
    }
    let diagonal = (0..3)
        .map(|axis| ((hi[axis] - lo[axis]) as f64).powi(2))
        .sum::<f64>()
        .sqrt();
    let tolerance = (2.0 * chord_tolerance_m).max(diagonal / 256.0);
    if tolerance.is_nan() || tolerance <= 0.0 || !tolerance.is_finite() {
        return None;
    }
    let mut out = Vec::new();
    let mut current: Vec<u32> = indices.to_vec();
    for level in 0..levels.min(2) {
        let options = tessifc_mesh::DecimateOptions {
            target_ratio: tessifc_mesh::LOD_TARGET_RATIO,
            tolerance: tolerance * if level == 0 { 1.0 } else { 4.0 },
            max_triangles: tessifc_mesh::MAX_DECIMATE_TRIANGLES,
        };
        let Some(coarse) = tessifc_mesh::decimate_f32(positions, &current, &options) else {
            break;
        };
        current = coarse.clone();
        out.push(coarse);
    }
    (!out.is_empty()).then_some(out)
}

/// A mesh's texture coordinates as the writer takes them: two floats per
/// vertex, or nothing.
fn flat_uvs(mesh: &tessifc_mesh::Mesh64) -> Vec<f32> {
    if !mesh.has_uvs() {
        return Vec::new();
    }
    mesh.uvs.iter().flat_map(|uv| [uv[0], uv[1]]).collect()
}

/// The media type an image format name or file name implies, when it does.
fn mime_of(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let extension = lower.rsplit('.').next().unwrap_or(&lower);
    let mime = match extension.trim() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        _ => return None,
    };
    Some(mime.to_string())
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

    /// A quad styled with a rendering and a pixel texture, mapped by an
    /// indexed triangle texture map, as a wall's body.
    const TEXTURED_WALL: &str = "#1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,0.,0.),(1.,1.,0.),(0.,1.,0.)));
         #2=IFCTRIANGULATEDFACESET(#1,$,.F.,((1,2,3),(1,3,4)),$);
         #3=IFCTEXTUREVERTEXLIST(((0.,0.),(1.,0.),(1.,1.),(0.,1.)));
         #4=IFCINDEXEDTRIANGLETEXTUREMAP((#10),#2,#3,((1,2,3),(1,3,4)));
         #10=IFCPIXELTEXTURE(.T.,.T.,$,$,$,2,1,3,(\"0FF0000\",\"00000FF\"));
         #13=IFCCOLOURRGB($,0.8,0.2,0.1);
         #14=IFCSURFACESTYLERENDERING(#13,0.,IFCNORMALISEDRATIOMEASURE(0.5),$,$,$,$,IFCSPECULAREXPONENT(32.),.BLINN.);
         #16=IFCSURFACESTYLEWITHTEXTURES((#10));
         #17=IFCSURFACESTYLE('brick',.BOTH.,(#14,#16));
         #18=IFCSTYLEDITEM(#2,(#17),$);
         #20=IFCSHAPEREPRESENTATION($,'Body','Tessellation',(#2));
         #21=IFCPRODUCTDEFINITIONSHAPE($,$,(#20));
         #22=IFCWALL('w',$,$,$,$,$,#21,$,$);
";

    fn pack_of(model: &Model, textures: bool) -> Vec<u8> {
        let settings = tessifc_geom::Settings {
            textures,
            ..tessifc_geom::Settings::default()
        };
        let result = Engine::with_settings(settings).evaluate(model);
        let mut packer = Packer::new("IFC4", 1.0, result.model_offset);
        for shape in result.shapes {
            packer.add_shape(shape);
        }
        packer.finish()
    }

    #[test]
    fn a_textured_wall_writes_its_material_texture_and_coordinates() {
        let model = model_of(TEXTURED_WALL);
        let bytes = pack_of(&model, true);
        let json = parse_json(&bytes);
        let geometry = &json["geometries"][0];
        assert_eq!(geometry["uv"]["count"], 4);
        assert_eq!(json["instances"]["material"]["type"], "u32");
        let material = &json["materials"][0];
        assert_eq!(material["color"], serde_json::json!([204, 51, 26, 255]));
        assert_eq!(material["shininess"], 32.0);
        assert_eq!(material["reflectance"], "BLINN");
        assert_eq!(material["texture"], 10);
        assert_eq!(material["source"], 17);
        let texture = &json["textures"][0];
        assert_eq!(texture["id"], 10);
        assert_eq!(texture["pixels"]["width"], 2);
        assert_eq!(texture["pixels"]["components"], 3);
        assert_eq!(texture["repeat"], serde_json::json!([true, true]));
        // The sections sit on eight-byte boundaries inside the binary chunk.
        let json_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let binary_start = 24 + json_len.div_ceil(8) * 8;
        let uv_at = geometry["uv"]["off"].as_u64().unwrap() as usize;
        let pixels_at = texture["pixels"]["off"].as_u64().unwrap() as usize;
        assert_eq!(uv_at % 8, 0);
        assert_eq!(pixels_at % 8, 0);
        assert_eq!(
            &bytes[binary_start + pixels_at..binary_start + pixels_at + 6],
            &[255, 0, 0, 0, 0, 255]
        );
        let material_at = json["instances"]["material"]["off"].as_u64().unwrap() as usize;
        assert_eq!(
            u32::from_le_bytes(
                bytes[binary_start + material_at..binary_start + material_at + 4]
                    .try_into()
                    .unwrap()
            ),
            0
        );
    }

    #[test]
    fn a_pack_without_textures_has_none_of_the_new_members() {
        let model = model_of(TEXTURED_WALL);
        let json = parse_json(&pack_of(&model, false));
        assert!(json["geometries"][0].get("uv").is_none());
        assert!(json["instances"].get("material").is_none());
        assert!(json.get("materials").is_none());
        assert!(json.get("textures").is_none());
    }

    #[test]
    fn a_streamed_texture_is_written_once() {
        let model = model_of(TEXTURED_WALL);
        let settings = tessifc_geom::Settings {
            textures: true,
            ..tessifc_geom::Settings::default()
        };
        let engine = Engine::with_settings(settings);
        let mut session = engine.session(&model);
        let first = session.next(&model, |_| true);
        let mut packer = Packer::new("IFC4", 1.0, session.model_offset());
        for shape in first.shapes {
            packer.add_shape(shape);
        }
        let (bytes, state) = packer.finish_chunk();
        assert_eq!(parse_json(&bytes)["textures"].as_array().unwrap().len(), 1);
        let mut next = Packer::continue_stream("IFC4", 1.0, session.model_offset(), state);
        let result = Engine::with_settings(tessifc_geom::Settings {
            textures: true,
            ..tessifc_geom::Settings::default()
        })
        .evaluate(&model);
        for shape in result.shapes {
            next.add_shape(shape);
        }
        let json = parse_json(&next.finish());
        assert!(
            json.get("textures").is_none(),
            "already written by the first chunk"
        );
        assert_eq!(
            json["materials"][0]["texture"], 10,
            "and still referred to by id"
        );
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
                material: None,
            }],
            color,
        });

        assert_eq!(packer.instance_count(), 0);
        assert_eq!(packer.geometry_count(), 0);
        let json = parse_json(&packer.finish());
        assert_eq!(json["diagnostics"][0]["id"], 7);
        assert_eq!(json["diagnostics"][0]["code"], "W_NON_FINITE_GEOMETRY");
    }

    /// A column: an extruded circle, whose fine tessellation has thousands of triangles.
    const COLUMN: &str = "#1=IFCCIRCLEPROFILEDEF(.AREA.,$,$,1.);
         #2=IFCDIRECTION((0.,0.,1.));
         #3=IFCEXTRUDEDAREASOLID(#1,$,#2,3.);
         #4=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));
         #5=IFCPRODUCTDEFINITIONSHAPE($,$,(#4));
         #6=IFCCOLUMN('c',$,$,$,$,$,#5,$,$);
";

    fn column_pack(levels: u8) -> serde_json::Value {
        let model = model_of(COLUMN);
        let settings = tessifc_geom::Settings {
            circle_segments: Some(512),
            lod_levels: levels,
            ..tessifc_geom::Settings::default()
        };
        let result = Engine::with_settings(settings).evaluate(&model);
        parse_json(&crate::report::pack_evaluation("IFC4", &result))
    }

    #[test]
    fn coarse_levels_are_written_only_when_asked_and_count_no_triangles() {
        let plain = column_pack(0);
        assert_eq!(plain["geometries"].as_array().unwrap().len(), 1);
        assert!(plain["geometries"][0].get("lod").is_none());
        let fine_triangles = plain["geometries"][0]["indices"]["count"].as_u64().unwrap() / 3;
        assert!(
            fine_triangles >= 1024,
            "the column is large enough for a level ({fine_triangles})"
        );

        let levelled = column_pack(2);
        let entries = levelled["geometries"].as_array().unwrap();
        assert_eq!(entries.len(), 3, "the base and two levels");
        let base = entries[0]["id"].as_u64().unwrap();
        assert_eq!(
            entries[1]["lod"],
            serde_json::json!({ "of": base, "level": 1 })
        );
        assert_eq!(
            entries[2]["lod"],
            serde_json::json!({ "of": base, "level": 2 })
        );
        let first = entries[1]["indices"]["count"].as_u64().unwrap() / 3;
        let second = entries[2]["indices"]["count"].as_u64().unwrap() / 3;
        assert!(
            first * 4 <= fine_triangles * 3,
            "level 1 removes at least a quarter ({first} of {fine_triangles})"
        );
        assert!(
            second < first,
            "level 2 is coarser than level 1 ({second} of {first})"
        );
        assert_eq!(
            entries[1]["positions"], entries[0]["positions"],
            "levels share the base positions"
        );
        assert_eq!(
            levelled["instances"]["count"], 1,
            "no instance places a level"
        );
        assert_eq!(
            levelled["stats"]["triangles"], plain["stats"]["triangles"],
            "the stats count base triangles only"
        );
    }
}
