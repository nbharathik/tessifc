// SPDX-License-Identifier: Apache-2.0
//! Cutting a mesh with a plane and keeping it closed. [`clip`] cuts and caps a
//! solid; [`difference_convex`] subtracts a convex solid using only plane clips,
//! which are exact: every new vertex lies on a known plane.

use crate::mesh::Mesh64;
use crate::triangulate::{PlaneBasis, signed_area, triangulate_face};
use glam::DVec3;

/// An oriented plane: the set of points where `normal . p == offset`.
///
/// The normal points **out** of the half-space that a clip keeps, so
/// `distance` is negative inside and positive outside. That convention makes
/// [`clip`] read the same way as `IfcHalfSpaceSolid`, whose `AgreementFlag`
/// says which side of the surface the material is on.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Plane {
    /// Unit normal, pointing out of the kept side.
    pub normal: DVec3,
    /// Signed distance from the origin along `normal`.
    pub offset: f64,
}

impl Plane {
    /// A plane from a normal and an offset. `None` if the normal is degenerate.
    pub fn new(normal: DVec3, offset: f64) -> Option<Plane> {
        let length = normal.length();
        if !length.is_finite() || length < 1e-12 || !offset.is_finite() {
            return None;
        }
        Some(Plane {
            normal: normal / length,
            offset: offset / length,
        })
    }

    /// A plane through `point` with the given normal.
    pub fn from_point_normal(point: DVec3, normal: DVec3) -> Option<Plane> {
        let length = normal.length();
        if !length.is_finite() || length < 1e-12 {
            return None;
        }
        let unit = normal / length;
        Some(Plane {
            normal: unit,
            offset: unit.dot(point),
        })
    }

    /// Signed distance: negative inside the kept half-space, positive outside.
    pub fn distance(&self, point: DVec3) -> f64 {
        self.normal.dot(point) - self.offset
    }

    /// The same plane facing the other way, so a clip keeps the other side.
    pub fn flipped(&self) -> Plane {
        Plane {
            normal: -self.normal,
            offset: -self.offset,
        }
    }

    /// Move `point` exactly onto the plane.
    ///
    /// Snapping matters more than it looks: the cap is built from the vertices
    /// that a clip decided were "on the plane", and a point that is a rounding
    /// error off will be found by neither the on-plane test nor its neighbour.
    fn snap(&self, point: DVec3) -> DVec3 {
        point - self.normal * self.distance(point)
    }
}

/// What a clip did, beyond the mesh it returned.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ClipOutcome {
    /// Nothing was on the far side; the mesh is unchanged.
    Untouched,
    /// Everything was on the far side; the result is empty.
    Removed,
    /// The mesh was cut, and every cut loop was closed with a cap.
    Capped,
    /// The mesh was cut, but the cut boundary did not chain into closed loops,
    /// so no cap was made. The result is an open shell and its volume is
    /// meaningless. This is what a non-manifold input produces.
    Open,
}

/// The result of one clip.
pub struct Clipped {
    /// The kept geometry.
    pub mesh: Mesh64,
    /// What happened.
    pub outcome: ClipOutcome,
}

/// Keep the part of `mesh` on the negative side of `plane`, capping the cut.
///
/// `tolerance` is the distance below which a vertex counts as lying on the
/// plane; pass the model's own length tolerance.
pub fn clip(mesh: &Mesh64, plane: &Plane, tolerance: f64) -> Clipped {
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1e-9
    };
    let mut out = Mesh64::new();
    // Each entry is one directed edge of the cut boundary, already reversed
    // into the winding the cap needs.
    let mut boundary: Vec<(DVec3, DVec3)> = Vec::new();
    let mut any_removed = false;
    let mut any_kept = false;

    for triangle in mesh.indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&c)) = (
            mesh.positions.get(triangle[0] as usize),
            mesh.positions.get(triangle[1] as usize),
            mesh.positions.get(triangle[2] as usize),
        ) else {
            continue;
        };
        let corners = [a, b, c];
        let distances = [
            plane.distance(corners[0]),
            plane.distance(corners[1]),
            plane.distance(corners[2]),
        ];

        if distances.iter().all(|d| *d > tol) {
            any_removed = true;
            continue;
        }
        any_kept = true;

        // A coplanar triangle is kept whole and contributes no cut edges, or its
        // three sides would break the boundary chaining.
        let coplanar = distances.iter().all(|d| d.abs() <= tol);
        if !coplanar && distances.iter().any(|d| *d > tol) {
            any_removed = true;
        }

        let polygon = if coplanar {
            corners.to_vec()
        } else {
            clip_triangle(plane, &corners, &distances, tol)
        };
        if polygon.len() < 3 {
            continue;
        }

        let base = out.positions.len() as u32;
        out.positions.extend_from_slice(&polygon);
        for index in 1..polygon.len() - 1 {
            out.push_triangle(base, base + index as u32, base + index as u32 + 1);
        }

        if coplanar {
            continue;
        }
        // An edge with both ends on the plane bounds the hole the cap fills;
        // reversed, because the cap faces the other way from the side wall.
        for index in 0..polygon.len() {
            let from = polygon[index];
            let to = polygon[(index + 1) % polygon.len()];
            if plane.distance(from).abs() <= tol
                && plane.distance(to).abs() <= tol
                && from.distance(to) > tol
            {
                boundary.push((to, from));
            }
        }
    }

    if !any_kept {
        return Clipped {
            mesh: Mesh64::new(),
            outcome: ClipOutcome::Removed,
        };
    }
    if !any_removed {
        return Clipped {
            mesh: mesh.clone(),
            outcome: ClipOutcome::Untouched,
        };
    }

    let outcome = match cap(&mut out, plane, &boundary, tol) {
        true => ClipOutcome::Capped,
        false => ClipOutcome::Open,
    };
    out.drop_unused_vertices();
    Clipped { mesh: out, outcome }
}

/// Sutherland-Hodgman for one triangle against one plane, keeping `d <= 0`.
fn clip_triangle(
    plane: &Plane,
    corners: &[DVec3; 3],
    distances: &[f64; 3],
    tol: f64,
) -> Vec<DVec3> {
    let mut output = Vec::with_capacity(4);
    for index in 0..3 {
        let next = (index + 1) % 3;
        let (here, there) = (distances[index], distances[next]);
        if here <= tol {
            output.push(corners[index]);
        }
        // Only a genuine crossing makes a new vertex. A vertex sitting on the
        // plane is already in the output and must not be duplicated.
        if (here < -tol && there > tol) || (here > tol && there < -tol) {
            let t = here / (here - there);
            output.push(plane.snap(corners[index] + (corners[next] - corners[index]) * t));
        }
    }
    output
}

/// Chain the cut boundary into loops and fill them. `false` if it did not close.
fn cap(mesh: &mut Mesh64, plane: &Plane, boundary: &[(DVec3, DVec3)], tol: f64) -> bool {
    if boundary.is_empty() {
        return true;
    }
    let Some(loops) = chain(boundary, tol) else {
        return false;
    };

    // Consistent winding from a closed solid: an outer loop encloses positive area
    // about the normal and a hole negative, so there is nothing to decide.
    let basis = PlaneBasis::from_normal(plane.normal, loops[0][0]);
    let mut outers: Vec<Vec<DVec3>> = Vec::new();
    let mut holes: Vec<Vec<DVec3>> = Vec::new();
    for ring in loops {
        let flat: Vec<_> = ring.iter().map(|point| basis.project(*point)).collect();
        if signed_area(&flat) >= 0.0 {
            outers.push(ring);
        } else {
            holes.push(ring);
        }
    }
    if outers.is_empty() {
        return false;
    }

    for outer in &outers {
        let mine: Vec<Vec<DVec3>> = if outers.len() == 1 {
            holes.clone()
        } else {
            holes
                .iter()
                .filter(|hole| contains(&basis, outer, hole[0]))
                .cloned()
                .collect()
        };
        let Ok(indices) = triangulate_face(outer, &mine) else {
            return false;
        };
        let base = mesh.positions.len() as u32;
        mesh.positions.extend_from_slice(outer);
        for hole in &mine {
            mesh.positions.extend_from_slice(hole);
        }
        for triangle in indices.chunks_exact(3) {
            mesh.push_triangle(base + triangle[0], base + triangle[1], base + triangle[2]);
        }
    }
    true
}

/// Link directed segments end to end into closed rings.
///
/// Returns `None` if any segment has no continuation, which means the input was
/// not a closed surface and there is no honest cap to draw. Ends are matched
/// within the tolerance rather than by an exact key, so a sliver's short
/// segments, which land a rounding error apart, still chain.
fn chain(segments: &[(DVec3, DVec3)], tol: f64) -> Option<Vec<Vec<DVec3>>> {
    let quantum = tol.max(1e-12);
    let key = |point: DVec3| {
        (
            (point.x / quantum).floor() as i64,
            (point.y / quantum).floor() as i64,
            (point.z / quantum).floor() as i64,
        )
    };

    let mut starting: std::collections::HashMap<(i64, i64, i64), Vec<usize>> = Default::default();
    for (index, segment) in segments.iter().enumerate() {
        starting.entry(key(segment.0)).or_default().push(index);
    }
    // The unused segment starting nearest to `point`, looked up in the cell
    // around it and its neighbours.
    let nearest = |point: DVec3, used: &[bool]| -> Option<usize> {
        let (kx, ky, kz) = key(point);
        let mut best: Option<(f64, usize)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let Some(candidates) = starting.get(&(
                        kx.wrapping_add(dx),
                        ky.wrapping_add(dy),
                        kz.wrapping_add(dz),
                    )) else {
                        continue;
                    };
                    for &index in candidates {
                        if used[index] {
                            continue;
                        }
                        let distance = segments[index].0.distance(point);
                        if distance <= tol && best.is_none_or(|(known, _)| distance < known) {
                            best = Some((distance, index));
                        }
                    }
                }
            }
        }
        best.map(|(_, index)| index)
    };

    let mut used = vec![false; segments.len()];
    let mut rings = Vec::new();
    for start in 0..segments.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let first = segments[start].0;
        let mut ring = vec![first];
        let mut cursor = segments[start].1;

        // A ring cannot be longer than the segment list; the bound is a
        // guarantee of termination, not an expectation.
        for _ in 0..=segments.len() {
            if cursor.distance(first) <= tol {
                break;
            }
            let next = nearest(cursor, &used)?;
            used[next] = true;
            ring.push(segments[next].0);
            cursor = segments[next].1;
        }
        if cursor.distance(first) > tol {
            return None;
        }
        if ring.len() >= 3 {
            rings.push(ring);
        }
    }
    if rings.is_empty() { None } else { Some(rings) }
}

/// Even-odd point-in-polygon, in the plane's own 2D basis.
fn contains(basis: &PlaneBasis, ring: &[DVec3], point: DVec3) -> bool {
    let target = basis.project(point);
    let flat: Vec<_> = ring.iter().map(|p| basis.project(*p)).collect();
    let mut inside = false;
    let mut j = flat.len() - 1;
    for i in 0..flat.len() {
        let (a, b) = (flat[i], flat[j]);
        if (a.y > target.y) != (b.y > target.y) {
            let x = a.x + (target.y - a.y) / (b.y - a.y) * (b.x - a.x);
            if target.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Clip a planar polygon by a plane, keeping the `d <= 0` side.
///
/// The polygon stays a polygon: no cap, no closure, nothing to chain. That is
/// what makes it the right primitive for cutting a *surface* rather than a
/// solid.
pub(crate) fn clip_polygon(points: &[DVec3], plane: &Plane, tol: f64) -> Vec<DVec3> {
    if points.len() < 3 {
        return Vec::new();
    }
    let mut output = Vec::with_capacity(points.len() + 2);
    for index in 0..points.len() {
        let next = (index + 1) % points.len();
        let here = plane.distance(points[index]);
        let there = plane.distance(points[next]);
        if here <= tol {
            output.push(points[index]);
        }
        if (here < -tol && there > tol) || (here > tol && there < -tol) {
            let t = here / (here - there);
            output.push(plane.snap(points[index] + (points[next] - points[index]) * t));
        }
    }
    output
}

/// Above this many distinct planes a mesh is not treated as convex: the
/// dedup is quadratic, and no convex solid a file draws needs so many.
pub const MAX_FACE_PLANES: usize = 4200;

/// The distinct face planes of a mesh, outward-facing.
///
/// Planes that repeat - and on a box every face is two triangles, so they all
/// do - are returned once. The comparison is on the unit normal and the offset
/// together, because two parallel faces of a slab are different planes.
/// Empty when there are more than [`MAX_FACE_PLANES`] of them.
pub fn face_planes(mesh: &Mesh64, tolerance: f64) -> Vec<Plane> {
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1e-9
    };
    let mut planes: Vec<Plane> = Vec::new();
    for triangle in mesh.indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&c)) = (
            mesh.positions.get(triangle[0] as usize),
            mesh.positions.get(triangle[1] as usize),
            mesh.positions.get(triangle[2] as usize),
        ) else {
            continue;
        };
        let normal = (b - a).cross(c - a);
        // A sliver triangle has no reliable normal, and a plane derived from
        // one would reject a perfectly convex solid.
        if normal.length() < tol * tol {
            continue;
        }
        let Some(plane) = Plane::from_point_normal(a, normal) else {
            continue;
        };
        if !planes.iter().any(|existing| {
            existing.normal.dot(plane.normal) > 1.0 - 1e-9
                && (existing.offset - plane.offset).abs() <= tol
        }) {
            if planes.len() >= MAX_FACE_PLANES {
                return Vec::new();
            }
            planes.push(plane);
        }
    }
    planes
}

/// True when every vertex lies on the inner side of every face plane.
///
/// Convexity is what makes the difference below exact, so it is checked rather
/// than assumed. Most walls, every opening, and every boxed half-space pass;
/// an L-shaped wall does not, and is told so instead of quietly coming out
/// wrong.
pub fn is_convex(mesh: &Mesh64, planes: &[Plane], tolerance: f64) -> bool {
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1e-9
    };
    !planes.is_empty()
        && mesh
            .positions
            .iter()
            .all(|point| planes.iter().all(|plane| plane.distance(*point) <= tol))
}

/// Subtract one convex solid from another, exactly.
///
/// See [`difference_convex_many`], which this calls with a single cutter.
pub fn difference_convex(body: &Mesh64, cutter: &Mesh64, tolerance: f64) -> Option<Mesh64> {
    difference_convex_many(body, std::slice::from_ref(cutter), tolerance)
}

/// Subtract convex cutters from an extrusion whose profile may be non-convex.
///
/// The body is split into one convex prism per cap triangle, each prism is cut
/// on its own, and only the triangles that separate the result from empty space
/// are kept. Returns `None` unless the body is a paired-cap extrusion and every
/// cutter is convex; the caller then emits the body uncut and says so.
pub fn difference_extrusion_many(
    body: &Mesh64,
    cutters: &[Mesh64],
    tolerance: f64,
) -> Option<Mesh64> {
    let tol = valid_tolerance(tolerance);
    if body.is_empty() || cutters.is_empty() {
        return None;
    }
    let cells = extrusion_cells(body, tol)?;
    if cells.len() < 2 {
        return None;
    }

    let cell_planes: Vec<Vec<Plane>> = cells.iter().map(|cell| face_planes(cell, tol)).collect();
    if cell_planes
        .iter()
        .zip(&cells)
        .any(|(planes, cell)| planes.len() < 4 || !is_convex(cell, planes, tol))
    {
        return None;
    }

    let cutter_planes: Vec<Vec<Plane>> = cutters
        .iter()
        .map(|cutter| face_planes(cutter, tol))
        .collect();
    if cutter_planes
        .iter()
        .zip(cutters)
        .any(|(planes, cutter)| planes.len() < 4 || !is_convex(cutter, planes, tol))
    {
        return None;
    }

    let mut candidates = Mesh64::new();
    for cell in &cells {
        let cut = difference_convex_many(cell, cutters, tol)?;
        candidates.append(&cut);
    }
    if candidates.is_empty() {
        return Some(Mesh64::new());
    }

    let inside_result = |point: DVec3| {
        cell_planes
            .iter()
            .any(|planes| point_in_convex(point, planes, tol))
            && !cutter_planes
                .iter()
                .any(|planes| point_in_convex(point, planes, tol))
    };
    let probe = (tol * 16.0).max(1e-9);
    let mut out = Mesh64::with_capacity(candidates.positions.len(), candidates.indices.len());
    for triangle in candidates.indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&c)) = (
            candidates.positions.get(triangle[0] as usize),
            candidates.positions.get(triangle[1] as usize),
            candidates.positions.get(triangle[2] as usize),
        ) else {
            continue;
        };
        let normal = (b - a).cross(c - a).normalize_or_zero();
        if normal == DVec3::ZERO {
            continue;
        }
        let centre = (a + b + c) / 3.0;
        let negative = inside_result(centre - normal * probe);
        let positive = inside_result(centre + normal * probe);
        // A real boundary has material on exactly one side; both sides is a prism
        // interface, neither is numerical debris.
        if negative == positive {
            continue;
        }
        let base = out.positions.len() as u32;
        out.positions.extend_from_slice(&[a, b, c]);
        if negative {
            out.push_triangle(base, base + 1, base + 2);
        } else {
            out.push_triangle(base, base + 2, base + 1);
        }
    }

    if out.is_empty() {
        return Some(out);
    }
    finish(&mut out, tol);
    Some(out)
}

/// Recover the triangular prisms whose union is a TessIFC extrusion.
fn extrusion_cells(mesh: &Mesh64, tol: f64) -> Option<Vec<Mesh64>> {
    if mesh.positions.len() < 6
        || !mesh.positions.len().is_multiple_of(2)
        || !mesh.indices.len().is_multiple_of(6)
    {
        return None;
    }
    let half = mesh.positions.len() / 2;
    let offset = mesh.positions[half] - mesh.positions[0];
    if offset.length() <= tol
        || (0..half).any(|index| {
            (mesh.positions[index + half] - mesh.positions[index] - offset).length() > tol
        })
    {
        return None;
    }

    let mut cells = Vec::new();
    let mut cap_triangles = Vec::new();
    let mut cap_index_count = 0;
    for pair in mesh.indices.chunks_exact(6) {
        let lower = [pair[0], pair[1], pair[2]];
        let upper = [pair[3], pair[4], pair[5]];
        if lower.iter().any(|index| *index as usize >= half)
            || upper.iter().any(|index| (*index as usize) < half)
        {
            break;
        }
        let mut lower_sorted = lower;
        lower_sorted.sort_unstable();
        let mut upper_sorted = upper.map(|index| index - half as u32);
        upper_sorted.sort_unstable();
        if lower_sorted != upper_sorted {
            break;
        }

        let bottom = lower.map(|index| mesh.positions[index as usize]);
        let top = bottom.map(|point| point + offset);
        let mut cell = Mesh64::with_capacity(6, 24);
        cell.positions.extend_from_slice(&bottom);
        cell.positions.extend_from_slice(&top);
        cell.push_triangle(0, 1, 2);
        cell.push_triangle(3, 5, 4);
        for edge in 0..3u32 {
            let next = (edge + 1) % 3;
            cell.push_triangle(edge, edge + 3, next + 3);
            cell.push_triangle(edge, next + 3, next);
        }
        cell.closed = Some(cell.is_edge_manifold());
        cell.fix_orientation();
        cells.push(cell);
        cap_triangles.push(lower);
        cap_index_count += 6;
    }
    if cells.is_empty() {
        return None;
    }

    // The side layout is proved too, or a mesh that merely starts with paired
    // triangles would be taken for an extrusion and lose its remaining faces.
    let mut edges = Vec::with_capacity(cap_triangles.len() * 3);
    for triangle in cap_triangles {
        for (a, b) in [
            (triangle[0], triangle[1]),
            (triangle[1], triangle[2]),
            (triangle[2], triangle[0]),
        ] {
            if a == b {
                return None;
            }
            edges.push((a.min(b), a.max(b)));
        }
    }
    edges.sort_unstable();
    let mut boundary = Vec::new();
    let mut edge_index = 0;
    while edge_index < edges.len() {
        let next = edges[edge_index..]
            .iter()
            .position(|edge| *edge != edges[edge_index])
            .map_or(edges.len(), |offset| edge_index + offset);
        match next - edge_index {
            1 => boundary.push(edges[edge_index]),
            2 => {}
            _ => return None,
        }
        edge_index = next;
    }
    let sides = &mesh.indices[cap_index_count..];
    if boundary.is_empty() || sides.len() != boundary.len() * 6 {
        return None;
    }

    let mut seen = vec![false; boundary.len()];
    for pair in sides.chunks_exact(6) {
        if pair.iter().any(|index| *index as usize >= half * 2) {
            return None;
        }
        let mut base_vertices: Vec<u32> = pair.iter().map(|index| index % half as u32).collect();
        base_vertices.sort_unstable();
        base_vertices.dedup();
        if base_vertices.len() != 2 {
            return None;
        }
        let edge = (base_vertices[0], base_vertices[1]);
        let boundary_index = boundary.binary_search(&edge).ok()?;
        if std::mem::replace(&mut seen[boundary_index], true) {
            return None;
        }

        if !matches_extrusion_side(pair, edge, half as u32) {
            return None;
        }
    }
    seen.into_iter().all(|present| present).then_some(cells)
}

fn matches_extrusion_side(pair: &[u32], edge: (u32, u32), half: u32) -> bool {
    let (a, b) = edge;
    let (top_a, top_b) = (a + half, b + half);
    let mut actual = [
        sorted_triangle([pair[0], pair[1], pair[2]]),
        sorted_triangle([pair[3], pair[4], pair[5]]),
    ];
    actual.sort_unstable();

    let mut diagonal_a_top_b = [
        sorted_triangle([a, b, top_b]),
        sorted_triangle([a, top_b, top_a]),
    ];
    diagonal_a_top_b.sort_unstable();
    let mut diagonal_b_top_a = [
        sorted_triangle([a, b, top_a]),
        sorted_triangle([b, top_b, top_a]),
    ];
    diagonal_b_top_a.sort_unstable();
    actual == diagonal_a_top_b || actual == diagonal_b_top_a
}

fn sorted_triangle(mut triangle: [u32; 3]) -> [u32; 3] {
    triangle.sort_unstable();
    triangle
}

fn point_in_convex(point: DVec3, planes: &[Plane], tol: f64) -> bool {
    planes.iter().all(|plane| plane.distance(point) <= tol)
}

/// The construction tolerance of the general booleans, in mesh units.
///
/// The cells and their cuts are computed exactly, and a tolerance as coarse
/// as a model's makes neighbouring faces disagree about where a cut lies.
/// The model tolerance is still what closes an input shell.
const GENERAL_TOLERANCE: f64 = 1e-7;

fn valid_tolerance(tolerance: f64) -> f64 {
    if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1e-9
    }
}

/// Subtract several convex solids from a convex one, exactly.
///
/// The result surface is the body's faces outside every cutter plus each
/// cutter's faces inside the body and outside the others, reversed; both halves
/// are plane clips of single triangles. All cutters are taken at once because
/// the body stops being convex after the first cut. Convexity is checked and
/// `None` means the caller should emit the body uncut and say so.
pub fn difference_convex_many(body: &Mesh64, cutters: &[Mesh64], tolerance: f64) -> Option<Mesh64> {
    let tol = valid_tolerance(tolerance);
    if body.is_empty() || cutters.is_empty() {
        return None;
    }
    let body_planes = face_planes(body, tol);
    if body_planes.len() < 4 || !is_convex(body, &body_planes, tol) {
        return None;
    }
    let mut cutter_planes = Vec::with_capacity(cutters.len());
    let mut cutter_bounds = Vec::with_capacity(cutters.len());
    for cutter in cutters {
        if cutter.is_empty() {
            return None;
        }
        let planes = face_planes(cutter, tol);
        if planes.len() < 4 || !is_convex(cutter, &planes, tol) {
            return None;
        }
        let (low, high) = cutter.bounds()?;
        cutter_bounds.push((low - DVec3::splat(tol), high + DVec3::splat(tol)));
        cutter_planes.push(planes);
    }

    let mut out = Mesh64::new();

    // The body's surface, with the part inside any cutter taken out.
    for triangle in body.indices.chunks_exact(3) {
        let (Some(&pa), Some(&pb), Some(&pc)) = (
            body.positions.get(triangle[0] as usize),
            body.positions.get(triangle[1] as usize),
            body.positions.get(triangle[2] as usize),
        ) else {
            continue;
        };
        let corners = [pa, pb, pc];
        let normal = (corners[1] - corners[0])
            .cross(corners[2] - corners[0])
            .normalize_or_zero();
        let low = corners[0].min(corners[1]).min(corners[2]);
        let high = corners[0].max(corners[1]).max(corners[2]);
        let mut pieces = vec![corners.to_vec()];
        for (planes, bounds) in cutter_planes.iter().zip(&cutter_bounds) {
            // A triangle nowhere near this cutter keeps its shape; the T-junctions this
            // leaves along neighbours are healed at the end.
            if low.cmpgt(bounds.1).any() || high.cmplt(bounds.0).any() {
                continue;
            }
            // A body face lying in a cutter face and pointing the same way is where the
            // hole opens, so that plane must not take part or the opening gets a lid.
            let relevant: Vec<Plane> = planes
                .iter()
                .filter(|plane| {
                    !(plane.normal.dot(normal) > 1.0 - 1e-9
                        && corners
                            .iter()
                            .all(|corner| plane.distance(*corner).abs() <= tol))
                })
                .copied()
                .collect();
            let mut next = Vec::new();
            for piece in &pieces {
                decompose(piece, &relevant, tol, false, &mut next);
            }
            pieces = next;
            if pieces.is_empty() {
                break;
            }
        }
        for piece in &pieces {
            emit(&mut out, piece);
        }
    }

    // Each cutter's surface, keeping what is inside the body and outside the
    // other cutters, facing the other way: this is the wall of the hole.
    for (index, cutter) in cutters.iter().enumerate() {
        for triangle in cutter.indices.chunks_exact(3) {
            let (Some(&pa), Some(&pb), Some(&pc)) = (
                cutter.positions.get(triangle[0] as usize),
                cutter.positions.get(triangle[1] as usize),
                cutter.positions.get(triangle[2] as usize),
            ) else {
                continue;
            };
            let corners = [pa, pb, pc];
            // A cutter face lying in a body face and pointing the same way is the cutter
            // breaking the surface, not a wall of the hole; keeping it would add a lid.
            let normal = (corners[1] - corners[0])
                .cross(corners[2] - corners[0])
                .normalize_or_zero();
            if body_planes.iter().any(|plane| {
                plane.normal.dot(normal) > 1.0 - 1e-9
                    && corners
                        .iter()
                        .all(|corner| plane.distance(*corner).abs() <= tol)
            }) {
                continue;
            }

            let mut piece = corners.to_vec();
            for plane in &body_planes {
                if piece.len() < 3 {
                    break;
                }
                piece = clip_polygon(&piece, plane, tol);
            }
            if piece.len() < 3 {
                continue;
            }

            // Where two openings overlap, the part of this one's wall that is
            // inside the other one is not a surface of the result.
            let mut pieces = vec![piece];
            for (other, planes) in cutter_planes.iter().enumerate() {
                if other == index {
                    continue;
                }
                let bounds = cutter_bounds[other];
                if pieces_low(&pieces).cmpgt(bounds.1).any()
                    || pieces_high(&pieces).cmplt(bounds.0).any()
                {
                    continue;
                }
                let mut next = Vec::new();
                for piece in &pieces {
                    decompose(piece, planes, tol, false, &mut next);
                }
                pieces = next;
                if pieces.is_empty() {
                    break;
                }
            }
            for piece in &mut pieces {
                piece.reverse();
                emit(&mut out, piece);
            }
        }
    }

    if out.is_empty() {
        return Some(Mesh64::new());
    }
    // The surfaces were triangulated independently and meet along the rim, so
    // they arrive stitched but not manifold.
    finish(&mut out, tol);
    Some(out)
}

/// Lower corner of a set of polygons.
fn pieces_low(pieces: &[Vec<DVec3>]) -> DVec3 {
    pieces
        .iter()
        .flatten()
        .fold(DVec3::splat(f64::INFINITY), |acc, point| acc.min(*point))
}

/// Upper corner of a set of polygons.
fn pieces_high(pieces: &[Vec<DVec3>]) -> DVec3 {
    pieces
        .iter()
        .flatten()
        .fold(DVec3::splat(f64::NEG_INFINITY), |acc, point| {
            acc.max(*point)
        })
}

/// Split a convex polygon by every plane, emitting the cells outside at least one.
///
/// Each plane splits a piece into at most two, but a piece that lies wholly on
/// one side is passed on without being split, so a triangle the cutter never
/// comes near costs one pass and emits itself. The work list is walked depth
/// first, in the order a recursion would visit, so a cutter with thousands of
/// planes cannot overflow the stack.
fn decompose(
    polygon: &[DVec3],
    planes: &[Plane],
    tol: f64,
    outside: bool,
    out: &mut Vec<Vec<DVec3>>,
) {
    let mut work: Vec<(Vec<DVec3>, usize, bool)> = vec![(polygon.to_vec(), 0, outside)];
    while let Some((polygon, at, outside)) = work.pop() {
        if polygon.len() < 3 {
            continue;
        }
        // A piece outside one plane is outside the cutter; stopping here keeps the
        // piece count linear and leaves T-junctions that heal_t_junctions removes.
        if outside {
            out.push(polygon);
            continue;
        }
        let Some(plane) = planes.get(at) else {
            continue;
        };
        let distances: Vec<f64> = polygon.iter().map(|point| plane.distance(*point)).collect();
        if distances.iter().all(|d| *d <= tol) {
            work.push((polygon, at + 1, false));
        } else if distances.iter().all(|d| *d >= -tol) {
            work.push((polygon, at + 1, true));
        } else {
            // Pushed in reverse, so the inner half is finished before the outer.
            work.push((clip_polygon(&polygon, &plane.flipped(), tol), at + 1, true));
            work.push((clip_polygon(&polygon, plane, tol), at + 1, false));
        }
    }
}

/// Fan-triangulate a convex polygon into a mesh, preserving its winding.
fn emit(mesh: &mut Mesh64, polygon: &[DVec3]) {
    let base = mesh.positions.len() as u32;
    mesh.positions.extend_from_slice(polygon);
    for index in 1..polygon.len() - 1 {
        mesh.push_triangle(base, base + index as u32, base + index as u32 + 1);
    }
}
/// Split a closed solid into convex cells whose union is the solid itself.
///
/// Cheapest first: a convex solid is its own cell, a translational prism is
/// swept from its merged cap, a paired-cap extrusion uses [`extrusion_cells`],
/// and anything else goes through the BSP in [`crate::bsp`]. `None` means no
/// decomposition could be proved; the caller emits the body uncut and says so.
pub fn convex_cells(mesh: &Mesh64, tolerance: f64) -> Option<Vec<Mesh64>> {
    convex_cells_or_reason(mesh, tolerance).ok()
}

/// [`convex_cells`], with the reason for a refusal.
pub fn convex_cells_or_reason(mesh: &Mesh64, tolerance: f64) -> Result<Vec<Mesh64>, String> {
    convex_cells_closing(mesh, tolerance, tolerance)
}

/// [`convex_cells_or_reason`] with the shell closed at `closing` before the
/// general decomposition runs at `tolerance`.
fn convex_cells_closing(
    mesh: &Mesh64,
    tolerance: f64,
    closing: f64,
) -> Result<Vec<Mesh64>, String> {
    let tol = valid_tolerance(tolerance);
    if mesh.is_empty() {
        return Err("an empty mesh".into());
    }
    let planes = face_planes(mesh, tol);
    if planes.len() >= 4 && is_convex(mesh, &planes, tol) {
        return Ok(vec![mesh.clone()]);
    }
    // The geometric prism proof first (any triangulation, fewest convex pieces),
    // then the index-layout proof, and the BSP takes whatever is left.
    if let Some(cells) = prism_cells(mesh, tol) {
        return Ok(cells);
    }
    if let Some(cells) = extrusion_cells(mesh, tol) {
        return Ok(cells);
    }
    crate::bsp::bsp_cells_closing(mesh, tol, closing)
}

/// Convex cells with their face planes and bounds, for point-in-solid tests.
struct CellSet {
    planes: Vec<Vec<Plane>>,
    bounds: Vec<(DVec3, DVec3)>,
}

impl CellSet {
    /// `None` if any cell is not a convex solid, which the caller must not guess past.
    fn new(meshes: Vec<Mesh64>, tol: f64) -> Option<CellSet> {
        let mut planes = Vec::with_capacity(meshes.len());
        let mut bounds = Vec::with_capacity(meshes.len());
        for cell in &meshes {
            let cell_planes = face_planes(cell, tol);
            if cell_planes.len() < 4 || !is_convex(cell, &cell_planes, tol) {
                return None;
            }
            let (low, high) = cell.bounds()?;
            bounds.push((low - DVec3::splat(tol), high + DVec3::splat(tol)));
            planes.push(cell_planes);
        }
        Some(CellSet { planes, bounds })
    }

    fn contains(&self, point: DVec3, tol: f64) -> bool {
        self.planes
            .iter()
            .zip(&self.bounds)
            .any(|(planes, (low, high))| {
                point.cmpge(*low).all()
                    && point.cmple(*high).all()
                    && point_in_convex(point, planes, tol)
            })
    }
}

/// Split a polygon by every plane, emitting every piece.
///
/// Unlike [`decompose`], nothing is dropped: a piece inside all the planes is
/// emitted too, so a later probe can judge each piece on its own. An explicit
/// work list keeps the plane count off the stack.
fn split_all(polygon: &[DVec3], planes: &[Plane], tol: f64, out: &mut Vec<Vec<DVec3>>) {
    // The flag marks a finished piece waiting to be emitted after the inner
    // half it was cut from, which keeps the order a recursion would produce.
    let mut work: Vec<(Vec<DVec3>, usize, bool)> = vec![(polygon.to_vec(), 0, false)];
    while let Some((polygon, at, finished)) = work.pop() {
        if finished {
            out.push(polygon);
            continue;
        }
        if polygon.len() < 3 {
            continue;
        }
        let Some(plane) = planes.get(at) else {
            out.push(polygon);
            continue;
        };
        let distances: Vec<f64> = polygon.iter().map(|point| plane.distance(*point)).collect();
        if distances.iter().all(|d| *d <= tol) {
            work.push((polygon, at + 1, false));
        } else if distances.iter().all(|d| *d >= -tol) {
            // Beyond this plane the piece is clear of the cell; the others need not cut it.
            out.push(polygon);
        } else {
            work.push((clip_polygon(&polygon, &plane.flipped(), tol), at + 1, true));
            work.push((clip_polygon(&polygon, plane, tol), at + 1, false));
        }
    }
}

/// Cut every candidate triangle along the faces of the cells it touches.
///
/// A cell face can be an interface under one neighbour and a real boundary
/// beside it; only once it is split along the neighbours' planes can a
/// two-sided probe judge each piece.
fn split_by_cells(candidates: &Mesh64, cells: &CellSet, tol: f64) -> Mesh64 {
    let mut out = Mesh64::with_capacity(candidates.positions.len(), candidates.indices.len());
    for triangle in candidates.indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&c)) = (
            candidates.positions.get(triangle[0] as usize),
            candidates.positions.get(triangle[1] as usize),
            candidates.positions.get(triangle[2] as usize),
        ) else {
            continue;
        };
        let low = a.min(b).min(c);
        let high = a.max(b).max(c);
        let mut pieces = vec![vec![a, b, c]];
        for (planes, bounds) in cells.planes.iter().zip(&cells.bounds) {
            if low.cmpgt(bounds.1).any() || high.cmplt(bounds.0).any() {
                continue;
            }
            let mut next = Vec::with_capacity(pieces.len());
            for piece in &pieces {
                split_all(piece, planes, tol, &mut next);
            }
            pieces = next;
        }
        for piece in &pieces {
            if piece.len() >= 3 {
                emit(&mut out, piece);
            }
        }
    }
    out
}

/// Keep the triangles that separate the result from empty space.
///
/// Every candidate triangle is probed a little on both sides. A real boundary
/// has material on exactly one side; both sides is an interface between two
/// cells and neither side is numerical debris. The survivors are welded and
/// healed into one shell.
fn keep_boundary(candidates: &Mesh64, inside_result: &dyn Fn(DVec3) -> bool, tol: f64) -> Mesh64 {
    let probe = (tol * 16.0).max(1e-9);
    let mut out = Mesh64::with_capacity(candidates.positions.len(), candidates.indices.len());
    for triangle in candidates.indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&c)) = (
            candidates.positions.get(triangle[0] as usize),
            candidates.positions.get(triangle[1] as usize),
            candidates.positions.get(triangle[2] as usize),
        ) else {
            continue;
        };
        let normal = (b - a).cross(c - a).normalize_or_zero();
        if normal == DVec3::ZERO {
            continue;
        }
        let centre = (a + b + c) / 3.0;
        let negative = inside_result(centre - normal * probe);
        let positive = inside_result(centre + normal * probe);
        if negative == positive {
            continue;
        }
        let base = out.positions.len() as u32;
        out.positions.extend_from_slice(&[a, b, c]);
        if negative {
            out.push_triangle(base, base + 1, base + 2);
        } else {
            out.push_triangle(base, base + 2, base + 1);
        }
    }
    if out.is_empty() {
        return out;
    }
    finish(&mut out, tol);
    out
}

/// The faces of two closed solids, each split along the other's cells, kept
/// where the probe finds material on one side only.
///
/// The boundary of any boolean of `a` and `b` lies on the boundary of `a` or
/// of `b`, so their own faces are the only candidates needed; the cells serve
/// to split them and to answer the probe.
fn combine(
    a: &Mesh64,
    a_set: &CellSet,
    b: &Mesh64,
    b_set: &CellSet,
    inside_result: &dyn Fn(DVec3) -> bool,
    tol: f64,
) -> Mesh64 {
    // Where two shells of one solid touch, a face can be interface and boundary
    // at once, so such a solid's faces are split by its own cells too.
    let own = |mesh: &Mesh64, set: &CellSet| {
        if has_opposite_faces(mesh, tol) {
            split_by_cells(mesh, set, tol)
        } else {
            mesh.clone()
        }
    };
    let mut candidates = split_by_cells(&own(a, a_set), b_set, tol);
    let from_b = split_by_cells(&own(b, b_set), a_set, tol);
    candidates.append(&drop_coincident(&from_b, a, tol));
    keep_boundary(&candidates, inside_result, tol)
}

/// Drop the pieces of `from_b` that lie on a face of `a` looking the other way.
///
/// A body that already holds its opening as a hole meets a cutter whose sides
/// coincide with the hole's. Both sides pass the probe, and the surface would
/// be drawn twice; the body's own face is the one kept.
fn drop_coincident(from_b: &Mesh64, a: &Mesh64, tol: f64) -> Mesh64 {
    let key = |plane: &Plane| {
        (
            (plane.normal.x * 1e6).round() as i64,
            (plane.normal.y * 1e6).round() as i64,
            (plane.normal.z * 1e6).round() as i64,
            (plane.offset / tol.max(1e-12)).round() as i64,
        )
    };
    let mut faces: std::collections::HashMap<(i64, i64, i64, i64), Vec<[DVec3; 3]>> =
        Default::default();
    for triangle in a.indices.chunks_exact(3) {
        let (Some(&p), Some(&q), Some(&r)) = (
            a.positions.get(triangle[0] as usize),
            a.positions.get(triangle[1] as usize),
            a.positions.get(triangle[2] as usize),
        ) else {
            continue;
        };
        if let Some(plane) = Plane::from_point_normal(p, (q - p).cross(r - p)) {
            faces
                .entry(key(&plane.flipped()))
                .or_default()
                .push([p, q, r]);
        }
    }
    if faces.is_empty() {
        return from_b.clone();
    }
    let mut out = Mesh64::with_capacity(from_b.positions.len(), from_b.indices.len());
    for triangle in from_b.indices.chunks_exact(3) {
        let (Some(&p), Some(&q), Some(&r)) = (
            from_b.positions.get(triangle[0] as usize),
            from_b.positions.get(triangle[1] as usize),
            from_b.positions.get(triangle[2] as usize),
        ) else {
            continue;
        };
        let Some(plane) = Plane::from_point_normal(p, (q - p).cross(r - p)) else {
            continue;
        };
        let centre = (p + q + r) / 3.0;
        let covered = faces
            .get(&key(&plane))
            .is_some_and(|list| list.iter().any(|face| inside_triangle(centre, face, tol)));
        if !covered {
            emit(&mut out, &[p, q, r]);
        }
    }
    out
}

/// Is a point on a triangle's plane inside the triangle, within the tolerance?
fn inside_triangle(point: DVec3, face: &[DVec3; 3], tol: f64) -> bool {
    let normal = (face[1] - face[0]).cross(face[2] - face[0]);
    let length = normal.length();
    if length <= tol * tol {
        return false;
    }
    let normal = normal / length;
    (0..3).all(|k| {
        let edge = face[(k + 1) % 3] - face[k];
        let edge_length = edge.length();
        edge_length > tol && edge.cross(point - face[k]).dot(normal) >= -tol * edge_length
    })
}

/// Does any face have another face on the same plane looking the other way?
///
/// That is the signature of two shells touching, or of a shell folded onto
/// itself; either way some face is an interface on part of its extent.
fn has_opposite_faces(mesh: &Mesh64, tol: f64) -> bool {
    let key = |plane: &Plane| {
        let scale = 1.0 / tol.max(1e-12);
        (
            (plane.normal.x * 1e6).round() as i64,
            (plane.normal.y * 1e6).round() as i64,
            (plane.normal.z * 1e6).round() as i64,
            (plane.offset * scale).round() as i64,
        )
    };
    let mut seen: std::collections::HashSet<(i64, i64, i64, i64)> = Default::default();
    let mut planes: Vec<Plane> = Vec::with_capacity(mesh.triangle_count());
    for triangle in mesh.indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&c)) = (
            mesh.positions.get(triangle[0] as usize),
            mesh.positions.get(triangle[1] as usize),
            mesh.positions.get(triangle[2] as usize),
        ) else {
            continue;
        };
        if let Some(plane) = Plane::from_point_normal(a, (b - a).cross(c - a)) {
            seen.insert(key(&plane));
            planes.push(plane);
        }
    }
    planes
        .iter()
        .any(|plane| seen.contains(&key(&plane.flipped())))
}

/// Close a boolean result: weld, heal what the weld could not close, merge the
/// coplanar fragments the cell splits left, and orient.
///
/// The four operators all end this way. The merge runs after the heal so a
/// vertex a neighbour needs is on the region's boundary by then and survives.
fn finish(mesh: &mut Mesh64, tol: f64) {
    if mesh.is_empty() {
        return;
    }
    crate::weld::weld_and_close(mesh, tol);
    // T-junctions only where two independently triangulated surfaces meet; a
    // shell the weld already closed has none worth searching for.
    if !mesh.is_edge_manifold() && crate::weld::heal_t_junctions(mesh, tol) > 0 {
        crate::weld::weld_and_close(mesh, tol);
    }
    crate::simplify::merge_coplanar(mesh, tol);
    mesh.fix_orientation();
}

/// True when the two boxes are apart by more than the tolerance.
fn boxes_apart(a: (DVec3, DVec3), b: (DVec3, DVec3), tol: f64) -> bool {
    a.0.cmpgt(b.1 + DVec3::splat(tol)).any() || a.1.cmplt(b.0 - DVec3::splat(tol)).any()
}

/// The union of two closed solids, built from their convex cells.
///
/// The boundary of `A u B` is the boundary of `A` outside `B` together with the
/// boundary of `B` outside `A`: each solid's faces are split along the other's
/// cells and the two-sided probe keeps the pieces outside the other solid.
/// Solids that do not overlap are simply put side by side. `None` when either
/// cannot be decomposed; the caller then appends the shells and says so.
pub fn union_general(a: &Mesh64, b: &Mesh64, tolerance: f64) -> Option<Mesh64> {
    union_general_or_reason(a, b, tolerance).ok()
}

/// [`union_general`], with the reason for a refusal.
pub fn union_general_or_reason(a: &Mesh64, b: &Mesh64, tolerance: f64) -> Result<Mesh64, String> {
    let close = valid_tolerance(tolerance);
    let tol = close.min(GENERAL_TOLERANCE);
    if a.is_empty() {
        return Ok(b.clone());
    }
    if b.is_empty() {
        return Ok(a.clone());
    }
    let (Some(bounds_a), Some(bounds_b)) = (a.bounds(), b.bounds()) else {
        return Err("an operand has no bounds".into());
    };
    if boxes_apart(bounds_a, bounds_b, tol) {
        let mut out = a.clone();
        out.append(b);
        return Ok(out);
    }
    let cells_a = CellSet::new(
        convex_cells_closing(a, tol, close).map_err(|why| format!("first operand: {why}"))?,
        tol,
    )
    .ok_or_else(|| "a cell of the first operand is not a convex solid".to_string())?;
    let cells_b = CellSet::new(
        convex_cells_closing(b, tol, close).map_err(|why| format!("second operand: {why}"))?,
        tol,
    )
    .ok_or_else(|| "a cell of the second operand is not a convex solid".to_string())?;
    let inside = |point: DVec3| cells_a.contains(point, tol) || cells_b.contains(point, tol);
    Ok(combine(a, &cells_a, b, &cells_b, &inside, tol))
}

/// The intersection of two closed solids, built from their convex cells.
///
/// Each solid's faces are split along the other's cells and the two-sided
/// probe keeps the pieces inside the other solid. An empty mesh means the
/// solids do not overlap; `None` means one of them could not be decomposed.
pub fn intersection_general(a: &Mesh64, b: &Mesh64, tolerance: f64) -> Option<Mesh64> {
    intersection_general_or_reason(a, b, tolerance).ok()
}

/// [`intersection_general`], with the reason for a refusal.
pub fn intersection_general_or_reason(
    a: &Mesh64,
    b: &Mesh64,
    tolerance: f64,
) -> Result<Mesh64, String> {
    let close = valid_tolerance(tolerance);
    let tol = close.min(GENERAL_TOLERANCE);
    if a.is_empty() || b.is_empty() {
        return Ok(Mesh64::new());
    }
    let (Some(bounds_a), Some(bounds_b)) = (a.bounds(), b.bounds()) else {
        return Err("an operand has no bounds".into());
    };
    if boxes_apart(bounds_a, bounds_b, tol) {
        return Ok(Mesh64::new());
    }
    let cells_a = CellSet::new(
        convex_cells_closing(a, tol, close).map_err(|why| format!("first operand: {why}"))?,
        tol,
    )
    .ok_or_else(|| "a cell of the first operand is not a convex solid".to_string())?;
    let cells_b = CellSet::new(
        convex_cells_closing(b, tol, close).map_err(|why| format!("second operand: {why}"))?,
        tol,
    )
    .ok_or_else(|| "a cell of the second operand is not a convex solid".to_string())?;
    let inside = |point: DVec3| cells_a.contains(point, tol) && cells_b.contains(point, tol);
    Ok(combine(a, &cells_a, b, &cells_b, &inside, tol))
}

/// Cut closed solids out of a surface that is not one.
///
/// A face-based model, an open shell or a solid with a hole in its skin has
/// no inside to reason about, so the cut is the one a surface allows: every
/// face is split along the cutters' cells and the pieces inside a cutter are
/// dropped. Nothing is added where a solid would gain the walls of the cut.
pub fn difference_surface_many_or_reason(
    surface: &Mesh64,
    cutters: &[Mesh64],
    tolerance: f64,
) -> Result<Mesh64, String> {
    let close = valid_tolerance(tolerance);
    let tol = close.min(GENERAL_TOLERANCE);
    if surface.is_empty() || cutters.is_empty() {
        return Err("an empty operand".into());
    }
    let mut cutter_cells = Vec::new();
    for (index, cutter) in cutters.iter().enumerate() {
        cutter_cells.extend(
            convex_cells_closing(cutter, tol, close)
                .map_err(|why| format!("cutter {}: {why}", index + 1))?,
        );
    }
    let cutter_set = CellSet::new(cutter_cells, tol)
        .ok_or_else(|| "a cutter cell is not a convex solid".to_string())?;
    let pieces = split_by_cells(surface, &cutter_set, tol);
    let mut out = Mesh64::with_capacity(pieces.positions.len(), pieces.indices.len());
    for triangle in pieces.indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&c)) = (
            pieces.positions.get(triangle[0] as usize),
            pieces.positions.get(triangle[1] as usize),
            pieces.positions.get(triangle[2] as usize),
        ) else {
            continue;
        };
        if cutter_set.contains((a + b + c) / 3.0, tol) {
            continue;
        }
        emit(&mut out, &[a, b, c]);
    }
    // A surface has no inside, so orienting it by volume is meaningless; the
    // rest of the tail still applies.
    crate::weld::weld_and_close(&mut out, tol);
    if !out.is_edge_manifold() && crate::weld::heal_t_junctions(&mut out, tol) > 0 {
        crate::weld::weld_and_close(&mut out, tol);
    }
    crate::simplify::merge_coplanar(&mut out, tol);
    Ok(out)
}

/// The largest number of distinct face planes a prism candidate may have.
///
/// A swept opening has one plane per profile edge plus two caps. A mesh with
/// hundreds of distinct planes is a curved or free-form shell, not a prism, and
/// the pairing search below is quadratic in this number.
const MAX_PRISM_PLANES: usize = 256;

/// The largest mesh a prism proof is attempted on.
///
/// Every check below is at least linear in the triangle count and the search
/// runs on meshes that turn out not to be prisms at all, so the budget matters
/// more than the ceiling. A swept opening or a tessellated wall is far under
/// this; a curved free-form shell is far over it and is refused immediately.
const MAX_PRISM_TRIANGLES: usize = 4096;

/// How many plane groups take part in the sweep-direction search.
///
/// The groups are in first-seen order and any two non-parallel sides settle the
/// direction, so a handful is always enough. The bound keeps a many-faced shell
/// that is not a prism from costing a quadratic scan before it is refused.
const MAX_PRISM_DIRECTION_GROUPS: usize = 24;

/// How far a face normal may lean out of the sweep and still be a side.
///
/// Exporter coordinates carry real noise, so this is a near-perpendicular test
/// rather than an exact one. A face a thousandth of a degree off is still a
/// side; a cap is nowhere near it.
const PRISM_SIDE_SKEW: f64 = 1e-5;

/// Recover the triangular prisms whose union is a translational sweep: two
/// parallel cap groups related by one displacement and side faces along it.
/// Reads the geometry, not an index layout, so triangles may arrive in any
/// order and winding.
fn prism_cells(mesh: &Mesh64, tol: f64) -> Option<Vec<Mesh64>> {
    let faces = prism_faces(mesh, tol)?;
    let groups = plane_groups(&faces, tol)?;

    // The sweep direction identifies a prism: two non-parallel side normals cross
    // to give it exactly, where a search for caps would pick a wall's two skins.
    let mut directions: Vec<DVec3> = Vec::new();
    let limit = groups.len().min(MAX_PRISM_DIRECTION_GROUPS);
    for i in 0..limit {
        for j in (i + 1)..limit {
            let cross = groups[i].normal.cross(groups[j].normal);
            let length = cross.length();
            if length < 1e-6 {
                continue;
            }
            let direction = canonical_normal(cross / length);
            if !directions
                .iter()
                .any(|known| known.dot(direction) > 1.0 - 1e-9)
            {
                directions.push(direction);
            }
        }
    }

    for direction in directions {
        // The caps are the groups the direction is not parallel to.
        let mut caps = Vec::new();
        let mut usable = true;
        for (index, group) in groups.iter().enumerate() {
            if group.normal.dot(direction).abs() <= PRISM_SIDE_SKEW {
                continue;
            }
            caps.push(index);
            if caps.len() > 2 {
                usable = false;
                break;
            }
        }
        if !usable || caps.len() != 2 {
            continue;
        }
        let (first, second) = (&groups[caps[0]], &groups[caps[1]]);
        if first.normal.dot(second.normal).abs() <= 1.0 - 1e-9
            || (first.offset - second.offset).abs() <= tol
        {
            continue;
        }
        if let Some(cells) = prism_from_caps(&faces, first, second, tol) {
            return Some(cells);
        }
    }
    None
}

/// One triangle of a candidate prism, with everything the search needs.
struct PrismFace {
    corners: [DVec3; 3],
    /// Unit normal, sign-canonicalised so that winding does not split a plane.
    canonical: DVec3,
    offset: f64,
    area: f64,
}

/// Triangles with a usable normal, or `None` when the mesh is not a closed
/// solid this can reason about.
fn prism_faces(mesh: &Mesh64, tol: f64) -> Option<Vec<PrismFace>> {
    if mesh.indices.len() < 3 * 5
        || mesh.indices.len() > MAX_PRISM_TRIANGLES * 3
        || !mesh.is_edge_manifold()
    {
        return None;
    }
    let mut faces = Vec::with_capacity(mesh.indices.len() / 3);
    for triangle in mesh.indices.chunks_exact(3) {
        let corners = [
            *mesh.positions.get(triangle[0] as usize)?,
            *mesh.positions.get(triangle[1] as usize)?,
            *mesh.positions.get(triangle[2] as usize)?,
        ];
        let cross = (corners[1] - corners[0]).cross(corners[2] - corners[0]);
        let length = cross.length();
        if length < tol * tol {
            continue;
        }
        let canonical = canonical_normal(cross / length);
        faces.push(PrismFace {
            corners,
            canonical,
            offset: canonical.dot(corners[0]),
            area: length * 0.5,
        });
    }
    (faces.len() >= 5).then_some(faces)
}

/// A normal with a deterministic sign, so that two oppositely wound triangles
/// on the same plane compare equal.
fn canonical_normal(normal: DVec3) -> DVec3 {
    let axis = if normal.x.abs() >= normal.y.abs() && normal.x.abs() >= normal.z.abs() {
        normal.x
    } else if normal.y.abs() >= normal.z.abs() {
        normal.y
    } else {
        normal.z
    };
    if axis < 0.0 { -normal } else { normal }
}

/// Triangles sharing a plane, whatever their winding.
struct PlaneGroup {
    normal: DVec3,
    offset: f64,
    area: f64,
    /// Area-weighted centroid, which for two congruent caps differs by exactly
    /// the sweep displacement.
    centroid: DVec3,
    members: Vec<usize>,
}

fn plane_groups(faces: &[PrismFace], tol: f64) -> Option<Vec<PlaneGroup>> {
    let mut groups: Vec<PlaneGroup> = Vec::new();
    for (index, face) in faces.iter().enumerate() {
        let existing = groups.iter_mut().find(|group| {
            group.normal.dot(face.canonical) > 1.0 - 1e-9
                && (group.offset - face.offset).abs() <= tol
        });
        let centre = (face.corners[0] + face.corners[1] + face.corners[2]) / 3.0;
        match existing {
            Some(group) => {
                group.centroid += centre * face.area;
                group.area += face.area;
                group.members.push(index);
            }
            None => {
                if groups.len() >= MAX_PRISM_PLANES {
                    return None;
                }
                groups.push(PlaneGroup {
                    normal: face.canonical,
                    offset: face.offset,
                    area: face.area,
                    centroid: centre * face.area,
                    members: vec![index],
                });
            }
        }
    }
    for group in &mut groups {
        if group.area <= 0.0 {
            return None;
        }
        group.centroid /= group.area;
    }
    (groups.len() >= 5).then_some(groups)
}

/// Prove that two parallel groups are the caps of one sweep, and build the cells.
fn prism_from_caps(
    faces: &[PrismFace],
    first: &PlaneGroup,
    second: &PlaneGroup,
    tol: f64,
) -> Option<Vec<Mesh64>> {
    // Congruent caps have equal area. Relative, because a cap in millimetres
    // and a cap in metres cannot share one absolute threshold.
    let scale = first.area.max(second.area);
    if (first.area - second.area).abs() > scale * 1e-6 + tol * tol {
        return None;
    }
    let displacement = second.centroid - first.centroid;
    let height = first.normal.dot(displacement);
    // A sweep that stays in the cap plane has no volume, and the outward
    // direction below would be undecidable.
    if height.abs() <= tol {
        return None;
    }

    // Every vertex of one cap must land on a vertex of the other. This is the
    // test that separates a genuine sweep from two unrelated parallel faces.
    let start = cap_vertices(faces, first, tol);
    let end = cap_vertices(faces, second, tol);
    if start.len() != end.len() || start.is_empty() {
        return None;
    }
    for point in &start {
        let moved = *point + displacement;
        if !end.iter().any(|other| (*other - moved).length() <= tol) {
            return None;
        }
    }

    // Side faces run along the sweep, so their normals are perpendicular to it.
    let direction = displacement.normalize_or_zero();
    if direction == DVec3::ZERO {
        return None;
    }
    let mut is_cap = vec![false; faces.len()];
    for &index in first.members.iter().chain(&second.members) {
        is_cap[index] = true;
    }
    let mut side_count = 0;
    for (index, face) in faces.iter().enumerate() {
        if is_cap[index] {
            continue;
        }
        side_count += 1;
        if face.canonical.dot(direction).abs() > PRISM_SIDE_SKEW {
            return None;
        }
    }
    if side_count == 0 {
        return None;
    }

    // The cap the sweep leaves from is the one the displacement points away
    // from, so its outward normal opposes the displacement.
    let outward = if height > 0.0 {
        -first.normal
    } else {
        first.normal
    };
    let triangles: Vec<[DVec3; 3]> = first
        .members
        .iter()
        .map(|&index| faces[index].corners)
        .collect();
    let mut cells = Vec::with_capacity(triangles.len());
    for polygon in merge_convex(&triangles, outward, tol) {
        match swept_prism(&polygon, outward, displacement) {
            Some(cell) => cells.push(cell),
            // A polygon that will not sweep is not a reason to lose the cut:
            // its own triangles still tile the same area.
            None => return None,
        }
    }
    if cells.is_empty() {
        return None;
    }
    Some(cells)
}

/// Join coplanar triangles into as few convex polygons as possible (Hertel and
/// Mehlhorn): drop every shared diagonal whose removal leaves both endpoints
/// convex. A merge that cannot be proved convex does not happen, so the worst
/// case is the triangulation this started from.
fn merge_convex(triangles: &[[DVec3; 3]], normal: DVec3, tol: f64) -> Vec<Vec<DVec3>> {
    let mut vertices: Vec<DVec3> = Vec::new();
    let index_of = |point: DVec3, vertices: &mut Vec<DVec3>| -> u32 {
        match vertices
            .iter()
            .position(|known| (*known - point).length() <= tol)
        {
            Some(index) => index as u32,
            None => {
                vertices.push(point);
                (vertices.len() - 1) as u32
            }
        }
    };
    let mut pieces: Vec<Vec<u32>> = Vec::with_capacity(triangles.len());
    for triangle in triangles {
        let loop_of = [
            index_of(triangle[0], &mut vertices),
            index_of(triangle[1], &mut vertices),
            index_of(triangle[2], &mut vertices),
        ];
        if loop_of[0] == loop_of[1] || loop_of[1] == loop_of[2] || loop_of[2] == loop_of[0] {
            continue;
        }
        pieces.push(loop_of.to_vec());
    }

    let mut merged = true;
    while merged {
        merged = false;
        'outer: for left in 0..pieces.len() {
            for right in (left + 1)..pieces.len() {
                let Some(joined) = join_if_convex(&pieces[left], &pieces[right], &vertices, normal)
                else {
                    continue;
                };
                pieces[left] = joined;
                pieces.remove(right);
                merged = true;
                break 'outer;
            }
        }
    }

    pieces
        .into_iter()
        .map(|piece| {
            piece
                .into_iter()
                .map(|index| vertices[index as usize])
                .collect()
        })
        .collect()
}

/// Splice two polygons across the one edge they share, if the result is convex.
fn join_if_convex(
    left: &[u32],
    right: &[u32],
    vertices: &[DVec3],
    normal: DVec3,
) -> Option<Vec<u32>> {
    // The shared edge runs opposite ways round the two polygons; two shared edges
    // would leave a hole after splicing, so exactly one is required.
    let mut shared = None;
    for (i, &a) in left.iter().enumerate() {
        let b = left[(i + 1) % left.len()];
        for (j, &c) in right.iter().enumerate() {
            let d = right[(j + 1) % right.len()];
            if a == d && b == c {
                if shared.is_some() {
                    return None;
                }
                shared = Some((i, j));
            }
        }
    }
    let (i, j) = shared?;

    // Left up to and including a, then the right polygon from d round to c,
    // then the rest of the left polygon.
    let mut joined = Vec::with_capacity(left.len() + right.len() - 2);
    for step in 0..left.len() {
        joined.push(left[(i + 1 + step) % left.len()]);
    }
    // joined now starts at b and ends at a. Insert the right polygon's own
    // path from b back round to a, which is everything but its shared edge.
    let mut inserted = Vec::with_capacity(right.len() - 2);
    for step in 1..right.len() - 1 {
        inserted.push(right[(j + 1 + step) % right.len()]);
    }
    joined.splice(0..0, inserted);
    // Rotate so the loop reads naturally; the winding is what matters.
    if joined.len() < 3 {
        return None;
    }
    // A vertex cannot appear twice: that would be a pinch, not a polygon.
    for (position, value) in joined.iter().enumerate() {
        if joined[position + 1..].contains(value) {
            return None;
        }
    }
    is_convex_loop(&joined, vertices, normal).then_some(joined)
}

/// Every turn in the same direction as the face normal.
fn is_convex_loop(loop_of: &[u32], vertices: &[DVec3], normal: DVec3) -> bool {
    let count = loop_of.len();
    for index in 0..count {
        let previous = vertices[loop_of[(index + count - 1) % count] as usize];
        let current = vertices[loop_of[index] as usize];
        let next = vertices[loop_of[(index + 1) % count] as usize];
        let turn = (current - previous).cross(next - current).dot(normal);
        // A straight run is allowed; a reflex corner is not.
        let scale = (current - previous).length() * (next - current).length();
        if turn < -scale * 1e-9 {
            return false;
        }
    }
    true
}

/// One convex cap polygon swept by `displacement`, wound outwards.
///
/// The cap is convex, so it fans from its first corner exactly: no
/// triangulator, and no way for the cell to disagree with the polygon it came
/// from.
fn swept_prism(base: &[DVec3], outward: DVec3, displacement: DVec3) -> Option<Mesh64> {
    let count = base.len();
    if count < 3 {
        return None;
    }
    let normal = crate::mesh::newell_normal(base);
    let base: Vec<DVec3> = if normal.dot(outward) >= 0.0 {
        base.to_vec()
    } else {
        base.iter().rev().copied().collect()
    };

    let offset = count as u32;
    let mut mesh = Mesh64::with_capacity(count * 2, (count - 2) * 6 + count * 6);
    for point in &base {
        mesh.push_vertex(*point);
    }
    for point in &base {
        mesh.push_vertex(*point + displacement);
    }
    for corner in 1..offset - 1 {
        mesh.push_triangle(0, corner, corner + 1);
        mesh.push_triangle(offset, corner + 1 + offset, corner + offset);
    }
    for edge in 0..offset {
        let a = edge;
        let b = (edge + 1) % offset;
        mesh.push_triangle(a, a + offset, b + offset);
        mesh.push_triangle(a, b + offset, b);
    }
    mesh.closed = Some(true);
    Some(mesh)
}

/// Distinct vertices of one cap.
fn cap_vertices(faces: &[PrismFace], group: &PlaneGroup, tol: f64) -> Vec<DVec3> {
    let mut points: Vec<DVec3> = Vec::new();
    for &index in &group.members {
        for corner in faces[index].corners {
            if !points.iter().any(|other| (*other - corner).length() <= tol) {
                points.push(corner);
            }
        }
    }
    points
}

/// Subtract closed solids from a closed solid, whatever their shape.
///
/// Body and cutters are split into convex cells by [`convex_cells`], each
/// side's faces are split along the other's cells, and a probe on both sides
/// of every piece keeps the ones separating the result from empty space.
/// `None` when either side fails to prove a decomposition within budget.
pub fn difference_prismatic_many(
    body: &Mesh64,
    cutters: &[Mesh64],
    tolerance: f64,
) -> Option<Mesh64> {
    difference_prismatic_many_or_reason(body, cutters, tolerance).ok()
}

/// Closed shells that weld into one surface because they touch along edges,
/// or `None` when the surface has no shared edge or does not come apart.
fn touching_shells(mesh: &Mesh64, close: f64) -> Option<Vec<Mesh64>> {
    let mut welded = mesh.clone();
    crate::weld::weld(&mut welded, close);
    if welded.edge_defects().1 == 0 {
        return None;
    }
    crate::weld::split_manifold_shells(mesh, close).filter(|shells| shells.len() >= 2)
}

/// [`difference_prismatic_many`], with the reason for a refusal.
pub fn difference_prismatic_many_or_reason(
    body: &Mesh64,
    cutters: &[Mesh64],
    tolerance: f64,
) -> Result<Mesh64, String> {
    let close = valid_tolerance(tolerance);
    let tol = close.min(GENERAL_TOLERANCE);
    if body.is_empty() || cutters.is_empty() {
        return Err("an empty operand".into());
    }
    // Cutters first: an opening is far smaller than the wall it cuts, so the
    // common refusal is found before the expensive side is touched.
    let mut cutter_cells = Vec::new();
    for (index, cutter) in cutters.iter().enumerate() {
        let cells = match convex_cells_closing(cutter, tol, close) {
            Ok(cells) => Ok(cells),
            // An opening of several touching solids is decomposed shell by shell.
            Err(why) => match touching_shells(cutter, close) {
                Some(shells) => shells
                    .iter()
                    .map(|shell| convex_cells_closing(shell, tol, close))
                    .collect::<Result<Vec<_>, _>>()
                    .map(|cells| cells.concat())
                    .map_err(|_| why),
                None => Err(why),
            },
        };
        cutter_cells.extend(cells.map_err(|why| format!("cutter {}: {why}", index + 1))?);
    }
    let body_cells = match convex_cells_closing(body, tol, close) {
        Ok(cells) => cells,
        // A body of several shells, such as the layers of a wall, is cut one
        // shell at a time; only a body in one piece is refused outright.
        Err(why) => {
            let shells = touching_shells(body, close).unwrap_or_else(|| {
                let mut welded = body.clone();
                crate::weld::weld_and_close(&mut welded, close);
                welded.connected_components()
            });
            if shells.len() < 2 {
                return Err(format!("body: {why}"));
            }
            let mut out = Mesh64::new();
            for (index, shell) in shells.iter().enumerate() {
                let cut = difference_convex_many(shell, cutters, close)
                    .or_else(|| difference_extrusion_many(shell, cutters, close))
                    .map(Ok)
                    .unwrap_or_else(|| difference_prismatic_many_or_reason(shell, cutters, close))
                    .map_err(|why| {
                        format!(
                            "body: shell {} of {}: {}",
                            index + 1,
                            shells.len(),
                            why.trim_start_matches("body: ")
                        )
                    })?;
                out.append(&cut);
            }
            return Ok(out);
        }
    };
    if body_cells.is_empty() || cutter_cells.is_empty() {
        return Err("no cells".into());
    }
    // Nothing here is cheaper than the convex path, so leave that case to it.
    if body_cells.len() == 1 && cutter_cells.len() == cutters.len() {
        return difference_convex_many(body, cutters, tol)
            .ok_or_else(|| "the convex difference was refused".to_string());
    }

    let body_set = CellSet::new(body_cells, tol)
        .ok_or_else(|| "a body cell is not a convex solid".to_string())?;
    let cutter_set = CellSet::new(cutter_cells, tol)
        .ok_or_else(|| "a cutter cell is not a convex solid".to_string())?;

    let mut cutter_faces = Mesh64::new();
    for cutter in cutters {
        cutter_faces.append(cutter);
    }
    let inside_result =
        |point: DVec3| body_set.contains(point, tol) && !cutter_set.contains(point, tol);
    Ok(combine(
        body,
        &body_set,
        &cutter_faces,
        &cutter_set,
        &inside_result,
        tol,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An axis-aligned box as a closed, outward-wound mesh.
    fn box_mesh(lo: DVec3, hi: DVec3) -> Mesh64 {
        let mut mesh = Mesh64::new();
        let corner = |i: usize| {
            DVec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            )
        };
        for index in 0..8 {
            mesh.push_vertex(corner(index));
        }
        // Each face counter-clockwise seen from outside.
        let faces = [
            [0, 2, 3, 1], // -z
            [4, 5, 7, 6], // +z
            [0, 1, 5, 4], // -y
            [2, 6, 7, 3], // +y
            [0, 4, 6, 2], // -x
            [1, 3, 7, 5], // +x
        ];
        for face in faces {
            mesh.push_triangle(face[0], face[1], face[2]);
            mesh.push_triangle(face[0], face[2], face[3]);
        }
        mesh
    }

    /// A 2 m cube with a 1 m pocket from the top: neither convex nor a prism.
    fn cup() -> Mesh64 {
        let block = box_mesh(DVec3::ZERO, DVec3::splat(2.0));
        let pocket = box_mesh(DVec3::new(0.5, 0.5, 1.0), DVec3::new(1.5, 1.5, 2.5));
        let cup = difference_convex(&block, &pocket, 1e-9).unwrap();
        assert!((cup.signed_volume() - 7.0).abs() < 1e-9);
        cup
    }

    #[test]
    fn two_overlapping_boxes_unite_without_counting_their_shared_eighth_twice() {
        let a = box_mesh(DVec3::ZERO, DVec3::ONE);
        let b = box_mesh(DVec3::splat(0.5), DVec3::splat(1.5));
        let joined = union_general(&a, &b, 1e-9).unwrap();
        assert!(joined.is_edge_manifold());
        assert!(
            (joined.signed_volume() - 1.875).abs() < 1e-9,
            "{}",
            joined.signed_volume()
        );
        assert!(
            (joined.surface_area() - 10.5).abs() < 1e-9,
            "{}",
            joined.surface_area()
        );
    }

    #[test]
    fn two_boxes_that_share_a_face_unite_into_one_box() {
        // The coplanar rule: the shared face is inside the union and goes.
        let a = box_mesh(DVec3::ZERO, DVec3::ONE);
        let b = box_mesh(DVec3::new(1.0, 0.0, 0.0), DVec3::new(2.0, 1.0, 1.0));
        let joined = union_general(&a, &b, 1e-9).unwrap();
        assert!(joined.is_edge_manifold());
        assert!((joined.signed_volume() - 2.0).abs() < 1e-9);
        assert!(
            (joined.surface_area() - 10.0).abs() < 1e-9,
            "{}",
            joined.surface_area()
        );
    }

    #[test]
    fn two_boxes_apart_unite_as_two_shells() {
        let a = box_mesh(DVec3::ZERO, DVec3::ONE);
        let b = box_mesh(DVec3::splat(3.0), DVec3::splat(4.0));
        let joined = union_general(&a, &b, 1e-9).unwrap();
        assert!((joined.signed_volume() - 2.0).abs() < 1e-9);
        assert_eq!(joined.triangle_count(), 24);
    }

    #[test]
    fn two_overlapping_boxes_intersect_in_their_overlap() {
        let a = box_mesh(DVec3::ZERO, DVec3::ONE);
        let b = box_mesh(DVec3::splat(0.5), DVec3::splat(1.5));
        let common = intersection_general(&a, &b, 1e-9).unwrap();
        assert!(common.is_edge_manifold());
        assert!((common.signed_volume() - 0.125).abs() < 1e-9);
        assert!((common.surface_area() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn two_boxes_that_only_touch_have_an_empty_intersection() {
        let a = box_mesh(DVec3::ZERO, DVec3::ONE);
        let b = box_mesh(DVec3::new(1.0, 0.0, 0.0), DVec3::new(2.0, 1.0, 1.0));
        let common = intersection_general(&a, &b, 1e-9).unwrap();
        assert!(common.is_empty());
    }

    fn edge_uses(mesh: &Mesh64) -> (usize, usize) {
        let mut uses: std::collections::HashMap<(u32, u32), usize> = Default::default();
        for triangle in mesh.indices.chunks_exact(3) {
            for k in 0..3 {
                let (a, b) = (triangle[k], triangle[(k + 1) % 3]);
                *uses.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        (
            uses.values().filter(|&&n| n == 1).count(),
            uses.values().filter(|&&n| n > 2).count(),
        )
    }

    #[test]
    fn the_cells_of_a_pocketed_body_reassemble_into_it() {
        let cup = cup();
        let far = box_mesh(DVec3::splat(10.0), DVec3::splat(11.0));
        let same = difference_prismatic_many(&cup, &[far], 1e-9).unwrap();
        let (open, over) = edge_uses(&same);
        // Area per face plane, before and after, to name what went missing.
        let per_plane = |mesh: &Mesh64| {
            let mut areas: Vec<(Plane, f64)> = Vec::new();
            for triangle in mesh.indices.chunks_exact(3) {
                let (a, b, c) = (
                    mesh.positions[triangle[0] as usize],
                    mesh.positions[triangle[1] as usize],
                    mesh.positions[triangle[2] as usize],
                );
                let cross = (b - a).cross(c - a);
                let Some(plane) = Plane::from_point_normal(a, cross) else {
                    continue;
                };
                let area = cross.length() * 0.5;
                match areas.iter_mut().find(|(known, _)| {
                    known.normal.dot(plane.normal) > 1.0 - 1e-6
                        && (known.offset - plane.offset).abs() < 1e-6
                }) {
                    Some((_, total)) => *total += area,
                    None => areas.push((plane, area)),
                }
            }
            areas
        };
        let before = per_plane(&cup);
        let after = per_plane(&same);
        let mut report = String::new();
        for (plane, area) in &before {
            let got = after
                .iter()
                .find(|(known, _)| {
                    known.normal.dot(plane.normal) > 1.0 - 1e-6
                        && (known.offset - plane.offset).abs() < 1e-6
                })
                .map(|(_, a)| *a)
                .unwrap_or(0.0);
            if (got - area).abs() > 1e-9 {
                report.push_str(&format!(
                    "
  plane n={:?} d={:.3}: area {area:.4} -> {got:.4}",
                    plane.normal, plane.offset
                ));
            }
        }
        assert!(
            (same.signed_volume() - 7.0).abs() < 1e-9,
            "volume {} ({open} boundary edges, {over} overused edges){report}",
            same.signed_volume()
        );
        assert!(
            same.is_edge_manifold(),
            "{open} boundary edges, {over} overused edges"
        );
    }

    #[test]
    fn a_pocketed_body_is_cut_by_the_general_path() {
        // The pocket makes the cup neither convex nor a prism, so only the BSP
        // cells can carry it; the side box takes 0.25 x 0.5 x 0.5 out of a wall.
        let cup = cup();
        let side = box_mesh(DVec3::new(-1.0, 0.75, 0.25), DVec3::new(0.25, 1.25, 0.75));
        let cut = difference_prismatic_many(&cup, &[side], 1e-9).unwrap();
        assert!(cut.is_edge_manifold(), "the result is one closed shell");
        let volume = cut.signed_volume();
        assert!((volume - 6.9375).abs() < 1e-9, "got {volume}");
    }

    #[test]
    fn a_cutter_flush_with_a_pocketed_body_leaves_no_lid() {
        // A notch open at the top and at two sides: flush faces must not be capped.
        let cup = cup();
        let notch = box_mesh(DVec3::new(-1.0, -1.0, 1.5), DVec3::new(0.25, 0.25, 3.0));
        let cut = difference_prismatic_many(&cup, &[notch], 1e-9).unwrap();
        assert!(
            cut.is_edge_manifold(),
            "a lid would leave the shell non-manifold"
        );
        let volume = cut.signed_volume();
        assert!(
            (volume - (7.0 - 0.25 * 0.25 * 0.5)).abs() < 1e-9,
            "got {volume}"
        );
    }

    #[test]
    fn a_ring_shaped_cutter_takes_a_ring_out_of_a_block() {
        // A square annulus is not convex and its cap has a hole: the BSP does it.
        let block = box_mesh(DVec3::ZERO, DVec3::new(2.0, 2.0, 0.5));
        let outer = box_mesh(DVec3::new(0.5, 0.5, -0.5), DVec3::new(1.5, 1.5, 1.0));
        let inner = box_mesh(DVec3::new(0.75, 0.75, -1.0), DVec3::new(1.25, 1.25, 1.5));
        let annulus = difference_convex(&outer, &inner, 1e-9).unwrap();
        let cut = difference_prismatic_many(&block, &[annulus], 1e-9).unwrap();
        assert!(cut.is_edge_manifold());
        let volume = cut.signed_volume();
        // The ring removes (1 - 0.25) x 0.5 and leaves the island in the middle.
        assert!((volume - (2.0 - 0.75 * 0.5)).abs() < 1e-9, "got {volume}");
    }

    /// A closed cylinder of radius `r` from `z0` to `z1` about the z axis.
    fn cylinder(r: f64, z0: f64, z1: f64, segments: usize) -> Mesh64 {
        let mut mesh = Mesh64::new();
        for z in [z0, z1] {
            for i in 0..segments {
                let angle = std::f64::consts::TAU * i as f64 / segments as f64;
                mesh.positions
                    .push(DVec3::new(r * angle.cos(), r * angle.sin(), z));
            }
        }
        let n = segments as u32;
        for i in 0..n {
            let j = (i + 1) % n;
            mesh.push_triangle(i, j, n + j);
            mesh.push_triangle(i, n + j, n + i);
        }
        let bottom = mesh.push_vertex(DVec3::new(0.0, 0.0, z0));
        let top = mesh.push_vertex(DVec3::new(0.0, 0.0, z1));
        for i in 0..n {
            let j = (i + 1) % n;
            mesh.push_triangle(bottom, j, i);
            mesh.push_triangle(top, n + i, n + j);
        }
        mesh.fix_orientation();
        mesh
    }

    #[test]
    fn two_crossing_cylinders_unite_into_one_closed_elbow() {
        let along_z = cylinder(0.1, -0.5, 0.5, 32);
        let mut along_x = cylinder(0.1, -0.5, 0.5, 32);
        along_x.transform(&glam::DMat4::from_rotation_y(std::f64::consts::FRAC_PI_2));
        let joined = union_general(&along_z, &along_x, 1e-9).unwrap();
        let (open, over) = joined.edge_defects();
        assert!(
            joined.is_edge_manifold(),
            "{open} boundary edges, {over} overused"
        );
        // Two cylinders less their Steinmetz overlap, within the tessellation.
        let expected = 2.0 * std::f64::consts::PI * 0.01 - 16.0 * 0.001 / 3.0;
        let volume = joined.signed_volume();
        assert!(
            (volume - expected).abs() < 0.02 * expected,
            "got {volume}, want {expected}"
        );
    }

    #[test]
    fn two_crossing_cylinders_intersect_in_a_steinmetz_solid() {
        let along_z = cylinder(0.5, -1.0, 1.0, 32);
        let mut along_x = cylinder(0.5, -1.0, 1.0, 32);
        along_x.transform(&glam::DMat4::from_rotation_y(std::f64::consts::FRAC_PI_2));
        let common = intersection_general(&along_z, &along_x, 1e-9).unwrap();
        let (open, over) = common.edge_defects();
        assert!(
            common.is_edge_manifold(),
            "{open} boundary edges, {over} overused"
        );
        let expected = 16.0 * 0.125 / 3.0;
        let volume = common.signed_volume();
        assert!(
            (volume - expected).abs() < 0.03 * expected,
            "got {volume}, want {expected}"
        );
    }

    #[test]
    fn a_wall_with_t_junctions_is_still_cut() {
        // A box whose top face is split unevenly on its two sides: not edge-manifold
        // as given, watertight once healed.
        let mut wall = box_mesh(DVec3::ZERO, DVec3::new(4.0, 0.3, 3.0));
        // Replace the top face (z = 3) by a fan around an extra midpoint on one edge.
        wall.indices.retain(|_| true);
        let top: Vec<usize> = (0..wall.triangle_count())
            .filter(|&t| {
                (0..3).all(|k| {
                    (wall.positions[wall.indices[t * 3 + k] as usize].z - 3.0).abs() < 1e-12
                })
            })
            .collect();
        let mut indices = wall.indices.clone();
        for &t in top.iter().rev() {
            indices.drain(t * 3..t * 3 + 3);
        }
        let a = wall.push_vertex(DVec3::new(0.0, 0.0, 3.0));
        let b = wall.push_vertex(DVec3::new(4.0, 0.0, 3.0));
        let c = wall.push_vertex(DVec3::new(4.0, 0.3, 3.0));
        let d = wall.push_vertex(DVec3::new(0.0, 0.3, 3.0));
        let m = wall.push_vertex(DVec3::new(2.0, 0.0, 3.0));
        indices.extend_from_slice(&[a, m, d, m, c, d, m, b, c]);
        wall.indices = indices;
        crate::weld::weld_and_close(&mut wall, 1e-9);
        assert!(
            !wall.is_edge_manifold(),
            "the fixture has a T-junction as given"
        );
        let window = box_mesh(DVec3::new(1.0, -1.0, 1.0), DVec3::new(2.0, 1.0, 2.0));
        let cut = difference_prismatic_many_or_reason(&wall, &[window], 1e-9)
            .unwrap_or_else(|why| panic!("{why}"));
        assert!(
            (cut.signed_volume() - (3.6 - 0.3)).abs() < 1e-9,
            "{}",
            cut.signed_volume()
        );
    }

    #[test]
    fn a_surface_loses_the_faces_inside_a_cutter() {
        // A single square panel with a window cut through it.
        let mut panel = Mesh64::new();
        let corners = [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(4.0, 0.0, 0.0),
            DVec3::new(4.0, 0.0, 3.0),
            DVec3::new(0.0, 0.0, 3.0),
        ];
        let base = panel.positions.len() as u32;
        panel.positions.extend_from_slice(&corners);
        panel.push_triangle(base, base + 1, base + 2);
        panel.push_triangle(base, base + 2, base + 3);
        let window = box_mesh(DVec3::new(1.0, -1.0, 1.0), DVec3::new(2.0, 1.0, 2.0));
        let cut = difference_surface_many_or_reason(&panel, &[window], 1e-4)
            .unwrap_or_else(|why| panic!("{why}"));
        assert!(
            (cut.surface_area() - 11.0).abs() < 1e-9,
            "{}",
            cut.surface_area()
        );
        // The open rims: the panel's outer edge plus the window's.
        let mut uses: std::collections::HashMap<(u32, u32), usize> = Default::default();
        for triangle in cut.indices.chunks_exact(3) {
            for k in 0..3 {
                let (a, b) = (triangle[k], triangle[(k + 1) % 3]);
                *uses.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        let rim: f64 = uses
            .iter()
            .filter(|(_, n)| **n == 1)
            .map(|((a, b), _)| cut.positions[*a as usize].distance(cut.positions[*b as usize]))
            .sum();
        assert!((rim - 18.0).abs() < 1e-9, "open rim length {rim}");
    }

    #[test]
    fn the_general_booleans_close_a_shell_at_the_model_tolerance() {
        // A box whose lid sits a hair above its walls: closed at 1e-4, open at 1e-7.
        let mut leaky = box_mesh(DVec3::ZERO, DVec3::new(2.0, 2.0, 2.0));
        for point in leaky.positions.iter_mut() {
            if point.z > 1.5 {
                point.z += 5e-5;
            }
        }
        // Split the vertices so the lid is its own patch before the weld.
        let mut split = Mesh64::new();
        for triangle in leaky.indices.chunks_exact(3) {
            let points: Vec<DVec3> = triangle
                .iter()
                .map(|&i| leaky.positions[i as usize])
                .collect();
            emit(&mut split, &points);
        }
        // The box is convex, so a pocket makes it a body the general path takes.
        let pocket = box_mesh(DVec3::new(0.5, 0.5, 1.0), DVec3::new(1.5, 1.5, 3.0));
        let cup = difference_prismatic_many_or_reason(&split, &[pocket], 1e-4)
            .unwrap_or_else(|why| panic!("{why}"));
        assert!(
            (cup.signed_volume() - 7.0).abs() < 1e-3,
            "{}",
            cup.signed_volume()
        );
        assert!(cup.is_edge_manifold());
    }

    #[test]
    fn a_body_of_two_shells_is_cut_shell_by_shell() {
        // Two wall layers side by side, a window through both.
        let mut layers = box_mesh(DVec3::ZERO, DVec3::new(4.0, 0.2, 3.0));
        let mut insulation = box_mesh(DVec3::new(0.0, 0.2, 0.0), DVec3::new(4.0, 0.3, 3.0));
        // Make the second layer non-convex so the convex path cannot take the whole body.
        let notch = box_mesh(DVec3::new(3.0, 0.1, 2.5), DVec3::new(5.0, 0.4, 3.5));
        insulation = difference_convex(&insulation, &notch, 1e-9).unwrap();
        layers.append(&insulation);
        let window = box_mesh(DVec3::new(1.0, -1.0, 1.0), DVec3::new(2.0, 1.0, 2.0));
        let cut = difference_prismatic_many_or_reason(&layers, &[window], 1e-4)
            .unwrap_or_else(|why| panic!("{why}"));
        let expected = 4.0 * 0.2 * 3.0 - 0.2 + (4.0 * 0.1 * 3.0 - 1.0 * 0.1 * 0.5) - 0.1;
        assert!(
            (cut.signed_volume() - expected).abs() < 1e-6,
            "{}",
            cut.signed_volume()
        );
    }

    /// A pyramid on the unit square at `x = 1`, its base wound to face the box
    /// it sits on and split along the same diagonal, so welding merges them.
    fn pyramid_on_box_face() -> Mesh64 {
        let mut mesh = Mesh64::new();
        let base = [
            mesh.push_vertex(DVec3::new(1.0, 0.0, 0.0)),
            mesh.push_vertex(DVec3::new(1.0, 1.0, 0.0)),
            mesh.push_vertex(DVec3::new(1.0, 1.0, 1.0)),
            mesh.push_vertex(DVec3::new(1.0, 0.0, 1.0)),
        ];
        let apex = mesh.push_vertex(DVec3::new(2.0, 0.5, 0.5));
        mesh.push_triangle(base[0], base[2], base[1]);
        mesh.push_triangle(base[0], base[3], base[2]);
        for side in 0..4 {
            mesh.push_triangle(base[side], base[(side + 1) % 4], apex);
        }
        mesh
    }

    #[test]
    fn openings_cut_a_body_of_touching_solids_exactly() {
        // A box and a pyramid sharing a face, welded as an evaluator delivers
        // them: the face survives once, its edges carry three triangles, and
        // the union is neither a solid nor a prism.
        let mut body = box_mesh(DVec3::ZERO, DVec3::new(1.0, 1.0, 1.0));
        body.append(&pyramid_on_box_face());
        crate::weld::weld_and_close(&mut body, 1e-4);
        assert!(
            convex_cells_or_reason(&body, 1e-4).is_err(),
            "the welded surface is not a solid on its own"
        );
        let window = box_mesh(DVec3::new(0.5, -1.0, 0.4), DVec3::new(1.5, 2.0, 0.6));
        let cut = difference_prismatic_many_or_reason(&body, &[window], 1e-4)
            .unwrap_or_else(|why| panic!("{why}"));
        let expected = 1.0 + 1.0 / 3.0 - 0.1 - 0.075;
        assert!(
            (cut.signed_volume() - expected).abs() < 1e-9,
            "{}",
            cut.signed_volume()
        );
        // And a cutter of touching solids cuts as one opening.
        let wall = box_mesh(DVec3::new(-1.0, 0.0, -1.0), DVec3::new(3.0, 1.0, 2.0));
        let notch = box_mesh(DVec3::new(2.5, 0.5, 1.5), DVec3::new(4.0, 2.0, 3.0));
        let wall = difference_convex(&wall, &notch, 1e-9).unwrap();
        let cut = difference_prismatic_many_or_reason(&wall, &[body], 1e-4)
            .unwrap_or_else(|why| panic!("{why}"));
        let expected = 4.0 * 3.0 - 0.125 - 1.0 - 1.0 / 3.0;
        assert!(
            (cut.signed_volume() - expected).abs() < 1e-9,
            "{}",
            cut.signed_volume()
        );
    }

    #[test]
    fn a_refusal_on_one_shell_of_many_is_still_a_body_refusal() {
        // Two open panels side by side: neither shell can be decomposed.
        let mut panels = Mesh64::new();
        for y in [0.0, 1.0] {
            let base = panels.positions.len() as u32;
            panels.positions.extend_from_slice(&[
                DVec3::new(0.0, y, 0.0),
                DVec3::new(2.0, y, 0.0),
                DVec3::new(2.0, y, 2.0),
                DVec3::new(0.0, y, 2.0),
            ]);
            panels.push_triangle(base, base + 1, base + 2);
            panels.push_triangle(base, base + 2, base + 3);
        }
        let cutter = box_mesh(DVec3::new(0.5, -1.0, 0.5), DVec3::new(1.5, 3.0, 1.5));
        let why = difference_prismatic_many_or_reason(&panels, &[cutter], 1e-4).unwrap_err();
        assert!(
            why.starts_with("body: shell 1 of 2: not a closed shell"),
            "{why}"
        );
    }

    #[test]
    fn a_cutter_coincident_with_an_existing_hole_changes_nothing() {
        // A wall that already holds its window as a hole, cut by that window again.
        let wall = box_mesh(DVec3::ZERO, DVec3::new(4.0, 0.3, 3.0));
        let window = box_mesh(DVec3::new(1.0, -1.0, 1.0), DVec3::new(2.0, 1.0, 2.0));
        // A second hole makes the body non-convex enough for the general path.
        let door = box_mesh(DVec3::new(3.0, -1.0, -1.0), DVec3::new(3.8, 1.0, 2.1));
        let holed = difference_convex_many(&wall, &[window.clone(), door], 1e-9).unwrap();
        let cut = difference_prismatic_many_or_reason(&holed, &[window], 1e-4)
            .unwrap_or_else(|why| panic!("{why}"));
        assert!(cut.is_edge_manifold());
        assert!((cut.signed_volume() - holed.signed_volume()).abs() < 1e-9);
        assert!(
            (cut.surface_area() - holed.surface_area()).abs() < 1e-9,
            "area {} vs {}",
            cut.surface_area(),
            holed.surface_area()
        );
    }

    #[test]
    fn a_cut_wall_comes_back_merged_and_unchanged() {
        // The shape the merge exists for: a wall with a window, cut by the
        // general path, whose faces arrive split along every cell plane.
        let wall = box_mesh(DVec3::ZERO, DVec3::new(4.0, 0.3, 3.0));
        let pocket = box_mesh(DVec3::new(3.0, -1.0, -1.0), DVec3::new(3.8, 1.0, 2.1));
        let body = difference_convex(&wall, &pocket, 1e-9).unwrap();
        let window = box_mesh(DVec3::new(1.0, -1.0, 1.0), DVec3::new(2.0, 1.0, 2.0));
        let cut = difference_prismatic_many_or_reason(&body, &[window], 1e-4)
            .unwrap_or_else(|why| panic!("{why}"));
        assert!(cut.is_edge_manifold(), "the merged result is still closed");
        let expected = body.signed_volume() - 1.0 * 0.3 * 1.0;
        assert!(
            (cut.signed_volume() - expected).abs() < 1e-9,
            "{} against {expected}",
            cut.signed_volume()
        );
        // The merge is the point: without it every face carries its cell
        // splits, and this cut comes back at 130 triangles rather than 116.
        assert!(
            cut.triangle_count() <= 120,
            "{} triangles after the merge",
            cut.triangle_count()
        );
    }

    #[test]
    fn a_merged_result_is_still_a_usable_operand() {
        let wall = box_mesh(DVec3::ZERO, DVec3::new(4.0, 0.3, 3.0));
        let window = box_mesh(DVec3::new(1.0, -1.0, 1.0), DVec3::new(2.0, 1.0, 2.0));
        let once = difference_convex_many(&wall, &[window], 1e-9).unwrap();
        let door = box_mesh(DVec3::new(2.5, -1.0, -1.0), DVec3::new(3.2, 1.0, 2.0));
        let twice = difference_prismatic_many_or_reason(&once, &[door], 1e-4)
            .unwrap_or_else(|why| panic!("{why}"));
        let expected = 3.6 - 0.3 - 0.7 * 0.3 * 2.0;
        assert!(
            (twice.signed_volume() - expected).abs() < 1e-9,
            "{} against {expected}",
            twice.signed_volume()
        );
        assert!(twice.is_edge_manifold());
    }

    #[test]
    fn a_plane_reports_which_side_a_point_is_on() {
        let plane = Plane::from_point_normal(DVec3::ZERO, DVec3::Z).unwrap();
        assert!(
            plane.distance(DVec3::new(0.0, 0.0, 1.0)) > 0.0,
            "outside is positive"
        );
        assert!(
            plane.distance(DVec3::new(0.0, 0.0, -1.0)) < 0.0,
            "inside is negative"
        );
        assert!(plane.distance(DVec3::new(5.0, 5.0, 0.0)).abs() < 1e-15);
        assert_eq!(plane.flipped().distance(DVec3::Z), -1.0);
    }

    #[test]
    fn a_normal_that_is_not_unit_still_measures_distance_correctly() {
        let plane = Plane::new(DVec3::new(0.0, 0.0, 5.0), 10.0).unwrap();
        // 5z = 10, so the plane is z = 2 and a point at z = 3 is one metre out.
        assert!((plane.distance(DVec3::new(0.0, 0.0, 3.0)) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_degenerate_normal_is_refused() {
        assert!(Plane::new(DVec3::ZERO, 1.0).is_none());
        assert!(Plane::from_point_normal(DVec3::ZERO, DVec3::ZERO).is_none());
        assert!(Plane::new(DVec3::Z, f64::NAN).is_none());
    }

    #[test]
    fn clipping_a_cube_in_half_halves_its_volume_and_keeps_it_closed() {
        let cube = box_mesh(DVec3::splat(-1.0), DVec3::splat(1.0));
        assert!(
            (cube.signed_volume() - 8.0).abs() < 1e-12,
            "the fixture itself"
        );

        let plane = Plane::from_point_normal(DVec3::ZERO, DVec3::Z).unwrap();
        let result = clip(&cube, &plane, 1e-9);
        assert_eq!(result.outcome, ClipOutcome::Capped);

        let mut half = result.mesh;
        crate::weld::weld_and_close(&mut half, 1e-9);
        assert_eq!(
            half.closed,
            Some(true),
            "a capped clip must still be a solid"
        );
        assert!(
            (half.signed_volume() - 4.0).abs() < 1e-12,
            "got {}",
            half.signed_volume()
        );
        let (lo, hi) = half.bounds().unwrap();
        assert!(
            (hi.z - 0.0).abs() < 1e-12,
            "the top should be the cut plane, got {}",
            hi.z
        );
        assert!((lo.z + 1.0).abs() < 1e-12);
    }

    #[test]
    fn an_oblique_cut_is_still_exact() {
        // Half of the cube along the diagonal plane x + y = 0.
        let cube = box_mesh(DVec3::splat(-1.0), DVec3::splat(1.0));
        let plane = Plane::new(DVec3::new(1.0, 1.0, 0.0), 0.0).unwrap();
        let mut half = clip(&cube, &plane, 1e-9).mesh;
        crate::weld::weld_and_close(&mut half, 1e-9);
        assert_eq!(half.closed, Some(true));
        assert!(
            (half.signed_volume() - 4.0).abs() < 1e-12,
            "got {}",
            half.signed_volume()
        );
    }

    #[test]
    fn a_clip_that_touches_nothing_says_so() {
        let cube = box_mesh(DVec3::splat(-1.0), DVec3::splat(1.0));
        let far = Plane::from_point_normal(DVec3::new(0.0, 0.0, 10.0), DVec3::Z).unwrap();
        let result = clip(&cube, &far, 1e-9);
        assert_eq!(result.outcome, ClipOutcome::Untouched);
        assert!((result.mesh.signed_volume() - 8.0).abs() < 1e-12);
    }

    #[test]
    fn a_clip_that_removes_everything_says_so() {
        let cube = box_mesh(DVec3::splat(-1.0), DVec3::splat(1.0));
        let behind = Plane::from_point_normal(DVec3::new(0.0, 0.0, -10.0), DVec3::Z).unwrap();
        let result = clip(&cube, &behind, 1e-9);
        assert_eq!(result.outcome, ClipOutcome::Removed);
        assert!(result.mesh.is_empty());
    }

    #[test]
    fn an_index_past_the_end_drops_the_triangle_rather_than_panicking() {
        let stale = Mesh64 {
            positions: vec![DVec3::ZERO; 3],
            indices: vec![0, 1, 7],
            closed: None,
            uvs: Vec::new(),
        };
        let plane = Plane::from_point_normal(DVec3::ZERO, DVec3::Z).unwrap();
        assert!(clip(&stale, &plane, 1e-9).mesh.is_empty());
        assert!(face_planes(&stale, 1e-9).is_empty());
    }

    #[test]
    fn a_cut_exactly_on_a_face_leaves_the_solid_alone() {
        // The plane through the top face. Nothing is above it, so nothing goes.
        let cube = box_mesh(DVec3::splat(-1.0), DVec3::splat(1.0));
        let plane = Plane::from_point_normal(DVec3::new(0.0, 0.0, 1.0), DVec3::Z).unwrap();
        let result = clip(&cube, &plane, 1e-9);
        assert_eq!(result.outcome, ClipOutcome::Untouched);
        assert!((result.mesh.signed_volume() - 8.0).abs() < 1e-12);
    }

    #[test]
    fn face_planes_finds_six_for_a_box_and_no_more() {
        let cube = box_mesh(DVec3::ZERO, DVec3::splat(1.0));
        let planes = face_planes(&cube, 1e-9);
        assert_eq!(planes.len(), 6, "twelve triangles, six distinct planes");
        assert!(is_convex(&cube, &planes, 1e-9));
    }

    #[test]
    fn an_l_shape_is_not_convex() {
        // Two boxes meeting at a corner. Every plane is still a face plane, but
        // the far corner of one box sits outside the other's.
        let mut shape = box_mesh(DVec3::ZERO, DVec3::new(3.0, 1.0, 1.0));
        shape.append(&box_mesh(DVec3::ZERO, DVec3::new(1.0, 3.0, 1.0)));
        let planes = face_planes(&shape, 1e-9);
        assert!(
            !is_convex(&shape, &planes, 1e-9),
            "an L is the textbook non-convex solid"
        );
    }

    #[test]
    fn subtracting_a_box_from_a_box_leaves_a_hole() {
        // A 4 x 4 x 1 slab with a 1 x 1 hole punched right through it.
        let slab = box_mesh(DVec3::new(-2.0, -2.0, 0.0), DVec3::new(2.0, 2.0, 1.0));
        let cutter = box_mesh(DVec3::new(-0.5, -0.5, -1.0), DVec3::new(0.5, 0.5, 2.0));

        let mut result = difference_convex(&slab, &cutter, 1e-9).expect("two boxes are convex");
        crate::weld::weld_and_close(&mut result, 1e-9);

        let volume = result.signed_volume();
        assert!(
            (volume - (16.0 - 1.0)).abs() < 1e-9,
            "16 less the 1x1x1 hole, got {volume}"
        );

        // Two 4x4 faces less the hole, four 4x1 sides and the four 1x1 hole walls:
        // 2*(16-1) + 4*4 + 4*1 = 50.
        let area = result.surface_area();
        assert!((area - 50.0).abs() < 1e-9, "expected 50, got {area}");
        assert_eq!(
            result.closed,
            Some(true),
            "a hole through a slab is still a solid"
        );
    }

    #[test]
    fn subtracting_a_box_that_only_dents_the_surface() {
        // The cutter enters from the top but stops halfway: a blind pocket.
        let block = box_mesh(DVec3::ZERO, DVec3::new(2.0, 2.0, 2.0));
        let cutter = box_mesh(DVec3::new(0.5, 0.5, 1.0), DVec3::new(1.5, 1.5, 3.0));
        let mut result = difference_convex(&block, &cutter, 1e-9).unwrap();
        crate::weld::weld_and_close(&mut result, 1e-9);
        let volume = result.signed_volume();
        assert!(
            (volume - (8.0 - 1.0)).abs() < 1e-9,
            "8 less a 1x1x1 pocket, got {volume}"
        );
        // The top face loses 1, and the pocket adds four 1x1 walls and a floor.
        let area = result.surface_area();
        assert!(
            (area - (24.0 - 1.0 + 5.0)).abs() < 1e-9,
            "expected 28, got {area}"
        );
        assert_eq!(result.closed, Some(true));
    }

    #[test]
    fn a_wall_with_two_windows_gets_both() {
        // After the first hole the wall is no longer convex, so cutting the openings
        // one after another would refuse the second.
        let wall = box_mesh(DVec3::ZERO, DVec3::new(6.0, 0.3, 3.0));
        let left = box_mesh(DVec3::new(1.0, -0.1, 1.0), DVec3::new(2.0, 0.4, 2.0));
        let right = box_mesh(DVec3::new(4.0, -0.1, 1.0), DVec3::new(5.0, 0.4, 2.0));

        let mut result =
            difference_convex_many(&wall, &[left, right], 1e-9).expect("all three are convex");
        crate::weld::weld_and_close(&mut result, 1e-9);

        let solid = 6.0 * 0.3 * 3.0;
        let hole = 1.0 * 0.3 * 1.0;
        let volume = result.signed_volume();
        assert!(
            (volume - (solid - 2.0 * hole)).abs() < 1e-9,
            "two holes, got {volume}"
        );
        assert_eq!(result.closed, Some(true));

        // Both faces lose two 1x1 squares, and each hole gains four 1x0.3
        // reveals: 2*(18 - 2) + 2*(0.9) + 2*(3*0.3) ... written out below.
        let faces = 2.0 * (6.0 * 3.0 - 2.0 * 1.0);
        let edges = 2.0 * (6.0 * 0.3) + 2.0 * (3.0 * 0.3);
        let reveals = 2.0 * (4.0 * 1.0 * 0.3);
        assert!(
            (result.surface_area() - (faces + edges + reveals)).abs() < 1e-9,
            "got {}",
            result.surface_area()
        );
    }

    #[test]
    fn subtracting_a_cutter_that_misses_changes_nothing() {
        let block = box_mesh(DVec3::ZERO, DVec3::splat(1.0));
        let cutter = box_mesh(DVec3::splat(5.0), DVec3::splat(6.0));
        let mut result = difference_convex(&block, &cutter, 1e-9).unwrap();
        crate::weld::weld_and_close(&mut result, 1e-9);
        assert!(
            (result.signed_volume() - 1.0).abs() < 1e-9,
            "got {}",
            result.signed_volume()
        );
        assert!((result.surface_area() - 6.0).abs() < 1e-9);
    }

    #[test]
    fn subtracting_a_cutter_that_swallows_the_body_leaves_nothing() {
        let block = box_mesh(DVec3::ZERO, DVec3::splat(1.0));
        let cutter = box_mesh(DVec3::splat(-1.0), DVec3::splat(2.0));
        let result = difference_convex(&block, &cutter, 1e-9).unwrap();
        assert!(result.is_empty(), "the body is entirely inside the cutter");
    }

    #[test]
    fn an_opening_flush_with_a_face_does_not_get_a_lid() {
        // The cutter's far face lands exactly on the body's far face, as an opening
        // modelled to a wall's outside does; keeping it would fill the hole in.
        let wall = box_mesh(DVec3::ZERO, DVec3::new(4.0, 0.3, 3.0));
        let opening = box_mesh(DVec3::new(1.0, -0.1, 1.0), DVec3::new(2.0, 0.3, 2.0));
        let mut result = difference_convex(&wall, &opening, 1e-9).unwrap();
        crate::weld::weld_and_close(&mut result, 1e-9);
        let volume = result.signed_volume();
        assert!(
            (volume - (4.0 * 0.3 * 3.0 - 1.0 * 0.3 * 1.0)).abs() < 1e-9,
            "the recess must actually be removed, got {volume}"
        );
    }

    #[test]
    fn a_non_convex_body_is_refused_rather_than_botched() {
        let mut shape = box_mesh(DVec3::ZERO, DVec3::new(3.0, 1.0, 1.0));
        shape.append(&box_mesh(DVec3::ZERO, DVec3::new(1.0, 3.0, 1.0)));
        let cutter = box_mesh(DVec3::splat(0.25), DVec3::splat(0.75));
        assert!(
            difference_convex(&shape, &cutter, 1e-9).is_none(),
            "refusing is the contract; the caller emits the body un-cut and says so"
        );
    }

    #[test]
    fn a_convex_cutter_punches_a_non_convex_extrusion() {
        // An L profile, triangulated before it is swept. The paired cap layout
        // is the contract produced by ifc-geom's extrusion evaluator.
        let outline = [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(3.0, 0.0, 0.0),
            DVec3::new(3.0, 1.0, 0.0),
            DVec3::new(1.0, 1.0, 0.0),
            DVec3::new(1.0, 3.0, 0.0),
            DVec3::new(0.0, 3.0, 0.0),
        ];
        let cap = [[0, 1, 3], [1, 2, 3], [0, 3, 5], [3, 4, 5]];
        let mut body = Mesh64::new();
        body.positions.extend_from_slice(&outline);
        body.positions
            .extend(outline.iter().map(|point| *point + DVec3::Z));
        for [a, b, c] in cap {
            body.push_triangle(a, c, b);
            body.push_triangle(a + 6, b + 6, c + 6);
        }
        for edge in 0..outline.len() as u32 {
            let next = (edge + 1) % outline.len() as u32;
            body.push_triangle(edge, next, next + 6);
            body.push_triangle(edge, next + 6, edge + 6);
        }
        body.closed = Some(body.is_edge_manifold());
        assert!(body.closed == Some(true));
        assert!((body.signed_volume() - 5.0).abs() < 1e-9);

        let cutter = box_mesh(DVec3::new(1.5, 0.25, -0.25), DVec3::new(2.5, 0.75, 1.25));
        let result = difference_extrusion_many(&body, std::slice::from_ref(&cutter), 1e-9)
            .expect("the paired caps identify a decomposable extrusion");
        assert!(result.is_edge_manifold(), "the punched L must stay closed");
        assert!(
            (result.signed_volume() - 4.5).abs() < 1e-8,
            "5 cubic units less a 0.5-unit opening, got {}",
            result.signed_volume()
        );
        assert!(
            (result.surface_area() - 24.0).abs() < 1e-8,
            "the two removed patches and four opening walls give area 24, got {}",
            result.surface_area()
        );

        let mut impostor = body;
        impostor.indices[cap.len() * 6] = 3;
        assert!(
            difference_extrusion_many(&impostor, std::slice::from_ref(&cutter), 1e-9).is_none(),
            "paired caps are not enough without matching extrusion sides"
        );
    }

    #[test]
    fn an_open_shell_refuses_to_be_capped() {
        // A single triangle is not a solid. Cutting it produces an open result
        // and the outcome must say so rather than inventing a cap.
        let mut sheet = Mesh64::new();
        sheet.push_vertex(DVec3::new(-1.0, 0.0, -1.0));
        sheet.push_vertex(DVec3::new(1.0, 0.0, -1.0));
        sheet.push_vertex(DVec3::new(0.0, 0.0, 1.0));
        sheet.push_triangle(0, 1, 2);
        let plane = Plane::from_point_normal(DVec3::ZERO, DVec3::Z).unwrap();
        let result = clip(&sheet, &plane, 1e-9);
        assert_eq!(result.outcome, ClipOutcome::Open);
        assert!(!result.mesh.is_empty(), "the kept part is still returned");
    }

    #[test]
    fn a_hole_right_through_a_tube_caps_with_the_hole_as_a_hole() {
        // Clipping a square tube gives a cap with an inner loop; a wrong winding rule
        // would fill the bore in.
        let mut tube = box_mesh(DVec3::new(-2.0, -2.0, -2.0), DVec3::new(2.0, 2.0, 2.0));
        let mut bore = box_mesh(DVec3::new(-1.0, -1.0, -2.0), DVec3::new(1.0, 1.0, 2.0));
        bore.flip_winding(); // an inner shell faces inward
        tube.append(&bore);

        let plane = Plane::from_point_normal(DVec3::ZERO, DVec3::Z).unwrap();
        let result = clip(&tube, &plane, 1e-9);
        assert_eq!(result.outcome, ClipOutcome::Capped);
        let mut half = result.mesh;
        crate::weld::weld_and_close(&mut half, 1e-9);
        // Half of a 4x4x4 block with a 2x2 bore: (16 - 4) * 2 = 24.
        assert!(
            (half.signed_volume() - 24.0).abs() < 1e-9,
            "got {}",
            half.signed_volume()
        );
    }

    /// An L-shaped prism, built the way an exporter tessellates a swept face
    /// set: caps triangulated in no particular order, sides last.
    fn l_prism(height: f64) -> Mesh64 {
        // An L with the notch in the +x +y corner. Its area is 3.
        let outline = [
            glam::DVec2::new(0.0, 0.0),
            glam::DVec2::new(2.0, 0.0),
            glam::DVec2::new(2.0, 1.0),
            glam::DVec2::new(1.0, 1.0),
            glam::DVec2::new(1.0, 2.0),
            glam::DVec2::new(0.0, 2.0),
        ];
        let polygon = crate::triangulate::Polygon2::new(outline.to_vec());
        let cap = crate::triangulate::triangulate_polygon(&polygon).unwrap();

        let mut mesh = Mesh64::new();
        for point in outline {
            mesh.push_vertex(DVec3::new(point.x, point.y, 0.0));
        }
        for point in outline {
            mesh.push_vertex(DVec3::new(point.x, point.y, height));
        }
        let count = outline.len() as u32;
        for triangle in cap.chunks_exact(3) {
            // Bottom faces down, top faces up.
            mesh.push_triangle(triangle[0], triangle[2], triangle[1]);
            mesh.push_triangle(
                triangle[0] + count,
                triangle[1] + count,
                triangle[2] + count,
            );
        }
        for edge in 0..count {
            let a = edge;
            let b = (edge + 1) % count;
            mesh.push_triangle(a, b, b + count);
            mesh.push_triangle(a, b + count, a + count);
        }
        mesh
    }

    #[test]
    fn a_convex_solid_is_its_own_only_cell() {
        let cube = box_mesh(DVec3::ZERO, DVec3::splat(1.0));
        let cells = convex_cells(&cube, 1e-9).expect("a cube decomposes");
        assert_eq!(cells.len(), 1);
    }

    #[test]
    fn an_l_prism_decomposes_into_convex_cells_that_keep_its_volume() {
        let prism = l_prism(3.0);
        let planes = face_planes(&prism, 1e-9);
        assert!(
            !is_convex(&prism, &planes, 1e-9),
            "the fixture has to be non-convex or it proves nothing"
        );

        let cells = convex_cells(&prism, 1e-9).expect("an L prism is a translational sweep");
        assert!(cells.len() >= 2, "got {} cells", cells.len());
        for cell in &cells {
            let cell_planes = face_planes(cell, 1e-9);
            assert!(
                cell_planes.len() >= 4 && is_convex(cell, &cell_planes, 1e-9),
                "every cell has to be convex or the difference is not exact"
            );
        }
        // Area 3, height 3.
        let total: f64 = cells.iter().map(|cell| cell.signed_volume().abs()).sum();
        assert!((total - 9.0).abs() < 1e-9, "cells hold {total}, want 9");
    }

    #[test]
    fn a_frustum_is_refused_rather_than_taken_for_a_prism() {
        // Parallel top and bottom of different sizes: taken as caps they would give
        // cells that do not tile the solid.
        let mut mesh = Mesh64::new();
        for point in [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(4.0, 0.0, 0.0),
            DVec3::new(4.0, 4.0, 0.0),
            DVec3::new(0.0, 4.0, 0.0),
            DVec3::new(1.0, 1.0, 2.0),
            DVec3::new(2.0, 1.0, 2.0),
            DVec3::new(2.0, 2.0, 2.0),
            DVec3::new(1.0, 2.0, 2.0),
        ] {
            mesh.push_vertex(point);
        }
        for face in [
            [0u32, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ] {
            mesh.push_triangle(face[0], face[1], face[2]);
        }
        assert!(
            prism_cells(&mesh, 1e-9).is_none(),
            "a frustum is not a translational prism and must be refused"
        );
    }

    #[test]
    fn a_box_is_cut_out_of_an_l_prism_exactly() {
        let body = l_prism(3.0);
        let cutter = box_mesh(DVec3::new(0.25, -0.5, 1.0), DVec3::new(0.75, 0.5, 2.0));
        assert!(
            difference_convex_many(&body, std::slice::from_ref(&cutter), 1e-9).is_none(),
            "the convex path has to refuse this, or the test proves nothing"
        );

        let result = difference_prismatic_many(&body, std::slice::from_ref(&cutter), 1e-9)
            .expect("a prismatic body with a convex cutter is decomposable");
        // The box reaches from y = -0.5 to y = 0.5 and the body starts at
        // y = 0, so it removes 0.5 by 0.5 by 1.0 of the body's 9.
        let volume = result.signed_volume().abs();
        assert!((volume - 8.75).abs() < 1e-6, "got {volume}, want 8.75");
        assert!(
            result.is_edge_manifold(),
            "the cut solid has to stay watertight"
        );
    }

    #[test]
    fn a_non_convex_prismatic_cutter_cuts_a_convex_body() {
        // The case real models actually hit: a tessellated L-shaped opening
        // through a plain rectangular wall.
        let body = box_mesh(DVec3::new(-1.0, -1.0, -1.0), DVec3::new(4.0, 4.0, 5.0));
        let cutter = l_prism(3.0);
        assert!(
            difference_convex_many(&body, std::slice::from_ref(&cutter), 1e-9).is_none(),
            "a non-convex cutter has to be refused by the convex path"
        );

        let result = difference_prismatic_many(&body, std::slice::from_ref(&cutter), 1e-9)
            .expect("a prismatic cutter is decomposable");
        // The box holds 5 by 5 by 6 = 150; the L prism inside it holds 9.
        let volume = result.signed_volume().abs();
        assert!((volume - 141.0).abs() < 1e-6, "got {volume}, want 141");
        assert!(
            result.is_edge_manifold(),
            "cutting must not leave the shell open"
        );
    }

    #[test]
    fn a_cut_prism_reports_its_own_surface_area_and_not_twice_over() {
        // The trap: decomposing into solid pieces gives the
        // right volume and doubles every internal face. Area is what catches it.
        let body = l_prism(3.0);
        let cutter = box_mesh(DVec3::new(0.25, -0.5, 1.0), DVec3::new(0.75, 0.5, 2.0));
        let result = difference_prismatic_many(&body, std::slice::from_ref(&cutter), 1e-9)
            .expect("decomposable");
        // The L prism has area 30 (caps 6, sides 24); the pocket removes 0.5 x 1.0
        // from the y = 0 wall and adds its own five faces.
        let expected = 30.0 - 0.5 + 0.5 + 2.0 * 0.5 + 2.0 * 0.25;
        let area = result.surface_area();
        assert!(
            (area - expected).abs() < 1e-6,
            "got {area}, want {expected}: an internal face is being emitted twice"
        );
    }

    #[test]
    fn an_oblique_prism_still_decomposes() {
        // A sweep off the cap normal is a shear; the displacement must come from the
        // cap centroids, not the normal.
        let mut prism = l_prism(3.0);
        let shear = DVec3::new(1.5, 0.5, 0.0);
        for point in prism.positions.iter_mut() {
            if point.z > 1.5 {
                *point += shear;
            }
        }
        let cells = convex_cells(&prism, 1e-9).expect("an oblique sweep is still a sweep");
        let total: f64 = cells.iter().map(|cell| cell.signed_volume().abs()).sum();
        // Shearing does not change volume.
        assert!((total - 9.0).abs() < 1e-9, "cells hold {total}, want 9");
    }

    #[test]
    fn an_l_cap_merges_into_two_convex_cells_not_four_triangles() {
        // An L tiles with two convex pieces rather than four ear-clipped triangles,
        // and every cell costs a pass of the exact difference.
        let prism = l_prism(3.0);
        let cells = convex_cells(&prism, 1e-9).expect("an L prism decomposes");
        assert!(
            cells.len() <= 2,
            "an L cap needs at most two convex pieces, got {}",
            cells.len()
        );
        // Merging must not lose or double any of the solid.
        let total: f64 = cells.iter().map(|cell| cell.signed_volume().abs()).sum();
        assert!((total - 9.0).abs() < 1e-9, "cells hold {total}, want 9");
        for cell in &cells {
            let planes = face_planes(cell, 1e-9);
            assert!(
                is_convex(cell, &planes, 1e-9),
                "a merged cell that is not convex breaks the exactness proof"
            );
            assert!(
                cell.is_edge_manifold(),
                "a swept convex cap is a closed solid"
            );
        }
    }

    #[test]
    fn a_cutter_with_thousands_of_planes_is_still_subtracted() {
        // One recursion level per plane used to run the stack out; the work list does not.
        let body = box_mesh(DVec3::new(-2.0, -2.0, 0.0), DVec3::new(2.0, 2.0, 2.0));
        let cutter = cylinder(1.0, -1.0, 3.0, 4000);
        let planes = face_planes(&cutter, 1e-9);
        assert!(planes.len() > 4000 && is_convex(&cutter, &planes, 1e-9));
        let result =
            difference_convex(&body, &cutter, 1e-9).expect("a convex cutter is subtracted");
        let expected = 32.0 - std::f64::consts::PI * 2.0;
        assert!(
            (result.signed_volume() - expected).abs() < 0.05,
            "got {}",
            result.signed_volume()
        );
    }

    #[test]
    fn a_mesh_with_too_many_planes_is_not_treated_as_convex() {
        let cutter = cylinder(1.0, 0.0, 1.0, MAX_FACE_PLANES + 8);
        assert!(face_planes(&cutter, 1e-9).is_empty());
    }
}
