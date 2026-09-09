// SPDX-License-Identifier: Apache-2.0
//! Convex cells of any closed solid, from a BSP of its own face planes.
//!
//! An outward-wound closed mesh partitions space when its faces are used as
//! splitters: the region behind a face is locally inside. A leaf reached with
//! nothing left behind it is an inside cell, convex by construction, and the
//! inside cells tile the solid. The classical construction from the computer
//! graphics literature, written from its published description.

use crate::clip::{Plane, clip_polygon};
use crate::mesh::Mesh64;
use glam::DVec3;

/// Most cells a decomposition may produce before it is refused.
pub const MAX_CELLS: usize = 4096;
/// Most polygon fragments the tree may hold before it is refused.
const MAX_FRAGMENTS: usize = 262_144;
/// Most splitting planes on any root-to-leaf path.
const MAX_DEPTH: usize = 8192;
/// Candidate splitters scored per node.
const SPLITTER_CANDIDATES: usize = 24;

struct Polygon {
    points: Vec<DVec3>,
    plane: Plane,
}

enum Side {
    Coplanar,
    Front,
    Back,
    Spanning,
}

fn classify(polygon: &Polygon, plane: &Plane, tol: f64) -> Side {
    let mut front = false;
    let mut back = false;
    for point in &polygon.points {
        let distance = plane.distance(*point);
        if distance > tol {
            front = true;
        } else if distance < -tol {
            back = true;
        }
    }
    match (front, back) {
        (false, false) => Side::Coplanar,
        (true, false) => Side::Front,
        (false, true) => Side::Back,
        (true, true) => Side::Spanning,
    }
}

/// A fragment thinner than the tolerance. Clipping leaves these along seams,
/// and one of them chosen as a splitter would declare an empty pocket solid.
fn is_sliver(points: &[DVec3], tol: f64) -> bool {
    if points.len() < 3 {
        return true;
    }
    let mut cross = DVec3::ZERO;
    let mut perimeter = 0.0;
    for index in 0..points.len() {
        let next = points[(index + 1) % points.len()];
        cross += points[index].cross(next);
        perimeter += (next - points[index]).length();
    }
    let area = cross.length() * 0.5;
    area <= tol * perimeter
}

/// The plane that separates the polygons best: few straddle it, and it
/// leaves the two sides balanced. Only a sample of candidates is scored.
fn choose_splitter(polygons: &[Polygon], tol: f64) -> Plane {
    let mut candidates: Vec<Plane> = Vec::with_capacity(SPLITTER_CANDIDATES);
    // Sampled across the whole list: neighbours on a curved surface all cut
    // nearly the same way, and a tree built from them alone runs deep.
    let stride = (polygons.len() / SPLITTER_CANDIDATES).max(1);
    for polygon in polygons.iter().step_by(stride) {
        if candidates.len() >= SPLITTER_CANDIDATES {
            break;
        }
        if !candidates.iter().any(|known| {
            known.normal.dot(polygon.plane.normal) > 1.0 - 1e-9
                && (known.offset - polygon.plane.offset).abs() <= tol
        }) {
            candidates.push(polygon.plane);
        }
    }
    let mut best = candidates[0];
    let mut best_score = f64::INFINITY;
    for candidate in candidates {
        let (mut front, mut back, mut spanning, mut coplanar) = (0i64, 0i64, 0i64, 0i64);
        for polygon in polygons {
            match classify(polygon, &candidate, tol) {
                Side::Coplanar => coplanar += 1,
                Side::Front => front += 1,
                Side::Back => back += 1,
                Side::Spanning => spanning += 1,
            }
        }
        let score = (front - back).abs() as f64 + 8.0 * spanning as f64 - coplanar as f64;
        if score < best_score {
            best_score = score;
            best = candidate;
        }
    }
    best
}

/// The six outward-wound faces of an axis-aligned box.
fn hull_faces(low: DVec3, high: DVec3) -> Vec<Vec<DVec3>> {
    let corner = |index: usize| {
        DVec3::new(
            if index & 1 == 0 { low.x } else { high.x },
            if index & 2 == 0 { low.y } else { high.y },
            if index & 4 == 0 { low.z } else { high.z },
        )
    };
    let quads: [[usize; 4]; 6] = [
        [0, 2, 3, 1],
        [4, 5, 7, 6],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 4, 6, 2],
        [1, 3, 7, 5],
    ];
    quads
        .iter()
        .map(|quad| quad.iter().map(|&index| corner(index)).collect())
        .collect()
}

/// Cut a convex polytope, held as its face polygons, by a plane.
///
/// Faces behind the plane stay, faces in front go, faces across it are
/// clipped, and the cut points make the new face as their convex hull in the
/// plane. A plane that touches nothing leaves the faces alone.
fn polytope_clip(faces: &mut Vec<Vec<DVec3>>, plane: &Plane, tol: f64) {
    let mut kept: Vec<Vec<DVec3>> = Vec::with_capacity(faces.len() + 1);
    let mut cut_points: Vec<DVec3> = Vec::new();
    let mut touched = false;
    let mut on_plane: Vec<DVec3> = Vec::new();
    for face in faces.iter() {
        let distances: Vec<f64> = face.iter().map(|point| plane.distance(*point)).collect();
        if distances.iter().all(|d| *d <= tol) {
            // A kept face that touches the plane shares those points with the
            // new face; without them the new face would leave a T-junction.
            for (point, distance) in face.iter().zip(&distances) {
                if distance.abs() <= tol {
                    on_plane.push(*point);
                }
            }
            kept.push(face.clone());
            continue;
        }
        touched = true;
        if distances.iter().all(|d| *d >= -tol) {
            continue;
        }
        let clipped = clip_polygon(face, plane, tol);
        for point in &clipped {
            if plane.distance(*point).abs() <= tol {
                cut_points.push(*point);
            }
        }
        if !is_sliver(&clipped, tol) {
            kept.push(clipped);
        }
    }
    if !touched {
        return;
    }
    cut_points.extend(on_plane);
    if cut_points.len() >= 3 {
        let hull = convex_hull_in_plane(&cut_points, plane, tol);
        if !is_sliver(&hull, tol) {
            kept.push(hull);
        }
    }
    *faces = kept;
}

/// The convex hull of coplanar points, wound anticlockwise about the plane
/// normal so the face looks out of the half-space the plane keeps.
fn convex_hull_in_plane(points: &[DVec3], plane: &Plane, tol: f64) -> Vec<DVec3> {
    let basis = crate::triangulate::PlaneBasis::from_normal(plane.normal, points[0]);
    let mut flat: Vec<(glam::DVec2, usize)> = points
        .iter()
        .enumerate()
        .map(|(index, point)| (basis.project(*point), index))
        .collect();
    flat.sort_by(|a, b| a.0.x.total_cmp(&b.0.x).then(a.0.y.total_cmp(&b.0.y)));
    flat.dedup_by(|a, b| a.0.distance(b.0) <= tol);
    if flat.len() < 3 {
        return Vec::new();
    }
    let cross = |o: glam::DVec2, a: glam::DVec2, b: glam::DVec2| {
        (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
    };
    // Andrew's monotone chain: lower hull, then upper hull.
    let mut hull: Vec<(glam::DVec2, usize)> = Vec::with_capacity(flat.len() * 2);
    for &entry in &flat {
        while hull.len() >= 2
            && cross(hull[hull.len() - 2].0, hull[hull.len() - 1].0, entry.0) <= 0.0
        {
            hull.pop();
        }
        hull.push(entry);
    }
    let lower = hull.len() + 1;
    for &entry in flat.iter().rev().skip(1) {
        while hull.len() >= lower
            && cross(hull[hull.len() - 2].0, hull[hull.len() - 1].0, entry.0) <= 0.0
        {
            hull.pop();
        }
        hull.push(entry);
    }
    hull.pop();
    // Points on a hull edge were dropped as collinear; they go back so the
    // face shares every vertex its neighbours have on that edge.
    let mut ordered: Vec<usize> = Vec::with_capacity(flat.len());
    for corner in 0..hull.len() {
        let (p, index) = hull[corner];
        let q = hull[(corner + 1) % hull.len()].0;
        ordered.push(index);
        let edge = q - p;
        let length = edge.length();
        if length <= tol {
            continue;
        }
        let mut along: Vec<(f64, usize)> = flat
            .iter()
            .filter_map(|(point, index)| {
                let offset = *point - p;
                let t = offset.dot(edge) / (length * length);
                let apart = (edge.x * offset.y - edge.y * offset.x).abs() / length;
                (apart <= tol && t * length > tol && (1.0 - t) * length > tol)
                    .then_some((t, *index))
            })
            .collect();
        along.sort_by(|a, b| a.0.total_cmp(&b.0));
        ordered.extend(along.into_iter().map(|(_, index)| index));
    }
    let mut face: Vec<DVec3> = ordered.iter().map(|index| points[*index]).collect();
    // The basis may be either-handed; the face has to turn about the normal.
    let normal = crate::mesh::newell_normal(&face);
    if normal.dot(plane.normal) < 0.0 {
        face.reverse();
    }
    face
}

/// Triangulate convex face polygons into one mesh.
///
/// A face with collinear vertices along an edge is fanned from its centre:
/// a fan from a corner would make zero-area triangles there, and dropping
/// those later would open the edge.
fn faces_to_mesh(faces: &[Vec<DVec3>], tol: f64) -> Mesh64 {
    let mut mesh = Mesh64::new();
    for face in faces {
        if face.len() < 3 {
            continue;
        }
        let base = mesh.positions.len() as u32;
        mesh.positions.extend_from_slice(face);
        let count = face.len();
        let straight = (0..count).any(|index| {
            let triple = [
                face[(index + count - 1) % count],
                face[index],
                face[(index + 1) % count],
            ];
            is_sliver(&triple, tol)
        });
        if count == 3 || !straight {
            for index in 1..count - 1 {
                mesh.push_triangle(base, base + index as u32, base + index as u32 + 1);
            }
            continue;
        }
        let centre = face.iter().sum::<DVec3>() / count as f64;
        let middle = mesh.push_vertex(centre);
        for index in 0..count {
            mesh.push_triangle(
                base + index as u32,
                base + ((index + 1) % count) as u32,
                middle,
            );
        }
    }
    mesh
}

/// Split a closed solid into convex cells that tile it.
///
/// `None` when the mesh is open, when the tree would exceed its budgets, or
/// when the cells' volumes do not add up to the solid's: a decomposition that
/// cannot be proved is not returned, and the caller falls back.
pub fn bsp_cells(mesh: &Mesh64, tolerance: f64) -> Option<Vec<Mesh64>> {
    bsp_cells_or_reason(mesh, tolerance).ok()
}

/// [`bsp_cells`], with the reason for a refusal.
pub fn bsp_cells_or_reason(mesh: &Mesh64, tolerance: f64) -> Result<Vec<Mesh64>, String> {
    bsp_cells_closing(mesh, tolerance, tolerance)
}

/// [`bsp_cells_or_reason`] with the shell closed at `closing`, a coarser
/// tolerance than the one the construction runs at.
pub(crate) fn bsp_cells_closing(
    mesh: &Mesh64,
    tolerance: f64,
    closing: f64,
) -> Result<Vec<Mesh64>, String> {
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1e-9
    };
    let close = if closing.is_finite() && closing > 0.0 {
        closing.max(tol)
    } else {
        tol
    };
    if mesh.is_empty() {
        return Err("empty mesh".into());
    }
    // The construction reads faces as outward and needs a closed surface: a
    // shell with T-junctions is healed first, faces that disagree about the
    // outside are turned to agree, and an inside-out shell is turned whole.
    let mut oriented;
    let mesh =
        if mesh.is_edge_manifold() && mesh.is_consistently_wound() && mesh.signed_volume() >= 0.0 {
            mesh
        } else {
            oriented = mesh.clone();
            if !oriented.is_edge_manifold() {
                crate::weld::weld_and_close(&mut oriented, close);
                if crate::weld::heal_t_junctions(&mut oriented, close) > 0 {
                    crate::weld::weld_and_close(&mut oriented, close);
                }
                if !oriented.is_edge_manifold() {
                    let (open, over) = oriented.edge_defects();
                    return Err(format!(
                        "not a closed shell ({open} boundary edges, {over} overused)"
                    ));
                }
            }
            if !oriented.is_consistently_wound() {
                crate::weld::orient_triangles_consistently(&mut oriented);
            }
            oriented.fix_orientation();
            &oriented
        };
    let target = mesh.signed_volume();
    let Some((low, high)) = mesh.bounds() else {
        return Err("no bounds".into());
    };
    if target.is_nan() || target <= tol * tol * tol {
        return Err("no volume".into());
    }

    let mut polygons = Vec::with_capacity(mesh.indices.len() / 3);
    for triangle in mesh.indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&c)) = (
            mesh.positions.get(triangle[0] as usize),
            mesh.positions.get(triangle[1] as usize),
            mesh.positions.get(triangle[2] as usize),
        ) else {
            continue;
        };
        let points = vec![a, b, c];
        if is_sliver(&points, tol) {
            continue;
        }
        let Some(plane) = Plane::from_point_normal(a, (b - a).cross(c - a)) else {
            continue;
        };
        polygons.push(Polygon { points, plane });
    }
    if polygons.is_empty() {
        return Err("no usable faces".into());
    }

    // Each cell is the half-spaces on its path, inside on the negative side.
    let mut cells: Vec<Vec<Plane>> = Vec::new();
    let mut fragments = polygons.len();
    let mut stack: Vec<(Vec<Polygon>, Vec<Plane>)> = vec![(polygons, Vec::new())];
    while let Some((polygons, path)) = stack.pop() {
        if path.len() >= MAX_DEPTH {
            return Err(format!("deeper than {MAX_DEPTH} planes"));
        }
        let splitter = choose_splitter(&polygons, tol);
        let mut front = Vec::new();
        let mut back = Vec::new();
        for polygon in polygons {
            match classify(&polygon, &splitter, tol) {
                // Absorbed by the splitter, whichever way it faces.
                Side::Coplanar => {}
                Side::Front => front.push(polygon),
                Side::Back => back.push(polygon),
                Side::Spanning => {
                    let ahead = clip_polygon(&polygon.points, &splitter.flipped(), tol);
                    let behind = clip_polygon(&polygon.points, &splitter, tol);
                    fragments += 1;
                    if fragments > MAX_FRAGMENTS {
                        return Err(format!("more than {MAX_FRAGMENTS} fragments"));
                    }
                    if !is_sliver(&ahead, tol) {
                        front.push(Polygon {
                            points: ahead,
                            plane: polygon.plane,
                        });
                    }
                    if !is_sliver(&behind, tol) {
                        back.push(Polygon {
                            points: behind,
                            plane: polygon.plane,
                        });
                    }
                }
            }
        }
        // Behind an outward face is inside; nothing more behind it means a cell.
        let mut inside = path.clone();
        inside.push(splitter);
        if back.is_empty() {
            cells.push(inside);
            if cells.len() > MAX_CELLS {
                return Err(format!("more than {MAX_CELLS} cells"));
            }
        } else {
            stack.push((back, inside));
        }
        if !front.is_empty() {
            let mut outside = path;
            outside.push(splitter.flipped());
            stack.push((front, outside));
        }
    }

    // Cells as meshes: a box round the solid, cut down by every half-space on
    // the cell's path. Each cut rebuilds the new face as the convex hull of the
    // cut points, so no boundary has to be chained and a sliver cannot fail.
    let margin = (high - low).length().max(1.0) + tol;
    let hull = hull_faces(low - DVec3::splat(margin), high + DVec3::splat(margin));
    let mut out = Vec::with_capacity(cells.len());
    let mut total = 0.0;
    let mut thin = 0usize;
    for planes in cells {
        let mut faces = hull.clone();
        for plane in &planes {
            polytope_clip(&mut faces, plane, tol);
            if faces.len() < 4 {
                break;
            }
        }
        if faces.len() < 4 {
            thin += 1;
            continue;
        }
        let mut cell = faces_to_mesh(&faces, tol);
        crate::weld::weld_and_close(&mut cell, tol);
        let volume = cell.signed_volume();
        if volume <= tol * tol * tol {
            thin += 1;
            continue;
        }
        total += volume;
        out.push(cell);
    }

    // The proof: the cells have to add up to the solid they came from.
    let slack = (target * 1e-6).max(tol * mesh.surface_area());
    if out.is_empty() {
        return Err("no cells".into());
    }
    if (total - target).abs() > slack {
        return Err(format!(
            "{} cells hold {total:.9} of a solid of {target:.9} ({thin} thin cells dropped)",
            out.len()
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::{difference_convex, face_planes, is_convex};

    fn hull_box(low: DVec3, high: DVec3) -> Mesh64 {
        let mut mesh = Mesh64::new();
        for face in hull_faces(low, high) {
            let base = mesh.positions.len() as u32;
            mesh.positions.extend_from_slice(&face);
            for index in 1..face.len() - 1 {
                mesh.push_triangle(base, base + index as u32, base + index as u32 + 1);
            }
        }
        crate::weld::weld_and_close(&mut mesh, 1e-9);
        mesh
    }

    fn cup() -> Mesh64 {
        // A 2 m cube with a 1 m square pocket from the top: not convex, not a prism.
        let block = hull_box(DVec3::ZERO, DVec3::splat(2.0));
        let pocket = hull_box(DVec3::new(0.5, 0.5, 1.0), DVec3::new(1.5, 1.5, 2.5));
        let cup = difference_convex(&block, &pocket, 1e-9).unwrap();
        assert!((cup.signed_volume() - 7.0).abs() < 1e-9);
        cup
    }

    fn cells_of(mesh: &Mesh64) -> Vec<Mesh64> {
        match bsp_cells_or_reason(mesh, 1e-9) {
            Ok(cells) => cells,
            Err(reason) => panic!("refused: {reason}"),
        }
    }

    #[test]
    fn a_box_is_its_own_only_cell() {
        let cells = cells_of(&hull_box(DVec3::ZERO, DVec3::ONE));
        assert_eq!(cells.len(), 1);
        assert!((cells[0].signed_volume() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_cells_of_a_pocketed_box_are_convex_and_add_up() {
        let cells = cells_of(&cup());
        let total: f64 = cells.iter().map(Mesh64::signed_volume).sum();
        assert!((total - 7.0).abs() < 1e-9, "cells sum to {total}");
        for cell in &cells {
            let planes = face_planes(cell, 1e-9);
            assert!(planes.len() >= 4 && is_convex(cell, &planes, 1e-9));
            assert!(cell.is_edge_manifold());
        }
    }

    #[test]
    fn an_inside_out_shell_is_turned_before_it_is_split() {
        let mut box_mesh = hull_box(DVec3::ZERO, DVec3::ONE);
        box_mesh.flip_winding();
        let cells = cells_of(&box_mesh);
        assert!((cells[0].signed_volume() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_shell_with_some_faces_turned_inward_is_still_split() {
        let mut mixed = hull_box(DVec3::ZERO, DVec3::ONE);
        // Turn every third triangle: still closed by the edge count, not by winding.
        for triangle in mixed.indices.chunks_exact_mut(3).step_by(3) {
            triangle.swap(1, 2);
        }
        assert!(mixed.is_edge_manifold() && !mixed.is_consistently_wound());
        let cells = cells_of(&mixed);
        let total: f64 = cells.iter().map(Mesh64::signed_volume).sum();
        assert!((total - 1.0).abs() < 1e-9, "cells sum to {total}");
    }

    #[test]
    fn an_open_shell_is_refused() {
        let mut open = hull_box(DVec3::ZERO, DVec3::ONE);
        open.indices.truncate(open.indices.len() - 3);
        assert!(bsp_cells(&open, 1e-9).is_none());
    }

    fn torus(major: f64, minor: f64, around: usize, across: usize) -> Mesh64 {
        let mut torus = Mesh64::new();
        for i in 0..around {
            let theta = std::f64::consts::TAU * i as f64 / around as f64;
            for j in 0..across {
                let phi = std::f64::consts::TAU * j as f64 / across as f64;
                let radial = major + minor * phi.cos();
                torus.positions.push(DVec3::new(
                    radial * theta.cos(),
                    radial * theta.sin(),
                    minor * phi.sin(),
                ));
            }
        }
        for i in 0..around {
            for j in 0..across {
                let a = (i * across + j) as u32;
                let b = (((i + 1) % around) * across + j) as u32;
                let c = (((i + 1) % around) * across + (j + 1) % across) as u32;
                let d = (i * across + (j + 1) % across) as u32;
                torus.push_triangle(a, b, c);
                torus.push_triangle(a, c, d);
            }
        }
        torus.fix_orientation();
        torus
    }

    #[test]
    fn a_ring_of_many_planes_stays_inside_the_budget() {
        // A torus at 48 by 24 segments: 2304 triangles and as many planes.
        let (major, minor) = (1.0, 0.3);
        let torus = torus(major, minor, 48, 24);
        let expected = 2.0 * std::f64::consts::PI.powi(2) * major * minor * minor;
        let volume = torus.signed_volume();
        assert!(
            (volume - expected).abs() < 0.02 * expected,
            "torus {volume}"
        );
        let started = std::time::Instant::now();
        let cells = cells_of(&torus);
        assert!(cells.len() <= MAX_CELLS);
        let total: f64 = cells.iter().map(Mesh64::signed_volume).sum();
        assert!(
            (total - volume).abs() < 1e-6 * volume,
            "cells sum to {total}"
        );
        assert!(
            started.elapsed().as_secs_f64() < 20.0,
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_ring_decomposes_at_a_model_tolerance() {
        let torus = torus(1.0, 0.3, 48, 24);
        let volume = torus.signed_volume();
        match bsp_cells_or_reason(&torus, 1e-6) {
            Ok(cells) => {
                let total: f64 = cells.iter().map(Mesh64::signed_volume).sum();
                assert!(
                    (total - volume).abs() < 1e-5 * volume,
                    "cells sum to {total}"
                );
            }
            Err(reason) => panic!("refused at 1e-6: {reason}"),
        }
    }
}
