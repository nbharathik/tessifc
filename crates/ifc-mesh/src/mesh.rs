// SPDX-License-Identifier: Apache-2.0
//! The mesh every evaluator produces and every consumer reads.
//! Positions are `f64` because survey-grade IFC coordinates lose about a
//! metre in `f32`; the `f32` conversion happens once, in `tessifc-pack`.

use glam::{DMat4, DVec3};

/// A triangle mesh in `f64`, in the local space of its producer.
/// The engine applies the placement afterwards, so identical items share one mesh.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh64 {
    /// Vertex positions, three components each.
    pub positions: Vec<DVec3>,
    /// Triangle indices, three per face. An index past the end of `positions`
    /// makes its triangle be dropped, never a panic.
    pub indices: Vec<u32>,
    /// Whether the producer believes this is a closed solid; `None` when nobody checked.
    pub closed: Option<bool>,
}

impl Mesh64 {
    /// An empty mesh.
    pub fn new() -> Self {
        Mesh64::default()
    }

    /// An empty mesh with room reserved.
    pub fn with_capacity(vertices: usize, indices: usize) -> Self {
        Mesh64 {
            positions: Vec::with_capacity(vertices),
            indices: Vec::with_capacity(indices),
            closed: None,
        }
    }

    /// Number of triangles.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// True when there is nothing to draw.
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty() || self.positions.is_empty()
    }

    /// Add a vertex, returning its index.
    pub fn push_vertex(&mut self, position: DVec3) -> u32 {
        self.positions.push(position);
        (self.positions.len() - 1) as u32
    }

    /// Add a triangle by vertex index.
    pub fn push_triangle(&mut self, a: u32, b: u32, c: u32) {
        self.indices.push(a);
        self.indices.push(b);
        self.indices.push(c);
    }

    /// Append another mesh, offsetting its indices.
    pub fn append(&mut self, other: &Mesh64) {
        let offset = self.positions.len() as u32;
        self.positions.extend_from_slice(&other.positions);
        self.indices
            .extend(other.indices.iter().map(|i| i + offset));
        self.closed = match (self.closed, other.closed) {
            (Some(a), Some(b)) => Some(a && b),
            _ => None,
        };
    }

    /// Apply a transform in place.
    pub fn transform(&mut self, matrix: &DMat4) {
        if *matrix == DMat4::IDENTITY {
            return;
        }
        for position in &mut self.positions {
            *position = matrix.transform_point3(*position);
        }
        // A mirroring transform reverses winding, so flip or every normal points inward.
        if matrix.determinant() < 0.0 {
            self.flip_winding();
        }
    }

    /// Reverse every triangle, which flips every normal.
    pub fn flip_winding(&mut self) {
        for triangle in self.indices.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
    }

    /// Axis-aligned bounding box, or `None` for an empty mesh.
    pub fn bounds(&self) -> Option<(DVec3, DVec3)> {
        let mut iter = self.positions.iter();
        let first = *iter.next()?;
        let mut lo = first;
        let mut hi = first;
        for position in iter {
            lo = lo.min(*position);
            hi = hi.max(*position);
        }
        Some((lo, hi))
    }

    /// Signed volume by the divergence theorem, in cubic units; meaningless unless closed.
    /// Recentred on the bounding box first, or a georeferenced model cancels catastrophically.
    pub fn signed_volume(&self) -> f64 {
        let Some((lo, hi)) = self.bounds() else {
            return 0.0;
        };
        let centre = (lo + hi) * 0.5;
        let mut total = 0.0;
        for triangle in self.indices.chunks_exact(3) {
            let (Some(&a), Some(&b), Some(&c)) = (
                self.positions.get(triangle[0] as usize),
                self.positions.get(triangle[1] as usize),
                self.positions.get(triangle[2] as usize),
            ) else {
                continue;
            };
            total += (a - centre).dot((b - centre).cross(c - centre));
        }
        total / 6.0
    }

    /// Total triangle area, in square units.
    pub fn surface_area(&self) -> f64 {
        let mut total = 0.0;
        for triangle in self.indices.chunks_exact(3) {
            let (Some(&a), Some(&b), Some(&c)) = (
                self.positions.get(triangle[0] as usize),
                self.positions.get(triangle[1] as usize),
                self.positions.get(triangle[2] as usize),
            ) else {
                continue;
            };
            total += (b - a).cross(c - a).length() * 0.5;
        }
        total
    }

    /// The triangles grouped by shared vertices, each group as its own mesh.
    ///
    /// Weld first: two shells that only touch by position stay apart here.
    /// A mesh in one piece comes back as a single clone of itself.
    pub fn connected_components(&self) -> Vec<Mesh64> {
        let count = self.positions.len();
        let mut parent: Vec<u32> = (0..count as u32).collect();
        fn root(parent: &mut [u32], mut index: u32) -> u32 {
            while parent[index as usize] != index {
                let next = parent[index as usize];
                parent[index as usize] = parent[next as usize];
                index = next;
            }
            index
        }
        for triangle in self.indices.chunks_exact(3) {
            for k in 1..3 {
                let (Some(&a), Some(&b)) = (triangle.first(), triangle.get(k)) else {
                    continue;
                };
                if (a as usize) < count && (b as usize) < count {
                    let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
                    if ra != rb {
                        parent[ra as usize] = rb;
                    }
                }
            }
        }
        let mut slot: std::collections::HashMap<u32, usize> = Default::default();
        let mut out: Vec<Mesh64> = Vec::new();
        let mut remap: Vec<u32> = vec![u32::MAX; count];
        for triangle in self.indices.chunks_exact(3) {
            let (Some(&a), Some(&b), Some(&c)) =
                (triangle.first(), triangle.get(1), triangle.get(2))
            else {
                continue;
            };
            if [a, b, c].iter().any(|&i| i as usize >= count) {
                continue;
            }
            let group = root(&mut parent, a);
            let which = *slot.entry(group).or_insert_with(|| {
                out.push(Mesh64::new());
                out.len() - 1
            });
            let mesh = &mut out[which];
            let mut local = [0u32; 3];
            for (index, &vertex) in [a, b, c].iter().enumerate() {
                if remap[vertex as usize] == u32::MAX {
                    remap[vertex as usize] = mesh.push_vertex(self.positions[vertex as usize]);
                }
                local[index] = remap[vertex as usize];
            }
            mesh.push_triangle(local[0], local[1], local[2]);
        }
        out
    }

    /// Does every shared edge run the opposite way in its two triangles?
    ///
    /// False when neighbouring faces disagree about which side is out, which
    /// a closed shell can do without failing the edge count.
    pub fn is_consistently_wound(&self) -> bool {
        let mut seen: std::collections::HashSet<(u32, u32)> = Default::default();
        for triangle in self.indices.chunks_exact(3) {
            for k in 0..3 {
                if !seen.insert((triangle[k], triangle[(k + 1) % 3])) {
                    return false;
                }
            }
        }
        true
    }

    /// Edges used once, and edges used more than twice: what keeps a shell from
    /// being closed. Both zero for a closed, consistently wound surface.
    pub fn edge_defects(&self) -> (usize, usize) {
        let mut uses: std::collections::HashMap<(u32, u32), usize> = Default::default();
        for triangle in self.indices.chunks_exact(3) {
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

    /// Is every edge shared by exactly two triangles? The cheap watertightness test.
    pub fn is_edge_manifold(&self) -> bool {
        use std::collections::HashMap;
        let mut counts: HashMap<(u32, u32), u32> = HashMap::new();
        for triangle in self.indices.chunks_exact(3) {
            for (u, v) in [
                (triangle[0], triangle[1]),
                (triangle[1], triangle[2]),
                (triangle[2], triangle[0]),
            ] {
                let key = if u < v { (u, v) } else { (v, u) };
                *counts.entry(key).or_insert(0) += 1;
            }
        }
        !counts.is_empty() && counts.values().all(|&count| count == 2)
    }

    /// Flip the winding when the signed volume is negative; returns true if it was reversed.
    pub fn fix_orientation(&mut self) -> bool {
        if self.signed_volume() < 0.0 {
            self.flip_winding();
            true
        } else {
            false
        }
    }

    /// Drop triangles with two identical corners or zero area; returns how many were removed.
    pub fn remove_degenerate_triangles(&mut self, epsilon: f64) -> usize {
        let before = self.triangle_count();
        let mut kept = Vec::with_capacity(self.indices.len());
        for triangle in self.indices.chunks_exact(3) {
            let (i, j, k) = (triangle[0], triangle[1], triangle[2]);
            if i == j || j == k || i == k {
                continue;
            }
            let (Some(&a), Some(&b), Some(&c)) = (
                self.positions.get(i as usize),
                self.positions.get(j as usize),
                self.positions.get(k as usize),
            ) else {
                continue;
            };
            if (b - a).cross(c - a).length() * 0.5 <= epsilon {
                continue;
            }
            kept.extend_from_slice(triangle);
        }
        self.indices = kept;
        before - self.triangle_count()
    }

    /// Remove vertices no triangle refers to, renumbering the rest.
    pub fn drop_unused_vertices(&mut self) {
        let mut remap = vec![u32::MAX; self.positions.len()];
        let mut kept = Vec::new();
        for index in &mut self.indices {
            let old = *index as usize;
            if old >= remap.len() {
                continue;
            }
            if remap[old] == u32::MAX {
                remap[old] = kept.len() as u32;
                kept.push(self.positions[old]);
            }
            *index = remap[old];
        }
        self.positions = kept;
    }
}

/// Flat per-triangle normals, in the mesh's own space; smoothing is applied later.
pub fn face_normals(mesh: &Mesh64) -> Vec<DVec3> {
    let mut normals = Vec::with_capacity(mesh.triangle_count());
    for triangle in mesh.indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&c)) = (
            mesh.positions.get(triangle[0] as usize),
            mesh.positions.get(triangle[1] as usize),
            mesh.positions.get(triangle[2] as usize),
        ) else {
            normals.push(DVec3::Z);
            continue;
        };
        let normal = (b - a).cross(c - a);
        let length = normal.length();
        normals.push(if length > 0.0 {
            normal / length
        } else {
            DVec3::Z
        });
    }
    normals
}

/// The Newell normal of a polygon, defined even for non-planar or collinear-leading input.
pub fn newell_normal(points: &[DVec3]) -> DVec3 {
    let mut normal = DVec3::ZERO;
    for index in 0..points.len() {
        let current = points[index];
        let next = points[(index + 1) % points.len()];
        normal.x += (current.y - next.y) * (current.z + next.z);
        normal.y += (current.z - next.z) * (current.x + next.x);
        normal.z += (current.x - next.x) * (current.y + next.y);
    }
    let length = normal.length();
    if length > 0.0 {
        normal / length
    } else {
        DVec3::Z
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_separate_boxes_are_two_components() {
        let mut mesh = Mesh64::new();
        for offset in [0.0, 5.0] {
            let base = mesh.positions.len() as u32;
            for corner in 0..8u32 {
                mesh.push_vertex(DVec3::new(
                    (corner & 1) as f64 + offset,
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
                mesh.push_triangle(base + quad[0], base + quad[1], base + quad[2]);
                mesh.push_triangle(base + quad[0], base + quad[2], base + quad[3]);
            }
        }
        let parts = mesh.connected_components();
        assert_eq!(parts.len(), 2);
        for part in &parts {
            assert_eq!(part.triangle_count(), 12);
            assert_eq!(part.positions.len(), 8);
            assert!(part.is_edge_manifold());
        }
    }

    /// A unit cube with outward-facing triangles.
    pub(crate) fn unit_cube() -> Mesh64 {
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
            [0, 3, 2], // bottom, normal down
            [4, 5, 6],
            [4, 6, 7], // top
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
        mesh.positions.extend_from_slice(&corners);
        for face in faces {
            mesh.push_triangle(face[0], face[1], face[2]);
        }
        mesh.closed = Some(true);
        mesh
    }

    #[test]
    fn cube_measurements() {
        let cube = unit_cube();
        assert_eq!(cube.triangle_count(), 12);
        assert!((cube.signed_volume() - 1.0).abs() < 1e-12);
        assert!((cube.surface_area() - 6.0).abs() < 1e-12);
        assert!(cube.is_edge_manifold());
        let (lo, hi) = cube.bounds().unwrap();
        assert_eq!(lo, DVec3::ZERO);
        assert_eq!(hi, DVec3::ONE);
    }

    #[test]
    fn volume_survives_georeferenced_coordinates() {
        // Integrating from the world origin loses precision at a UTM easting.
        let mut cube = unit_cube();
        cube.transform(&DMat4::from_translation(DVec3::new(
            420_000.0,
            5_900_000.0,
            12.0,
        )));
        assert!(
            (cube.signed_volume() - 1.0).abs() < 1e-9,
            "volume was {}",
            cube.signed_volume()
        );
    }

    #[test]
    fn flipping_reverses_the_sign() {
        let mut cube = unit_cube();
        cube.flip_winding();
        assert!(cube.signed_volume() < 0.0);
        assert!(cube.fix_orientation());
        assert!((cube.signed_volume() - 1.0).abs() < 1e-12);
        // Already right way out, so nothing to do the second time.
        assert!(!cube.fix_orientation());
    }

    #[test]
    fn a_mirroring_transform_keeps_normals_outward() {
        let mut cube = unit_cube();
        cube.transform(&DMat4::from_scale(DVec3::new(-1.0, 1.0, 1.0)));
        assert!(
            cube.signed_volume() > 0.0,
            "a mirrored cube must still be right way out, got {}",
            cube.signed_volume()
        );
    }

    #[test]
    fn appending_offsets_indices() {
        let mut left = unit_cube();
        let mut right = unit_cube();
        right.transform(&DMat4::from_translation(DVec3::new(2.0, 0.0, 0.0)));
        left.append(&right);
        assert_eq!(left.triangle_count(), 24);
        assert_eq!(left.positions.len(), 16);
        assert!((left.signed_volume() - 2.0).abs() < 1e-12);
    }

    #[test]
    fn degenerate_triangles_are_dropped() {
        let mut mesh = Mesh64::new();
        mesh.positions.push(DVec3::ZERO);
        mesh.positions.push(DVec3::X);
        mesh.positions.push(DVec3::X * 2.0);
        mesh.positions.push(DVec3::Y);
        mesh.push_triangle(0, 1, 2); // collinear, zero area
        mesh.push_triangle(0, 0, 1); // repeated corner
        mesh.push_triangle(0, 1, 3); // real
        assert_eq!(mesh.remove_degenerate_triangles(1e-12), 2);
        assert_eq!(mesh.triangle_count(), 1);
    }

    #[test]
    fn newell_handles_collinear_leading_vertices() {
        // The first three points are collinear, so a naive cross product would be zero.
        let square = [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(1.0, 0.0, 0.0),
            DVec3::new(2.0, 0.0, 0.0),
            DVec3::new(2.0, 2.0, 0.0),
            DVec3::new(0.0, 2.0, 0.0),
        ];
        let normal = newell_normal(&square);
        assert!((normal - DVec3::Z).length() < 1e-12, "got {normal}");
    }

    #[test]
    fn unused_vertices_are_dropped() {
        let mut mesh = unit_cube();
        mesh.positions.push(DVec3::new(9.0, 9.0, 9.0));
        assert_eq!(mesh.positions.len(), 9);
        mesh.drop_unused_vertices();
        assert_eq!(mesh.positions.len(), 8);
        assert!((mesh.signed_volume() - 1.0).abs() < 1e-12);
    }
}
