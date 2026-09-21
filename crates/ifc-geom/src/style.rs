// SPDX-License-Identifier: Apache-2.0
//! Colours: the item's style, its mapped source's style, the product's material,
//! then a class palette. The IFC2X3 `IfcPresentationStyleAssignment` indirection
//! is read in every schema, since IFC4 files from older exporters still use it.

use crate::context::EvalCtx;
use std::sync::Arc;
use tessifc_model::{Entity, Model, Relation, Value};

/// A colour, straight-alpha, 0-255.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    /// True when the colour is not fully opaque.
    pub fn is_transparent(&self) -> bool {
        self.0[3] < 255
    }
}

/// How a surface reflects light and what is painted on it, as the file says.
#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    /// The surface colour with its opacity.
    pub colour: Rgba,
    /// The diffuse colour, or the surface colour scaled by a factor.
    pub diffuse: Option<[f32; 3]>,
    /// The specular colour, or the surface colour scaled by a factor.
    pub specular: Option<[f32; 3]>,
    /// The specular exponent, when the highlight is one.
    pub shininess: Option<f32>,
    /// The specular roughness in 0..1, when the highlight is one.
    pub roughness: Option<f32>,
    /// The reflectance method's name, such as `BLINN` or `METAL`.
    pub reflectance: Option<String>,
    /// The first texture layer of the style.
    pub texture: Option<Arc<Texture>>,
    /// The `IfcSurfaceStyle` this came from.
    pub style: u32,
}

/// One texture layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Texture {
    /// The texture's express id.
    pub id: u32,
    /// Where its pixels are.
    pub source: TextureSource,
    /// Whether the texture repeats along s and along t.
    pub repeat: [bool; 2],
    /// A 2D affine transform of the coordinates, `[a, b, c, d, tx, ty]`
    /// with `s' = a s + c t + tx` and `t' = b s + d t + ty`.
    pub transform: Option<[f64; 6]>,
    /// How coordinates are generated when the mesh carries none.
    pub generator: Option<TextureGenerator>,
}

/// Where a texture's pixels come from.
#[derive(Clone, Debug, PartialEq)]
pub enum TextureSource {
    /// A URL or relative path the file refers to.
    Url(String),
    /// An encoded image, with its format name as the file gives it.
    Blob {
        /// The `RasterFormat`, such as `PNG` or `JPG`.
        format: String,
        /// The encoded bytes.
        bytes: Vec<u8>,
    },
    /// Raw pixels, `components` bytes each, rows from the bottom.
    Pixels {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// Bytes per pixel: 1 to 4.
        components: u8,
        /// The pixel bytes, `width * height * components` of them.
        bytes: Vec<u8>,
    },
}

/// An `IfcTextureCoordinateGenerator` on a texture.
#[derive(Clone, Debug, PartialEq)]
pub struct TextureGenerator {
    /// The generator's mode, such as `COORD`.
    pub mode: String,
}

/// Upper bound on the pixels a pixel texture may declare.
const MAX_TEXTURE_PIXELS: u64 = 1 << 26;

/// The material of one representation item, when the file styles it.
///
/// `None` says the item carries no surface style of its own; the caller's
/// colour fallbacks still apply.
pub(crate) fn item_material(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Option<Arc<Material>> {
    let style = styled_surface_style(ctx.model, item.id()).or_else(|| {
        // A mapped item's style lives on the items inside the representation it maps.
        let source = item.attr("MappingSource").as_entity()?;
        let representation = source.attr("MappedRepresentation").as_entity()?;
        representation_surface_style(ctx, representation)
    })?;
    ctx.material_of(style, || read_material(ctx, style))
}

/// The `IfcSurfaceStyle` behind whatever `IfcStyledItem` points at this express id.
fn styled_surface_style(model: &Model, express_id: u32) -> Option<u32> {
    for styled in model.inverse().get(Relation::Styles, express_id) {
        let Some(entity) = model.entity(*styled) else {
            continue;
        };
        if let Some(style) = surface_style_of(entity.attr("Styles"), 0) {
            return Some(style);
        }
    }
    None
}

fn representation_surface_style(ctx: &EvalCtx<'_>, representation: Entity<'_>) -> Option<u32> {
    let items = representation.attr("Items").as_list()?;
    for value in items {
        let Some(item) = value.as_entity() else {
            continue;
        };
        if let Some(style) = styled_surface_style(ctx.model, item.id()) {
            return Some(style);
        }
        if item.is_a("IfcMappedItem")
            && let Some(source) = item.attr("MappingSource").as_entity()
            && let Some(inner) = source.attr("MappedRepresentation").as_entity()
            && let Some(style) = ctx
                .nested(|| Ok(representation_surface_style(ctx, inner)))
                .ok()
                .flatten()
        {
            return Some(style);
        }
    }
    None
}

/// The first `IfcSurfaceStyle` in a style assignment, through the IFC2X3 indirection.
fn surface_style_of(value: Value<'_>, depth: usize) -> Option<u32> {
    if depth >= MAX_STYLE_DEPTH {
        return None;
    }
    for entry in value.as_list()? {
        let Some(style) = entry.as_entity() else {
            continue;
        };
        if style.is_a("IfcPresentationStyleAssignment") {
            if let Some(found) = surface_style_of(style.attr("Styles"), depth + 1) {
                return Some(found);
            }
            continue;
        }
        if style.is_a("IfcSurfaceStyle") && surface_style_colour(style).is_some() {
            return Some(style.id());
        }
    }
    None
}

/// Read an `IfcSurfaceStyle` into a material: the rendering's extras and the
/// first texture layer. `None` when it has no colour at all.
fn read_material(ctx: &EvalCtx<'_>, style_id: u32) -> Option<Material> {
    let style = ctx.model.entity(style_id)?;
    let colour = surface_style_colour(style)?;
    let mut material = Material {
        colour,
        diffuse: None,
        specular: None,
        shininess: None,
        roughness: None,
        reflectance: None,
        texture: None,
        style: style_id,
    };
    let base = [
        f32::from(colour.0[0]) / 255.0,
        f32::from(colour.0[1]) / 255.0,
        f32::from(colour.0[2]) / 255.0,
    ];
    for entry in style.attr("Styles").as_list()? {
        let Some(part) = entry.as_entity() else {
            continue;
        };
        if part.is_a("IfcSurfaceStyleRendering") {
            material.diffuse = colour_or_factor(part.attr("DiffuseColour"), base);
            material.specular = colour_or_factor(part.attr("SpecularColour"), base);
            match part.attr("SpecularHighlight") {
                Value::Typed(typed) if typed.is("IFCSPECULARROUGHNESS") => {
                    material.roughness = typed
                        .value()
                        .as_f64()
                        .filter(|value| value.is_finite())
                        .map(|value| value.clamp(0.0, 1.0) as f32);
                }
                Value::Typed(typed) if typed.is("IFCSPECULAREXPONENT") => {
                    material.shininess = typed
                        .value()
                        .as_f64()
                        .filter(|value| value.is_finite() && *value >= 0.0)
                        .map(|value| value as f32);
                }
                other => {
                    material.shininess = other
                        .as_f64()
                        .filter(|value| value.is_finite() && *value >= 0.0)
                        .map(|value| value as f32);
                }
            }
            material.reflectance = part
                .attr("ReflectanceMethod")
                .as_text()
                .map(|text| text.decode())
                .filter(|name| !name.is_empty() && name != "NOTDEFINED");
        }
        if part.is_a("IfcSurfaceStyleWithTextures") && material.texture.is_none() {
            let mut layers = 0usize;
            for (index, texture) in part
                .attr("Textures")
                .as_list()
                .into_iter()
                .flatten()
                .enumerate()
            {
                layers = index + 1;
                if index > 0 {
                    continue;
                }
                if let Some(texture) = texture.as_entity() {
                    material.texture = ctx.texture_of(texture.id(), || read_texture(ctx, texture));
                }
            }
            if layers > 1 {
                ctx.diag.info(
                    crate::error::codes::TEXTURE_LAYERS_IGNORED,
                    style_id,
                    format!("{} texture layers; only the first is carried", layers),
                );
            }
        }
    }
    Some(material)
}

/// An `IfcColourOrFactor`: a colour of its own, or a factor of the base colour.
fn colour_or_factor(value: Value<'_>, base: [f32; 3]) -> Option<[f32; 3]> {
    if let Some(entity) = value.as_entity() {
        let [red, green, blue] = colour_rgb(entity)?;
        return Some([
            f32::from(red) / 255.0,
            f32::from(green) / 255.0,
            f32::from(blue) / 255.0,
        ]);
    }
    let factor = value.as_f64().filter(|value| value.is_finite())?;
    let factor = factor.clamp(0.0, 1.0) as f32;
    Some([base[0] * factor, base[1] * factor, base[2] * factor])
}

/// Read one `IfcSurfaceTexture`; `None` when its pixels cannot be had.
fn read_texture(ctx: &EvalCtx<'_>, texture: Entity<'_>) -> Option<Texture> {
    let limit = ctx.settings.max_texture_bytes;
    let source = if texture.is_a("IfcImageTexture") {
        let url = texture
            .attr("URLReference")
            .as_text()
            .or_else(|| texture.attr("UrlReference").as_text())?
            .decode();
        if url.is_empty() {
            return None;
        }
        TextureSource::Url(url)
    } else if texture.is_a("IfcBlobTexture") {
        let format = texture
            .attr("RasterFormat")
            .as_text()
            .map(|text| text.decode())
            .unwrap_or_default();
        let bytes = decode_step_binary(texture.attr("RasterCode").as_text()?.raw(), limit)?;
        TextureSource::Blob { format, bytes }
    } else if texture.is_a("IfcPixelTexture") {
        let width = u32::try_from(texture.attr("Width").as_i64()?).ok()?;
        let height = u32::try_from(texture.attr("Height").as_i64()?).ok()?;
        let components = u8::try_from(texture.attr("ColourComponents").as_i64()?).ok()?;
        if width == 0 || height == 0 || !(1..=4).contains(&components) {
            return None;
        }
        let pixel_count = u64::from(width) * u64::from(height);
        let expected = pixel_count.checked_mul(u64::from(components))?;
        if pixel_count > MAX_TEXTURE_PIXELS || expected > limit as u64 {
            return None;
        }
        let mut bytes = Vec::with_capacity(expected as usize);
        for pixel in texture.attr("Pixel").as_list()? {
            let Some(text) = pixel.as_text() else {
                continue;
            };
            let decoded = decode_step_binary(text.raw(), limit)?;
            if decoded.len() != usize::from(components) {
                return None;
            }
            bytes.extend_from_slice(&decoded);
            if bytes.len() > expected as usize {
                return None;
            }
        }
        if bytes.len() != expected as usize {
            return None;
        }
        TextureSource::Pixels {
            width,
            height,
            components,
            bytes,
        }
    } else {
        return None;
    };
    let repeat = [
        texture.attr("RepeatS").as_bool().unwrap_or(true),
        texture.attr("RepeatT").as_bool().unwrap_or(true),
    ];
    let transform = texture
        .attr("TextureTransform")
        .as_entity()
        .and_then(texture_transform);
    let generator = ctx
        .generator_of(texture.id())
        .map(|mode| TextureGenerator { mode });
    Some(Texture {
        id: texture.id(),
        source,
        repeat,
        transform,
        generator,
    })
}

/// An `IfcCartesianTransformationOperator2D` as a 2D affine, `[a, b, c, d, tx, ty]`.
fn texture_transform(operator: Entity<'_>) -> Option<[f64; 6]> {
    let axis = |name: &str| -> Option<[f64; 2]> {
        let direction = operator.attr(name).as_entity()?;
        let mut values = direction.attr("DirectionRatios").as_list()?.floats();
        let x = values.next()?;
        let y = values.next().unwrap_or(0.0);
        let length = (x * x + y * y).sqrt();
        (length > 0.0 && length.is_finite()).then_some([x / length, y / length])
    };
    let scale = operator
        .attr("Scale")
        .as_f64()
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(1.0);
    let scale2 = operator
        .attr("Scale2")
        .as_f64()
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(scale);
    let axis1 = axis("Axis1").unwrap_or([1.0, 0.0]);
    let axis2 = axis("Axis2").unwrap_or([-axis1[1], axis1[0]]);
    let origin: [f64; 2] = operator
        .attr("LocalOrigin")
        .as_entity()
        .and_then(|point| {
            let mut values = point.attr("Coordinates").as_list()?.floats();
            Some([values.next()?, values.next().unwrap_or(0.0)])
        })
        .unwrap_or([0.0, 0.0]);
    let transform = [
        axis1[0] * scale,
        axis1[1] * scale,
        axis2[0] * scale2,
        axis2[1] * scale2,
        origin[0],
        origin[1],
    ];
    transform
        .iter()
        .all(|value| value.is_finite())
        .then_some(transform)
}

/// Decode a STEP binary literal: a leading count of unused bits, then hex digits.
///
/// `None` past `limit` bytes, or for anything that is not hexadecimal.
pub fn decode_step_binary(raw: &[u8], limit: usize) -> Option<Vec<u8>> {
    let digits = raw.get(1..)?;
    if !raw[0].is_ascii_digit() || !digits.len().is_multiple_of(2) || digits.len() / 2 > limit {
        return None;
    }
    let nibble = |byte: u8| (byte as char).to_digit(16).map(|value| value as u8);
    digits
        .chunks_exact(2)
        .map(|pair| Some(nibble(pair[0])? << 4 | nibble(pair[1])?))
        .collect()
}

/// The colour of one representation item, or `None` if the file does not say.
///
/// `product` is used only for the material fallback.
pub fn item_colour(ctx: &EvalCtx<'_>, item: Entity<'_>, product: Entity<'_>) -> Option<Rgba> {
    item_style(ctx, item).or_else(|| material_colour(ctx, product))
}

/// The colour the item carries in its own right, with no product fallback.
///
/// What the items inside a mapped representation need, since their colours differ.
pub(crate) fn item_style(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Option<Rgba> {
    if let Some(colour) = styled_colour(ctx.model, item.id()) {
        return Some(colour);
    }

    // A mapped item's style lives on the items inside the representation it maps.
    if item.is_a("IfcMappedItem")
        && let Some(source) = item.attr("MappingSource").as_entity()
        && let Some(representation) = source.attr("MappedRepresentation").as_entity()
        && let Some(colour) = representation_colour(ctx, representation)
    {
        return Some(colour);
    }

    None
}

/// What a part of a product is coloured when its own item says nothing.
///
/// The material first, then the class palette; never another item's colour.
pub(crate) fn fallback_colour(ctx: &EvalCtx<'_>, product: Entity<'_>) -> Rgba {
    material_colour(ctx, product).unwrap_or_else(|| class_colour(&product.class_name()))
}

/// The first colour any item of a representation carries.
fn representation_colour(ctx: &EvalCtx<'_>, representation: Entity<'_>) -> Option<Rgba> {
    let items = representation.attr("Items").as_list()?;
    for value in items {
        let Some(item) = value.as_entity() else {
            continue;
        };
        if let Some(colour) = styled_colour(ctx.model, item.id()) {
            return Some(colour);
        }
        if item.is_a("IfcMappedItem")
            && let Some(source) = item.attr("MappingSource").as_entity()
            && let Some(inner) = source.attr("MappedRepresentation").as_entity()
            && let Some(colour) = ctx
                .nested(|| Ok(representation_colour(ctx, inner)))
                .ok()
                .flatten()
        {
            return Some(colour);
        }
    }
    None
}

/// The colour of whatever `IfcStyledItem` points at this express id.
fn styled_colour(model: &Model, express_id: u32) -> Option<Rgba> {
    for styled in model.inverse().get(Relation::Styles, express_id) {
        let Some(entity) = model.entity(*styled) else {
            continue;
        };
        if let Some(colour) = colour_of_styles(entity.attr("Styles")) {
            return Some(colour);
        }
    }
    None
}

/// Bounds recursion through a self-referential `IfcPresentationStyleAssignment`.
const MAX_STYLE_DEPTH: usize = 16;

/// Walk an `IfcStyleAssignmentSelect` set down to a surface colour.
fn colour_of_styles(value: tessifc_model::Value<'_>) -> Option<Rgba> {
    colour_of_styles_at(value, 0)
}

fn colour_of_styles_at(value: tessifc_model::Value<'_>, depth: usize) -> Option<Rgba> {
    if depth >= MAX_STYLE_DEPTH {
        return None;
    }
    let list = value.as_list()?;
    for entry in list {
        let Some(style) = entry.as_entity() else {
            continue;
        };
        // IFC2X3: one more level of indirection, and IFC4 files still use it.
        if style.is_a("IfcPresentationStyleAssignment") {
            if let Some(colour) = colour_of_styles_at(style.attr("Styles"), depth + 1) {
                return Some(colour);
            }
            continue;
        }
        if style.is_a("IfcSurfaceStyle")
            && let Some(colour) = surface_style_colour(style)
        {
            return Some(colour);
        }
    }
    None
}

/// `IfcSurfaceStyle.Styles` holds the rendering or shading that has the colour.
fn surface_style_colour(style: Entity<'_>) -> Option<Rgba> {
    let list = style.attr("Styles").as_list()?;
    for entry in list {
        let Some(shading) = entry.as_entity() else {
            continue;
        };
        // Rendering is a subtype of Shading, so this covers both.
        if !shading.is_a("IfcSurfaceStyleShading") {
            continue;
        }
        let Some(rgb) = shading.attr("SurfaceColour").as_entity() else {
            continue;
        };
        let Some([red, green, blue]) = colour_rgb(rgb) else {
            continue;
        };
        // Transparency, not opacity: 0 is opaque and 1 is invisible.
        let transparency = shading.attr("Transparency").as_f64().unwrap_or(0.0);
        let alpha = ((1.0 - transparency.clamp(0.0, 1.0)) * 255.0).round() as u8;
        return Some(Rgba([red, green, blue, alpha]));
    }
    None
}

/// `IfcColourRgb`, whose components run 0 to 1.
fn colour_rgb(entity: Entity<'_>) -> Option<[u8; 3]> {
    let channel = |name: &str| -> Option<u8> {
        let value = entity.attr(name).as_f64()?;
        Some((value.clamp(0.0, 1.0) * 255.0).round() as u8)
    };
    Some([channel("Red")?, channel("Green")?, channel("Blue")?])
}

/// The colour of the product's material, if it has one that is styled.
///
/// Follows both a plain `IfcMaterial` and the layer-set wrappers a wall uses.
fn material_colour(ctx: &EvalCtx<'_>, product: Entity<'_>) -> Option<Rgba> {
    let model = ctx.model;
    let by_material = ctx.material_colours(material_colour_table);
    for material in model.inverse().get(Relation::Material, product.id()) {
        if let Some(colour) = by_material.get(material) {
            return Some(*colour);
        }
        // A wall points at a layer set usage; the colour is on the layers inside.
        let Some(entity) = model.entity(*material) else {
            continue;
        };
        for inner in layered_materials(entity) {
            if let Some(colour) = by_material.get(&inner) {
                return Some(*colour);
            }
        }
    }
    None
}

/// Materials reachable from a layer set, a layer set usage, or a material list.
fn layered_materials(entity: Entity<'_>) -> Vec<u32> {
    let mut found = Vec::new();
    let set = entity
        .attr("ForLayerSet")
        .as_entity()
        .or_else(|| entity.attr("MaterialLayers").as_entity())
        .unwrap_or(entity);
    for name in ["MaterialLayers", "Materials", "MaterialConstituents"] {
        let Some(list) = set.attr(name).as_list() else {
            continue;
        };
        for value in list {
            let Some(member) = value.as_entity() else {
                continue;
            };
            match member.attr("Material").as_entity() {
                Some(material) => found.push(material.id()),
                None => found.push(member.id()),
            }
        }
    }
    found
}

/// Every material that has a styled representation, mapped to its colour.
///
/// Built once, because the link runs from representation to material.
fn material_colour_table(model: &Model) -> std::collections::HashMap<u32, Rgba> {
    let mut table = std::collections::HashMap::new();
    for definition in model.entities_of_type("IfcMaterialDefinitionRepresentation") {
        let Some(subject) = definition.attr("RepresentedMaterial").as_entity() else {
            continue;
        };
        let Some(representations) = definition.attr("Representations").as_list() else {
            continue;
        };
        for value in representations {
            let Some(representation) = value.as_entity() else {
                continue;
            };
            let Some(items) = representation.attr("Items").as_list() else {
                continue;
            };
            for item in items {
                let Some(entity) = item.as_entity() else {
                    continue;
                };
                // Here the styled item usually *is* the representation item.
                let colour = if entity.is_a("IfcStyledItem") {
                    colour_of_styles(entity.attr("Styles"))
                } else {
                    styled_colour(model, entity.id())
                };
                if let Some(colour) = colour {
                    table.entry(subject.id()).or_insert(colour);
                }
            }
        }
    }
    table
}

/// A colour per IFC class, for a file that carries no styles at all.
///
/// A convention for legibility; opaque except for the classes seen through.
pub fn class_colour(class: &str) -> Rgba {
    Rgba(match class {
        "IfcWall" | "IfcWallStandardCase" | "IfcWallElementedCase" => [200, 195, 185, 255],
        "IfcSlab" | "IfcSlabStandardCase" => [160, 160, 160, 255],
        "IfcBeam" | "IfcColumn" | "IfcMember" | "IfcPlate" => [120, 130, 150, 255],
        "IfcDoor" | "IfcDoorStandardCase" => [150, 110, 70, 255],
        "IfcWindow" | "IfcWindowStandardCase" => [120, 180, 210, 160],
        "IfcSpace" => [180, 220, 180, 60],
        "IfcRoof" => [140, 90, 80, 255],
        "IfcStair" | "IfcStairFlight" | "IfcRamp" | "IfcRampFlight" => [170, 170, 175, 255],
        "IfcRailing" => [110, 110, 115, 255],
        "IfcFurnishingElement" | "IfcFurniture" => [190, 160, 130, 255],
        "IfcCovering" => [210, 210, 205, 255],
        "IfcCurtainWall" => [140, 190, 210, 140],
        "IfcFooting" | "IfcPile" => [130, 125, 120, 255],
        "IfcFlowSegment" | "IfcDuctSegment" | "IfcPipeSegment" => [190, 170, 90, 255],
        "IfcSite" => [150, 170, 130, 255],
        "IfcOpeningElement" => [220, 120, 120, 100],
        _ => [190, 190, 190, 255],
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::context::{DiagnosticSink, Settings, Tolerances};
    use crate::units::Units;
    use tessifc_step::{ParseOptions, parse};

    fn model_of(data: &str) -> Model {
        let source = format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{data}ENDSEC;\n"
        );
        Model::new(parse(source.as_bytes(), &ParseOptions::default()))
    }

    fn colour_of(model: &Model, item: u32, product: u32) -> Option<Rgba> {
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(
            model,
            Units::default(),
            Tolerances::default(),
            &settings,
            &sink,
        );
        item_colour(
            &ctx,
            model.entity(item).unwrap(),
            model.entity(product).unwrap(),
        )
    }

    #[test]
    fn a_self_referential_style_assignment_terminates() {
        // The assignment lists itself; the walk must stop at the depth ceiling.
        let model = model_of(
            "#1=IFCWALL('g',$,$,$,$,$,#2,$,$);
             #2=IFCPRODUCTDEFINITIONSHAPE($,$,(#3));
             #3=IFCSHAPEREPRESENTATION($,'Body','Tessellation',(#4));
             #4=IFCCARTESIANPOINT((0.,0.,0.));
             #13=IFCPRESENTATIONSTYLEASSIGNMENT((#13));
             #14=IFCSTYLEDITEM(#4,(#13),$);
",
        );
        assert!(colour_of(&model, 4, 1).is_none());
    }

    /// A red opaque surface style as #10..#13, applied to `item`.
    fn red_style(item: u32, assignment: bool) -> String {
        let styles = if assignment {
            format!(
                "#13=IFCPRESENTATIONSTYLEASSIGNMENT((#12));\n#14=IFCSTYLEDITEM(#{item},(#13),$);\n"
            )
        } else {
            format!("#14=IFCSTYLEDITEM(#{item},(#12),$);\n")
        };
        format!(
            "#10=IFCCOLOURRGB($,1.,0.,0.);\n\
             #11=IFCSURFACESTYLERENDERING(#10,0.,$,$,$,$,$,$,.NOTDEFINED.);\n\
             #12=IFCSURFACESTYLE('red',.BOTH.,(#11));\n{styles}"
        )
    }

    const WALL: &str = "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
         #2=IFCDIRECTION((0.,0.,1.));\n\
         #3=IFCEXTRUDEDAREASOLID(#1,$,#2,3.);\n\
         #9=IFCWALL('a',$,'W',$,$,$,$,$,$);\n";

    #[test]
    fn an_ifc4_styled_item_gives_its_colour() {
        let model = model_of(&format!("{WALL}{}", red_style(3, false)));
        assert_eq!(colour_of(&model, 3, 9), Some(Rgba([255, 0, 0, 255])));
    }

    #[test]
    fn the_ifc2x3_style_assignment_indirection_is_followed() {
        // The same file with IfcPresentationStyleAssignment in between.
        let model = model_of(&format!("{WALL}{}", red_style(3, true)));
        assert_eq!(
            colour_of(&model, 3, 9),
            Some(Rgba([255, 0, 0, 255])),
            "a reader that only knows IFC4 sees a grey building"
        );
    }

    #[test]
    fn transparency_is_not_opacity() {
        let model = model_of(&format!(
            "{WALL}#10=IFCCOLOURRGB($,0.,0.,1.);\n\
             #11=IFCSURFACESTYLERENDERING(#10,0.75,$,$,$,$,$,$,.NOTDEFINED.);\n\
             #12=IFCSURFACESTYLE('glass',.BOTH.,(#11));\n\
             #14=IFCSTYLEDITEM(#3,(#12),$);\n"
        ));
        let colour = colour_of(&model, 3, 9).unwrap();
        assert_eq!(
            colour.0[3], 64,
            "0.75 transparency is a quarter opaque, not three quarters"
        );
        assert!(colour.is_transparent());
    }

    #[test]
    fn shading_without_rendering_still_has_a_colour() {
        let model = model_of(&format!(
            "{WALL}#10=IFCCOLOURRGB($,0.,1.,0.);\n\
             #11=IFCSURFACESTYLESHADING(#10);\n\
             #12=IFCSURFACESTYLE('green',.BOTH.,(#11));\n\
             #14=IFCSTYLEDITEM(#3,(#12),$);\n"
        ));
        assert_eq!(colour_of(&model, 3, 9), Some(Rgba([0, 255, 0, 255])));
    }

    #[test]
    fn a_mapped_item_takes_the_colour_of_what_it_maps() {
        let model = model_of(&format!(
            "{WALL}#4=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #5=IFCAXIS2PLACEMENT3D(#4,$,$);\n\
             #6=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));\n\
             #7=IFCREPRESENTATIONMAP(#5,#6);\n\
             #8=IFCMAPPEDITEM(#7,$);\n{}",
            red_style(3, false)
        ));
        assert_eq!(
            colour_of(&model, 8, 9),
            Some(Rgba([255, 0, 0, 255])),
            "the style is on the mapped geometry, not on the placement of it"
        );
    }

    #[test]
    fn a_material_colour_is_the_next_fallback() {
        let model = model_of(&format!(
            "{WALL}#20=IFCMATERIAL('Brick');\n\
             #21=IFCRELASSOCIATESMATERIAL('m',$,$,$,(#9),#20);\n\
             #10=IFCCOLOURRGB($,0.5,0.25,0.125);\n\
             #11=IFCSURFACESTYLERENDERING(#10,0.,$,$,$,$,$,$,.NOTDEFINED.);\n\
             #12=IFCSURFACESTYLE('brick',.BOTH.,(#11));\n\
             #22=IFCSTYLEDITEM($,(#12),$);\n\
             #23=IFCSTYLEDREPRESENTATION($,'Style','Material',(#22));\n\
             #24=IFCMATERIALDEFINITIONREPRESENTATION($,$,(#23),#20);\n"
        ));
        assert_eq!(colour_of(&model, 3, 9), Some(Rgba([128, 64, 32, 255])));
    }

    #[test]
    fn a_file_with_no_styles_says_so() {
        let model = model_of(WALL);
        assert_eq!(
            colour_of(&model, 3, 9),
            None,
            "None, so the caller can pick a default"
        );
    }

    /// A red quad styled with rendering extras and a texture layer, as `#1..#18`.
    pub(crate) const TEXTURED_QUAD: &str = "#1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,0.,0.),(1.,1.,0.),(0.,1.,0.)));
         #2=IFCTRIANGULATEDFACESET(#1,$,.F.,((1,2,3),(1,3,4)),$);
         #3=IFCTEXTUREVERTEXLIST(((0.,0.),(1.,0.),(1.,1.),(0.,1.)));
         #4=IFCINDEXEDTRIANGLETEXTUREMAP((#10),#2,#3,((1,2,3),(1,3,4)));
         #10=IFCIMAGETEXTURE(.T.,.F.,'BUMP',#11,$,'bricks.png');
         #11=IFCCARTESIANTRANSFORMATIONOPERATOR2D($,$,#12,2.);
         #12=IFCCARTESIANPOINT((0.5,0.));
         #13=IFCCOLOURRGB($,0.8,0.2,0.1);
         #14=IFCSURFACESTYLERENDERING(#13,0.,IFCNORMALISEDRATIOMEASURE(0.5),$,$,$,#15,IFCSPECULARROUGHNESS(0.3),.METAL.);
         #15=IFCCOLOURRGB($,1.,1.,1.);
         #16=IFCSURFACESTYLEWITHTEXTURES((#10));
         #17=IFCSURFACESTYLE('brick',.BOTH.,(#14,#16));
         #18=IFCSTYLEDITEM(#2,(#17),$);
";

    fn material_of(model: &Model, item: u32, textures: bool) -> Option<Arc<Material>> {
        let settings = Settings {
            textures,
            ..Settings::default()
        };
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(
            model,
            Units::default(),
            Tolerances::default(),
            &settings,
            &sink,
        );
        item_material(&ctx, model.entity(item).unwrap())
    }

    #[test]
    fn a_rendering_styles_extras_and_its_texture_layer_are_read() {
        let model = model_of(TEXTURED_QUAD);
        let material = material_of(&model, 2, true).expect("a styled item");
        assert_eq!(material.colour, Rgba([204, 51, 26, 255]));
        assert_eq!(material.style, 17);
        // A factor scales the surface colour; a colour stands on its own.
        let diffuse = material.diffuse.unwrap();
        assert!((diffuse[0] - 0.4).abs() < 1e-6 && (diffuse[1] - 0.1).abs() < 1e-6);
        assert_eq!(material.specular, Some([1.0, 1.0, 1.0]));
        assert_eq!(material.roughness, Some(0.3));
        assert_eq!(material.shininess, None);
        assert_eq!(material.reflectance.as_deref(), Some("METAL"));
        let texture = material.texture.as_ref().expect("a texture");
        assert_eq!(texture.id, 10);
        assert_eq!(texture.repeat, [true, false]);
        assert_eq!(texture.source, TextureSource::Url("bricks.png".into()));
        let transform = texture.transform.unwrap();
        assert_eq!(transform, [2.0, 0.0, 0.0, 2.0, 0.5, 0.0]);
        assert!(texture.generator.is_none());
    }

    #[test]
    fn pixel_and_blob_textures_are_decoded_and_bounded() {
        let model = model_of(&format!(
            "{TEXTURED_QUAD}#30=IFCPIXELTEXTURE(.T.,.T.,$,$,$,2,2,3,(\"0FF0000\",\"000FF00\",\"00000FF\",\"0FFFFFF\"));
             #31=IFCBLOBTEXTURE(.F.,.F.,$,$,$,'PNG',\"089504E470D0A1A0A\");
             #32=IFCSURFACESTYLEWITHTEXTURES((#30,#31));
             #33=IFCSURFACESTYLE('pixels',.BOTH.,(#14,#32));
             #34=IFCCARTESIANPOINT((0.,0.,0.));
             #35=IFCSTYLEDITEM(#34,(#33),$);
             #36=IFCSURFACESTYLEWITHTEXTURES((#31));
             #37=IFCSURFACESTYLE('blob',.BOTH.,(#14,#36));
             #38=IFCCARTESIANPOINT((1.,0.,0.));
             #39=IFCSTYLEDITEM(#38,(#37),$);
             #40=IFCTEXTURECOORDINATEGENERATOR((#31),'COORD',$);
"
        ));
        let pixels = material_of(&model, 34, true).unwrap();
        match &pixels.texture.as_ref().unwrap().source {
            TextureSource::Pixels {
                width,
                height,
                components,
                bytes,
            } => {
                assert_eq!((*width, *height, *components), (2, 2, 3));
                assert_eq!(bytes, &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
            }
            other => panic!("expected pixels, got {other:?}"),
        }
        let blob = material_of(&model, 38, true).unwrap();
        let texture = blob.texture.as_ref().unwrap();
        assert_eq!(
            texture.source,
            TextureSource::Blob {
                format: "PNG".into(),
                bytes: vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
            }
        );
        assert_eq!(
            texture.generator.as_ref().map(|g| g.mode.as_str()),
            Some("COORD")
        );
        assert_eq!(decode_step_binary(b"0ABC", 16), None, "an odd digit count");
        assert_eq!(decode_step_binary(b"0ABCD", 1), None, "over the limit");
        assert_eq!(decode_step_binary(b"0ABCD", 2), Some(vec![0xab, 0xcd]));
    }

    #[test]
    fn textures_off_reads_no_material() {
        let model = model_of(TEXTURED_QUAD);
        assert!(material_of(&model, 2, true).is_some());
        // The colour path is untouched either way.
        assert_eq!(
            item_colour(
                &EvalCtx::new(
                    &model,
                    Units::default(),
                    Tolerances::default(),
                    &Settings::default(),
                    &DiagnosticSink::default()
                ),
                model.entity(2).unwrap(),
                model.entity(2).unwrap()
            ),
            Some(Rgba([204, 51, 26, 255]))
        );
    }

    #[test]
    fn the_class_palette_is_opaque_except_where_it_should_not_be() {
        assert!(!class_colour("IfcWall").is_transparent());
        assert!(
            class_colour("IfcSpace").is_transparent(),
            "rooms have to be seen through"
        );
        assert!(class_colour("IfcWindow").is_transparent());
        assert_eq!(
            class_colour("IfcNotARealClass"),
            class_colour("IfcBuildingElementProxy")
        );
    }
}
