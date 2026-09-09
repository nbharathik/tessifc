// SPDX-License-Identifier: Apache-2.0
//! Merging vertices that are the same point.
//!
//! An IFC B-rep names its vertices once per face, so a cube arrives as
//! 24 or 36 vertices rather than 8. That costs memory, breaks edge counting
//! (nothing is watertight if no two faces share a vertex), and stops any
//! boolean kernel before it starts.
//!
//! Welding is a hash grid on quantised coordinates, not a spatial search. It is
//! linear, it needs no tree, and the quantum is the model's own precision, so
//! two points the file considers identical become identical here.

use crate::mesh::Mesh64;
use glam::DVec3;
use std::collections::{HashMap, HashSet, VecDeque};

/// Largest grid index a quantised coordinate may take.
const MAX_CELL_INDEX: f64 = 1e15;

/// Merge vertices closer together than `tolerance`, in place.
///
/// Returns the number of vertices removed. Triangles that collapse to a line
/// once their corners merge are dropped, because a zero-area triangle is not
/// geometry and every later stage has to special-case it.
///
/// The grid is exact rather than approximate: two points land in the same cell
/// or they do not. A pair straddling a cell boundary at 1.0001 times the
/// tolerance stays separate, which is the price of doing this in one pass.
/// Neighbour-cell probing would fix that and triple the cost; the tolerance
/// comes from the file's own precision, so the case is rare in practice.
pub fn weld(mesh: &mut Mesh64, tolerance: f64) -> usize {
    if mesh.positions.is_empty() || !tolerance.is_finite() || tolerance <= 0.0 {
        return 0;
    }
    let Some((low, high)) = mesh.bounds() else {
        return 0;
    };
    let before = mesh.positions.len();
    // Quantise from the box corner, and keep the cell index inside what f64
    // counts exactly: a georeferenced coordinate would otherwise saturate the
    // cast and weld the whole mesh into one vertex.
    let span = (high - low).max_element();
    let quantum = if span.is_finite() && span > tolerance * MAX_CELL_INDEX {
        span / MAX_CELL_INDEX
    } else {
        tolerance
    };
    let inverse = 1.0 / quantum;

    let mut cells: HashMap<(i64, i64, i64), u32> = HashMap::with_capacity(before);
    let mut remap = Vec::with_capacity(before);
    let mut kept: Vec<DVec3> = Vec::with_capacity(before);

    for position in &mesh.positions {
        let offset = *position - low;
        let key = (
            (offset.x * inverse).round() as i64,
            (offset.y * inverse).round() as i64,
            (offset.z * inverse).round() as i64,
        );
        match cells.get(&key) {
            Some(&index) => remap.push(index),
            None => {
                let index = kept.len() as u32;
                cells.insert(key, index);
                // Keep the first position seen rather than an average: an
                // average would move the geometry, and the whole point is that
                // these are the same point already.
                kept.push(*position);
                remap.push(index);
            }
        }
    }

    mesh.positions = kept;
    let mut indices = Vec::with_capacity(mesh.indices.len());
    for triangle in mesh.indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&c)) = (
            remap.get(triangle[0] as usize),
            remap.get(triangle[1] as usize),
            remap.get(triangle[2] as usize),
        ) else {
            continue;
        };
        if a == b || b == c || a == c {
            continue;
        }
        indices.extend_from_slice(&[a, b, c]);
    }
    mesh.indices = indices;

    before - mesh.positions.len()
}

/// Remove coincident triangles, regardless of their winding.
///
/// IFC exporters sometimes repeat the same face through two representation
/// items, or emit both sides of a zero-thickness surface. Once vertices have
/// been welded those faces have the same three indices. Keeping both makes a
/// depth buffer alternate between them while the camera moves; one triangle is
/// sufficient for a two-sided viewer and for all mesh measurements.
pub fn remove_duplicate_triangles(mesh: &mut Mesh64) -> usize {
    let before = mesh.triangle_count();
    let mut seen = HashSet::with_capacity(before);
    let mut kept = Vec::with_capacity(mesh.indices.len());
    for triangle in mesh.indices.chunks_exact(3) {
        let mut key = [triangle[0], triangle[1], triangle[2]];
        key.sort_unstable();
        if seen.insert(key) {
            kept.extend_from_slice(triangle);
        }
    }
    mesh.indices = kept;
    before - mesh.triangle_count()
}

/// Make adjacent triangles traverse their shared edge in opposite directions.
///
/// A closedness check only counts edge uses, so it cannot see a locally
/// reversed face. Those faces render as alternating light and dark patches and
/// make the artifact shimmer during orbit. This walks every orientable
/// connected component and repairs the local winding without assuming which
/// side of an open surface is outside.
pub fn orient_triangles_consistently(mesh: &mut Mesh64) -> usize {
    let triangles = mesh.triangle_count();
    if triangles < 2 {
        return 0;
    }

    // `direction` is true when this triangle traverses the canonical edge from
    // its smaller vertex index to its larger one. Neighbours must disagree.
    let mut uses: HashMap<(u32, u32), Vec<(usize, bool)>> = HashMap::with_capacity(triangles * 3);
    for (triangle_index, triangle) in mesh.indices.chunks_exact(3).enumerate() {
        for (from, to) in [
            (triangle[0], triangle[1]),
            (triangle[1], triangle[2]),
            (triangle[2], triangle[0]),
        ] {
            let edge = (from.min(to), from.max(to));
            uses.entry(edge)
                .or_default()
                .push((triangle_index, from < to));
        }
    }

    let mut neighbours: Vec<Vec<(usize, bool)>> = vec![Vec::new(); triangles];
    for edge_uses in uses.values() {
        // More than two uses is non-manifold. There is no unambiguous pair to
        // orient, so leave it for the caller's non-manifold diagnostic.
        if let [(left, left_direction), (right, right_direction)] = edge_uses.as_slice() {
            // If original directions match, exactly one triangle must flip.
            let relative_flip = left_direction == right_direction;
            neighbours[*left].push((*right, relative_flip));
            neighbours[*right].push((*left, relative_flip));
        }
    }

    let mut flips: Vec<Option<bool>> = vec![None; triangles];
    let mut queue = VecDeque::new();
    for seed in 0..triangles {
        if flips[seed].is_some() {
            continue;
        }
        flips[seed] = Some(false);
        queue.push_back(seed);
        while let Some(current) = queue.pop_front() {
            let current_flip = flips[current].unwrap_or(false);
            for &(neighbour, relative_flip) in &neighbours[current] {
                let needed = current_flip ^ relative_flip;
                if flips[neighbour].is_none() {
                    flips[neighbour] = Some(needed);
                    queue.push_back(neighbour);
                }
            }
        }
    }

    let mut changed = 0;
    for (triangle, flip) in mesh.indices.chunks_exact_mut(3).zip(flips) {
        if flip == Some(true) {
            triangle.swap(1, 2);
            changed += 1;
        }
    }
    changed
}

/// Weld, drop degenerate and coincident triangles, repair local winding, then
/// decide whether the result is a closed solid.
///
/// The order matters: closedness cannot be judged before welding, because an
/// unwelded B-rep shares no edges at all and would always look open.
pub fn weld_and_close(mesh: &mut Mesh64, tolerance: f64) -> usize {
    let removed = weld(mesh, tolerance);
    mesh.remove_degenerate_triangles(tolerance * tolerance);
    remove_duplicate_triangles(mesh);
    orient_triangles_consistently(mesh);
    mesh.closed = Some(mesh.is_edge_manifold());
    removed
}

/// Separate two faces that drew the same chord inside themselves.
///
/// Two triangulated faces meeting along a rim may each cut the same corner off
/// it, and then that chord exists twice: once inside each face. The surface is
/// watertight, but the edge is used four times and no edge count can tell that
/// from a real defect. Splitting one of the two copies at its own midpoint
/// changes no geometry at all, since the midpoint of a straight segment lies on
/// it, and leaves every edge used twice.
///
/// Returns how many edges were separated.
pub fn split_coincident_edges(mesh: &mut Mesh64, tolerance: f64) -> usize {
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1e-9
    };
    let mut users: std::collections::HashMap<(u32, u32), Vec<usize>> = Default::default();
    for (index, triangle) in mesh.indices.chunks_exact(3).enumerate() {
        for step in 0..3 {
            let (a, b) = (triangle[step], triangle[(step + 1) % 3]);
            users.entry((a.min(b), a.max(b))).or_default().push(index);
        }
    }
    // Only an edge used exactly four times, and only when the four triangles
    // are four distinct ones. Anything else is a real defect, not this.
    let mut work: Vec<((u32, u32), usize, usize)> = Vec::new();
    for (&edge, holders) in &users {
        if holders.len() != 4 {
            continue;
        }
        let mut distinct = holders.clone();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() != 4 {
            continue;
        }
        // One opposite pair: a triangle running a to b and one running b to a.
        let direction = |index: usize, from: u32, to: u32| -> bool {
            let base = index * 3;
            (0..3).any(|step| {
                mesh.indices[base + step] == from && mesh.indices[base + (step + 1) % 3] == to
            })
        };
        let forward = distinct
            .iter()
            .find(|&&index| direction(index, edge.0, edge.1));
        let backward = distinct
            .iter()
            .find(|&&index| direction(index, edge.1, edge.0));
        if let (Some(&one), Some(&other)) = (forward, backward) {
            work.push((edge, one, other));
        }
    }
    if work.is_empty() {
        return 0;
    }
    work.sort_unstable();

    let mut split: std::collections::HashMap<usize, ((u32, u32), u32)> = Default::default();
    let mut separated = 0usize;
    for (edge, one, other) in work {
        if split.contains_key(&one) || split.contains_key(&other) {
            continue;
        }
        let (Some(&a), Some(&b)) = (
            mesh.positions.get(edge.0 as usize),
            mesh.positions.get(edge.1 as usize),
        ) else {
            continue;
        };
        if a.distance(b) <= tol {
            continue;
        }
        let middle = mesh.push_vertex((a + b) * 0.5);
        split.insert(one, (edge, middle));
        split.insert(other, (edge, middle));
        separated += 1;
    }

    let mut indices = Vec::with_capacity(mesh.indices.len() + separated * 6);
    for (index, triangle) in mesh.indices.chunks_exact(3).enumerate() {
        match split.get(&index) {
            Some((edge, middle)) => {
                let at = (0..3)
                    .find(|&step| {
                        let (a, b) = (triangle[step], triangle[(step + 1) % 3]);
                        (a.min(b), a.max(b)) == *edge
                    })
                    .unwrap_or(0);
                let (a, b, c) = (triangle[at], triangle[(at + 1) % 3], triangle[(at + 2) % 3]);
                indices.extend_from_slice(&[a, *middle, c, *middle, b, c]);
            }
            None => indices.extend_from_slice(triangle),
        }
    }
    mesh.indices = indices;
    separated
}

/// Split triangle edges that have another vertex sitting in the middle of them.
///
/// A T-junction is what you get when two surfaces meet along a line that one
/// side has subdivided and the other has not. The mesh looks watertight, the
/// volume is right, and it is still not edge-manifold: the long edge is used
/// once while the two short ones facing it are used once each. Renderers show
/// it as a hairline crack, and any algorithm that walks edges sees a hole.
///
/// They are unavoidable when two independently triangulated surfaces are
/// stitched - which is exactly what a difference does - so they are repaired
/// afterwards rather than prevented.
///
/// Weld first: this matches vertices by position, and two copies of the same
/// corner are two different obstacles. Returns how many splits were made.
pub fn heal_t_junctions(mesh: &mut Mesh64, tolerance: f64) -> usize {
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1e-9
    };
    let Some((low, high)) = mesh.bounds() else {
        return 0;
    };
    // A grid fine enough to cut the candidate list down, coarse enough that a
    // long edge does not sweep thousands of cells.
    let extent = (high - low).max_element().max(tol);
    let cell = extent / 32.0;
    let index_of = |point: DVec3| {
        (
            ((point.x - low.x) / cell).floor() as i64,
            ((point.y - low.y) / cell).floor() as i64,
            ((point.z - low.z) / cell).floor() as i64,
        )
    };
    let mut grid: std::collections::HashMap<(i64, i64, i64), Vec<u32>> = Default::default();
    for (index, point) in mesh.positions.iter().enumerate() {
        grid.entry(index_of(*point)).or_default().push(index as u32);
    }

    let mut splits = 0;
    let mut pending: Vec<[u32; 3]> = mesh
        .indices
        .chunks_exact(3)
        .map(|triangle| [triangle[0], triangle[1], triangle[2]])
        .collect();
    let mut done: Vec<[u32; 3]> = Vec::with_capacity(pending.len());

    // Each split replaces one triangle with two, so the total is bounded; the
    // cap is a guarantee of termination rather than an expectation. The
    // candidate budget keeps a huge flat mesh, whose cells hold thousands of
    // vertices each, from turning quadratic: past it the rest is left as is.
    let limit = pending.len() * 8 + 64;
    let mut budget = mesh
        .positions
        .len()
        .saturating_mul(64)
        .saturating_add(1 << 20);
    while let Some(triangle) = pending.pop() {
        if done.len() + pending.len() > limit || budget == 0 {
            done.push(triangle);
            continue;
        }
        let mut split = None;
        'edges: for edge in 0..3 {
            let a = triangle[edge];
            let b = triangle[(edge + 1) % 3];
            let (Some(&pa), Some(&pb)) = (
                mesh.positions.get(a as usize),
                mesh.positions.get(b as usize),
            ) else {
                continue;
            };
            let along = pb - pa;
            let length_squared = along.length_squared();
            if length_squared <= tol * tol {
                continue;
            }
            let lo = index_of(pa.min(pb) - DVec3::splat(tol));
            let hi = index_of(pa.max(pb) + DVec3::splat(tol));
            for x in lo.0..=hi.0 {
                for y in lo.1..=hi.1 {
                    for z in lo.2..=hi.2 {
                        let Some(bucket) = grid.get(&(x, y, z)) else {
                            continue;
                        };
                        for candidate in bucket {
                            budget = budget.saturating_sub(1);
                            if *candidate == a
                                || *candidate == b
                                || *candidate == triangle[(edge + 2) % 3]
                            {
                                continue;
                            }
                            let point = mesh.positions[*candidate as usize];
                            let t = (point - pa).dot(along) / length_squared;
                            // Strictly between the ends, and on the line.
                            if t <= 0.0 || t >= 1.0 {
                                continue;
                            }
                            let foot = pa + along * t;
                            if foot.distance(point) > tol {
                                continue;
                            }
                            if point.distance(pa) <= tol || point.distance(pb) <= tol {
                                continue;
                            }
                            split = Some((edge, *candidate));
                            break 'edges;
                        }
                    }
                }
            }
        }
        match split {
            // Connect the intruding vertex to the corner opposite the edge it
            // landed on. Both halves go back on the list: an edge can carry
            // more than one.
            Some((edge, vertex)) => {
                let a = triangle[edge];
                let b = triangle[(edge + 1) % 3];
                let c = triangle[(edge + 2) % 3];
                pending.push([a, vertex, c]);
                pending.push([vertex, b, c]);
                splits += 1;
            }
            None => done.push(triangle),
        }
    }

    mesh.indices = done.into_iter().flatten().collect();
    splits
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_t_junction_is_split_and_the_shell_closes() {
        // A square made of one big triangle on one side of the diagonal and
        // two small ones on the other, so the long edge faces two short ones.
        let mut mesh = Mesh64::new();
        let a = mesh.push_vertex(DVec3::new(0.0, 0.0, 0.0));
        let b = mesh.push_vertex(DVec3::new(2.0, 0.0, 0.0));
        let c = mesh.push_vertex(DVec3::new(2.0, 2.0, 0.0));
        let middle = mesh.push_vertex(DVec3::new(1.0, 0.0, 0.0));
        mesh.push_triangle(a, b, c);
        mesh.push_triangle(a, middle, c);
        mesh.push_triangle(middle, b, c);

        assert_eq!(
            heal_t_junctions(&mut mesh, 1e-9),
            1,
            "one edge, one intruder"
        );
        assert_eq!(mesh.triangle_count(), 4, "the big triangle becomes two");
        // The seam is now shared: the two interior edges either side of the
        // middle vertex are each used twice, where the long edge was used once.
        let mut interior = 0;
        for triangle in mesh.indices.chunks_exact(3) {
            for edge in 0..3 {
                let pair = (triangle[edge], triangle[(edge + 1) % 3]);
                if (pair.0 == a && pair.1 == middle) || (pair.0 == middle && pair.1 == a) {
                    interior += 1;
                }
            }
        }
        assert_eq!(interior, 2, "the split edge is shared by both sides now");
        assert_eq!(
            heal_t_junctions(&mut mesh, 1e-9),
            0,
            "a second pass finds nothing"
        );
    }

    #[test]
    fn healing_a_clean_mesh_changes_nothing() {
        let mut mesh = Mesh64::new();
        mesh.push_vertex(DVec3::ZERO);
        mesh.push_vertex(DVec3::X);
        mesh.push_vertex(DVec3::Y);
        mesh.push_triangle(0, 1, 2);
        let before = mesh.indices.clone();
        assert_eq!(heal_t_junctions(&mut mesh, 1e-9), 0);
        assert_eq!(mesh.indices, before);
    }
    use super::*;
    use glam::DVec3;

    /// A cube built the way a B-rep names it: every face with its own vertices.
    fn unwelded_cube() -> Mesh64 {
        let corners = [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(1.0, 0.0, 0.0),
            DVec3::new(1.0, 1.0, 0.0),
            DVec3::new(0.0, 1.0, 0.0),
            DVec3::new(0.0, 0.0, 1.0),
            DVec3::new(1.0, 0.0, 1.0),
            DVec3::new(1.0, 1.0, 1.0),
            DVec3::new(0.0, 1.0, 1.0),
        ];
        let faces = [
            [0, 2, 1],
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
        ];
        let mut mesh = Mesh64::new();
        for face in faces {
            let base = mesh.positions.len() as u32;
            for &corner in &face {
                mesh.positions.push(corners[corner]);
            }
            mesh.push_triangle(base, base + 1, base + 2);
        }
        mesh
    }

    #[test]
    fn welding_a_brep_cube_recovers_eight_vertices() {
        let mut mesh = unwelded_cube();
        assert_eq!(mesh.positions.len(), 36);
        assert!(
            !mesh.is_edge_manifold(),
            "unwelded, so nothing shares an edge"
        );

        let removed = weld(&mut mesh, 1e-6);
        assert_eq!(removed, 28);
        assert_eq!(mesh.positions.len(), 8);
        assert_eq!(mesh.triangle_count(), 12);
        assert!(
            mesh.is_edge_manifold(),
            "welded, so it should be watertight now"
        );
        assert!((mesh.signed_volume() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn weld_and_close_sets_the_flag() {
        let mut mesh = unwelded_cube();
        weld_and_close(&mut mesh, 1e-6);
        assert_eq!(mesh.closed, Some(true));
    }

    #[test]
    fn duplicate_triangles_are_removed_after_welding() {
        let mut mesh = Mesh64::new();
        mesh.positions.extend([
            DVec3::ZERO,
            DVec3::X,
            DVec3::Y,
            DVec3::ZERO,
            DVec3::X,
            DVec3::Y,
        ]);
        mesh.push_triangle(0, 1, 2);
        mesh.push_triangle(5, 4, 3);

        weld_and_close(&mut mesh, 1e-9);

        assert_eq!(mesh.positions.len(), 3);
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(mesh.closed, Some(false));
    }

    #[test]
    fn adjacent_triangles_with_mixed_winding_are_repaired() {
        let mut mesh = Mesh64::new();
        mesh.positions.extend([
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(1.0, 0.0, 0.0),
            DVec3::new(1.0, 1.0, 0.0),
            DVec3::new(0.0, 1.0, 0.0),
        ]);
        mesh.push_triangle(0, 1, 2);
        // The shared edge is 2 -> 0 in both triangles, so this one is reversed.
        mesh.push_triangle(0, 3, 2);

        assert_eq!(orient_triangles_consistently(&mut mesh), 1);
        let first = &mesh.indices[0..3];
        let second = &mesh.indices[3..6];
        let first_normal = (mesh.positions[first[1] as usize] - mesh.positions[first[0] as usize])
            .cross(mesh.positions[first[2] as usize] - mesh.positions[first[0] as usize]);
        let second_normal = (mesh.positions[second[1] as usize]
            - mesh.positions[second[0] as usize])
            .cross(mesh.positions[second[2] as usize] - mesh.positions[second[0] as usize]);
        assert!(first_normal.dot(second_normal) > 0.0);
    }

    #[test]
    fn an_open_mesh_is_reported_open() {
        let mut mesh = unwelded_cube();
        mesh.indices.truncate(9); // three faces only
        weld_and_close(&mut mesh, 1e-6);
        assert_eq!(mesh.closed, Some(false));
    }

    #[test]
    fn points_further_apart_than_the_tolerance_stay_separate() {
        let mut mesh = Mesh64::new();
        mesh.positions.push(DVec3::ZERO);
        mesh.positions.push(DVec3::new(1.0, 0.0, 0.0));
        mesh.positions.push(DVec3::new(0.0, 1.0, 0.0));
        mesh.push_triangle(0, 1, 2);
        assert_eq!(weld(&mut mesh, 1e-6), 0);
        assert_eq!(mesh.positions.len(), 3);
    }

    #[test]
    fn a_collapsed_triangle_is_dropped() {
        let mut mesh = Mesh64::new();
        mesh.positions.push(DVec3::ZERO);
        mesh.positions.push(DVec3::new(1e-9, 0.0, 0.0)); // merges with the first
        mesh.positions.push(DVec3::new(0.0, 1.0, 0.0));
        mesh.push_triangle(0, 1, 2);
        weld(&mut mesh, 1e-6);
        assert_eq!(mesh.positions.len(), 2);
        assert_eq!(mesh.triangle_count(), 0, "the triangle became a line");
    }

    #[test]
    fn welding_an_empty_mesh_is_harmless() {
        let mut mesh = Mesh64::new();
        assert_eq!(weld(&mut mesh, 1e-6), 0);
    }

    #[test]
    fn a_zero_tolerance_does_nothing() {
        let mut mesh = unwelded_cube();
        assert_eq!(weld(&mut mesh, 0.0), 0);
        assert_eq!(mesh.positions.len(), 36);
    }

    #[test]
    fn a_non_finite_tolerance_does_nothing() {
        for tolerance in [f64::NAN, f64::INFINITY] {
            let mut mesh = unwelded_cube();
            assert_eq!(weld(&mut mesh, tolerance), 0);
            assert_eq!(mesh.positions.len(), 36);
        }
    }

    #[test]
    fn a_tiny_tolerance_at_a_georeferenced_origin_keeps_the_corners_apart() {
        let mut mesh = unwelded_cube();
        for position in &mut mesh.positions {
            *position += DVec3::new(420_000.0, 5_900_000.0, 12.0);
        }
        weld(&mut mesh, 1e-13);
        assert_eq!(mesh.positions.len(), 8);
        assert_eq!(mesh.triangle_count(), 12);
    }
}
