// SPDX-License-Identifier: Apache-2.0
//! Tessellated face sets: geometry that is already triangles.
//!
//! Every index in them is one-based.

use crate::context::EvalCtx;
use crate::error::GeomError;
use crate::registry::{Registry, SolidEvaluator};
use glam::DVec3;
use std::collections::HashMap;
use tessifc_mesh::{Mesh64, triangulate_face, weld_and_close};
use tessifc_model::Entity;

/// The texture coordinates an indexed texture map gives a face set.
struct TextureMap {
    /// The `IfcTextureVertexList`, as read.
    coords: Vec<[f32; 2]>,
    /// Per triangle of an `IfcTriangulatedFaceSet`: zero-based coordinate
    /// indices, or `None` when the map follows `CoordIndex`.
    triangles: Option<Vec<Vec<usize>>>,
    /// Per `IfcIndexedPolygonalFace` id: zero-based coordinate indices.
    faces: HashMap<u32, Vec<usize>>,
}

/// The indexed texture map of a face set, when textures are wanted and it has one.
fn texture_map(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Option<TextureMap> {
    if !ctx.settings.textures {
        return None;
    }
    let map = ctx.model.entity(ctx.texture_map_of(item.id())?)?;
    let coords: Vec<[f32; 2]> = map
        .attr("TexCoords")
        .as_entity()?
        .attr("TexCoordsList")
        .as_list()?
        .filter_map(|row| {
            let mut values = row.as_list()?.floats();
            Some([values.next()? as f32, values.next()? as f32])
        })
        .collect();
    let zero_based = |value: tessifc_model::Value<'_>| -> Option<usize> {
        usize::try_from(value.as_i64()?.checked_sub(1)?)
            .ok()
            .filter(|index| *index < coords.len())
    };
    let mut triangles = None;
    let mut faces = HashMap::new();
    if map.is_a("IfcIndexedTriangleTextureMap") {
        triangles = map.attr("TexCoordIndex").as_list().map(|rows| {
            rows.map(|row| {
                row.as_list()
                    .map(|values| values.filter_map(zero_based).collect())
                    .unwrap_or_default()
            })
            .collect()
        });
    } else {
        for value in map.attr("TexCoordIndices").as_list()? {
            let Some(entry) = value.as_entity() else {
                continue;
            };
            let Some(face) = entry.attr("TexCoordsOf").as_entity() else {
                continue;
            };
            let indices: Vec<usize> = entry
                .attr("TexCoordIndex")
                .as_list()
                .map(|values| values.filter_map(zero_based).collect())
                .unwrap_or_default();
            faces.insert(face.id(), indices);
        }
    }
    Some(TextureMap {
        coords,
        triangles,
        faces,
    })
}

/// Coordinates for a face's `corners` corner indices, when the map has them all.
fn face_uvs(map: &TextureMap, indices: &[usize], corners: usize) -> Option<Vec<[f32; 2]>> {
    (indices.len() == corners && corners >= 3)
        .then(|| indices.iter().map(|&index| map.coords[index]).collect())
}

/// Read the coordinates of an `IfcCartesianPointList2D` or `3D`.
fn coordinates(ctx: &EvalCtx<'_>, entity: Entity<'_>) -> Result<Vec<DVec3>, GeomError> {
    let list = entity
        .attr("CoordList")
        .as_list()
        .ok_or_else(|| GeomError::missing("CoordList"))?;
    let mut points = Vec::new();
    for row in list {
        let Some(mut values) = row.as_list().map(|list| list.floats()) else {
            continue;
        };
        let x = values.next().unwrap_or(0.0);
        let y = values.next().unwrap_or(0.0);
        let z = values.next().unwrap_or(0.0);
        points.push(DVec3::new(
            ctx.units.length(x),
            ctx.units.length(y),
            ctx.units.length(z),
        ));
    }
    if points.is_empty() {
        return Err(GeomError::Degenerate("an empty coordinate list".into()));
    }
    Ok(points)
}

/// Resolve a one-based index, optionally through a `PnIndex` indirection.
///
/// Face indices point into `PnIndex` when present, else straight at coordinates.
fn resolve(index: i64, pn: &Option<Vec<i64>>, points: usize) -> Option<usize> {
    // Checked: a hostile i64::MIN would overflow the one-based subtraction.
    let first = usize::try_from(index.checked_sub(1)?).ok()?;
    let resolved = match pn {
        Some(table) => usize::try_from(table.get(first)?.checked_sub(1)?).ok()?,
        None => first,
    };
    (resolved < points).then_some(resolved)
}

/// `IfcTriangulatedFaceSet`: coordinates plus a list of index triples.
pub struct TriangulatedFaceSet;

impl SolidEvaluator for TriangulatedFaceSet {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcTriangulatedFaceSet"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        // A texture map gives corners their own coordinates, so the shared
        // vertices are split face by face and welded back where they agree.
        if ctx.settings.textures && ctx.texture_map_of(item.id()).is_some() {
            let faces = triangulated_faces(ctx, item)?;
            let members: Vec<&Face> = faces.iter().collect();
            return finish(ctx, item, mesh_of_faces(ctx, &members));
        }
        let list = item
            .attr("Coordinates")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Coordinates"))?;
        let points = coordinates(ctx, list)?;
        let pn = index_table(item, "PnIndex");

        let faces = item
            .attr("CoordIndex")
            .as_list()
            .ok_or_else(|| GeomError::missing("CoordIndex"))?;
        let mut mesh = Mesh64::new();
        mesh.positions = points.clone();

        let mut skipped = 0;
        for row in faces {
            let Some(values) = row.as_list() else {
                continue;
            };
            let corners: Vec<usize> = values
                .filter_map(|value| value.as_i64())
                .filter_map(|index| resolve(index, &pn, points.len()))
                .collect();
            if corners.len() != 3 {
                skipped += 1;
                continue;
            }
            mesh.push_triangle(corners[0] as u32, corners[1] as u32, corners[2] as u32);
        }
        if skipped > 0 {
            ctx.diag.warn(
                crate::error::codes::DEGENERATE_GEOMETRY,
                item.id(),
                format!("{skipped} face(s) did not resolve to three valid vertices"),
            );
        }
        finish(ctx, item, mesh)
    }
}

/// One face of a set: an outer loop and its holes, as points.
struct Face {
    outer: Vec<DVec3>,
    holes: Vec<Vec<DVec3>>,
    /// The entity to blame when the face will not triangulate.
    id: u32,
    /// Texture coordinates of the outer loop's corners, when the file maps them.
    uvs: Option<Vec<[f32; 2]>>,
}

/// The faces of an `IfcPolygonalFaceSet` in file order.
fn polygonal_faces(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Vec<Face>, GeomError> {
    let list = item
        .attr("Coordinates")
        .as_entity()
        .ok_or_else(|| GeomError::missing("Coordinates"))?;
    let points = coordinates(ctx, list)?;
    let pn = index_table(item, "PnIndex");
    let faces = item
        .attr("Faces")
        .as_list()
        .ok_or_else(|| GeomError::missing("Faces"))?;
    let map = texture_map(ctx, item);
    let mut unmapped = 0usize;
    let mut out = Vec::new();
    for value in faces {
        let Some(face) = value.as_entity() else {
            continue;
        };
        let outer: Vec<DVec3> = face
            .attr("CoordIndex")
            .as_list()
            .map(|list| {
                list.filter_map(|value| value.as_i64())
                    .filter_map(|index| resolve(index, &pn, points.len()))
                    .map(|index| points[index])
                    .collect()
            })
            .unwrap_or_default();
        let uvs = map.as_ref().and_then(|map| {
            let found = map
                .faces
                .get(&face.id())
                .and_then(|indices| face_uvs(map, indices, outer.len()));
            if found.is_none() && outer.len() >= 3 {
                unmapped += 1;
            }
            found
        });
        // A face too small to triangulate still keeps its slot for the colour index.
        let mut holes: Vec<Vec<DVec3>> = Vec::new();
        // IfcIndexedPolygonalFaceWithVoids carries its holes in InnerCoordIndices.
        if let Some(inner) = face.attr("InnerCoordIndices").as_list() {
            for row in inner {
                let Some(values) = row.as_list() else {
                    continue;
                };
                let hole: Vec<DVec3> = values
                    .filter_map(|value| value.as_i64())
                    .filter_map(|index| resolve(index, &pn, points.len()))
                    .map(|index| points[index])
                    .collect();
                if hole.len() >= 3 {
                    holes.push(hole);
                }
            }
        }
        out.push(Face {
            outer,
            holes,
            id: face.id(),
            uvs,
        });
    }
    if unmapped > 0 {
        ctx.diag.info(
            crate::error::codes::TEXTURE_MAP_IGNORED,
            item.id(),
            format!("{unmapped} face(s) have no matching texture coordinates"),
        );
    }
    Ok(out)
}

/// The faces of an `IfcTriangulatedFaceSet` in file order, one triangle each.
fn triangulated_faces(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Vec<Face>, GeomError> {
    let list = item
        .attr("Coordinates")
        .as_entity()
        .ok_or_else(|| GeomError::missing("Coordinates"))?;
    let points = coordinates(ctx, list)?;
    let pn = index_table(item, "PnIndex");
    let faces = item
        .attr("CoordIndex")
        .as_list()
        .ok_or_else(|| GeomError::missing("CoordIndex"))?;
    let map = texture_map(ctx, item);
    let mut unmapped = 0usize;
    let mut out = Vec::new();
    for (position, row) in faces.enumerate() {
        let Some(values) = row.as_list() else {
            continue;
        };
        // A map without its own indices follows the coordinate indices.
        let raw: Vec<i64> = values.filter_map(|value| value.as_i64()).collect();
        let outer: Vec<DVec3> = raw
            .iter()
            .filter_map(|&index| resolve(index, &pn, points.len()))
            .map(|index| points[index])
            .collect();
        let uvs = map.as_ref().and_then(|map| {
            let own: Vec<usize>;
            let indices: &[usize] = match &map.triangles {
                Some(rows) => rows.get(position).map(Vec::as_slice).unwrap_or(&[]),
                None => {
                    own = raw
                        .iter()
                        .filter_map(|&index| usize::try_from(index.checked_sub(1)?).ok())
                        .filter(|index| *index < map.coords.len())
                        .collect();
                    &own
                }
            };
            let found = face_uvs(map, indices, outer.len());
            if found.is_none() && outer.len() == 3 {
                unmapped += 1;
            }
            found
        });
        out.push(Face {
            outer: if outer.len() == 3 { outer } else { Vec::new() },
            holes: Vec::new(),
            id: item.id(),
            uvs,
        });
    }
    if unmapped > 0 {
        ctx.diag.info(
            crate::error::codes::TEXTURE_MAP_IGNORED,
            item.id(),
            format!("{unmapped} triangle(s) have no matching texture coordinates"),
        );
    }
    Ok(out)
}

/// Triangulate faces into one mesh; faces that will not triangulate are reported.
fn mesh_of_faces(ctx: &EvalCtx<'_>, faces: &[&Face]) -> Mesh64 {
    let mut mesh = Mesh64::new();
    for face in faces {
        if face.outer.len() < 3 {
            continue;
        }
        match triangulate_face(&face.outer, &face.holes) {
            Ok(indices) => {
                let base = mesh.positions.len() as u32;
                mesh.positions.extend_from_slice(&face.outer);
                for hole in &face.holes {
                    mesh.positions.extend_from_slice(hole);
                }
                crate::eval::uv::push_face_uvs(
                    &mut mesh,
                    base as usize,
                    &face.outer,
                    &face.holes,
                    face.uvs.as_deref(),
                );
                for triangle in indices.chunks_exact(3) {
                    mesh.push_triangle(base + triangle[0], base + triangle[1], base + triangle[2]);
                }
            }
            Err(error) => ctx.diag.warn(
                crate::error::codes::TRIANGULATION_FAILED,
                face.id,
                error.to_string(),
            ),
        }
    }
    mesh
}

/// A face set split by its `IfcIndexedColourMap`: one mesh per colour, in
/// first-seen order, with `None` for faces the map leaves out.
///
/// `None` when the item has no colour map, so the caller evaluates it whole.
pub fn coloured_parts(
    ctx: &EvalCtx<'_>,
    item: Entity<'_>,
) -> Option<Vec<(Option<crate::style::Rgba>, Mesh64)>> {
    let map = ctx.model.entity(ctx.colour_map_of(item.id())?)?;
    let faces = if item.is_a("IfcPolygonalFaceSet") {
        polygonal_faces(ctx, item).ok()?
    } else if item.is_a("IfcTriangulatedFaceSet") {
        triangulated_faces(ctx, item).ok()?
    } else {
        return None;
    };
    let colours: Vec<crate::style::Rgba> = {
        let list = map.attr("Colours").as_entity()?;
        let rows = list.attr("ColourList").as_list()?;
        let alpha = map
            .attr("Opacity")
            .as_f64()
            .map(|opacity| (opacity.clamp(0.0, 1.0) * 255.0).round() as u8)
            .unwrap_or(255);
        rows.filter_map(|row| {
            let mut values = row.as_list()?.floats();
            let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            Some(crate::style::Rgba([
                channel(values.next()?),
                channel(values.next()?),
                channel(values.next()?),
                alpha,
            ]))
        })
        .collect()
    };
    let indices: Vec<Option<usize>> = map
        .attr("ColourIndex")
        .as_list()?
        .map(|value| {
            value
                .as_i64()
                .and_then(|index| usize::try_from(index.checked_sub(1)?).ok())
                .filter(|index| *index < colours.len())
        })
        .collect();

    // Groups keep first-seen order so the pack is deterministic.
    let mut groups: Vec<(Option<usize>, Vec<&Face>)> = Vec::new();
    let mut slots: std::collections::HashMap<Option<usize>, usize> = Default::default();
    for (position, face) in faces.iter().enumerate() {
        let colour = indices.get(position).copied().flatten();
        match slots.get(&colour) {
            Some(&slot) => groups[slot].1.push(face),
            None => {
                slots.insert(colour, groups.len());
                groups.push((colour, vec![face]));
            }
        }
    }
    let mut parts = Vec::with_capacity(groups.len());
    for (colour, members) in groups {
        let mut mesh = mesh_of_faces(ctx, &members);
        if mesh.is_empty() {
            continue;
        }
        if ctx.settings.weld {
            weld_and_close(&mut mesh, ctx.tol.len);
        } else {
            mesh.closed = Some(mesh.is_edge_manifold());
        }
        parts.push((colour.map(|index| colours[index]), mesh));
    }
    Some(parts)
}

/// `IfcPolygonalFaceSet`: faces of any number of corners, optionally with voids.
pub struct PolygonalFaceSet;

impl SolidEvaluator for PolygonalFaceSet {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcPolygonalFaceSet"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let faces = polygonal_faces(ctx, item)?;
        let members: Vec<&Face> = faces.iter().collect();
        finish(ctx, item, mesh_of_faces(ctx, &members))
    }
}

/// Read an optional list of one-based indices.
fn index_table(item: Entity<'_>, name: &str) -> Option<Vec<i64>> {
    let list = item.attr(name).as_list()?;
    let values: Vec<i64> = list.filter_map(|value| value.as_i64()).collect();
    (!values.is_empty()).then_some(values)
}

/// Weld, decide closedness, and honour the declared `Closed` flag.
fn finish(ctx: &EvalCtx<'_>, item: Entity<'_>, mut mesh: Mesh64) -> Result<Mesh64, GeomError> {
    if mesh.is_empty() {
        return Err(GeomError::Degenerate(
            "a face set with no usable faces".into(),
        ));
    }
    if ctx.settings.weld {
        weld_and_close(&mut mesh, ctx.tol.len);
    } else {
        mesh.closed = Some(mesh.is_edge_manifold());
    }
    // Orientation is fixed only when the file says closed and the measurement agrees.
    if item.attr("Closed").as_bool() == Some(true) && mesh.closed == Some(true) {
        mesh.fix_orientation();
    }
    Ok(mesh)
}

/// Register the tessellated evaluators.
pub fn register(registry: &mut Registry) {
    registry.register_solid(Box::new(TriangulatedFaceSet));
    registry.register_solid(Box::new(PolygonalFaceSet));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::tests::{eval_solid, eval_textured_solid, model_of};

    /// The eight corners of a unit cube as a coordinate list.
    const CUBE_POINTS: &str = "((0.,0.,0.),(1.,0.,0.),(1.,1.,0.),(0.,1.,0.),\
                               (0.,0.,1.),(1.,0.,1.),(1.,1.,1.),(0.,1.,1.))";

    #[test]
    fn a_triangulated_cube_measures_right() {
        // One-based indices throughout.
        let model = model_of(&format!(
            "#1=IFCCARTESIANPOINTLIST3D({CUBE_POINTS},$);\n\
             #2=IFCTRIANGULATEDFACESET(#1,$,.T.,((1,3,2),(1,4,3),(5,6,7),(5,7,8),\
             (1,2,6),(1,6,5),(2,3,7),(2,7,6),(3,4,8),(3,8,7),(4,1,5),(4,5,8)),$);\n"
        ));
        let mesh = eval_solid(&model, 2).unwrap();
        assert_eq!(mesh.triangle_count(), 12);
        assert_eq!(mesh.closed, Some(true));
        assert!(
            (mesh.signed_volume() - 1.0).abs() < 1e-9,
            "got {}",
            mesh.signed_volume()
        );
        assert!((mesh.surface_area() - 6.0).abs() < 1e-9);
    }

    /// A unit cube face set with a texture map in which the front face, corners
    /// 1 2 6 5, has its own coordinates: a seam at each of those corners.
    fn textured_cube(map: &str) -> String {
        format!(
            "#1=IFCCARTESIANPOINTLIST3D({CUBE_POINTS},$);\n\
             #2=IFCTRIANGULATEDFACESET(#1,$,.T.,((1,3,2),(1,4,3),(5,6,7),(5,7,8),\
             (1,2,6),(1,6,5),(2,3,7),(2,7,6),(3,4,8),(3,8,7),(4,1,5),(4,5,8)),$);\n\
             #3=IFCTEXTUREVERTEXLIST(((0.,0.),(1.,0.),(1.,1.),(0.,1.),(0.,0.),(1.,0.),(1.,1.),(0.,1.),(0.5,0.5)));\n\
             {map}"
        )
    }

    #[test]
    fn a_texture_map_gives_the_quad_its_coordinates() {
        let model = model_of(crate::style::tests::TEXTURED_QUAD);
        let (mesh, diagnostics) = eval_textured_solid(&model, 2);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(mesh.has_uvs());
        assert_eq!(mesh.positions.len(), 4);
        let at = |x: f64, y: f64| {
            mesh.positions
                .iter()
                .position(|p| (p.x - x).abs() < 1e-9 && (p.y - y).abs() < 1e-9)
                .unwrap()
        };
        assert_eq!(mesh.uvs[at(1.0, 1.0)], [1.0, 1.0]);
        assert_eq!(mesh.uvs[at(0.0, 1.0)], [0.0, 1.0]);
        // The plain evaluation carries nothing.
        assert!(!eval_solid(&model, 2).unwrap().has_uvs());
    }

    #[test]
    fn a_map_without_its_own_indices_follows_the_coordinate_indices() {
        let model = model_of(&textured_cube(
            "#4=IFCINDEXEDTRIANGLETEXTUREMAP((#10),#2,#3,$);\n\
             #10=IFCIMAGETEXTURE(.T.,.T.,$,$,$,'a.png');\n",
        ));
        let (mesh, diagnostics) = eval_textured_solid(&model, 2);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(mesh.has_uvs());
        assert_eq!(
            mesh.positions.len(),
            8,
            "one coordinate per corner, no seam"
        );
        assert_eq!(mesh.closed, Some(true));
    }

    #[test]
    fn a_seam_keeps_vertices_apart_but_the_cube_stays_closed() {
        // The front face maps its corners to the ninth coordinate; the other
        // faces follow the corner numbers.
        let model = model_of(&textured_cube(
            "#4=IFCINDEXEDTRIANGLETEXTUREMAP((#10),#2,#3,((1,3,2),(1,4,3),(5,6,7),(5,7,8),\
             (9,9,9),(9,9,9),(2,3,7),(2,7,6),(3,4,8),(3,8,7),(4,1,5),(4,5,8)));\n\
             #10=IFCIMAGETEXTURE(.T.,.T.,$,$,$,'a.png');\n",
        ));
        let (mesh, diagnostics) = eval_textured_solid(&model, 2);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(mesh.has_uvs());
        assert_eq!(mesh.positions.len(), 12, "the four front corners are split");
        assert_eq!(mesh.closed, Some(true), "closedness is judged on positions");
        assert!((mesh.signed_volume() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_map_that_does_not_match_is_noted_and_left_out() {
        let model = model_of(&textured_cube(
            "#4=IFCINDEXEDTRIANGLETEXTUREMAP((#10),#2,#3,((1,2),(1,4,3)));\n\
             #10=IFCIMAGETEXTURE(.T.,.T.,$,$,$,'a.png');\n",
        ));
        let (mesh, diagnostics) = eval_textured_solid(&model, 2);
        let mesh = mesh.unwrap();
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == crate::error::codes::TEXTURE_MAP_IGNORED),
            "{diagnostics:?}"
        );
        // Eleven triangles are unmapped and get zeros; the mesh still carries coordinates.
        assert!(mesh.has_uvs());
        assert_eq!(mesh.closed, Some(true));
    }

    #[test]
    fn indices_are_one_based() {
        // A zero index is out of range in a one-based list, so the face is dropped.
        let model = model_of(&format!(
            "#1=IFCCARTESIANPOINTLIST3D({CUBE_POINTS},$);\n\
             #2=IFCTRIANGULATEDFACESET(#1,$,$,((0,1,2)),$);\n"
        ));
        assert!(matches!(
            eval_solid(&model, 2),
            Err(GeomError::Degenerate(_))
        ));
    }

    #[test]
    fn an_out_of_range_index_drops_its_face_not_the_model() {
        let model = model_of(&format!(
            "#1=IFCCARTESIANPOINTLIST3D({CUBE_POINTS},$);\n\
             #2=IFCTRIANGULATEDFACESET(#1,$,$,((1,2,3),(1,2,999)),$);\n"
        ));
        let mesh = eval_solid(&model, 2).unwrap();
        assert_eq!(
            mesh.triangle_count(),
            1,
            "the good face survives, the bad one goes"
        );
    }

    #[test]
    fn a_polygonal_face_set_handles_quads() {
        let model = model_of(&format!(
            "#1=IFCCARTESIANPOINTLIST3D({CUBE_POINTS},$);\n\
             #10=IFCINDEXEDPOLYGONALFACE((1,4,3,2));\n\
             #11=IFCINDEXEDPOLYGONALFACE((5,6,7,8));\n\
             #12=IFCINDEXEDPOLYGONALFACE((1,2,6,5));\n\
             #13=IFCINDEXEDPOLYGONALFACE((2,3,7,6));\n\
             #14=IFCINDEXEDPOLYGONALFACE((3,4,8,7));\n\
             #15=IFCINDEXEDPOLYGONALFACE((4,1,5,8));\n\
             #2=IFCPOLYGONALFACESET(#1,.T.,(#10,#11,#12,#13,#14,#15),$);\n"
        ));
        let mesh = eval_solid(&model, 2).unwrap();
        assert_eq!(mesh.closed, Some(true), "six quads make a closed box");
        assert!(
            (mesh.signed_volume() - 1.0).abs() < 1e-9,
            "got {}",
            mesh.signed_volume()
        );
        assert_eq!(mesh.triangle_count(), 12, "each quad becomes two triangles");
    }

    #[test]
    fn a_face_with_a_void_loses_the_hole_area() {
        // A 4x4 square in z=0 with a 2x2 hole: area 12, not 16.
        let model = model_of(
            "#1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(4.,0.,0.),(4.,4.,0.),(0.,4.,0.),\
             (1.,1.,0.),(3.,1.,0.),(3.,3.,0.),(1.,3.,0.)),$);\n\
             #10=IFCINDEXEDPOLYGONALFACEWITHVOIDS((1,2,3,4),((5,6,7,8)));\n\
             #2=IFCPOLYGONALFACESET(#1,.F.,(#10),$);\n",
        );
        let mesh = eval_solid(&model, 2).unwrap();
        let area = mesh.surface_area();
        assert!(
            (area - 12.0).abs() < 1e-9,
            "16 minus the 4 of the hole, got {area}"
        );
    }

    #[test]
    fn a_pn_index_indirection_is_followed() {
        // PnIndex reverses the point order, so face (1,2,3) reads points 3,2,1.
        let model = model_of(
            "#1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,0.,0.),(0.,1.,0.)),$);\n\
             #2=IFCTRIANGULATEDFACESET(#1,$,$,((1,2,3)),(3,2,1));\n",
        );
        let mesh = eval_solid(&model, 2).unwrap();
        assert_eq!(mesh.triangle_count(), 1);
        assert!((mesh.surface_area() - 0.5).abs() < 1e-12);
    }

    #[test]
    fn units_scale_a_face_set() {
        let model = model_of(
            "#1=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);\n\
             #2=IFCUNITASSIGNMENT((#1));\n\
             #3=IFCPROJECT('g',$,'P',$,$,$,$,$,#2);\n\
             #4=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1000.,0.,0.),(0.,1000.,0.)),$);\n\
             #5=IFCTRIANGULATEDFACESET(#4,$,$,((1,2,3)),$);\n",
        );
        let mesh = eval_solid(&model, 5).unwrap();
        assert!(
            (mesh.surface_area() - 0.5).abs() < 1e-12,
            "got {}",
            mesh.surface_area()
        );
    }
}
