// SPDX-License-Identifier: Apache-2.0
//! Turning polygons into triangles: 2D profiles with holes for swept solids,
//! and 3D faces projected onto their own plane for B-reps. Both go through
//! `earcutr` ear clipping; where it fails, the caller gets an error.

use crate::mesh::newell_normal;
use glam::{DVec2, DVec3};

/// A polygon in the plane: one outer loop, any number of inner loops, any winding.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Polygon2 {
    /// The outer boundary.
    pub outer: Vec<DVec2>,
    /// Holes. Each is a closed loop inside `outer`.
    pub holes: Vec<Vec<DVec2>>,
}

impl Polygon2 {
    /// A polygon with no holes.
    pub fn new(outer: Vec<DVec2>) -> Self {
        Polygon2 {
            outer,
            holes: Vec::new(),
        }
    }

    /// Signed area of the outer loop; positive when it is counter-clockwise.
    pub fn signed_area(&self) -> f64 {
        signed_area(&self.outer)
    }

    /// Total vertex count across every loop.
    pub fn vertex_count(&self) -> usize {
        self.outer.len() + self.holes.iter().map(Vec::len).sum::<usize>()
    }

    /// True when there is not enough here to make a single triangle.
    pub fn is_degenerate(&self) -> bool {
        self.outer.len() < 3
    }
}

/// Signed area of a closed loop. Positive is counter-clockwise.
pub fn signed_area(points: &[DVec2]) -> f64 {
    let mut total = 0.0;
    for index in 0..points.len() {
        let current = points[index];
        let next = points[(index + 1) % points.len()];
        total += current.x * next.y - next.x * current.y;
    }
    total * 0.5
}

/// What went wrong.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TriangulateError {
    /// Fewer than three points in the outer loop.
    #[error("the outer loop has {0} points, which cannot make a triangle")]
    TooFewPoints(usize),
    /// The outer loop encloses no area.
    #[error("the outer loop has no area")]
    ZeroArea,
    /// Ear clipping produced nothing usable.
    #[error("triangulation produced no triangles from {0} points")]
    Failed(usize),
}

/// Upper bound on the vertices one polygon may carry into ear clipping.
const MAX_POLYGON_VERTICES: usize = 100_000;

/// Triangulate a 2D polygon with holes; indices refer to `outer` then each hole in order.
pub fn triangulate_polygon(polygon: &Polygon2) -> Result<Vec<u32>, TriangulateError> {
    if polygon.is_degenerate() {
        return Err(TriangulateError::TooFewPoints(polygon.outer.len()));
    }
    let area = polygon.signed_area();
    if !area.is_finite() || area.abs() <= f64::EPSILON {
        return Err(TriangulateError::ZeroArea);
    }
    // Ear clipping is quadratic in the worst case and the count comes from the file.
    if polygon.vertex_count() > MAX_POLYGON_VERTICES {
        return Err(TriangulateError::Failed(polygon.vertex_count()));
    }

    let mut flat = Vec::with_capacity(polygon.vertex_count() * 2);
    for point in &polygon.outer {
        flat.push(point.x);
        flat.push(point.y);
    }
    let mut hole_starts = Vec::with_capacity(polygon.holes.len());
    for hole in &polygon.holes {
        if hole.len() < 3 {
            // A hole with two points is not a hole; skip it rather than refuse the profile.
            continue;
        }
        hole_starts.push(flat.len() / 2);
        for point in hole {
            flat.push(point.x);
            flat.push(point.y);
        }
    }

    let indices = earcutr::earcut(&flat, &hole_starts, 2)
        .map_err(|_| TriangulateError::Failed(polygon.vertex_count()))?;
    if indices.is_empty() {
        return Err(TriangulateError::Failed(polygon.vertex_count()));
    }
    Ok(indices.into_iter().map(|index| index as u32).collect())
}

/// True when the triangles do not tile the polygon: the outline crosses itself,
/// or the holes are not nested inside it. The result is then a guess.
///
/// Linear in the vertex count, so it is affordable on any profile a file can
/// declare. Call it on the output of [`triangulate_polygon`].
pub fn triangulation_deviates(polygon: &Polygon2, indices: &[u32]) -> bool {
    let mut points = polygon.outer.clone();
    let mut expected = signed_area(&polygon.outer).abs();
    for hole in &polygon.holes {
        // Short holes are skipped by the triangulator, so they are not indexed.
        if hole.len() < 3 {
            continue;
        }
        expected -= signed_area(hole).abs();
        points.extend_from_slice(hole);
    }
    // A hole larger than the outline is not nested; nothing to compare against.
    if expected <= 0.0 || expected.is_nan() {
        return false;
    }
    let mut covered = 0.0;
    for triangle in indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&c)) = (
            points.get(triangle[0] as usize),
            points.get(triangle[1] as usize),
            points.get(triangle[2] as usize),
        ) else {
            continue;
        };
        covered += ((b - a).perp_dot(c - a) * 0.5).abs();
    }
    (covered - expected).abs() > expected * 1e-6
}

/// An orthonormal basis for the plane a set of points lies in.
#[derive(Clone, Copy, Debug)]
pub struct PlaneBasis {
    /// A point on the plane.
    pub origin: DVec3,
    /// Unit normal.
    pub normal: DVec3,
    /// First in-plane axis.
    pub u: DVec3,
    /// Second in-plane axis, `normal.cross(u)`.
    pub v: DVec3,
}

impl PlaneBasis {
    /// Build a basis from a face's points using the Newell normal; `None` if collinear or too few.
    pub fn from_points(points: &[DVec3]) -> Option<PlaneBasis> {
        if points.len() < 3 {
            return None;
        }
        let normal = newell_normal(points);
        if normal.length_squared() < 0.5 {
            // newell_normal returns a unit vector or falls back to Z; shorter means degenerate.
            return None;
        }
        Some(PlaneBasis::from_normal(normal, points[0]))
    }

    /// A basis for a plane whose normal is known; `u.cross(v) == normal`, so a
    /// counter-clockwise projected loop has positive [`signed_area`].
    pub fn from_normal(normal: DVec3, origin: DVec3) -> PlaneBasis {
        let normal = normal.normalize_or(DVec3::Z);
        // The smallest component of the normal gives an axis not parallel to it.
        let helper = if normal.x.abs() < normal.y.abs() && normal.x.abs() < normal.z.abs() {
            DVec3::X
        } else if normal.y.abs() < normal.z.abs() {
            DVec3::Y
        } else {
            DVec3::Z
        };
        let u = normal.cross(helper).normalize();
        let v = normal.cross(u);
        PlaneBasis {
            origin,
            normal,
            u,
            v,
        }
    }

    /// Project a world point into plane coordinates.
    pub fn project(&self, point: DVec3) -> DVec2 {
        let offset = point - self.origin;
        DVec2::new(offset.dot(self.u), offset.dot(self.v))
    }

    /// Lift a plane coordinate back into world space.
    pub fn unproject(&self, point: DVec2) -> DVec3 {
        self.origin + self.u * point.x + self.v * point.y
    }
}

/// Triangulate a 3D face on its own plane; indices as in [`triangulate_polygon`].
/// Triangles are wound to match the face's Newell normal.
pub fn triangulate_face(
    outer: &[DVec3],
    holes: &[Vec<DVec3>],
) -> Result<Vec<u32>, TriangulateError> {
    if outer.len() < 3 {
        return Err(TriangulateError::TooFewPoints(outer.len()));
    }
    // A lone triangle needs no projection, and it is the common case.
    if outer.len() == 3 && holes.is_empty() {
        return Ok(vec![0, 1, 2]);
    }

    let basis = PlaneBasis::from_points(outer).ok_or(TriangulateError::ZeroArea)?;
    let polygon = Polygon2 {
        outer: outer.iter().map(|&p| basis.project(p)).collect(),
        holes: holes
            .iter()
            .map(|hole| hole.iter().map(|&p| basis.project(p)).collect())
            .collect(),
    };
    let mut indices = triangulate_polygon(&polygon)?;

    // Projection can mirror the polygon; then the triangles come back wound the wrong way.
    if polygon.signed_area() < 0.0 {
        for triangle in indices.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
    }
    Ok(indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Vec<DVec2> {
        vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(2.0, 0.0),
            DVec2::new(2.0, 2.0),
            DVec2::new(0.0, 2.0),
        ]
    }

    fn triangulated_area(polygon: &Polygon2, indices: &[u32]) -> f64 {
        let mut points = polygon.outer.clone();
        for hole in &polygon.holes {
            points.extend_from_slice(hole);
        }
        let mut area = 0.0;
        for triangle in indices.chunks_exact(3) {
            let a = points[triangle[0] as usize];
            let b = points[triangle[1] as usize];
            let c = points[triangle[2] as usize];
            area += ((b - a).perp_dot(c - a) * 0.5).abs();
        }
        area
    }

    #[test]
    fn a_square_becomes_two_triangles() {
        let polygon = Polygon2::new(square());
        let indices = triangulate_polygon(&polygon).unwrap();
        assert_eq!(indices.len(), 6);
        assert!((triangulated_area(&polygon, &indices) - 4.0).abs() < 1e-12);
    }

    #[test]
    fn the_triangles_cover_exactly_the_polygon_area() {
        // Whatever ear clipping does, the triangles must tile the polygon exactly.
        let polygon = Polygon2 {
            outer: square(),
            holes: vec![vec![
                DVec2::new(0.5, 0.5),
                DVec2::new(0.5, 1.5),
                DVec2::new(1.5, 1.5),
                DVec2::new(1.5, 0.5),
            ]],
        };
        let indices = triangulate_polygon(&polygon).unwrap();
        let expected = 4.0 - 1.0;
        assert!(
            (triangulated_area(&polygon, &indices) - expected).abs() < 1e-9,
            "got {}",
            triangulated_area(&polygon, &indices)
        );
    }

    #[test]
    fn winding_does_not_matter() {
        let mut reversed = square();
        reversed.reverse();
        let polygon = Polygon2::new(reversed);
        let indices = triangulate_polygon(&polygon).unwrap();
        assert!((triangulated_area(&polygon, &indices) - 4.0).abs() < 1e-12);
    }

    #[test]
    fn a_concave_polygon_is_handled() {
        // An L shape: the naive fan triangulation gets this wrong.
        let polygon = Polygon2::new(vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(2.0, 0.0),
            DVec2::new(2.0, 1.0),
            DVec2::new(1.0, 1.0),
            DVec2::new(1.0, 2.0),
            DVec2::new(0.0, 2.0),
        ]);
        let indices = triangulate_polygon(&polygon).unwrap();
        assert!((triangulated_area(&polygon, &indices) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn degenerate_input_is_refused_not_guessed() {
        assert_eq!(
            triangulate_polygon(&Polygon2::new(vec![DVec2::ZERO, DVec2::X])),
            Err(TriangulateError::TooFewPoints(2))
        );
        let collinear = Polygon2::new(vec![DVec2::ZERO, DVec2::X, DVec2::X * 2.0]);
        assert_eq!(
            triangulate_polygon(&collinear),
            Err(TriangulateError::ZeroArea)
        );
    }

    #[test]
    fn a_crossing_outline_is_flagged_as_a_guess() {
        // A bow tie: the net signed area is positive, so the cheap guards let
        // it through, but the triangles cover far more than the outline does.
        let polygon = Polygon2::new(vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(6.0, 0.0),
            DVec2::new(0.0, 2.0),
            DVec2::new(2.0, 6.0),
        ]);
        let indices = triangulate_polygon(&polygon).unwrap();
        assert!(triangulation_deviates(&polygon, &indices));
    }

    #[test]
    fn a_sound_triangulation_is_not_flagged() {
        let polygon = Polygon2 {
            outer: square(),
            holes: vec![vec![
                DVec2::new(0.5, 0.5),
                DVec2::new(0.5, 1.5),
                DVec2::new(1.5, 1.5),
                DVec2::new(1.5, 0.5),
            ]],
        };
        let indices = triangulate_polygon(&polygon).unwrap();
        assert!(!triangulation_deviates(&polygon, &indices));

        let l_shape = Polygon2::new(vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(2.0, 0.0),
            DVec2::new(2.0, 1.0),
            DVec2::new(1.0, 1.0),
            DVec2::new(1.0, 2.0),
            DVec2::new(0.0, 2.0),
        ]);
        let indices = triangulate_polygon(&l_shape).unwrap();
        assert!(!triangulation_deviates(&l_shape, &indices));
    }

    #[test]
    fn a_two_point_hole_is_skipped_not_fatal() {
        let polygon = Polygon2 {
            outer: square(),
            holes: vec![vec![DVec2::new(0.5, 0.5), DVec2::new(1.0, 1.0)]],
        };
        let indices = triangulate_polygon(&polygon).unwrap();
        assert!((triangulated_area(&polygon, &indices) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn a_face_in_3d_is_projected_and_wound_outward() {
        // A square in the plane z = 5, wound counter-clockwise seen from +Z.
        let outer = vec![
            DVec3::new(0.0, 0.0, 5.0),
            DVec3::new(2.0, 0.0, 5.0),
            DVec3::new(2.0, 2.0, 5.0),
            DVec3::new(0.0, 2.0, 5.0),
        ];
        let indices = triangulate_face(&outer, &[]).unwrap();
        assert_eq!(indices.len(), 6);
        let expected = newell_normal(&outer);
        for triangle in indices.chunks_exact(3) {
            let a = outer[triangle[0] as usize];
            let b = outer[triangle[1] as usize];
            let c = outer[triangle[2] as usize];
            let normal = (b - a).cross(c - a).normalize();
            assert!(
                normal.dot(expected) > 0.9,
                "triangle wound against the face normal: {normal} against {expected}"
            );
        }
    }

    #[test]
    fn a_vertical_face_is_projected_correctly() {
        // In the plane x = 3, which is where a bad basis choice shows up.
        let outer = vec![
            DVec3::new(3.0, 0.0, 0.0),
            DVec3::new(3.0, 2.0, 0.0),
            DVec3::new(3.0, 2.0, 2.0),
            DVec3::new(3.0, 0.0, 2.0),
        ];
        let indices = triangulate_face(&outer, &[]).unwrap();
        let mut area = 0.0f64;
        for triangle in indices.chunks_exact(3) {
            let a = outer[triangle[0] as usize];
            let b = outer[triangle[1] as usize];
            let c = outer[triangle[2] as usize];
            area += (b - a).cross(c - a).length() * 0.5;
        }
        assert!((area - 4.0).abs() < 1e-9, "got {area}");
    }

    #[test]
    fn a_triangle_face_short_circuits() {
        let outer = vec![DVec3::ZERO, DVec3::X, DVec3::Y];
        assert_eq!(triangulate_face(&outer, &[]).unwrap(), vec![0, 1, 2]);
    }
}
