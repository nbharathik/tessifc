// SPDX-License-Identifier: Apache-2.0
//! Merging vertices that are the same point. A B-rep names each vertex once per
//! face, so nothing is watertight until they are merged. The weld is a hash grid
//! on coordinates quantised to the model's own precision.

use crate::mesh::Mesh64;
use glam::DVec3;
use std::collections::{HashMap, HashSet, VecDeque};

/// Largest grid index a quantised coordinate may take.
const MAX_CELL_INDEX: f64 = 1e15;

/// Merge vertices closer together than `tolerance`, in place.
///
/// Returns the number of vertices removed; triangles that collapse to a line
/// are dropped. The grid is exact: a pair straddling a cell boundary stays
/// separate, which is the price of one pass.
pub fn weld(mesh: &mut Mesh64, tolerance: f64) -> usize {
    if mesh.positions.is_empty() || !tolerance.is_finite() || tolerance <= 0.0 {
        return 0;
    }
    let Some((low, high)) = mesh.bounds() else {
        return 0;
    };
    let before = mesh.positions.len();
    // Quantise from the box corner and keep the cell index exact in f64, or a
    // georeferenced coordinate saturates the cast and welds everything together.
    let span = (high - low).max_element();
    let quantum = if span.is_finite() && span > tolerance * MAX_CELL_INDEX {
        span / MAX_CELL_INDEX
    } else {
        tolerance
    };
    let inverse = 1.0 / quantum;

    // A texture seam is two vertices at one position with different
    // coordinates; welding keeps them apart, and closedness is judged on
    // positions alone by the callers that need it.
    let carry = mesh.has_uvs();
    let mut cells: HashMap<(i64, i64, i64, u32, u32), u32> = HashMap::with_capacity(before);
    let mut remap = Vec::with_capacity(before);
    let mut kept: Vec<DVec3> = Vec::with_capacity(before);
    let mut kept_uvs: Vec<[f32; 2]> = Vec::new();

    for (at, position) in mesh.positions.iter().enumerate() {
        let offset = *position - low;
        let uv = if carry { mesh.uvs[at] } else { [0.0, 0.0] };
        let key = (
            (offset.x * inverse).round() as i64,
            (offset.y * inverse).round() as i64,
            (offset.z * inverse).round() as i64,
            uv[0].to_bits(),
            uv[1].to_bits(),
        );
        match cells.get(&key) {
            Some(&index) => remap.push(index),
            None => {
                let index = kept.len() as u32;
                cells.insert(key, index);
                // The first position seen, not an average: an average would move the geometry.
                kept.push(*position);
                if carry {
                    kept_uvs.push(uv);
                }
                remap.push(index);
            }
        }
    }

    mesh.positions = kept;
    mesh.uvs = kept_uvs;
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
/// Exporters repeat faces and emit both sides of zero-thickness surfaces; after
/// welding those share three indices, and two of them flicker in a depth buffer.
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
    mesh.closed = Some(if mesh.has_uvs() {
        // Texture seams keep vertices apart; the shell is judged on positions.
        let mut plain = mesh.clone();
        plain.drop_uvs();
        weld(&mut plain, tolerance);
        plain.is_edge_manifold()
    } else {
        mesh.is_edge_manifold()
    });
    removed
}

/// Separate two faces that drew the same chord inside themselves, so the
/// edge is no longer used four times: one copy is split at its midpoint, which
/// changes no geometry. Returns how many edges were separated.
pub fn split_coincident_edges(mesh: &mut Mesh64, tolerance: f64) -> usize {
    mesh.drop_uvs();
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
/// A T-junction leaves a mesh watertight but not edge-manifold, which renders
/// as a hairline crack; stitching two triangulations, as a difference does,
/// always makes some. Weld first, since vertices are matched by position.
/// Returns how many splits were made.
pub fn heal_t_junctions(mesh: &mut Mesh64, tolerance: f64) -> usize {
    mesh.drop_uvs();
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

    // The cap only guarantees termination; the candidate budget keeps a huge flat
    // mesh from turning quadratic, and past it the rest is left as is.
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
            // Connect the intruding vertex to the opposite corner; both halves go back
            // on the list because an edge can carry more than one.
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

/// Drop needle triangles thinner than `tolerance` that no properly shared
/// edge depends on: every edge of one is either unpaired or overused.
///
/// Two faces meeting along an edge one exporter placed twice, a whisker
/// apart, leave such a needle between them after welding. Removing it turns
/// its overused edges into shared ones and its open edge into nothing.
/// Returns how many were dropped.
pub fn drop_hanging_slivers(mesh: &mut Mesh64, tolerance: f64) -> usize {
    mesh.drop_uvs();
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1e-9
    };
    let mut uses: HashMap<(u32, u32), usize> = HashMap::with_capacity(mesh.indices.len());
    for triangle in mesh.indices.chunks_exact(3) {
        for step in 0..3 {
            let (from, to) = (triangle[step], triangle[(step + 1) % 3]);
            *uses.entry((from.min(to), from.max(to))).or_default() += 1;
        }
    }
    let before = mesh.triangle_count();
    let mut kept = Vec::with_capacity(mesh.indices.len());
    for triangle in mesh.indices.chunks_exact(3) {
        let hanging = (0..3).all(|step| {
            let (from, to) = (triangle[step], triangle[(step + 1) % 3]);
            uses.get(&(from.min(to), from.max(to))) != Some(&2)
        });
        let corners = (
            mesh.positions.get(triangle[0] as usize),
            mesh.positions.get(triangle[1] as usize),
            mesh.positions.get(triangle[2] as usize),
        );
        let thin = match corners {
            (Some(&a), Some(&b), Some(&c)) if hanging => {
                let longest = (b - a).length().max((c - b).length()).max((a - c).length());
                longest > 0.0 && (b - a).cross(c - a).length() < tol * longest
            }
            _ => false,
        };
        if !thin {
            kept.extend_from_slice(triangle);
        }
    }
    mesh.indices = kept;
    before - mesh.triangle_count()
}

/// Most triangles one edge may carry before the split gives up on it.
const MAX_EDGE_USES: usize = 64;

/// Separate solids that weld into one surface into one closed shell each.
///
/// Two solids sharing a face weld into a surface whose shared edges carry
/// three or four triangles. Around every edge the faces are sorted by angle,
/// and the two face sides that look into the same wedge bound the same
/// region of space; a face with material on both sides is written into both
/// shells. A region is solid when its boundary, wound away from it, encloses
/// a positive volume, so the outside is never a shell and winding is not
/// trusted. A face with the same region on both sides separates nothing and
/// is dropped. `None` when no edge is shared, so there is nothing to split,
/// when a face lies on part of another, and unless every shell comes out
/// closed. Each shell has `closed` set.
pub fn split_manifold_shells(mesh: &Mesh64, tolerance: f64) -> Option<Vec<Mesh64>> {
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1e-9
    };
    let mut welded = mesh.clone();
    weld(&mut welded, tol);
    welded.remove_degenerate_triangles(tol * tol);
    // Two solids rarely split a shared edge the same way; stitch first. A
    // face both of them wrote survives once and is handed to each below.
    if heal_t_junctions(&mut welded, tol) > 0 {
        weld(&mut welded, tol);
        welded.remove_degenerate_triangles(tol * tol);
    }
    remove_duplicate_triangles(&mut welded);
    let triangles = welded.triangle_count();
    let uses = edge_uses(&welded);
    if triangles < 4 || uses.values().all(|holders| holders.len() <= 2) {
        return None;
    }
    // A face lying on part of another shares no edge with it there, so the
    // wedges round the edges cannot tell the regions apart.
    if coplanar_faces_overlap(&welded, tol)? {
        return None;
    }

    // One node per face side: the front of triangle `t` is `2 t`, its back `2 t + 1`.
    let mut parent: Vec<usize> = (0..triangles * 2).collect();
    fn root(parent: &mut [usize], mut index: usize) -> usize {
        while parent[index] != index {
            parent[index] = parent[parent[index]];
            index = parent[index];
        }
        index
    }
    let unite = |parent: &mut [usize], a: usize, b: usize| {
        let (ra, rb) = (root(parent, a), root(parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    };
    let normal_of = |index: usize| -> DVec3 {
        let t = &welded.indices[index * 3..index * 3 + 3];
        let (a, b, c) = (
            welded.positions[t[0] as usize],
            welded.positions[t[1] as usize],
            welded.positions[t[2] as usize],
        );
        (b - a).cross(c - a)
    };

    let mut keyed: Vec<(&(u32, u32), &Vec<usize>)> = uses.iter().collect();
    keyed.sort_unstable_by_key(|(edge, _)| **edge);
    for (&(a, b), holders) in keyed {
        if holders.len() < 2 {
            continue;
        }
        if holders.len() > MAX_EDGE_USES {
            return None;
        }
        let pa = welded.positions[a as usize];
        let pb = welded.positions[b as usize];
        let axis = (pb - pa).try_normalize()?;
        // Each face by its angle round the edge. Of two coincident faces, the
        // one whose normal points on round the edge sits next to the wedge
        // after them, which is the same geometric choice at all its edges.
        let mut fans: Vec<(f64, usize, bool)> = Vec::with_capacity(holders.len());
        let mut frame: Option<(DVec3, DVec3)> = None;
        for &index in holders {
            let triangle = &welded.indices[index * 3..index * 3 + 3];
            let &other = triangle.iter().find(|&&v| v != a && v != b)?;
            let offset = welded.positions[other as usize] - pa;
            let d = (offset - axis * offset.dot(axis)).try_normalize()?;
            let (u, v) = *frame.get_or_insert_with(|| (d, axis.cross(d)));
            let mut angle = d.dot(v).atan2(d.dot(u));
            if angle < 0.0 {
                angle += std::f64::consts::TAU;
            }
            let forward = normal_of(index).dot(axis.cross(d)) > 0.0;
            fans.push((angle, index, forward));
        }
        fans.sort_by(|x, y| {
            if (x.0 - y.0).abs() <= 1e-6 {
                x.2.cmp(&y.2)
            } else {
                x.0.total_cmp(&y.0)
            }
        });
        let (u, v) = frame?;
        let count = fans.len();
        let wedges: Vec<(f64, DVec3)> = (0..count)
            .map(|wedge| {
                let (from, _, _) = fans[wedge];
                let (to, _, _) = fans[(wedge + 1) % count];
                let mut width = to - from;
                if wedge + 1 == count {
                    width += std::f64::consts::TAU;
                }
                let bisector = from + width * 0.5;
                (width, u * bisector.cos() + v * bisector.sin())
            })
            .collect();
        // Which side of each face looks into the wedge after it and the one
        // before it. A coincident pair has no wedge between them to look into,
        // so those sides are the other flank's opposite.
        let facing = |index: usize, towards: DVec3| {
            index * 2 + usize::from(normal_of(index).dot(towards) < 0.0)
        };
        let mut after: Vec<Option<usize>> = vec![None; count];
        let mut before: Vec<Option<usize>> = vec![None; count];
        for (wedge, &(width, towards)) in wedges.iter().enumerate() {
            if width > 1e-6 {
                after[wedge] = Some(facing(fans[wedge].1, towards));
                before[(wedge + 1) % count] = Some(facing(fans[(wedge + 1) % count].1, towards));
            }
        }
        for face in 0..count {
            match (after[face], before[face]) {
                (Some(_), Some(_)) => {}
                (Some(side), None) => before[face] = Some(side ^ 1),
                (None, Some(side)) => after[face] = Some(side ^ 1),
                (None, None) => return None,
            }
        }
        for wedge in 0..count {
            unite(&mut parent, after[wedge]?, before[(wedge + 1) % count]?);
        }
    }

    // Emit each region's boundary wound away from it, in first-seen order;
    // a face with one region on both sides is nobody's boundary.
    let mut slot_of: HashMap<usize, usize> = HashMap::new();
    let mut shells: Vec<Mesh64> = Vec::new();
    let mut remap: Vec<Vec<u32>> = Vec::new();
    for node in 0..triangles * 2 {
        let index = node / 2;
        let group = root(&mut parent, node);
        if group == root(&mut parent, node ^ 1) {
            continue;
        }
        let which = *slot_of.entry(group).or_insert_with(|| {
            shells.push(Mesh64::new());
            remap.push(vec![u32::MAX; welded.positions.len()]);
            shells.len() - 1
        });
        let triangle = &welded.indices[index * 3..index * 3 + 3];
        let mut local = [0u32; 3];
        for (corner, &vertex) in triangle.iter().enumerate() {
            let slot = &mut remap[which][vertex as usize];
            if *slot == u32::MAX {
                *slot = shells[which].push_vertex(welded.positions[vertex as usize]);
            }
            local[corner] = *slot;
        }
        // The region on the front means the normal points into it: turn it round.
        if node % 2 == 0 {
            shells[which].push_triangle(local[0], local[2], local[1]);
        } else {
            shells[which].push_triangle(local[0], local[1], local[2]);
        }
    }
    let mut out = Vec::with_capacity(shells.len());
    for mut shell in shells {
        let volume = shell.signed_volume();
        if volume <= tol * shell.surface_area() {
            // The outside, or a sheet between coincident faces.
            continue;
        }
        if !shell.is_edge_manifold() || !shell.is_consistently_wound() {
            return None;
        }
        shell.closed = Some(true);
        out.push(shell);
    }
    Some(out)
}

/// Most coplanar triangles compared pairwise before the check gives up.
const MAX_COPLANAR_GROUP: usize = 2048;

/// Do two coplanar triangles share any area, beyond touching along an edge?
/// `None` when a plane holds more triangles than the check will compare.
fn coplanar_faces_overlap(mesh: &Mesh64, tol: f64) -> Option<bool> {
    let mut groups: HashMap<(i64, i64, i64, i64), Vec<usize>> = HashMap::new();
    let mut corners: Vec<[DVec3; 3]> = Vec::with_capacity(mesh.triangle_count());
    for (index, triangle) in mesh.indices.chunks_exact(3).enumerate() {
        let (a, b, c) = (
            mesh.positions[triangle[0] as usize],
            mesh.positions[triangle[1] as usize],
            mesh.positions[triangle[2] as usize],
        );
        corners.push([a, b, c]);
        let Some(normal) = (b - a).cross(c - a).try_normalize() else {
            continue;
        };
        // One key per plane, whichever way its faces wind.
        let normal = if normal.x < 0.0
            || (normal.x == 0.0 && (normal.y < 0.0 || (normal.y == 0.0 && normal.z < 0.0)))
        {
            -normal
        } else {
            normal
        };
        let key = (
            (normal.x * 1e6).round() as i64,
            (normal.y * 1e6).round() as i64,
            (normal.z * 1e6).round() as i64,
            (a.dot(normal) / tol).round() as i64,
        );
        groups.entry(key).or_default().push(index);
    }
    for members in groups.values() {
        if members.len() < 2 {
            continue;
        }
        if members.len() > MAX_COPLANAR_GROUP {
            return None;
        }
        let normal = {
            let [a, b, c] = corners[members[0]];
            (b - a).cross(c - a).normalize_or_zero()
        };
        // Two in-plane axes to compare along.
        let u = normal
            .cross(DVec3::X)
            .try_normalize()
            .or_else(|| normal.cross(DVec3::Y).try_normalize())?;
        let v = normal.cross(u);
        let flat = |t: usize| corners[t].map(|p| glam::DVec2::new(p.dot(u), p.dot(v)));
        for (slot, &one) in members.iter().enumerate() {
            let first = flat(one);
            for &other in &members[slot + 1..] {
                if triangles_share_area(&first, &flat(other), tol) {
                    return Some(true);
                }
            }
        }
    }
    Some(false)
}

/// Separating-axis test in the plane; touching along an edge is not sharing.
fn triangles_share_area(a: &[glam::DVec2; 3], b: &[glam::DVec2; 3], tol: f64) -> bool {
    for triangle in [a, b] {
        for step in 0..3 {
            let edge = triangle[(step + 1) % 3] - triangle[step];
            let axis = glam::DVec2::new(-edge.y, edge.x);
            if axis.length_squared() == 0.0 {
                return false;
            }
            let axis = axis.normalize();
            let range = |t: &[glam::DVec2; 3]| {
                let d = t.map(|p| p.dot(axis));
                (d[0].min(d[1]).min(d[2]), d[0].max(d[1]).max(d[2]))
            };
            let (low_a, high_a) = range(a);
            let (low_b, high_b) = range(b);
            if high_a <= low_b + tol || high_b <= low_a + tol {
                return false;
            }
        }
    }
    true
}

/// Every edge with the triangles that use it, in triangle order.
fn edge_uses(mesh: &Mesh64) -> HashMap<(u32, u32), Vec<usize>> {
    let mut uses: HashMap<(u32, u32), Vec<usize>> = HashMap::with_capacity(mesh.indices.len());
    for (index, triangle) in mesh.indices.chunks_exact(3).enumerate() {
        for step in 0..3 {
            let (from, to) = (triangle[step], triangle[(step + 1) % 3]);
            uses.entry((from.min(to), from.max(to)))
                .or_default()
                .push(index);
        }
    }
    uses
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

    /// An axis-aligned box, outward wound, with its own vertices.
    fn box_at(lo: DVec3, hi: DVec3) -> Mesh64 {
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
        let faces = [
            [0, 2, 3, 1],
            [4, 5, 7, 6],
            [0, 1, 5, 4],
            [2, 6, 7, 3],
            [0, 4, 6, 2],
            [1, 3, 7, 5],
        ];
        for face in faces {
            mesh.push_triangle(face[0], face[1], face[2]);
            mesh.push_triangle(face[0], face[2], face[3]);
        }
        mesh
    }

    fn boxes(cells: &[(DVec3, DVec3)]) -> Mesh64 {
        let mut mesh = Mesh64::new();
        for &(lo, hi) in cells {
            mesh.append(&box_at(lo, hi));
        }
        mesh
    }

    fn volumes(shells: &[Mesh64]) -> Vec<f64> {
        let mut out: Vec<f64> = shells.iter().map(Mesh64::signed_volume).collect();
        out.sort_by(|a, b| a.partial_cmp(b).unwrap());
        out
    }

    #[test]
    fn two_boxes_sharing_a_face_come_apart_closed() {
        let mesh = boxes(&[
            (DVec3::ZERO, DVec3::new(1.0, 1.0, 1.0)),
            (DVec3::new(1.0, 0.0, 0.0), DVec3::new(3.0, 1.0, 1.0)),
        ]);
        let mut welded = mesh.clone();
        weld_and_close(&mut welded, 1e-9);
        assert!(
            welded.edge_defects().1 > 0,
            "the shared face makes overused edges"
        );
        let shells = split_manifold_shells(&mesh, 1e-9).expect("two closed shells");
        assert_eq!(shells.len(), 2);
        for shell in &shells {
            assert_eq!(shell.closed, Some(true));
            assert_eq!(
                shell.triangle_count(),
                12,
                "each keeps its copy of the shared face"
            );
        }
        let volumes = volumes(&shells);
        assert!((volumes[0] - 1.0).abs() < 1e-12 && (volumes[1] - 2.0).abs() < 1e-12);
    }

    #[test]
    fn a_t_of_three_boxes_gives_three() {
        let mesh = boxes(&[
            (DVec3::new(0.0, 0.0, 0.0), DVec3::new(1.0, 1.0, 1.0)),
            (DVec3::new(1.0, 0.0, 0.0), DVec3::new(2.0, 1.0, 1.0)),
            (DVec3::new(2.0, 0.0, 0.0), DVec3::new(3.0, 1.0, 1.0)),
            (DVec3::new(1.0, 1.0, 0.0), DVec3::new(2.0, 2.0, 1.0)),
        ]);
        let shells = split_manifold_shells(&mesh, 1e-9).expect("four closed shells");
        assert_eq!(shells.len(), 4);
        for volume in volumes(&shells) {
            assert!((volume - 1.0).abs() < 1e-12, "{volume}");
        }
    }

    #[test]
    fn a_box_on_part_of_a_face_is_refused() {
        // The second box's back lies inside the first box's face, so the two
        // faces overlap without sharing the edges that would separate them.
        let mesh = boxes(&[
            (DVec3::ZERO, DVec3::new(1.0, 1.0, 1.0)),
            (DVec3::new(1.0, 0.0, 0.0), DVec3::new(2.0, 1.0, 0.5)),
        ]);
        assert!(split_manifold_shells(&mesh, 1e-9).is_none());
    }

    #[test]
    fn a_dangling_fin_is_dropped() {
        let mut mesh = box_at(DVec3::ZERO, DVec3::ONE);
        let a = mesh.push_vertex(DVec3::new(1.0, 0.0, 0.0));
        let b = mesh.push_vertex(DVec3::new(1.0, 1.0, 0.0));
        let c = mesh.push_vertex(DVec3::new(2.0, 0.5, 0.0));
        mesh.push_triangle(a, b, c);
        let shells = split_manifold_shells(&mesh, 1e-9).expect("the box alone");
        assert_eq!(shells.len(), 1);
        assert_eq!(shells[0].triangle_count(), 12);
        assert!((shells[0].signed_volume() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_grid_of_eight_boxes_gives_eight() {
        let mut cells = Vec::new();
        for i in 0..2 {
            for j in 0..2 {
                for k in 0..2 {
                    let lo = DVec3::new(i as f64, j as f64, k as f64);
                    cells.push((lo, lo + DVec3::ONE));
                }
            }
        }
        let shells = split_manifold_shells(&boxes(&cells), 1e-9).expect("eight closed shells");
        assert_eq!(shells.len(), 8);
        assert!(volumes(&shells).iter().all(|v| (v - 1.0).abs() < 1e-12));
    }

    #[test]
    fn a_box_missing_a_triangle_is_refused() {
        let mut mesh = boxes(&[
            (DVec3::ZERO, DVec3::new(1.0, 1.0, 1.0)),
            (DVec3::new(1.0, 0.0, 0.0), DVec3::new(2.0, 1.0, 1.0)),
        ]);
        mesh.indices.truncate(mesh.indices.len() - 3);
        assert!(split_manifold_shells(&mesh, 1e-9).is_none());
    }

    #[test]
    fn a_surface_with_no_shared_edge_has_nothing_to_split() {
        let mut torus = Mesh64::new();
        let (major, minor, around, tube) = (2.0, 0.5, 24, 12);
        for i in 0..around {
            let u = std::f64::consts::TAU * i as f64 / around as f64;
            for j in 0..tube {
                let v = std::f64::consts::TAU * j as f64 / tube as f64;
                let r = major + minor * v.cos();
                torus.push_vertex(DVec3::new(r * u.cos(), r * u.sin(), minor * v.sin()));
            }
        }
        let at = |i: usize, j: usize| ((i % around) * tube + (j % tube)) as u32;
        for i in 0..around {
            for j in 0..tube {
                torus.push_triangle(at(i, j), at(i + 1, j), at(i + 1, j + 1));
                torus.push_triangle(at(i, j), at(i + 1, j + 1), at(i, j + 1));
            }
        }
        assert!(split_manifold_shells(&torus, 1e-9).is_none());
        assert!(split_manifold_shells(&box_at(DVec3::ZERO, DVec3::ONE), 1e-9).is_none());
    }

    #[test]
    fn a_fin_inside_a_box_splits_it_in_two() {
        // A wall through the middle of a box, welded to its sides: material
        // on both sides, so it is the face two half boxes share.
        let mut mesh = box_at(DVec3::ZERO, DVec3::new(2.0, 1.0, 1.0));
        let fin = [
            mesh.push_vertex(DVec3::new(1.0, 0.0, 0.0)),
            mesh.push_vertex(DVec3::new(1.0, 1.0, 0.0)),
            mesh.push_vertex(DVec3::new(1.0, 1.0, 1.0)),
            mesh.push_vertex(DVec3::new(1.0, 0.0, 1.0)),
        ];
        mesh.push_triangle(fin[0], fin[1], fin[2]);
        mesh.push_triangle(fin[0], fin[2], fin[3]);
        // Split the box faces along the fin so its edges are shared.
        let mut split = Mesh64::new();
        for triangle in mesh.indices.chunks_exact(3) {
            let points: Vec<DVec3> = triangle
                .iter()
                .map(|&i| mesh.positions[i as usize])
                .collect();
            let straddles =
                points.iter().any(|p| p.x < 1.0 - 1e-9) && points.iter().any(|p| p.x > 1.0 + 1e-9);
            if !straddles {
                let base = split.positions.len() as u32;
                split.positions.extend(points);
                split.push_triangle(base, base + 1, base + 2);
                continue;
            }
            let plane =
                crate::clip::Plane::from_point_normal(DVec3::new(1.0, 0.0, 0.0), DVec3::X).unwrap();
            for half in [
                crate::clip::clip_polygon(&points, &plane, 1e-9),
                crate::clip::clip_polygon(&points, &plane.flipped(), 1e-9),
            ] {
                let base = split.positions.len() as u32;
                split.positions.extend(half.iter().copied());
                for k in 1..half.len().saturating_sub(1) {
                    split.push_triangle(base, base + k as u32, base + k as u32 + 1);
                }
            }
        }
        let shells = split_manifold_shells(&split, 1e-9).expect("two half boxes");
        assert_eq!(shells.len(), 2);
        for shell in &shells {
            assert!(
                (shell.signed_volume() - 1.0).abs() < 1e-12,
                "{}",
                shell.signed_volume()
            );
        }
    }

    #[test]
    fn a_repeated_face_is_dropped_but_an_opposite_one_is_kept() {
        let mut mesh = boxes(&[
            (DVec3::ZERO, DVec3::new(1.0, 1.0, 1.0)),
            (DVec3::new(1.0, 0.0, 0.0), DVec3::new(2.0, 1.0, 1.0)),
        ]);
        // The first box's +x face again, same winding: a repeat, not a third solid.
        let repeat: Vec<u32> = mesh.indices[30..36].to_vec();
        mesh.indices.extend_from_slice(&repeat);
        let shells = split_manifold_shells(&mesh, 1e-9).expect("two closed shells");
        assert_eq!(shells.len(), 2);
        assert_eq!(shells.iter().map(Mesh64::triangle_count).sum::<usize>(), 24);
    }

    #[test]
    fn a_flipped_second_box_still_comes_apart_outward() {
        let mut mesh = boxes(&[
            (DVec3::ZERO, DVec3::new(1.0, 1.0, 1.0)),
            (DVec3::new(1.0, 0.0, 0.0), DVec3::new(2.0, 1.0, 1.0)),
        ]);
        let second = mesh.indices.len() / 2;
        for triangle in mesh.indices[second..].chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
        let shells = split_manifold_shells(&mesh, 1e-9).expect("two closed shells");
        assert_eq!(shells.len(), 2);
        for shell in &shells {
            assert!(
                (shell.signed_volume() - 1.0).abs() < 1e-12,
                "wound outward again"
            );
        }
    }
}
