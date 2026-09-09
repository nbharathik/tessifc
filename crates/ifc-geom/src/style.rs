// SPDX-License-Identifier: Apache-2.0
//! Colours: the item's style, its mapped source's style, the product's material,
//! then a class palette. The IFC2X3 `IfcPresentationStyleAssignment` indirection
//! is read in every schema, since IFC4 files from older exporters still use it.

use crate::context::EvalCtx;
use tessifc_model::{Entity, Model, Relation};

/// A colour, straight-alpha, 0-255.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    /// True when the colour is not fully opaque.
    pub fn is_transparent(&self) -> bool {
        self.0[3] < 255
    }
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
mod tests {
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
