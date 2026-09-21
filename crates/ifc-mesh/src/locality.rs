// SPDX-License-Identifier: Apache-2.0
//! Triangle and vertex order for the GPU's vertex caches. A B-rep lists its
//! triangles face by face, so shared vertices have left the post-transform
//! cache by the time they are reused; fanning around vertices fixes that.

use crate::mesh::Mesh64;

/// The post-transform cache size the triangle order is tuned for.
pub const VERTEX_CACHE_SIZE: u32 = 16;

/// Reorder triangles and vertices for cache locality, in place.
///
/// Geometry is unchanged: every triangle keeps its corners and winding. A mesh
/// with an index outside `positions` or a trailing partial triangle is left
/// exactly as it was, so a caller can run this before validation. Vertices no
/// triangle references are dropped.
pub fn optimize_vertex_locality(mesh: &mut Mesh64) {
    let vertex_count = mesh.positions.len();
    if mesh.indices.len() < 6
        || !mesh.indices.len().is_multiple_of(3)
        || mesh
            .indices
            .iter()
            .any(|&index| index as usize >= vertex_count)
    {
        return;
    }
    let ordered = reorder_triangles(&mesh.indices, vertex_count, VERTEX_CACHE_SIZE);
    if mesh.has_uvs() {
        let (_, uvs) = renumber_vertices(&ordered, &mesh.uvs);
        mesh.uvs = uvs;
    }
    let (indices, positions) = renumber_vertices(&ordered, &mesh.positions);
    mesh.indices = indices;
    mesh.positions = positions;
}

/// Reorder triangles for cache locality without renumbering any vertex: the
/// same fanning order as [`optimize_vertex_locality`], returned as new
/// indices over the same positions. Invalid input comes back as a copy.
pub fn optimize_index_locality(indices: &[u32], vertex_count: usize) -> Vec<u32> {
    if indices.len() < 6
        || !indices.len().is_multiple_of(3)
        || indices.iter().any(|&index| index as usize >= vertex_count)
    {
        return indices.to_vec();
    }
    reorder_triangles(indices, vertex_count, VERTEX_CACHE_SIZE)
}

/// Reorder the triangles of a `positions`, `indices` pair already narrowed to
/// `f32`, returning the new arrays. The same rules as
/// [`optimize_vertex_locality`]; invalid input comes back unchanged.
pub fn optimize_vertex_locality_f32(positions: &[f32], indices: &[u32]) -> (Vec<f32>, Vec<u32>) {
    let vertex_count = positions.len() / 3;
    if indices.len() < 6
        || !indices.len().is_multiple_of(3)
        || !positions.len().is_multiple_of(3)
        || indices.iter().any(|&index| index as usize >= vertex_count)
    {
        return (positions.to_vec(), indices.to_vec());
    }
    let ordered = reorder_triangles(indices, vertex_count, VERTEX_CACHE_SIZE);
    let mut remap = vec![u32::MAX; vertex_count];
    let mut out_indices = Vec::with_capacity(ordered.len());
    let mut out_positions = Vec::with_capacity(positions.len());
    for index in ordered {
        let slot = &mut remap[index as usize];
        if *slot == u32::MAX {
            *slot = (out_positions.len() / 3) as u32;
            let at = index as usize * 3;
            out_positions.extend_from_slice(&positions[at..at + 3]);
        }
        out_indices.push(*slot);
    }
    (out_positions, out_indices)
}

/// Number the vertices in first-use order and drop the unreferenced ones.
fn renumber_vertices<T: Copy>(indices: &[u32], positions: &[T]) -> (Vec<u32>, Vec<T>) {
    let mut remap = vec![u32::MAX; positions.len()];
    let mut out_indices = Vec::with_capacity(indices.len());
    let mut out_positions = Vec::with_capacity(positions.len());
    for &index in indices {
        let slot = &mut remap[index as usize];
        if *slot == u32::MAX {
            *slot = out_positions.len() as u32;
            out_positions.push(positions[index as usize]);
        }
        out_indices.push(*slot);
    }
    (out_indices, out_positions)
}

/// Greedy fanning order around a live vertex, choosing the next vertex from
/// the last triangle's corners by how recently it entered a FIFO cache of
/// `cache_size` entries, and falling back to the most recently touched vertex
/// that still has triangles. Every index must be below `vertex_count` and the
/// length a multiple of three.
fn reorder_triangles(indices: &[u32], vertex_count: usize, cache_size: u32) -> Vec<u32> {
    let triangle_count = indices.len() / 3;

    // Triangles around each vertex, by counting sort.
    let mut offsets = vec![0u32; vertex_count + 1];
    for &index in indices {
        offsets[index as usize + 1] += 1;
    }
    for vertex in 0..vertex_count {
        offsets[vertex + 1] += offsets[vertex];
    }
    let mut fill: Vec<u32> = offsets[..vertex_count].to_vec();
    let mut adjacent = vec![0u32; indices.len()];
    for (triangle, corners) in indices.chunks_exact(3).enumerate() {
        for &vertex in corners {
            let slot = &mut fill[vertex as usize];
            adjacent[*slot as usize] = triangle as u32;
            *slot += 1;
        }
    }
    let mut live: Vec<u32> = (0..vertex_count)
        .map(|vertex| offsets[vertex + 1] - offsets[vertex])
        .collect();

    let mut cache_time = vec![0u32; vertex_count];
    let mut emitted = vec![false; triangle_count];
    let mut recent: Vec<u32> = Vec::new();
    let mut candidates: Vec<u32> = Vec::with_capacity(cache_size as usize * 3);
    let mut output = Vec::with_capacity(indices.len());
    let mut time = cache_size + 1;
    let mut cursor = 0usize;
    let mut fan = Some(0u32);

    while let Some(vertex) = fan {
        candidates.clear();
        let range = offsets[vertex as usize] as usize..offsets[vertex as usize + 1] as usize;
        for &triangle in &adjacent[range] {
            if emitted[triangle as usize] {
                continue;
            }
            emitted[triangle as usize] = true;
            let at = triangle as usize * 3;
            for &corner in &indices[at..at + 3] {
                output.push(corner);
                recent.push(corner);
                candidates.push(corner);
                live[corner as usize] -= 1;
                if time - cache_time[corner as usize] > cache_size {
                    cache_time[corner as usize] = time;
                    time += 1;
                }
            }
        }

        // Prefer a corner that is still in the cache and whose remaining
        // triangles fit before it would be evicted.
        let mut best: Option<u32> = None;
        let mut best_priority = 0u32;
        for &corner in &candidates {
            let remaining = live[corner as usize];
            if remaining == 0 {
                continue;
            }
            let age = time - cache_time[corner as usize];
            let priority = if age + 2 * remaining <= cache_size {
                age
            } else {
                0
            };
            if best.is_none() || priority > best_priority {
                best = Some(corner);
                best_priority = priority;
            }
        }
        if best.is_none() {
            while let Some(corner) = recent.pop() {
                if live[corner as usize] > 0 {
                    best = Some(corner);
                    break;
                }
            }
        }
        if best.is_none() {
            while cursor < vertex_count {
                if live[cursor] > 0 {
                    best = Some(cursor as u32);
                    break;
                }
                cursor += 1;
            }
        }
        fan = best;
    }
    output
}

/// Average cache misses per triangle for a FIFO cache of `cache_size`
/// vertices. A measurement, for tests and tooling.
pub fn cache_miss_ratio(indices: &[u32], cache_size: usize) -> f64 {
    if indices.len() < 3 || cache_size == 0 {
        return 0.0;
    }
    let mut cache: Vec<u32> = Vec::with_capacity(cache_size);
    let mut head = 0usize;
    let mut misses = 0usize;
    for &index in indices {
        if cache.contains(&index) {
            continue;
        }
        misses += 1;
        if cache.len() < cache_size {
            cache.push(index);
        } else {
            cache[head] = index;
            head = (head + 1) % cache_size;
        }
    }
    misses as f64 / (indices.len() / 3) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec3;

    /// A grid of quads triangulated face by face, the way a B-rep arrives.
    fn grid(width: usize, height: usize) -> Mesh64 {
        let mut mesh = Mesh64::new();
        for y in 0..=height {
            for x in 0..=width {
                mesh.push_vertex(DVec3::new(x as f64, y as f64, 0.0));
            }
        }
        let stride = (width + 1) as u32;
        for y in 0..height as u32 {
            for x in 0..width as u32 {
                let a = y * stride + x;
                mesh.push_triangle(a, a + 1, a + stride + 1);
                mesh.push_triangle(a, a + stride + 1, a + stride);
            }
        }
        mesh
    }

    fn triangle_set(mesh: &Mesh64) -> Vec<[DVec3; 3]> {
        let mut set: Vec<[DVec3; 3]> = mesh
            .indices
            .chunks_exact(3)
            .map(|t| {
                let corners = [
                    mesh.positions[t[0] as usize],
                    mesh.positions[t[1] as usize],
                    mesh.positions[t[2] as usize],
                ];
                // Rotate so the lexicographically smallest corner leads; winding is kept.
                let start = (0..3)
                    .min_by(|&i, &j| {
                        corners[i]
                            .to_array()
                            .partial_cmp(&corners[j].to_array())
                            .unwrap()
                    })
                    .unwrap();
                [
                    corners[start],
                    corners[(start + 1) % 3],
                    corners[(start + 2) % 3],
                ]
            })
            .collect();
        set.sort_by(|a, b| {
            a.iter()
                .flat_map(|v| v.to_array())
                .partial_cmp(b.iter().flat_map(|v| v.to_array()))
                .unwrap()
        });
        set
    }

    #[test]
    fn keeps_every_triangle_and_its_winding() {
        let before = grid(40, 40);
        let mut after = before.clone();
        optimize_vertex_locality(&mut after);
        assert_eq!(after.indices.len(), before.indices.len());
        assert_eq!(after.positions.len(), before.positions.len());
        assert_eq!(triangle_set(&after), triangle_set(&before));
    }

    #[test]
    fn improves_the_miss_ratio_of_a_scattered_order() {
        let mut mesh = grid(60, 60);
        // Scatter the triangles so no two neighbours are adjacent in the list.
        let triangles: Vec<[u32; 3]> = mesh
            .indices
            .chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .collect();
        let count = triangles.len();
        let step = 977;
        mesh.indices = (0..count)
            .flat_map(|i| triangles[(i * step) % count])
            .collect();
        let before = cache_miss_ratio(&mesh.indices, 16);
        optimize_vertex_locality(&mut mesh);
        let after = cache_miss_ratio(&mesh.indices, 16);
        assert!(before > 2.5, "scattered order should miss, got {before}");
        assert!(after < 1.0, "fanned order should hit, got {after}");
    }

    #[test]
    fn numbers_vertices_by_first_use_and_drops_unused_ones() {
        let mut mesh = grid(3, 3);
        mesh.push_vertex(DVec3::new(99.0, 99.0, 99.0));
        let vertices = mesh.positions.len();
        optimize_vertex_locality(&mut mesh);
        assert_eq!(mesh.positions.len(), vertices - 1);
        let mut seen = 0u32;
        for &index in &mesh.indices {
            assert!(index <= seen);
            if index == seen {
                seen += 1;
            }
        }
        assert_eq!(seen as usize, mesh.positions.len());
    }

    #[test]
    fn leaves_invalid_input_alone() {
        let mut mesh = grid(2, 2);
        mesh.indices.push(1_000_000);
        mesh.indices.push(0);
        mesh.indices.push(1);
        let before = mesh.clone();
        optimize_vertex_locality(&mut mesh);
        assert_eq!(mesh, before);

        let mut partial = grid(2, 2);
        partial.indices.push(0);
        let before = partial.clone();
        optimize_vertex_locality(&mut partial);
        assert_eq!(partial, before);

        let mut single = Mesh64::new();
        for corner in [DVec3::X, DVec3::Y, DVec3::Z] {
            single.push_vertex(corner);
        }
        single.push_triangle(0, 1, 2);
        let before = single.clone();
        optimize_vertex_locality(&mut single);
        assert_eq!(single, before);
    }

    #[test]
    fn f32_variant_matches_the_f64_one() {
        let mesh = grid(9, 5);
        let positions: Vec<f32> = mesh
            .positions
            .iter()
            .flat_map(|p| [p.x as f32, p.y as f32, p.z as f32])
            .collect();
        let (out_positions, out_indices) = optimize_vertex_locality_f32(&positions, &mesh.indices);
        let mut reference = mesh.clone();
        optimize_vertex_locality(&mut reference);
        assert_eq!(out_indices, reference.indices);
        let expected: Vec<f32> = reference
            .positions
            .iter()
            .flat_map(|p| [p.x as f32, p.y as f32, p.z as f32])
            .collect();
        assert_eq!(out_positions, expected);
    }
}
