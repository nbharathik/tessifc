// SPDX-License-Identifier: Apache-2.0
//! Mesh simplification by quadric error metrics: edges collapse onto one of
//! their own vertices, cheapest first, until the target count or the error
//! tolerance stops it. After Garland and Heckbert (1997) and Hoppe's subset
//! placement, from the papers. The output only ever drops vertices, so a
//! coarse level can share the fine mesh's positions.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use glam::DVec3;

use crate::locality::optimize_index_locality;
use crate::mesh::Mesh64;

/// Meshes under this many triangles are not worth a coarse level.
pub const LOD_MIN_TRIANGLES: usize = 1024;
/// Meshes over this many triangles are refused rather than simplified.
pub const MAX_DECIMATE_TRIANGLES: usize = 1_000_000;
/// The triangle count a coarse level aims for, as a fraction of the fine one.
pub const LOD_TARGET_RATIO: f64 = 0.25;
/// A result that removed less than this fraction of the triangles is discarded.
const MIN_REMOVED_RATIO: f64 = 0.25;
/// A collapse that turns a surviving triangle's normal by more than this is refused.
const MAX_TURN_COS: f64 = 0.5;
/// The plane of a triangle smaller than this contributes no error.
const MIN_PLANE_AREA: f64 = 1e-18;
/// Collapses one call may apply, whatever the input.
const MAX_COLLAPSES: usize = 4_000_000;

/// How far a simplification may go.
#[derive(Clone, Debug, PartialEq)]
pub struct DecimateOptions {
    /// Stop once the triangle count has fallen to this fraction of the input, in `(0, 1]`.
    pub target_ratio: f64,
    /// The largest distance from any plane of the merged region a kept vertex
    /// may have, in mesh units; a collapse past it is never applied.
    pub tolerance: f64,
    /// Inputs with more triangles are refused.
    pub max_triangles: usize,
}

impl Default for DecimateOptions {
    fn default() -> Self {
        DecimateOptions {
            target_ratio: LOD_TARGET_RATIO,
            tolerance: 0.01,
            max_triangles: MAX_DECIMATE_TRIANGLES,
        }
    }
}

/// A symmetric 4x4 quadric as its ten upper-triangle entries.
#[derive(Clone, Copy, Debug, Default)]
struct Quadric([f64; 10]);

impl Quadric {
    fn from_plane(normal: DVec3, d: f64) -> Self {
        let (a, b, c) = (normal.x, normal.y, normal.z);
        Quadric([
            a * a,
            a * b,
            a * c,
            a * d,
            b * b,
            b * c,
            b * d,
            c * c,
            c * d,
            d * d,
        ])
    }

    fn add(&mut self, other: &Quadric) {
        for (mine, theirs) in self.0.iter_mut().zip(other.0.iter()) {
            *mine += theirs;
        }
    }

    fn sum(&self, other: &Quadric) -> Quadric {
        let mut out = *self;
        out.add(other);
        out
    }

    /// `v^T Q v` for `v = (x, y, z, 1)`: the sum of squared plane distances.
    fn evaluate(&self, p: DVec3) -> f64 {
        let q = &self.0;
        q[0] * p.x * p.x
            + 2.0 * q[1] * p.x * p.y
            + 2.0 * q[2] * p.x * p.z
            + 2.0 * q[3] * p.x
            + q[4] * p.y * p.y
            + 2.0 * q[5] * p.y * p.z
            + 2.0 * q[6] * p.y
            + q[7] * p.z * p.z
            + 2.0 * q[8] * p.z
            + q[9]
    }
}

/// A candidate collapse of `removed` onto `kept`, ordered cheapest first and
/// then by its vertices, so equal costs resolve the same way every run.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    cost: f64,
    removed: u32,
    kept: u32,
    version_removed: u32,
    version_kept: u32,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed, so the binary max-heap pops the cheapest first.
        other
            .cost
            .total_cmp(&self.cost)
            .then_with(|| other.removed.cmp(&self.removed))
            .then_with(|| other.kept.cmp(&self.kept))
    }
}

/// The working state of one simplification.
struct Decimation<'a> {
    positions: &'a [DVec3],
    triangles: Vec<[u32; 3]>,
    alive: Vec<bool>,
    /// Triangles around each vertex; dead ones are skipped where they are met.
    around: Vec<Vec<u32>>,
    quadrics: Vec<Quadric>,
    locked: Vec<bool>,
    dead: Vec<bool>,
    version: Vec<u32>,
    live_triangles: usize,
}

impl Decimation<'_> {
    fn normal_of(&self, corners: [u32; 3]) -> DVec3 {
        let a = self.positions[corners[0] as usize];
        let b = self.positions[corners[1] as usize];
        let c = self.positions[corners[2] as usize];
        (b - a).cross(c - a)
    }

    /// Neighbours of a vertex over its live triangles, sorted and unique.
    fn neighbours(&self, vertex: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for &triangle in &self.around[vertex as usize] {
            if !self.alive[triangle as usize] {
                continue;
            }
            for &corner in &self.triangles[triangle as usize] {
                if corner != vertex {
                    out.push(corner);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn cost(&self, removed: u32, kept: u32) -> f64 {
        self.quadrics[removed as usize]
            .sum(&self.quadrics[kept as usize])
            .evaluate(self.positions[kept as usize])
    }

    fn push_edges_around(
        &self,
        vertex: u32,
        heap: &mut BinaryHeap<Candidate>,
        tolerance_squared: f64,
    ) {
        for other in self.neighbours(vertex) {
            for (removed, kept) in [(vertex, other), (other, vertex)] {
                if self.locked[removed as usize] {
                    continue;
                }
                let cost = self.cost(removed, kept);
                // A NaN cost fails the test too.
                if cost.is_nan() || cost > tolerance_squared {
                    continue;
                }
                heap.push(Candidate {
                    cost,
                    removed,
                    kept,
                    version_removed: self.version[removed as usize],
                    version_kept: self.version[kept as usize],
                });
            }
        }
    }

    /// Whether collapsing `removed` onto `kept` keeps the surface a surface:
    /// the link condition, no triangle turned or flattened.
    fn valid(&self, removed: u32, kept: u32) -> bool {
        let mine = self.neighbours(removed);
        let theirs = self.neighbours(kept);
        if !mine.contains(&kept) {
            return false;
        }
        let shared = mine.iter().filter(|vertex| theirs.contains(vertex)).count();
        let on_edge = self.around[removed as usize]
            .iter()
            .filter(|&&triangle| {
                self.alive[triangle as usize] && self.triangles[triangle as usize].contains(&kept)
            })
            .count();
        if shared != on_edge || on_edge == 0 {
            return false;
        }
        for &triangle in &self.around[removed as usize] {
            if !self.alive[triangle as usize] {
                continue;
            }
            let corners = self.triangles[triangle as usize];
            if corners.contains(&kept) {
                continue;
            }
            let before = self.normal_of(corners);
            let moved = corners.map(|corner| if corner == removed { kept } else { corner });
            let after = self.normal_of(moved);
            let lengths = before.length() * after.length();
            let flattened = after.length_squared();
            if flattened.is_nan()
                || flattened <= MIN_PLANE_AREA
                || lengths.is_nan()
                || lengths <= 0.0
            {
                return false;
            }
            if before.dot(after) < MAX_TURN_COS * lengths {
                return false;
            }
        }
        true
    }

    fn collapse(&mut self, removed: u32, kept: u32) {
        let mut moved = Vec::new();
        for &triangle in &self.around[removed as usize] {
            let index = triangle as usize;
            if !self.alive[index] {
                continue;
            }
            if self.triangles[index].contains(&kept) {
                self.alive[index] = false;
                self.live_triangles -= 1;
                continue;
            }
            for corner in self.triangles[index].iter_mut() {
                if *corner == removed {
                    *corner = kept;
                }
            }
            moved.push(triangle);
        }
        let quadric = self.quadrics[removed as usize];
        self.quadrics[kept as usize].add(&quadric);
        let mut list = std::mem::take(&mut self.around[removed as usize]);
        list.retain(|triangle| moved.contains(triangle));
        self.around[kept as usize].extend(list);
        self.dead[removed as usize] = true;
        self.version[removed as usize] = self.version[removed as usize].wrapping_add(1);
        self.version[kept as usize] = self.version[kept as usize].wrapping_add(1);
    }
}

/// Simplify a mesh. Returns the surviving triangles' indices, in the input's
/// vertex numbering (every output vertex is an input vertex), or `None` when
/// the input is invalid, over the size limit, or when fewer than a quarter of
/// the triangles could go within the tolerance. Boundary and non-manifold
/// vertices never move, a closed manifold stays one, and the result is the
/// same on every run.
pub fn decimate(mesh: &Mesh64, options: &DecimateOptions) -> Option<Vec<u32>> {
    decimate_indices(&mesh.positions, &mesh.indices, options)
}

/// [`decimate`] over positions already narrowed to `f32`, three per vertex.
pub fn decimate_f32(
    positions: &[f32],
    indices: &[u32],
    options: &DecimateOptions,
) -> Option<Vec<u32>> {
    if !positions.len().is_multiple_of(3) {
        return None;
    }
    let widened: Vec<DVec3> = positions
        .chunks_exact(3)
        .map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64))
        .collect();
    decimate_indices(&widened, indices, options)
}

fn decimate_indices(
    positions: &[DVec3],
    indices: &[u32],
    options: &DecimateOptions,
) -> Option<Vec<u32>> {
    let triangle_count = indices.len() / 3;
    if !indices.len().is_multiple_of(3)
        || triangle_count < 2
        || triangle_count > options.max_triangles
        || !(options.target_ratio > 0.0 && options.target_ratio <= 1.0)
        || !(options.tolerance > 0.0 && options.tolerance.is_finite())
        || indices
            .iter()
            .any(|&index| index as usize >= positions.len())
        || positions.iter().any(|p| !p.is_finite())
    {
        return None;
    }
    let vertex_count = positions.len();
    let triangles: Vec<[u32; 3]> = indices
        .chunks_exact(3)
        .map(|corners| [corners[0], corners[1], corners[2]])
        .collect();

    // Boundary and non-manifold edges lock their vertices in place.
    let mut edge_counts: HashMap<(u32, u32), u32> = HashMap::with_capacity(indices.len());
    for corners in &triangles {
        for (u, v) in [
            (corners[0], corners[1]),
            (corners[1], corners[2]),
            (corners[2], corners[0]),
        ] {
            let key = if u < v { (u, v) } else { (v, u) };
            *edge_counts.entry(key).or_insert(0) += 1;
        }
    }
    let mut locked = vec![false; vertex_count];
    for (&(u, v), &count) in &edge_counts {
        if count != 2 {
            locked[u as usize] = true;
            locked[v as usize] = true;
        }
    }
    let mut around: Vec<Vec<u32>> = vec![Vec::new(); vertex_count];
    let mut quadrics = vec![Quadric::default(); vertex_count];
    for (index, corners) in triangles.iter().enumerate() {
        let a = positions[corners[0] as usize];
        let normal = (positions[corners[1] as usize] - a).cross(positions[corners[2] as usize] - a);
        if normal.length_squared() > MIN_PLANE_AREA {
            let unit = normal.normalize();
            let plane = Quadric::from_plane(unit, -unit.dot(a));
            for &corner in corners {
                quadrics[corner as usize].add(&plane);
            }
        }
        for &corner in corners {
            around[corner as usize].push(index as u32);
        }
    }
    let mut state = Decimation {
        positions,
        triangles,
        alive: vec![true; triangle_count],
        around,
        quadrics,
        locked,
        dead: vec![false; vertex_count],
        version: vec![0; vertex_count],
        live_triangles: triangle_count,
    };

    let tolerance_squared = options.tolerance * options.tolerance;
    let target = ((triangle_count as f64) * options.target_ratio)
        .ceil()
        .max(2.0) as usize;
    let mut heap = BinaryHeap::with_capacity(edge_counts.len() * 2);
    let mut edges: Vec<(u32, u32)> = edge_counts.keys().copied().collect();
    edges.sort_unstable();
    for (u, v) in edges {
        for (removed, kept) in [(u, v), (v, u)] {
            if state.locked[removed as usize] {
                continue;
            }
            let cost = state.cost(removed, kept);
            if cost <= tolerance_squared {
                heap.push(Candidate {
                    cost,
                    removed,
                    kept,
                    version_removed: 0,
                    version_kept: 0,
                });
            }
        }
    }

    let mut collapses = 0usize;
    while let Some(candidate) = heap.pop() {
        if state.live_triangles <= target || collapses >= MAX_COLLAPSES {
            break;
        }
        let removed = candidate.removed as usize;
        let kept = candidate.kept as usize;
        if state.dead[removed]
            || state.dead[kept]
            || state.version[removed] != candidate.version_removed
            || state.version[kept] != candidate.version_kept
        {
            continue;
        }
        if !state.valid(candidate.removed, candidate.kept) {
            continue;
        }
        state.collapse(candidate.removed, candidate.kept);
        collapses += 1;
        state.push_edges_around(candidate.kept, &mut heap, tolerance_squared);
    }

    let removed = triangle_count - state.live_triangles;
    if (removed as f64) < (triangle_count as f64) * MIN_REMOVED_RATIO {
        return None;
    }
    let mut out = Vec::with_capacity(state.live_triangles * 3);
    for (index, corners) in state.triangles.iter().enumerate() {
        if state.alive[index] {
            out.extend_from_slice(corners);
        }
    }
    Some(optimize_index_locality(&out, vertex_count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh(positions: Vec<[f64; 3]>, indices: Vec<u32>) -> Mesh64 {
        Mesh64 {
            positions: positions
                .into_iter()
                .map(|p| DVec3::new(p[0], p[1], p[2]))
                .collect(),
            indices,
            closed: None,
            uvs: Vec::new(),
        }
    }

    /// A square fanned around its centre: four triangles, the centre interior.
    fn fanned_square() -> Mesh64 {
        mesh(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.5, 0.5, 0.0],
            ],
            vec![0, 1, 4, 1, 2, 4, 2, 3, 4, 3, 0, 4],
        )
    }

    fn cube() -> Mesh64 {
        mesh(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0, 1.0],
                [1.0, 1.0, 1.0],
                [0.0, 1.0, 1.0],
            ],
            vec![
                0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 2, 3, 7, 2, 7, 6, 0, 4, 7, 0,
                7, 3, 1, 2, 6, 1, 6, 5,
            ],
        )
    }

    /// A closed cylinder of `segments` sides with capped ends fanned around centres.
    fn cylinder(segments: u32) -> Mesh64 {
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        for ring in 0..2 {
            for s in 0..segments {
                let angle = s as f64 / segments as f64 * std::f64::consts::TAU;
                positions.push([angle.cos(), angle.sin(), ring as f64 * 2.0]);
            }
        }
        for s in 0..segments {
            let next = (s + 1) % segments;
            indices.extend_from_slice(&[
                s,
                next,
                segments + next,
                s,
                segments + next,
                segments + s,
            ]);
        }
        let bottom = positions.len() as u32;
        positions.push([0.0, 0.0, 0.0]);
        let top = positions.len() as u32;
        positions.push([0.0, 0.0, 2.0]);
        for s in 0..segments {
            let next = (s + 1) % segments;
            indices.extend_from_slice(&[bottom, next, s, top, segments + s, segments + next]);
        }
        mesh(positions, indices)
    }

    /// An open grid of `n` by `n` quads in the plane, as triangles.
    fn grid(n: u32) -> Mesh64 {
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        for y in 0..=n {
            for x in 0..=n {
                positions.push([x as f64, y as f64, 0.0]);
            }
        }
        let at = |x: u32, y: u32| y * (n + 1) + x;
        for y in 0..n {
            for x in 0..n {
                indices.extend_from_slice(&[
                    at(x, y),
                    at(x + 1, y),
                    at(x + 1, y + 1),
                    at(x, y),
                    at(x + 1, y + 1),
                    at(x, y + 1),
                ]);
            }
        }
        mesh(positions, indices)
    }

    fn distance_to_triangle(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> f64 {
        // Closest point on a triangle, by regions of the barycentric plane.
        let ab = b - a;
        let ac = c - a;
        let ap = p - a;
        let d1 = ab.dot(ap);
        let d2 = ac.dot(ap);
        if d1 <= 0.0 && d2 <= 0.0 {
            return ap.length();
        }
        let bp = p - b;
        let d3 = ab.dot(bp);
        let d4 = ac.dot(bp);
        if d3 >= 0.0 && d4 <= d3 {
            return bp.length();
        }
        let vc = d1 * d4 - d3 * d2;
        if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
            let v = d1 / (d1 - d3);
            return (p - (a + ab * v)).length();
        }
        let cp = p - c;
        let d5 = ab.dot(cp);
        let d6 = ac.dot(cp);
        if d6 >= 0.0 && d5 <= d6 {
            return cp.length();
        }
        let vb = d5 * d2 - d1 * d6;
        if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
            let w = d2 / (d2 - d6);
            return (p - (a + ac * w)).length();
        }
        let va = d3 * d6 - d5 * d4;
        if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
            let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
            return (p - (b + (c - b) * w)).length();
        }
        let denominator = 1.0 / (va + vb + vc);
        let v = vb * denominator;
        let w = vc * denominator;
        (p - (a + ab * v + ac * w)).length()
    }

    fn distance_to_mesh(p: DVec3, positions: &[DVec3], indices: &[u32]) -> f64 {
        indices
            .chunks_exact(3)
            .map(|t| {
                distance_to_triangle(
                    p,
                    positions[t[0] as usize],
                    positions[t[1] as usize],
                    positions[t[2] as usize],
                )
            })
            .fold(f64::INFINITY, f64::min)
    }

    fn triangle_normals_agree(positions: &[DVec3], before: &[u32], after: &[u32]) -> bool {
        let reference = {
            let t = &before[..3];
            (positions[t[1] as usize] - positions[t[0] as usize])
                .cross(positions[t[2] as usize] - positions[t[0] as usize])
        };
        after.chunks_exact(3).all(|t| {
            let n = (positions[t[1] as usize] - positions[t[0] as usize])
                .cross(positions[t[2] as usize] - positions[t[0] as usize]);
            n.length_squared() > 0.0 && n.dot(reference) > 0.0
        })
    }

    #[test]
    fn a_flat_fan_collapses_to_two_triangles_at_no_error() {
        let square = fanned_square();
        let options = DecimateOptions {
            target_ratio: 0.1,
            tolerance: 1e-9,
            ..Default::default()
        };
        let out = decimate(&square, &options).expect("the centre vertex can go");
        assert_eq!(out.len(), 6, "two triangles remain");
        assert!(!out.contains(&4), "the interior vertex is the one dropped");
        assert!(
            triangle_normals_agree(&square.positions, &square.indices, &out),
            "no triangle flipped"
        );
    }

    #[test]
    fn a_cube_has_nothing_to_remove_within_a_small_tolerance() {
        let options = DecimateOptions {
            tolerance: 0.01,
            ..Default::default()
        };
        assert_eq!(decimate(&cube(), &options), None);
    }

    #[test]
    fn a_cylinder_reduces_within_the_tolerance_and_stays_closed() {
        let fine = cylinder(128);
        let options = DecimateOptions {
            target_ratio: 0.25,
            tolerance: 0.02,
            ..Default::default()
        };
        let out = decimate(&fine, &options).expect("a fine cylinder simplifies");
        assert!(
            out.len() < fine.indices.len() * 3 / 4,
            "at least a quarter of the triangles went ({} of {})",
            out.len() / 3,
            fine.indices.len() / 3
        );
        assert!(
            out.iter()
                .all(|&index| (index as usize) < fine.positions.len()),
            "every output vertex is an input vertex"
        );
        let coarse = Mesh64 {
            positions: fine.positions.clone(),
            indices: out.clone(),
            closed: None,
            uvs: Vec::new(),
        };
        assert!(coarse.is_edge_manifold(), "a closed manifold stays closed");
        let worst = fine
            .positions
            .iter()
            .map(|&p| distance_to_mesh(p, &fine.positions, &out))
            .fold(0.0, f64::max);
        assert!(
            worst <= options.tolerance * 2.0,
            "every original vertex stays near the output ({worst})"
        );
        assert!(
            coarse.signed_volume() > 0.0
                && (coarse.signed_volume() - fine.signed_volume()).abs()
                    < fine.signed_volume() * 0.05,
            "the volume is kept"
        );
    }

    #[test]
    fn an_open_grid_keeps_its_rim() {
        let fine = grid(24);
        let options = DecimateOptions {
            target_ratio: 0.1,
            tolerance: 1e-6,
            ..Default::default()
        };
        let out = decimate(&fine, &options).expect("a flat grid simplifies freely");
        let n = 24u32;
        let rim: Vec<u32> = (0..=n)
            .flat_map(|i| [i, i * (n + 1), n * (n + 1) + i, i * (n + 1) + n])
            .collect();
        for vertex in rim {
            assert!(out.contains(&vertex), "rim vertex {vertex} survives");
        }
        assert!(
            triangle_normals_agree(&fine.positions, &fine.indices, &out),
            "no flips on a plane"
        );
    }

    #[test]
    fn the_result_is_deterministic_and_the_f32_path_agrees() {
        let fine = cylinder(64);
        let options = DecimateOptions {
            target_ratio: 0.3,
            tolerance: 0.05,
            ..Default::default()
        };
        let first = decimate(&fine, &options).unwrap();
        let second = decimate(&fine, &options).unwrap();
        assert_eq!(first, second);
        let flat: Vec<f32> = fine
            .positions
            .iter()
            .flat_map(|p| [p.x as f32, p.y as f32, p.z as f32])
            .collect();
        let narrow = decimate_f32(&flat, &fine.indices, &options).unwrap();
        assert_eq!(
            narrow.len(),
            first.len(),
            "the f32 path removes the same number of triangles"
        );
    }

    #[test]
    fn bad_input_is_refused() {
        let options = DecimateOptions::default();
        assert_eq!(
            decimate(&mesh(vec![[0.0; 3]; 3], vec![0, 1, 7, 0, 1, 2]), &options),
            None,
            "an index past the end"
        );
        assert_eq!(
            decimate(&mesh(vec![[0.0; 3]; 3], vec![0, 1, 2, 0]), &options),
            None,
            "a partial triangle"
        );
        assert_eq!(
            decimate(
                &fanned_square(),
                &DecimateOptions {
                    tolerance: 0.0,
                    ..options.clone()
                }
            ),
            None,
            "a zero tolerance"
        );
        assert_eq!(
            decimate(
                &fanned_square(),
                &DecimateOptions {
                    max_triangles: 2,
                    ..options.clone()
                }
            ),
            None,
            "over the size limit"
        );
        assert_eq!(
            decimate(
                &mesh(vec![[f64::NAN; 3]; 3], vec![0, 1, 2, 0, 2, 1]),
                &options
            ),
            None,
            "a non-finite position"
        );
        assert_eq!(
            decimate_f32(&[0.0; 4], &[0, 0, 0], &options),
            None,
            "positions not in threes"
        );
    }
}
