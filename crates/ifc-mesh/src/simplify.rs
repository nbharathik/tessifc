// SPDX-License-Identifier: Apache-2.0
//! Merging the coplanar fragments a boolean leaves behind. Only vertices strictly
//! inside a flat face go; a rim vertex is shared with neighbouring faces.

use crate::mesh::Mesh64;
use crate::triangulate::triangulate_face;
use glam::DVec3;
use std::collections::HashMap;

/// Most triangles that may meet at one removable vertex.
///
/// A fan larger than this is a cone tip or a tessellation artefact, not the
/// inside of a flat face, and ear clipping it is not worth the time.
const MAX_FAN: usize = 64;

/// How finely a plane's normal is quantised when a fan is tested for flatness.
///
/// Coarser than the geometry tolerance on purpose: a miss keeps a vertex, and
/// keeping a vertex is always safe.
const NORMAL_QUANTUM: f64 = 1e6;

/// Sweeps over the mesh. Removing a vertex can make a neighbour removable.
const PASSES: usize = 3;

/// Merge coplanar triangles by removing the vertices interior to a flat face.
///
/// Each removable vertex is replaced by a re-triangulation of the ring around
/// it; a vertex is kept whenever anything is in doubt. Returns how many
/// triangles were removed. Weld first: this works on indices, not positions.
///
/// ```
/// use tessifc_mesh::{Mesh64, merge_coplanar};
/// use glam::DVec3;
///
/// // One square as four triangles around a middle vertex.
/// let mut mesh = Mesh64::new();
/// for corner in [(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0), (1.0, 1.0)] {
///     mesh.push_vertex(DVec3::new(corner.0, corner.1, 0.0));
/// }
/// for edge in [(0, 1), (1, 2), (2, 3), (3, 0)] {
///     mesh.push_triangle(edge.0, edge.1, 4);
/// }
/// assert_eq!(merge_coplanar(&mut mesh, 1e-9), 2);
/// assert_eq!(mesh.triangle_count(), 2);
/// ```
pub fn merge_coplanar(mesh: &mut Mesh64, tolerance: f64) -> usize {
    mesh.drop_uvs();
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1e-9
    };
    if mesh.triangle_count() < 3 {
        return 0;
    }

    // Every edge in the mesh, kept up to date as vertices go, so a
    // re-triangulation can be told whether an edge it wants already exists.
    let mut edge_uses: HashMap<(u32, u32), u32> = HashMap::new();
    for triangle in mesh.indices.chunks_exact(3) {
        for step in 0..3 {
            let (a, b) = (triangle[step], triangle[(step + 1) % 3]);
            *edge_uses.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }

    let mut removed = 0usize;
    for _ in 0..PASSES {
        let pass = one_pass(mesh, tol, &mut edge_uses);
        removed += pass;
        if pass == 0 {
            break;
        }
    }
    if removed > 0 {
        mesh.drop_unused_vertices();
    }
    removed
}

/// One sweep over every vertex, in ascending index order.
fn one_pass(mesh: &mut Mesh64, tol: f64, edge_uses: &mut HashMap<(u32, u32), u32>) -> usize {
    let mut fans: Vec<Vec<usize>> = vec![Vec::new(); mesh.positions.len()];
    for (index, triangle) in mesh.indices.chunks_exact(3).enumerate() {
        for &corner in triangle {
            if let Some(fan) = fans.get_mut(corner as usize) {
                fan.push(index);
            }
        }
    }

    let mut dropped: Vec<bool> = vec![false; mesh.triangle_count()];
    let mut added: Vec<[u32; 3]> = Vec::new();
    let mut removed = 0usize;
    for vertex in 0..mesh.positions.len() as u32 {
        let fan = &fans[vertex as usize];
        if fan.len() < 3 || fan.len() > MAX_FAN {
            continue;
        }
        // A triangle already replaced this pass cannot be reasoned about.
        if fan.iter().any(|&triangle| dropped[triangle]) {
            continue;
        }
        let Some(replacement) = remove_vertex(mesh, vertex, fan, tol, edge_uses) else {
            continue;
        };
        for &triangle in fan {
            let base = triangle * 3;
            for step in 0..3 {
                let (a, b) = (
                    mesh.indices[base + step],
                    mesh.indices[base + (step + 1) % 3],
                );
                if let Some(count) = edge_uses.get_mut(&(a.min(b), a.max(b))) {
                    *count = count.saturating_sub(1);
                }
            }
            dropped[triangle] = true;
        }
        for triangle in &replacement {
            for step in 0..3 {
                let (a, b) = (triangle[step], triangle[(step + 1) % 3]);
                *edge_uses.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
        removed += fan.len() - replacement.len();
        added.extend(replacement);
    }
    if removed == 0 {
        return 0;
    }

    let mut indices = Vec::with_capacity(mesh.indices.len());
    for (index, triangle) in mesh.indices.chunks_exact(3).enumerate() {
        if !dropped[index] {
            indices.extend_from_slice(triangle);
        }
    }
    for triangle in added {
        indices.extend_from_slice(&triangle);
    }
    mesh.indices = indices;
    removed
}

/// Re-triangulate the ring around one vertex without it.
///
/// `None` unless the fan is flat, closes into one ring, and re-triangulates
/// into something that shows the rest of the mesh exactly the ring it showed
/// before.
fn remove_vertex(
    mesh: &Mesh64,
    vertex: u32,
    fan: &[usize],
    tol: f64,
    edge_uses: &HashMap<(u32, u32), u32>,
) -> Option<Vec<[u32; 3]>> {
    // Flat, and every triangle facing the same way: the ring is then a plane
    // polygon with the vertex inside it.
    let mut plane: Option<(i64, i64, i64, i64)> = None;
    let mut normal = DVec3::ZERO;
    for &triangle in fan {
        let base = triangle * 3;
        let (a, b, c) = (
            *mesh.positions.get(*mesh.indices.get(base)? as usize)?,
            *mesh.positions.get(*mesh.indices.get(base + 1)? as usize)?,
            *mesh.positions.get(*mesh.indices.get(base + 2)? as usize)?,
        );
        let cross = (b - a).cross(c - a);
        let length = cross.length();
        if length <= tol * tol {
            return None;
        }
        let unit = cross / length;
        let key = (
            (unit.x * NORMAL_QUANTUM).round() as i64,
            (unit.y * NORMAL_QUANTUM).round() as i64,
            (unit.z * NORMAL_QUANTUM).round() as i64,
            (unit.dot(a) / tol).round() as i64,
        );
        match plane {
            None => {
                plane = Some(key);
                normal = unit;
            }
            Some(known) if known == key => {}
            Some(_) => return None,
        }
    }

    // The ring: each triangle's edge opposite the vertex, chained head to tail.
    let mut next: HashMap<u32, u32> = HashMap::new();
    for &triangle in fan {
        let base = triangle * 3;
        let corners = [
            mesh.indices[base],
            mesh.indices[base + 1],
            mesh.indices[base + 2],
        ];
        let at = corners.iter().position(|&corner| corner == vertex)?;
        let (from, to) = (corners[(at + 1) % 3], corners[(at + 2) % 3]);
        if from == vertex || to == vertex || next.insert(from, to).is_some() {
            return None;
        }
    }
    let start = *next.keys().min()?;
    let mut ring = Vec::with_capacity(next.len());
    let mut cursor = start;
    for _ in 0..=next.len() {
        ring.push(cursor);
        cursor = *next.get(&cursor)?;
        if cursor == start {
            break;
        }
    }
    if cursor != start || ring.len() != next.len() || ring.len() < 3 {
        return None;
    }
    // A ring that visits a vertex twice is a pinch, not a polygon.
    let distinct: std::collections::HashSet<u32> = ring.iter().copied().collect();
    if distinct.len() != ring.len() || distinct.contains(&vertex) {
        return None;
    }

    let points: Vec<DVec3> = ring
        .iter()
        .map(|&index| mesh.positions[index as usize])
        .collect();
    // The ring must turn the way the fan faces, or the vertex is not inside it.
    if crate::mesh::newell_normal(&points).dot(normal) <= 0.0 {
        return None;
    }
    let local = triangulate_face(&points, &[]).ok()?;
    let out: Vec<[u32; 3]> = local
        .chunks_exact(3)
        .map(|triangle| {
            [
                ring[triangle[0] as usize],
                ring[triangle[1] as usize],
                ring[triangle[2] as usize],
            ]
        })
        .collect();
    let out = restore_boundary_vertices(out, &next)?;
    // A ring of n vertices is n - 2 triangles; the fan was n. Anything else
    // means ear clipping did not cover the ring.
    if out.len() + 2 != fan.len() {
        return None;
    }

    // The rim it shows the rest of the mesh has to be the ring it replaced.
    let mut after: HashMap<(u32, u32), u32> = HashMap::new();
    for triangle in &out {
        for step in 0..3 {
            *after
                .entry((triangle[step], triangle[(step + 1) % 3]))
                .or_insert(0) += 1;
        }
    }
    let mut rim = 0usize;
    for (&(from, to), &count) in &after {
        if after.contains_key(&(to, from)) {
            continue;
        }
        if count != 1 || next.get(&from) != Some(&to) {
            return None;
        }
        rim += 1;
    }
    if rim != next.len() {
        return None;
    }

    // And no edge it introduces may already exist elsewhere, or that edge ends
    // up used more than twice and the surface stops being manifold.
    let mut before: HashMap<(u32, u32), u32> = HashMap::new();
    for &triangle in fan {
        let base = triangle * 3;
        for step in 0..3 {
            let (a, b) = (
                mesh.indices[base + step],
                mesh.indices[base + (step + 1) % 3],
            );
            *before.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }
    let mut wanted: HashMap<(u32, u32), u32> = HashMap::new();
    for triangle in &out {
        for step in 0..3 {
            let (a, b) = (triangle[step], triangle[(step + 1) % 3]);
            *wanted.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }
    for (edge, &count) in &wanted {
        let outside = edge_uses
            .get(edge)
            .copied()
            .unwrap_or(0)
            .saturating_sub(before.get(edge).copied().unwrap_or(0));
        if count + outside > 2 {
            return None;
        }
    }
    Some(out)
}

/// Put back the boundary vertices ear clipping skipped as collinear: a
/// triangle edge spanning several boundary edges is fanned through the
/// vertices it passed. `next` maps each boundary vertex to the one after it,
/// every loop in one map; `None` when the map is inconsistent.
pub fn restore_boundary_vertices(
    triangles: Vec<[u32; 3]>,
    next: &HashMap<u32, u32>,
) -> Option<Vec<[u32; 3]>> {
    let mut directed: HashMap<(u32, u32), u32> = HashMap::new();
    for triangle in &triangles {
        for step in 0..3 {
            *directed
                .entry((triangle[step], triangle[(step + 1) % 3]))
                .or_insert(0) += 1;
        }
    }
    let mut pending = triangles;
    let mut done: Vec<[u32; 3]> = Vec::with_capacity(pending.len());
    // Each split consumes one skipped vertex, so the work is bounded by the
    // ring; the cap only guarantees that.
    let limit = pending.len() + next.len() * 2 + 8;
    while let Some(triangle) = pending.pop() {
        if done.len() + pending.len() > limit {
            done.push(triangle);
            continue;
        }
        let mut split = None;
        for edge in 0..3 {
            let (from, to) = (triangle[edge], triangle[(edge + 1) % 3]);
            // Only an edge facing empty space, which starts on the ring and
            // does not follow it.
            if directed.contains_key(&(to, from))
                || directed.get(&(from, to)).copied().unwrap_or(0) != 1
            {
                continue;
            }
            let Some(&expected) = next.get(&from) else {
                continue;
            };
            if expected == to || !next.contains_key(&to) {
                continue;
            }
            let mut through = Vec::new();
            let mut cursor = expected;
            for _ in 0..next.len() {
                if cursor == to {
                    break;
                }
                through.push(cursor);
                cursor = *next.get(&cursor)?;
            }
            let opposite = triangle[(edge + 2) % 3];
            if cursor == to && !through.is_empty() && !through.contains(&opposite) {
                split = Some((edge, through));
                break;
            }
        }
        match split {
            Some((edge, through)) => {
                let opposite = triangle[(edge + 2) % 3];
                let mut previous = triangle[edge];
                for vertex in through {
                    pending.push([previous, vertex, opposite]);
                    previous = vertex;
                }
                pending.push([previous, triangle[(edge + 1) % 3], opposite]);
            }
            None => done.push(triangle),
        }
    }
    Some(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fan_of_coplanar_triangles_becomes_one_polygon() {
        let mut mesh = Mesh64::new();
        for corner in [(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0), (1.0, 1.0)] {
            mesh.push_vertex(DVec3::new(corner.0, corner.1, 0.0));
        }
        for edge in [(0, 1), (1, 2), (2, 3), (3, 0)] {
            mesh.push_triangle(edge.0, edge.1, 4);
        }
        let before = mesh.surface_area();
        assert_eq!(merge_coplanar(&mut mesh, 1e-9), 2);
        assert_eq!(mesh.triangle_count(), 2);
        assert!((mesh.surface_area() - before).abs() < 1e-12);
        assert_eq!(mesh.positions.len(), 4, "the middle vertex is gone");
    }

    #[test]
    fn a_face_with_a_hole_loses_its_interior_vertices_only() {
        // A square annulus whose eight trapezoid triangles are each fanned
        // about their own centroid: 24 triangles back to the minimal 8.
        let mut mesh = Mesh64::new();
        for corner in [
            (0.0, 0.0),
            (4.0, 0.0),
            (4.0, 4.0),
            (0.0, 4.0),
            (1.0, 1.0),
            (3.0, 1.0),
            (3.0, 3.0),
            (1.0, 3.0),
        ] {
            mesh.push_vertex(DVec3::new(corner.0, corner.1, 0.0));
        }
        for step in 0..4u32 {
            let (a, b) = (step, (step + 1) % 4);
            for corners in [[a, b, 4 + b], [a, 4 + b, 4 + a]] {
                let points: Vec<DVec3> = corners
                    .iter()
                    .map(|&index| mesh.positions[index as usize])
                    .collect();
                let middle = mesh.push_vertex((points[0] + points[1] + points[2]) / 3.0);
                for edge in 0..3 {
                    mesh.push_triangle(corners[edge], corners[(edge + 1) % 3], middle);
                }
            }
        }
        assert_eq!(mesh.triangle_count(), 24);
        assert_eq!(merge_coplanar(&mut mesh, 1e-9), 16);
        assert_eq!(mesh.triangle_count(), 8);
        assert!(
            (mesh.surface_area() - 12.0).abs() < 1e-9,
            "{}",
            mesh.surface_area()
        );
        assert_eq!(
            mesh.positions.len(),
            8,
            "every rim vertex survives, no more"
        );
    }

    #[test]
    fn a_vertex_where_the_surface_folds_is_kept() {
        let mut mesh = Mesh64::new();
        for corner in [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(1.0, 0.0, 0.0),
            DVec3::new(1.0, 1.0, 0.0),
            DVec3::new(0.0, 1.0, 0.0),
            DVec3::new(0.5, 0.5, 0.0),
            DVec3::new(0.5, 0.5, 1.0),
        ] {
            mesh.push_vertex(corner);
        }
        for edge in [(0, 1), (1, 2), (2, 3), (3, 0)] {
            mesh.push_triangle(edge.0, edge.1, 4);
        }
        // One triangle standing out of the plane at the middle vertex.
        mesh.push_triangle(0, 4, 5);
        let before = mesh.triangle_count();
        assert_eq!(merge_coplanar(&mut mesh, 1e-9), 0);
        assert_eq!(mesh.triangle_count(), before);
    }

    #[test]
    fn a_split_cube_comes_back_to_twelve_triangles() {
        let mut cube = Mesh64::new();
        for corner in 0..8u32 {
            cube.push_vertex(DVec3::new(
                (corner & 1) as f64,
                ((corner >> 1) & 1) as f64,
                ((corner >> 2) & 1) as f64,
            ));
        }
        for quad in [
            [0, 2, 3, 1],
            [4, 5, 7, 6],
            [0, 1, 5, 4],
            [2, 6, 7, 3],
            [0, 4, 6, 2],
            [1, 3, 7, 5],
        ] {
            cube.push_triangle(quad[0], quad[1], quad[2]);
            cube.push_triangle(quad[0], quad[2], quad[3]);
        }
        let volume = cube.signed_volume();
        let mut split = Mesh64::new();
        for triangle in cube.indices.chunks_exact(3) {
            let points: Vec<DVec3> = triangle
                .iter()
                .map(|&index| cube.positions[index as usize])
                .collect();
            let centre = (points[0] + points[1] + points[2]) / 3.0;
            let base = split.positions.len() as u32;
            split.positions.extend_from_slice(&points);
            let middle = split.push_vertex(centre);
            for step in 0..3u32 {
                split.push_triangle(base + step, base + (step + 1) % 3, middle);
            }
        }
        crate::weld::weld_and_close(&mut split, 1e-9);
        assert_eq!(split.triangle_count(), 36);
        assert_eq!(merge_coplanar(&mut split, 1e-9), 24);
        assert_eq!(split.triangle_count(), 12);
        assert!(split.is_edge_manifold());
        assert!((split.signed_volume() - volume).abs() < 1e-12);
    }

    #[test]
    fn a_rim_vertex_two_faces_share_is_kept() {
        // Two faces meeting along an edge that both have subdivided at vertex 4.
        let mut mesh = Mesh64::new();
        for corner in [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(2.0, 0.0, 0.0),
            DVec3::new(2.0, 2.0, 0.0),
            DVec3::new(0.0, 2.0, 0.0),
            DVec3::new(1.0, 0.0, 0.0),
            DVec3::new(1.0, 0.0, -2.0),
            DVec3::new(0.0, 0.0, -2.0),
            DVec3::new(2.0, 0.0, -2.0),
        ] {
            mesh.push_vertex(corner);
        }
        mesh.push_triangle(0, 4, 3);
        mesh.push_triangle(4, 2, 3);
        mesh.push_triangle(4, 1, 2);
        mesh.push_triangle(0, 6, 5);
        mesh.push_triangle(0, 5, 4);
        mesh.push_triangle(4, 5, 7);
        mesh.push_triangle(4, 7, 1);
        let before = mesh.triangle_count();
        merge_coplanar(&mut mesh, 1e-9);
        assert!(
            mesh.positions
                .iter()
                .any(|point| *point == DVec3::new(1.0, 0.0, 0.0)),
            "the shared rim vertex must survive"
        );
        assert!(mesh.triangle_count() <= before);
    }
}
