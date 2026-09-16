// SPDX-License-Identifier: Apache-2.0
//! IGP, the IFC Geometry Pack: a 24-byte header, a UTF-8 JSON index and an
//! 8-byte-aligned binary chunk, readable with a `DataView` or `numpy`. The v0
//! layout is normative, specified in `docs/igp-format.md`; changing it needs an RFC.
//!
//! ```
//! use tessifc_pack::{IgpWriter, Geometry, Instance};
//!
//! let mut writer = IgpWriter::new("IFC4", 0.001);
//! let geometry = writer.add_geometry(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0], &[0, 1, 2]);
//! writer.add_instance(Instance {
//!     geometry_id: geometry,
//!     express_id: 42,
//!     class: "IfcWall".into(),
//!     transform: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
//!     color: [200, 200, 200, 255],
//!     flags: 0,
//!     provenance: Default::default(),
//! });
//! let bytes = writer.finish();
//! assert_eq!(&bytes[0..4], b"IGP\0");
//! ```

#![deny(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::mem::size_of;

/// `IGP\0`, little-endian.
pub const MAGIC: u32 = 0x0050_4749;
/// The only version this crate writes.
pub const VERSION: u32 = 0;

/// Header flag: this is a partial chunk in a stream.
pub const FLAG_STREAMING: u32 = 1 << 0;
/// Header flag: positions are f64 rather than f32.
pub const FLAG_F64_POSITIONS: u32 = 1 << 1;

/// Instance flag: draw this transparently.
pub const INSTANCE_TRANSPARENT: u16 = 1 << 0;
/// Instance flag: this is an opening, hidden by default.
pub const INSTANCE_OPENING: u16 = 1 << 1;
/// Instance flag: this is an `IfcSpace`, hidden by default.
pub const INSTANCE_SPACE: u16 = 1 << 2;
/// Instance flag: something was diagnosed about this instance.
pub const INSTANCE_HAS_DIAGNOSTIC: u16 = 1 << 3;
/// Instance flag: non-physical reference geometry, hidden by default in viewers.
pub const INSTANCE_REFERENCE: u16 = 1 << 4;

/// One placed instance of a geometry.
#[derive(Clone, Debug)]
pub struct Instance {
    /// Which geometry to draw.
    pub geometry_id: u32,
    /// The IFC express id, used for picking and for looking the entity back up.
    pub express_id: u32,
    /// The IFC class name.
    pub class: String,
    /// Column-major 4x4, applied after the model offset.
    pub transform: [f32; 16],
    /// RGBA.
    pub color: [u8; 4],
    /// `INSTANCE_*` bits.
    pub flags: u16,
    /// Where the mesh came from.
    pub provenance: Provenance,
}

/// Where one drawn mesh came from, as the index writes it.
///
/// Identical rows collapse into one entry of the pack's `provenance` table, so
/// a family placed a thousand times costs one row.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Provenance {
    /// The `IfcShapeRepresentation` it came from.
    pub representation: u32,
    /// The representation item the evaluator was given.
    pub item: u32,
    /// The item's IFC class, which is the name the evaluator is registered for.
    pub evaluator: String,
    /// What became of its booleans: `none`, `exact`, `surface` or `refused`.
    pub boolean: &'static str,
}

/// The code of the first diagnostic raised against an entity, if any.
fn first_fallback(diagnostics: &[DiagnosticRecord], express_id: u32) -> Option<&str> {
    diagnostics
        .iter()
        .find(|record| record.express_id == Some(express_id))
        .map(|record| record.code.as_str())
}

/// One unique mesh.
#[derive(Clone, Debug)]
pub struct Geometry {
    /// Its id, which instances refer to.
    pub id: u32,
    /// Vertex count.
    pub vertex_count: usize,
    /// Index count, three per triangle.
    pub index_count: usize,
    /// `[minx, miny, minz, maxx, maxy, maxz]`.
    pub bbox: [f32; 6],
    /// Whether the producer found this mesh a closed solid; `None` when
    /// nobody checked. A reader needs it to know what may be capped at a
    /// section plane and what a volume figure would mean.
    pub closed: Option<bool>,
}

/// Where a chunk sits in a stream of packs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamPosition {
    /// Zero-based index of this chunk.
    pub chunk: u32,
    /// True for the last chunk, which carries the diagnostics and stats.
    pub is_final: bool,
    /// Products evaluated up to and including this chunk.
    pub products_done: usize,
    /// Products the stream will evaluate in total.
    pub products_total: usize,
}

/// What a stream carries from one chunk's writer to the next.
///
/// Geometry ids are global across a stream, and a mesh that appeared in an
/// earlier chunk is referred to by id rather than written again. This is the
/// memory of which ids exist.
#[derive(Clone, Debug, Default)]
pub struct StreamState {
    /// Content hash to geometry id, for every mesh written so far.
    pub known: HashMap<u64, u32>,
    /// The id the next new mesh receives.
    pub next_geometry_id: u32,
}

/// Builds an IGP file, or one chunk of a streamed one.
pub struct IgpWriter {
    schema: String,
    length_scale_to_m: f64,
    model_offset: [f64; 3],
    georef: Option<String>,
    geometries: Vec<Geometry>,
    positions: Vec<Vec<f32>>,
    indices: Vec<Vec<u32>>,
    instances: Vec<Instance>,
    non_finite_positions: usize,
    by_hash: HashMap<u64, u32>,
    first_geometry_id: u32,
    stream: Option<StreamPosition>,
    diagnostics: Vec<DiagnosticRecord>,
    stats: HashMap<String, f64>,
}

/// A diagnostic as it appears in the pack.
#[derive(Clone, Debug)]
pub struct DiagnosticRecord {
    /// The instance it concerns, if any.
    pub express_id: Option<u32>,
    /// Source line, or 0.
    pub line: u32,
    /// `info`, `warn` or `error`.
    pub severity: String,
    /// The stable code.
    pub code: String,
    /// Human-readable detail.
    pub message: String,
}

impl IgpWriter {
    /// A writer for one model.
    ///
    /// `schema` is the name the index carries; `docs/igp-format.md` accepts
    /// `IFC2X3`, `IFC4` and `IFC4X3`.
    pub fn new(schema: &str, length_scale_to_m: f64) -> Self {
        IgpWriter {
            schema: schema.to_string(),
            length_scale_to_m,
            model_offset: [0.0; 3],
            georef: None,
            geometries: Vec::new(),
            positions: Vec::new(),
            indices: Vec::new(),
            instances: Vec::new(),
            non_finite_positions: 0,
            by_hash: HashMap::new(),
            first_geometry_id: 0,
            stream: None,
            diagnostics: Vec::new(),
            stats: HashMap::new(),
        }
    }

    /// A writer for the next chunk of a stream.
    ///
    /// Ids continue from where the previous chunk stopped, and a mesh the
    /// stream has already written is not written again: `add_geometry`
    /// returns its earlier id. Say where the chunk sits with
    /// [`IgpWriter::set_stream`].
    pub fn continue_stream(schema: &str, length_scale_to_m: f64, state: StreamState) -> Self {
        let mut writer = IgpWriter::new(schema, length_scale_to_m);
        writer.by_hash = state.known;
        writer.first_geometry_id = state.next_geometry_id;
        writer
    }

    /// Mark this pack as one chunk of a stream.
    pub fn set_stream(&mut self, position: StreamPosition) {
        self.stream = Some(position);
    }

    /// The id the next new mesh will receive.
    pub fn next_geometry_id(&self) -> u32 {
        self.first_geometry_id
            .saturating_add(u32::try_from(self.geometries.len()).unwrap_or(u32::MAX))
    }

    /// Set the offset that was subtracted from every position.
    pub fn set_model_offset(&mut self, offset: [f64; 3]) {
        self.model_offset = offset;
    }

    /// Set the georeferencing block, already serialised as a JSON object.
    ///
    /// It is written verbatim as the index's optional `georef` member.
    pub fn set_georef(&mut self, json: Option<String>) {
        self.georef = json;
    }

    /// Record a statistic for the `stats` block.
    pub fn set_stat(&mut self, name: &str, value: f64) {
        self.stats.insert(name.to_string(), value);
    }

    /// Add a diagnostic.
    pub fn add_diagnostic(&mut self, record: DiagnosticRecord) {
        self.diagnostics.push(record);
    }

    /// Add a mesh, returning its geometry id.
    ///
    /// Identical meshes collapse to one entry. That is where a model of two
    /// hundred identical chairs stops costing two hundred meshes, and it is why
    /// the hash is over the actual bytes rather than over the source id.
    pub fn add_geometry(&mut self, positions: &[f32], indices: &[u32]) -> u32 {
        if positions.iter().any(|value| !value.is_finite()) {
            return self.add_geometry_owned(positions.to_vec(), indices.to_vec());
        }
        let hash = hash_mesh(positions, indices);
        if let Some(&existing) = self.by_hash.get(&hash) {
            return existing;
        }
        self.store_geometry(hash, positions.to_vec(), indices.to_vec(), None)
    }

    /// Add an already-owned mesh without copying its two largest arrays.
    ///
    /// This is the preferred boundary for an evaluator that has just narrowed
    /// f64 geometry to GPU-ready f32. Duplicate input is still discarded by
    /// content hash, exactly as in [`IgpWriter::add_geometry`].
    pub fn add_geometry_owned(&mut self, positions: Vec<f32>, indices: Vec<u32>) -> u32 {
        self.add_geometry_owned_closed(positions, indices, None)
    }

    /// [`IgpWriter::add_geometry_owned`], recording whether the mesh is closed.
    ///
    /// Identical meshes collapse to one entry and agree on closedness, since
    /// the hash is over the same bytes the answer was derived from.
    pub fn add_geometry_owned_closed(
        &mut self,
        mut positions: Vec<f32>,
        indices: Vec<u32>,
        closed: Option<bool>,
    ) -> u32 {
        // A non-finite coordinate has no JSON spelling and cannot sit inside
        // the bounding box the format says every position is inside.
        for value in positions.iter_mut().filter(|value| !value.is_finite()) {
            *value = 0.0;
            self.non_finite_positions += 1;
        }
        let hash = hash_mesh(&positions, &indices);
        if let Some(&existing) = self.by_hash.get(&hash) {
            return existing;
        }

        self.store_geometry(hash, positions, indices, closed)
    }

    /// Position components replaced with zero because they were not finite.
    pub fn non_finite_positions(&self) -> usize {
        self.non_finite_positions
    }

    fn store_geometry(
        &mut self,
        hash: u64,
        positions: Vec<f32>,
        mut indices: Vec<u32>,
        closed: Option<bool>,
    ) -> u32 {
        // A stale index would be narrowed onto another vertex or read past the
        // array, so such triangles are dropped, as is a trailing partial one.
        let vertex_count = positions.len() / 3;
        if !indices.len().is_multiple_of(3)
            || indices.iter().any(|&index| index as usize >= vertex_count)
        {
            let mut kept = Vec::with_capacity(indices.len());
            for triangle in indices.chunks_exact(3) {
                if triangle
                    .iter()
                    .all(|&index| (index as usize) < vertex_count)
                {
                    kept.extend_from_slice(triangle);
                }
            }
            indices = kept;
        }
        let mut bbox = [f32::MAX, f32::MAX, f32::MAX, f32::MIN, f32::MIN, f32::MIN];
        for vertex in positions.chunks_exact(3) {
            for axis in 0..3 {
                bbox[axis] = bbox[axis].min(vertex[axis]);
                bbox[axis + 3] = bbox[axis + 3].max(vertex[axis]);
            }
        }
        if positions.is_empty() {
            bbox = [0.0; 6];
        }

        let id = self.next_geometry_id();
        self.geometries.push(Geometry {
            id,
            vertex_count: positions.len() / 3,
            index_count: indices.len(),
            bbox,
            closed,
        });
        self.positions.push(positions);
        self.indices.push(indices);
        self.by_hash.insert(hash, id);
        id
    }

    /// Add a placed instance.
    pub fn add_instance(&mut self, instance: Instance) {
        self.instances.push(instance);
    }

    /// How many unique geometries.
    pub fn geometry_count(&self) -> usize {
        self.geometries.len()
    }

    /// How many instances.
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// Serialise.
    ///
    /// Deterministic: the same input always produces the same bytes. Instances
    /// are sorted by express id, classes by name, geometries and diagnostics
    /// keep the order they were added in, and nothing carries a timestamp, a
    /// hostname or a path (see `docs/igp-format.md`).
    pub fn finish(self) -> Vec<u8> {
        self.finish_chunk().0
    }

    /// Serialise, and hand back what the next chunk of a stream needs.
    pub fn finish_chunk(mut self) -> (Vec<u8>, StreamState) {
        let state = StreamState {
            next_geometry_id: self.next_geometry_id(),
            known: std::mem::take(&mut self.by_hash),
        };
        (self.serialise(), state)
    }

    fn serialise(mut self) -> Vec<u8> {
        self.instances.sort_by_key(|instance| instance.express_id);
        // Bit 3 says an instance has something to say about it, so a viewer can
        // mark it without carrying the diagnostics list into the render loop.
        let complained_about: std::collections::HashSet<u32> = self
            .diagnostics
            .iter()
            .filter_map(|record| record.express_id)
            .collect();
        for instance in &mut self.instances {
            if complained_about.contains(&instance.express_id) {
                instance.flags |= INSTANCE_HAS_DIAGNOSTIC;
            }
        }

        // Provenance rows in first-seen order over the sorted instances, identical
        // rows collapsed, so the same model always produces the same bytes.
        let mut provenance_rows: Vec<&Provenance> = Vec::new();
        let mut provenance_of: Vec<u32> = Vec::with_capacity(self.instances.len());
        {
            let mut seen: HashMap<(u32, u32, &str, &str), u32> = HashMap::new();
            for instance in &self.instances {
                let key = (
                    instance.provenance.representation,
                    instance.provenance.item,
                    instance.provenance.evaluator.as_str(),
                    instance.provenance.boolean,
                );
                let index = *seen.entry(key).or_insert_with(|| {
                    provenance_rows.push(&instance.provenance);
                    provenance_rows.len() as u32 - 1
                });
                provenance_of.push(index);
            }
        }
        let provenance_json = provenance_rows
            .iter()
            .map(|row| {
                let fallback = match first_fallback(&self.diagnostics, row.item) {
                    Some(code) => format!("\"{}\"", escape(code)),
                    None => "null".to_string(),
                };
                format!(
                    "{{\"rep\":{},\"item\":{},\"evaluator\":\"{}\",\"fallback\":{fallback},\"boolean\":\"{}\"}}",
                    row.representation,
                    row.item,
                    escape(&row.evaluator),
                    row.boolean
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        let mut classes: Vec<String> = self
            .instances
            .iter()
            .map(|instance| instance.class.clone())
            .collect();
        classes.sort();
        classes.dedup();
        let class_index: HashMap<&str, u16> = classes
            .iter()
            .enumerate()
            .map(|(index, name)| (name.as_str(), index as u16))
            .collect();

        // Offsets first, then serialise straight into the output buffer; a separate
        // BIN buffer would double the peak memory.
        let mut binary_len = 0usize;
        let mut geometry_json = Vec::new();

        for (local, geometry) in self.geometries.iter().enumerate() {
            let positions = &self.positions[local];
            let indices = &self.indices[local];

            binary_len = aligned(binary_len);
            let positions_at = binary_len;
            binary_len += positions.len() * size_of::<f32>();

            binary_len = aligned(binary_len);
            let indices_at = binary_len;
            // u16 where it fits, which halves the index data on the small
            // meshes that make up most of a model.
            let narrow = geometry.vertex_count <= u16::MAX as usize;
            binary_len += indices.len()
                * if narrow {
                    size_of::<u16>()
                } else {
                    size_of::<u32>()
                };

            geometry_json.push(format!(
                "{{\"id\":{},\"positions\":{{\"off\":{},\"count\":{}}},\
                 \"indices\":{{\"off\":{},\"count\":{},\"type\":\"{}\"}},\
                 \"bbox\":[{}],\"primitive\":\"triangles\"{}}}",
                geometry.id,
                positions_at,
                geometry.vertex_count,
                indices_at,
                geometry.index_count,
                if narrow { "u16" } else { "u32" },
                geometry
                    .bbox
                    .iter()
                    .map(format_float)
                    .collect::<Vec<_>>()
                    .join(","),
                match geometry.closed {
                    Some(true) => ",\"closed\":true",
                    Some(false) => ",\"closed\":false",
                    None => "",
                },
            ));
        }

        let count = self.instances.len();
        binary_len = aligned(binary_len);
        let geometry_id_at = binary_len;
        binary_len += count * size_of::<u32>();
        binary_len = aligned(binary_len);
        let express_id_at = binary_len;
        binary_len += count * size_of::<u32>();
        binary_len = aligned(binary_len);
        let class_id_at = binary_len;
        binary_len += count * size_of::<u16>();
        binary_len = aligned(binary_len);
        let transform_at = binary_len;
        binary_len += count * 16 * size_of::<f32>();
        binary_len = aligned(binary_len);
        let color_at = binary_len;
        binary_len += count * 4;
        binary_len = aligned(binary_len);
        let flags_at = binary_len;
        binary_len += count * size_of::<u16>();
        binary_len = aligned(binary_len);
        let provenance_at = binary_len;
        binary_len += count * size_of::<u32>();

        let mut json = String::with_capacity(4096 + geometry_json.len() * 96);
        json.push_str(&format!(
            "{{\"igp\":{VERSION},\"generator\":\"tessifc {}\",\"schema\":\"{}\",",
            env!("CARGO_PKG_VERSION"),
            escape(&self.schema)
        ));
        json.push_str(&format!(
            "\"units\":{{\"length_scale_to_m\":{}}},",
            format_double(&self.length_scale_to_m)
        ));
        json.push_str(&format!(
            "\"model_offset\":[{},{},{}],",
            format_double(&self.model_offset[0]),
            format_double(&self.model_offset[1]),
            format_double(&self.model_offset[2])
        ));
        if let Some(georef) = &self.georef {
            json.push_str(&format!("\"georef\":{georef},"));
        }
        if let Some(stream) = self.stream {
            json.push_str(&format!(
                "\"stream\":{{\"chunk\":{},\"final\":{},\"products_done\":{},\"products_total\":{}}},",
                stream.chunk, stream.is_final, stream.products_done, stream.products_total
            ));
        }
        json.push_str(&format!("\"geometries\":[{}],", geometry_json.join(",")));
        json.push_str(&format!(
            "\"instances\":{{\"count\":{count},\
             \"geometry_id\":{{\"off\":{geometry_id_at},\"type\":\"u32\"}},\
             \"express_id\":{{\"off\":{express_id_at},\"type\":\"u32\"}},\
             \"class_id\":{{\"off\":{class_id_at},\"type\":\"u16\"}},\
             \"transform\":{{\"off\":{transform_at},\"type\":\"f32x16\"}},\
             \"color\":{{\"off\":{color_at},\"type\":\"u8x4\"}},\
             \"flags\":{{\"off\":{flags_at},\"type\":\"u16\"}},             \"provenance\":{{\"off\":{provenance_at},\"type\":\"u32\"}}}},"
        ));
        json.push_str(&format!("\"provenance\":[{provenance_json}],"));
        json.push_str(&format!(
            "\"classes\":[{}],",
            classes
                .iter()
                .map(|name| format!("\"{}\"", escape(name)))
                .collect::<Vec<_>>()
                .join(",")
        ));

        let diagnostics: Vec<String> = self
            .diagnostics
            .iter()
            .map(|record| {
                format!(
                    "{{\"id\":{},\"line\":{},\"sev\":\"{}\",\"code\":\"{}\",\"msg\":\"{}\"}}",
                    record
                        .express_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "null".into()),
                    record.line,
                    escape(&record.severity),
                    escape(&record.code),
                    escape(&record.message)
                )
            })
            .collect();
        json.push_str(&format!("\"diagnostics\":[{}],", diagnostics.join(",")));

        let mut stats: Vec<(&String, &f64)> = self.stats.iter().collect();
        // By name: f64 is not Ord, and the name is what makes the output stable.
        stats.sort_by(|a, b| a.0.cmp(b.0));
        json.push_str(&format!(
            "\"stats\":{{{}}}}}",
            stats
                .iter()
                .map(|(name, value)| format!("\"{}\":{}", escape(name), format_double(value)))
                .collect::<Vec<_>>()
                .join(",")
        ));

        let json_bytes = json.into_bytes();
        // The header field is u32. A truncating cast would describe the pack
        // wrongly, so an index this large is refused instead.
        let Ok(json_len) = u32::try_from(json_bytes.len()) else {
            return Vec::new();
        };
        let json_len = json_len as usize;
        let padded = json_len.div_ceil(8) * 8;

        let mut out = Vec::with_capacity(24 + padded + binary_len);
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(json_len as u32).to_le_bytes());
        out.extend_from_slice(&(binary_len as u64).to_le_bytes());
        let flags = match self.stream {
            Some(stream) if !stream.is_final => FLAG_STREAMING,
            _ => 0,
        };
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&json_bytes);
        out.resize(24 + padded, b' ');
        let binary_start = out.len();

        for (local, geometry) in self.geometries.iter().enumerate() {
            let positions = &self.positions[local];
            let indices = &self.indices[local];
            align_binary(&mut out, binary_start);
            for value in positions {
                out.extend_from_slice(&value.to_le_bytes());
            }
            align_binary(&mut out, binary_start);
            if geometry.vertex_count <= u16::MAX as usize {
                for &index in indices {
                    out.extend_from_slice(&(index as u16).to_le_bytes());
                }
            } else {
                for &index in indices {
                    out.extend_from_slice(&index.to_le_bytes());
                }
            }
        }

        align_binary(&mut out, binary_start);
        for instance in &self.instances {
            out.extend_from_slice(&instance.geometry_id.to_le_bytes());
        }
        align_binary(&mut out, binary_start);
        for instance in &self.instances {
            out.extend_from_slice(&instance.express_id.to_le_bytes());
        }
        align_binary(&mut out, binary_start);
        for instance in &self.instances {
            let index = class_index
                .get(instance.class.as_str())
                .copied()
                .unwrap_or(0);
            out.extend_from_slice(&index.to_le_bytes());
        }
        align_binary(&mut out, binary_start);
        for instance in &self.instances {
            for value in instance.transform {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        align_binary(&mut out, binary_start);
        for instance in &self.instances {
            out.extend_from_slice(&instance.color);
        }
        align_binary(&mut out, binary_start);
        for instance in &self.instances {
            out.extend_from_slice(&instance.flags.to_le_bytes());
        }
        align_binary(&mut out, binary_start);
        for index in &provenance_of {
            out.extend_from_slice(&index.to_le_bytes());
        }
        debug_assert_eq!(out.len(), binary_start + binary_len);
        out
    }
}

fn aligned(length: usize) -> usize {
    length.div_ceil(8) * 8
}

fn align_binary(bytes: &mut Vec<u8>, binary_start: usize) {
    let binary_len = bytes.len() - binary_start;
    bytes.resize(binary_start + aligned(binary_len), 0);
}

/// Format a float so that the JSON is valid and stable.
///
/// Rust prints `inf` and `NaN`, which are not JSON. They should never reach
/// here, and if they do a zero is better than a file no parser will read.
fn format_float(value: &f32) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "0".into()
    }
}

fn format_double(value: &f64) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "0".into()
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// A content hash over a mesh, for deduplication.
fn hash_mesh(positions: &[f32], indices: &[u32]) -> u64 {
    // FNV-1a over the raw bytes. Collisions cost a wrongly shared mesh, so the
    // vertex and index counts go in first to make one vanishingly unlikely.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &byte in bytes {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(&(positions.len() as u64).to_le_bytes());
    eat(&(indices.len() as u64).to_le_bytes());
    for value in positions {
        eat(&value.to_le_bytes());
    }
    for value in indices {
        eat(&value.to_le_bytes());
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle() -> (Vec<f32>, Vec<u32>) {
        (
            vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            vec![0, 1, 2],
        )
    }

    fn identity() -> [f32; 16] {
        [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]
    }

    fn instance(geometry_id: u32, express_id: u32, class: &str) -> Instance {
        Instance {
            geometry_id,
            express_id,
            class: class.into(),
            transform: identity(),
            color: [255, 255, 255, 255],
            flags: 0,
            provenance: Provenance::default(),
        }
    }

    /// Read the header the way `docs/igp-format.md` says to.
    fn parse_header(bytes: &[u8]) -> (u32, u32, usize, u64, serde_json::Value) {
        let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let json_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let bin_len = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
        let json = std::str::from_utf8(&bytes[24..24 + json_len]).unwrap();
        (
            magic,
            version,
            json_len,
            bin_len,
            serde_json::from_str(json).unwrap(),
        )
    }

    #[test]
    fn the_header_is_what_the_spec_says() {
        let mut writer = IgpWriter::new("IFC4", 0.001);
        let (positions, indices) = triangle();
        let geometry = writer.add_geometry(&positions, &indices);
        writer.add_instance(instance(geometry, 42, "IfcWall"));
        let bytes = writer.finish();

        let (magic, version, json_len, bin_len, json) = parse_header(&bytes);
        assert_eq!(magic, MAGIC);
        assert_eq!(&bytes[0..4], b"IGP\0");
        assert_eq!(version, 0);
        assert_eq!(json["igp"], 0);
        assert_eq!(json["schema"], "IFC4");
        // The BIN chunk starts on an 8-byte boundary.
        let bin_start = 24 + json_len.div_ceil(8) * 8;
        assert_eq!(bin_start % 8, 0);
        assert_eq!(bytes.len(), bin_start + bin_len as usize);
    }

    #[test]
    fn identical_meshes_share_one_geometry() {
        // The instancing win: two hundred identical chairs, one mesh.
        let mut writer = IgpWriter::new("IFC4", 1.0);
        let (positions, indices) = triangle();
        let first = writer.add_geometry(&positions, &indices);
        let second = writer.add_geometry(&positions, &indices);
        assert_eq!(first, second);
        assert_eq!(writer.geometry_count(), 1);
    }

    #[test]
    fn owned_meshes_use_the_same_content_deduplication() {
        let mut writer = IgpWriter::new("IFC4", 1.0);
        let (positions, indices) = triangle();
        let first = writer.add_geometry_owned(positions.clone(), indices.clone());
        let second = writer.add_geometry_owned(positions, indices);
        assert_eq!(first, second);
        assert_eq!(writer.geometry_count(), 1);
    }

    #[test]
    fn different_meshes_do_not_share() {
        let mut writer = IgpWriter::new("IFC4", 1.0);
        let (positions, indices) = triangle();
        let first = writer.add_geometry(&positions, &indices);
        let moved: Vec<f32> = positions.iter().map(|value| value + 1.0).collect();
        let second = writer.add_geometry(&moved, &indices);
        assert_ne!(first, second);
        assert_eq!(writer.geometry_count(), 2);
    }

    #[test]
    fn positions_and_indices_survive_the_round_trip() {
        let mut writer = IgpWriter::new("IFC4", 1.0);
        let (positions, indices) = triangle();
        let geometry = writer.add_geometry(&positions, &indices);
        writer.add_instance(instance(geometry, 1, "IfcWall"));
        let bytes = writer.finish();

        let (_, _, json_len, _, json) = parse_header(&bytes);
        let bin_start = 24 + json_len.div_ceil(8) * 8;
        let entry = &json["geometries"][0];
        let offset = entry["positions"]["off"].as_u64().unwrap() as usize;
        let count = entry["positions"]["count"].as_u64().unwrap() as usize;
        assert_eq!(count, 3);

        let mut read = Vec::new();
        for index in 0..count * 3 {
            let at = bin_start + offset + index * 4;
            read.push(f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()));
        }
        assert_eq!(read, positions);

        let index_offset = entry["indices"]["off"].as_u64().unwrap() as usize;
        assert_eq!(entry["indices"]["type"], "u16", "three vertices fit in u16");
        let mut read_indices = Vec::new();
        for index in 0..3 {
            let at = bin_start + index_offset + index * 2;
            read_indices.push(u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap()) as u32);
        }
        assert_eq!(read_indices, indices);
    }

    #[test]
    fn instances_are_sorted_by_express_id() {
        let mut writer = IgpWriter::new("IFC4", 1.0);
        let (positions, indices) = triangle();
        let geometry = writer.add_geometry(&positions, &indices);
        writer.add_instance(instance(geometry, 99, "IfcSlab"));
        writer.add_instance(instance(geometry, 7, "IfcWall"));
        let bytes = writer.finish();

        let (_, _, json_len, _, json) = parse_header(&bytes);
        let bin_start = 24 + json_len.div_ceil(8) * 8;
        let offset = json["instances"]["express_id"]["off"].as_u64().unwrap() as usize;
        let first = u32::from_le_bytes(
            bytes[bin_start + offset..bin_start + offset + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(first, 7, "ascending express id is required for determinism");
    }

    #[test]
    fn every_instance_column_survives_direct_serialisation() {
        let mut writer = IgpWriter::new("IFC4", 1.0);
        let (positions, indices) = triangle();
        let geometry = writer.add_geometry_owned(positions, indices);
        let transform = [
            1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0,
        ];
        writer.add_instance(Instance {
            geometry_id: geometry,
            express_id: 42,
            class: "IfcWindow".into(),
            transform,
            color: [11, 22, 33, 44],
            flags: 0x1234,
            provenance: Provenance::default(),
        });
        let bytes = writer.finish();

        let (_, _, json_len, _, json) = parse_header(&bytes);
        let binary_start = 24 + json_len.div_ceil(8) * 8;
        let instances = &json["instances"];
        let at = |column: &str| binary_start + instances[column]["off"].as_u64().unwrap() as usize;
        assert_eq!(
            u32::from_le_bytes(
                bytes[at("geometry_id")..at("geometry_id") + 4]
                    .try_into()
                    .unwrap()
            ),
            geometry
        );
        assert_eq!(
            u32::from_le_bytes(
                bytes[at("express_id")..at("express_id") + 4]
                    .try_into()
                    .unwrap()
            ),
            42
        );
        let class_id = u16::from_le_bytes(
            bytes[at("class_id")..at("class_id") + 2]
                .try_into()
                .unwrap(),
        );
        assert_eq!(json["classes"][class_id as usize], "IfcWindow");
        let mut decoded_transform = [0.0; 16];
        for (index, value) in decoded_transform.iter_mut().enumerate() {
            let offset = at("transform") + index * size_of::<f32>();
            *value = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        }
        assert_eq!(decoded_transform, transform);
        assert_eq!(&bytes[at("color")..at("color") + 4], &[11, 22, 33, 44]);
        assert_eq!(
            u16::from_le_bytes(bytes[at("flags")..at("flags") + 2].try_into().unwrap()),
            0x1234
        );
    }

    #[test]
    fn the_same_input_gives_byte_identical_output() {
        // Determinism is what makes regression testing a hash comparison rather
        // than a tolerance argument.
        let build = || {
            let mut writer = IgpWriter::new("IFC4", 0.001);
            let (positions, indices) = triangle();
            let geometry = writer.add_geometry(&positions, &indices);
            writer.add_instance(instance(geometry, 42, "IfcWall"));
            writer.add_instance(instance(geometry, 7, "IfcSlab"));
            writer.set_stat("products", 2.0);
            writer.finish()
        };
        assert_eq!(build(), build());
    }

    #[test]
    fn a_wide_mesh_uses_u32_indices() {
        let mut writer = IgpWriter::new("IFC4", 1.0);
        let vertices = 70_000;
        let positions = vec![0.0f32; vertices * 3];
        let indices = vec![0u32, 1, 2];
        let geometry = writer.add_geometry(&positions, &indices);
        writer.add_instance(instance(geometry, 1, "IfcWall"));
        let bytes = writer.finish();
        let (_, _, _, _, json) = parse_header(&bytes);
        assert_eq!(json["geometries"][0]["indices"]["type"], "u32");
    }

    #[test]
    fn a_message_with_quotes_does_not_break_the_json() {
        let mut writer = IgpWriter::new("IFC4", 1.0);
        writer.add_diagnostic(DiagnosticRecord {
            express_id: Some(1),
            line: 5,
            severity: "warn".into(),
            code: "W_TEST".into(),
            message: "a \"quoted\" thing\nwith a newline".into(),
        });
        let bytes = writer.finish();
        let (_, _, _, _, json) = parse_header(&bytes);
        assert_eq!(json["diagnostics"][0]["code"], "W_TEST");
        assert!(
            json["diagnostics"][0]["msg"]
                .as_str()
                .unwrap()
                .contains("quoted")
        );
    }

    #[test]
    fn a_schema_name_with_a_quote_stays_inside_its_string() {
        let mut writer = IgpWriter::new("IFC4\" , \"evil\":1, \"x\":\"", 1.0);
        let (positions, indices) = triangle();
        let geometry = writer.add_geometry(&positions, &indices);
        writer.add_instance(instance(geometry, 1, "IfcWall"));
        let (_, _, _, _, json) = parse_header(&writer.finish());
        assert_eq!(json["schema"], "IFC4\" , \"evil\":1, \"x\":\"");
        assert!(json.get("evil").is_none());
    }

    #[test]
    fn a_non_finite_position_is_replaced_and_counted() {
        let mut writer = IgpWriter::new("IFC4", 1.0);
        let positions = vec![
            0.0,
            0.0,
            0.0,
            f32::INFINITY,
            0.0,
            0.0,
            0.0,
            f32::NAN,
            f32::NEG_INFINITY,
        ];
        let geometry = writer.add_geometry(&positions, &[0, 1, 2]);
        writer.add_instance(instance(geometry, 1, "IfcWall"));
        assert_eq!(writer.non_finite_positions(), 3);
        let bytes = writer.finish();

        let (_, _, json_len, _, json) = parse_header(&bytes);
        let binary_start = 24 + json_len.div_ceil(8) * 8;
        let entry = &json["geometries"][0];
        let bbox: Vec<f32> = entry["bbox"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_f64().unwrap() as f32)
            .collect();
        let offset = entry["positions"]["off"].as_u64().unwrap() as usize;
        for index in 0..9 {
            let at = binary_start + offset + index * 4;
            let value = f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
            assert!(value.is_finite(), "component {index} is {value}");
            let axis = index % 3;
            assert!(value >= bbox[axis] && value <= bbox[axis + 3]);
        }
        for axis in 0..3 {
            assert!(bbox[axis] <= bbox[axis + 3]);
        }
    }

    #[test]
    fn an_empty_pack_is_still_valid() {
        let bytes = IgpWriter::new("IFC4", 1.0).finish();
        let (magic, _, _, bin_len, json) = parse_header(&bytes);
        assert_eq!(magic, MAGIC);
        assert_eq!(bin_len, 0);
        assert_eq!(json["instances"]["count"], 0);
        assert!(json.get("stream").is_none(), "a whole pack is not a chunk");
    }

    #[test]
    fn a_stream_keeps_geometry_ids_global_and_writes_each_mesh_once() {
        let (positions, indices) = triangle();
        let moved: Vec<f32> = positions.iter().map(|value| value + 1.0).collect();

        let mut first = IgpWriter::new("IFC4", 1.0);
        first.set_stream(StreamPosition {
            chunk: 0,
            is_final: false,
            products_done: 1,
            products_total: 2,
        });
        let a = first.add_geometry(&positions, &indices);
        first.add_instance(instance(a, 1, "IfcWall"));
        let (first_bytes, state) = first.finish_chunk();
        assert_eq!(a, 0);
        assert_eq!(state.next_geometry_id, 1);

        let mut second = IgpWriter::continue_stream("IFC4", 1.0, state);
        second.set_stream(StreamPosition {
            chunk: 1,
            is_final: true,
            products_done: 2,
            products_total: 2,
        });
        // The same mesh again: the earlier id comes back and nothing is written.
        let again = second.add_geometry(&positions, &indices);
        assert_eq!(again, a);
        let b = second.add_geometry(&moved, &indices);
        assert_eq!(b, 1, "ids continue across chunks");
        second.add_instance(instance(again, 2, "IfcWall"));
        second.add_instance(instance(b, 3, "IfcSlab"));
        let (second_bytes, state) = second.finish_chunk();
        assert_eq!(state.next_geometry_id, 2);

        let (_, _, _, _, first_json) = parse_header(&first_bytes);
        let (_, _, _, _, second_json) = parse_header(&second_bytes);
        let first_flags = u32::from_le_bytes(first_bytes[20..24].try_into().unwrap());
        let second_flags = u32::from_le_bytes(second_bytes[20..24].try_into().unwrap());
        assert_eq!(
            first_flags & FLAG_STREAMING,
            FLAG_STREAMING,
            "a partial chunk is flagged"
        );
        assert_eq!(second_flags & FLAG_STREAMING, 0, "the final chunk is not");
        assert_eq!(first_json["stream"]["chunk"], 0);
        assert_eq!(first_json["stream"]["final"], false);
        assert_eq!(second_json["stream"]["final"], true);
        assert_eq!(second_json["stream"]["products_total"], 2);
        assert_eq!(second_json["geometries"].as_array().unwrap().len(), 1);
        assert_eq!(second_json["geometries"][0]["id"], 1);
        assert_eq!(second_json["instances"]["count"], 2);
    }

    #[test]
    fn a_stale_index_drops_its_triangle_rather_than_moving_it() {
        let mut writer = IgpWriter::new("IFC4", 1.0);
        let positions = vec![0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let geometry = writer.add_geometry(&positions, &[0, 1, 2, 0, 1, 70_000, 2]);
        writer.add_instance(instance(geometry, 1, "IfcWall"));
        let bytes = writer.finish();
        let (_, _, _, _, json) = parse_header(&bytes);
        assert_eq!(json["geometries"][0]["indices"]["count"], 3);
        assert_eq!(json["geometries"][0]["indices"]["type"], "u16");
    }
}
