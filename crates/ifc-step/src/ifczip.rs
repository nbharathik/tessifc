// SPDX-License-Identifier: Apache-2.0
//! IFCZIP: one `.ifc` file inside a ZIP archive, read with a local walk of the
//! central directory and inflated within a declared size. No ZIP64, no
//! encryption, methods stored and deflate only.

use crate::diag::{DiagCode, Diagnostic};
use crate::image::ModelImage;
use crate::parse::{ParseOptions, parse};
use std::fmt;
use tessifc_schema::SchemaId;

/// Bytes the end-of-directory record search looks back over: the record
/// plus the longest comment it may carry.
const EOCD_SEARCH: usize = 22 + 65_535;
const EOCD_SIGNATURE: [u8; 4] = [b'P', b'K', 5, 6];
const CENTRAL_SIGNATURE: [u8; 4] = [b'P', b'K', 1, 2];
const LOCAL_SIGNATURE: [u8; 4] = [b'P', b'K', 3, 4];

/// Limits an archive may not exceed.
#[derive(Clone, Debug)]
pub struct UnzipOptions {
    /// Refuse an entry that declares, or inflates to, more than this many bytes.
    pub max_uncompressed_bytes: usize,
    /// Refuse a directory with more entries than this.
    pub max_entries: usize,
}

impl Default for UnzipOptions {
    fn default() -> Self {
        UnzipOptions {
            max_uncompressed_bytes: 1 << 30,
            max_entries: 1024,
        }
    }
}

/// Why an archive could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZipError {
    /// The archive is not a ZIP this reader accepts; the text says where it failed.
    Malformed(String),
    /// An entry declares or inflates to more than the limit.
    TooLarge {
        /// Bytes the entry declares, or the limit when the inflate ran past it.
        declared: u64,
        /// The limit in force.
        limit: usize,
    },
    /// No entry in the archive is an IFC file.
    NoEntry,
}

impl fmt::Display for ZipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZipError::Malformed(what) => write!(f, "malformed IFCZIP archive: {what}"),
            ZipError::TooLarge { declared, limit } => {
                write!(
                    f,
                    "IFCZIP entry of {declared} bytes exceeds the limit of {limit}"
                )
            }
            ZipError::NoEntry => f.write_str("the IFCZIP archive holds no .ifc entry"),
        }
    }
}

impl std::error::Error for ZipError {}

impl ZipError {
    /// The diagnostic code for this failure.
    pub fn code(&self) -> DiagCode {
        match self {
            ZipError::TooLarge { .. } => DiagCode::IFCZIP_TOO_LARGE,
            ZipError::Malformed(_) | ZipError::NoEntry => DiagCode::IFCZIP_MALFORMED,
        }
    }
}

/// What came out of an archive.
#[derive(Clone, Debug)]
pub struct Unzipped {
    /// The entry's bytes, the plain IFC text.
    pub bytes: Vec<u8>,
    /// The entry's name inside the archive.
    pub name: String,
    /// How many `.ifc` entries the archive holds; only the first is read.
    pub ifc_entries: usize,
}

/// Does this look like a ZIP archive rather than STEP text?
pub fn is_ifczip(bytes: &[u8]) -> bool {
    bytes.starts_with(&LOCAL_SIGNATURE) || bytes.starts_with(&EOCD_SIGNATURE)
}

/// Read the first `.ifc` entry of an archive.
///
/// Sizes come from the central directory and are checked against the limits
/// before anything is inflated; the CRC-32 is verified afterwards.
pub fn unzip_ifc(bytes: &[u8], opts: &UnzipOptions) -> Result<Unzipped, ZipError> {
    let directory = end_of_directory(bytes)?;
    if directory.entries > opts.max_entries {
        return Err(ZipError::Malformed(format!(
            "{} entries, more than the {} allowed",
            directory.entries, opts.max_entries
        )));
    }
    let mut ifc_entries = 0usize;
    let mut chosen: Option<Entry> = None;
    let mut only: Option<Entry> = None;
    let mut at = directory.offset;
    for index in 0..directory.entries {
        let entry = central_entry(bytes, at, index)?;
        at = entry.next;
        if entry.name.to_ascii_lowercase().ends_with(".ifc") {
            ifc_entries += 1;
            if chosen.is_none() {
                chosen = Some(entry);
            }
        } else if index == 0 && directory.entries == 1 {
            // A lone entry under another name is still the file.
            only = Some(entry);
        }
    }
    let entry = chosen.or(only).ok_or(ZipError::NoEntry)?;
    if entry.uncompressed > opts.max_uncompressed_bytes as u64 {
        return Err(ZipError::TooLarge {
            declared: entry.uncompressed,
            limit: opts.max_uncompressed_bytes,
        });
    }
    let data = local_data(bytes, &entry)?;
    let out = match entry.method {
        0 => {
            if data.len() as u64 != entry.uncompressed {
                return Err(ZipError::Malformed("stored entry sizes disagree".into()));
            }
            data.to_vec()
        }
        8 => miniz_oxide::inflate::decompress_to_vec_with_limit(data, opts.max_uncompressed_bytes)
            .map_err(|error| match error.status {
                miniz_oxide::inflate::TINFLStatus::HasMoreOutput => ZipError::TooLarge {
                    declared: entry.uncompressed,
                    limit: opts.max_uncompressed_bytes,
                },
                _ => ZipError::Malformed(format!("deflate stream: {error:?}")),
            })?,
        other => {
            return Err(ZipError::Malformed(format!(
                "compression method {other} is not stored or deflate"
            )));
        }
    };
    if out.len() as u64 != entry.uncompressed {
        return Err(ZipError::Malformed(format!(
            "entry inflated to {} bytes, not the declared {}",
            out.len(),
            entry.uncompressed
        )));
    }
    if crc32(&out) != entry.crc {
        return Err(ZipError::Malformed("CRC-32 mismatch".into()));
    }
    Ok(Unzipped {
        bytes: out,
        name: entry.name,
        ifc_entries,
    })
}

/// Parse a file that may be plain STEP or an IFCZIP archive.
///
/// An archive that cannot be read gives an empty image whose diagnostics
/// carry the reason; a second `.ifc` entry is reported and ignored.
pub fn open(bytes: &[u8], opts: &ParseOptions) -> ModelImage {
    if !is_ifczip(bytes) {
        return parse(bytes, opts);
    }
    open_archive(bytes, opts).0
}

/// [`open`], also handing back the plain STEP text for a caller that keeps
/// the source to edit or export it. Plain input comes back as given.
pub fn open_source(bytes: Vec<u8>, opts: &ParseOptions) -> (ModelImage, Vec<u8>) {
    if !is_ifczip(&bytes) {
        return (parse(&bytes, opts), bytes);
    }
    let (image, inflated) = open_archive(&bytes, opts);
    (image, inflated.unwrap_or_default())
}

fn open_archive(bytes: &[u8], opts: &ParseOptions) -> (ModelImage, Option<Vec<u8>>) {
    let limits = UnzipOptions {
        max_uncompressed_bytes: opts.max_ifczip_bytes,
        ..UnzipOptions::default()
    };
    match unzip_ifc(bytes, &limits) {
        Ok(unzipped) => {
            let mut image = parse(&unzipped.bytes, opts);
            if unzipped.ifc_entries > 1 {
                image.diagnostics.push(Diagnostic::warning(
                    DiagCode::IFCZIP_MULTIPLE_ENTRIES,
                    0,
                    format!(
                        "the archive holds {} .ifc entries; only {} was read",
                        unzipped.ifc_entries, unzipped.name
                    ),
                ));
            }
            (image, Some(unzipped.bytes))
        }
        Err(error) => {
            let mut image = ModelImage::empty(opts.schema_override.unwrap_or(SchemaId::Ifc4));
            image
                .diagnostics
                .push(Diagnostic::error(error.code(), 0, error.to_string()));
            (image, None)
        }
    }
}

struct Directory {
    offset: usize,
    entries: usize,
}

struct Entry {
    name: String,
    method: u16,
    crc: u32,
    compressed: u64,
    uncompressed: u64,
    local_offset: usize,
    next: usize,
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let slice = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// A field `offset` bytes into a record at `at`, or `None` past the end.
fn signature_at(bytes: &[u8], at: usize) -> Option<&[u8]> {
    bytes.get(at..at.checked_add(4)?)
}

fn malformed(what: &str) -> ZipError {
    ZipError::Malformed(what.into())
}

/// Find the end-of-central-directory record, scanning back over its comment.
fn end_of_directory(bytes: &[u8]) -> Result<Directory, ZipError> {
    if bytes.len() < 22 {
        return Err(malformed("shorter than an end-of-directory record"));
    }
    let floor = bytes.len().saturating_sub(EOCD_SEARCH);
    let mut at = bytes.len() - 22;
    loop {
        if bytes[at..at + 4] == EOCD_SIGNATURE {
            let comment = read_u16(bytes, at + 20).ok_or_else(|| malformed("short record"))?;
            if at + 22 + comment as usize == bytes.len() {
                break;
            }
        }
        if at == floor {
            return Err(malformed("no end-of-directory record"));
        }
        at -= 1;
    }
    let disk = read_u16(bytes, at + 4).unwrap_or(1);
    let directory_disk = read_u16(bytes, at + 6).unwrap_or(1);
    let entries_here = read_u16(bytes, at + 8).unwrap_or(0);
    let entries = read_u16(bytes, at + 10).unwrap_or(0);
    let size = read_u32(bytes, at + 12).unwrap_or(u32::MAX);
    let offset = read_u32(bytes, at + 16).unwrap_or(u32::MAX);
    if disk != 0 || directory_disk != 0 || entries_here != entries {
        return Err(malformed("a multi-disk archive"));
    }
    if entries == u16::MAX || size == u32::MAX || offset == u32::MAX {
        return Err(malformed("ZIP64 is not supported"));
    }
    let end = (offset as usize)
        .checked_add(size as usize)
        .ok_or_else(|| malformed("directory offset overflows"))?;
    if end > at {
        return Err(malformed("the central directory runs past its record"));
    }
    Ok(Directory {
        offset: offset as usize,
        entries: entries as usize,
    })
}

/// One central directory header at `at`.
fn central_entry(bytes: &[u8], at: usize, index: usize) -> Result<Entry, ZipError> {
    let short = || ZipError::Malformed(format!("central directory entry {index} is truncated"));
    if signature_at(bytes, at) != Some(&CENTRAL_SIGNATURE) {
        return Err(ZipError::Malformed(format!(
            "central directory entry {index} has no signature"
        )));
    }
    let flags = read_u16(bytes, at + 8).ok_or_else(short)?;
    let method = read_u16(bytes, at + 10).ok_or_else(short)?;
    let crc = read_u32(bytes, at + 16).ok_or_else(short)?;
    let compressed = read_u32(bytes, at + 20).ok_or_else(short)?;
    let uncompressed = read_u32(bytes, at + 24).ok_or_else(short)?;
    let name_len = read_u16(bytes, at + 28).ok_or_else(short)? as usize;
    let extra_len = read_u16(bytes, at + 30).ok_or_else(short)? as usize;
    let comment_len = read_u16(bytes, at + 32).ok_or_else(short)? as usize;
    let local_offset = read_u32(bytes, at + 42).ok_or_else(short)?;
    if flags & 1 != 0 {
        return Err(ZipError::Malformed(format!("entry {index} is encrypted")));
    }
    if compressed == u32::MAX || uncompressed == u32::MAX || local_offset == u32::MAX {
        return Err(malformed("ZIP64 is not supported"));
    }
    let name_start = at.checked_add(46).ok_or_else(short)?;
    let name_end = name_start.checked_add(name_len).ok_or_else(short)?;
    let name = bytes.get(name_start..name_end).ok_or_else(short)?;
    let next = name_end
        .checked_add(extra_len)
        .and_then(|n| n.checked_add(comment_len))
        .ok_or_else(short)?;
    if next > bytes.len() {
        return Err(short());
    }
    Ok(Entry {
        name: String::from_utf8_lossy(name).into_owned(),
        method,
        crc,
        compressed: compressed as u64,
        uncompressed: uncompressed as u64,
        local_offset: local_offset as usize,
        next,
    })
}

/// The compressed bytes of an entry, found through its local header.
fn local_data<'a>(bytes: &'a [u8], entry: &Entry) -> Result<&'a [u8], ZipError> {
    let at = entry.local_offset;
    let short = || malformed("local file header is truncated");
    if signature_at(bytes, at) != Some(&LOCAL_SIGNATURE) {
        return Err(malformed("local file header has no signature"));
    }
    let name_len = read_u16(bytes, at + 26).ok_or_else(short)? as usize;
    let extra_len = read_u16(bytes, at + 28).ok_or_else(short)? as usize;
    let start = at
        .checked_add(30)
        .and_then(|n| n.checked_add(name_len))
        .and_then(|n| n.checked_add(extra_len))
        .ok_or_else(short)?;
    let end = start
        .checked_add(entry.compressed as usize)
        .ok_or_else(short)?;
    bytes
        .get(start..end)
        .ok_or_else(|| malformed("entry data runs past the end of the archive"))
}

/// CRC-32 (IEEE 802.3), as ZIP uses it.
fn crc32(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut n = 0;
        while n < 256 {
            let mut c = n as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[n] = c;
            n += 1;
        }
        table
    };
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc = TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal ZIP writer for the tests: stored or deflated entries, no extras.
    pub(crate) fn archive(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut directory = Vec::new();
        for (name, data, deflate) in entries {
            let (method, payload) = if *deflate {
                (8u16, miniz_oxide::deflate::compress_to_vec(data, 6))
            } else {
                (0u16, data.to_vec())
            };
            let crc = crc32(data);
            let offset = out.len() as u32;
            out.extend_from_slice(&LOCAL_SIGNATURE);
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&payload);

            directory.extend_from_slice(&CENTRAL_SIGNATURE);
            directory.extend_from_slice(&20u16.to_le_bytes());
            directory.extend_from_slice(&20u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&method.to_le_bytes());
            directory.extend_from_slice(&[0; 4]);
            directory.extend_from_slice(&crc.to_le_bytes());
            directory.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
            directory.extend_from_slice(&[0; 2]);
            directory.extend_from_slice(&[0; 2]);
            directory.extend_from_slice(&[0; 2]);
            directory.extend_from_slice(&[0; 2]);
            directory.extend_from_slice(&[0; 4]);
            directory.extend_from_slice(&offset.to_le_bytes());
            directory.extend_from_slice(name.as_bytes());
        }
        let directory_offset = out.len() as u32;
        out.extend_from_slice(&directory);
        out.extend_from_slice(&EOCD_SIGNATURE);
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        out.extend_from_slice(&directory_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    const WALL: &[u8] = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n#1=IFCWALL('a',$,'W',$,$,$,$,$,$);\nENDSEC;\nEND-ISO-10303-21;\n";

    #[test]
    fn crc32_matches_the_reference_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn stored_and_deflated_archives_open_to_the_same_model() {
        for deflate in [false, true] {
            let zip = archive(&[("model.ifc", WALL, deflate)]);
            assert!(is_ifczip(&zip));
            let unzipped = unzip_ifc(&zip, &UnzipOptions::default()).unwrap();
            assert_eq!(unzipped.bytes, WALL);
            assert_eq!(unzipped.name, "model.ifc");
            assert_eq!(unzipped.ifc_entries, 1);
            let image = open(&zip, &ParseOptions::default());
            assert!(
                !image.diagnostics.has_errors(),
                "{:?}",
                image.diagnostics.items()
            );
            assert_eq!(image.len(), 1);
        }
        assert!(!is_ifczip(WALL));
        assert_eq!(open(WALL, &ParseOptions::default()).len(), 1);
    }

    #[test]
    fn an_archive_with_a_comment_and_a_second_entry_warns() {
        let mut zip = archive(&[("a.ifc", WALL, true), ("b.IFC", WALL, false)]);
        let comment = b"made by a test";
        let at = zip.len() - 2;
        zip[at..].copy_from_slice(&(comment.len() as u16).to_le_bytes());
        zip.extend_from_slice(comment);
        let image = open(&zip, &ParseOptions::default());
        assert_eq!(image.len(), 1);
        assert!(
            image
                .diagnostics
                .items()
                .iter()
                .any(|d| d.code == DiagCode::IFCZIP_MULTIPLE_ENTRIES)
        );
    }

    #[test]
    fn a_lone_entry_under_another_name_is_still_read() {
        let zip = archive(&[("payload.bin", WALL, true)]);
        assert_eq!(
            unzip_ifc(&zip, &UnzipOptions::default()).unwrap().bytes,
            WALL
        );
        let zip = archive(&[("a.txt", WALL, true), ("b.txt", WALL, true)]);
        assert_eq!(
            unzip_ifc(&zip, &UnzipOptions::default()).unwrap_err(),
            ZipError::NoEntry
        );
    }

    #[test]
    fn a_wrong_crc_is_refused() {
        let mut zip = archive(&[("model.ifc", WALL, true)]);
        // The local header's CRC field starts at byte 14.
        zip[14] ^= 0xFF;
        // The central directory copy is what is checked, so corrupt that too.
        let central = zip.windows(4).position(|w| w == CENTRAL_SIGNATURE).unwrap();
        zip[central + 16] ^= 0xFF;
        assert!(matches!(
            unzip_ifc(&zip, &UnzipOptions::default()),
            Err(ZipError::Malformed(what)) if what.contains("CRC")
        ));
    }

    #[test]
    fn sizes_over_the_limit_are_refused_before_and_after_inflating() {
        let zip = archive(&[("model.ifc", WALL, true)]);
        let small = UnzipOptions {
            max_uncompressed_bytes: 16,
            ..UnzipOptions::default()
        };
        assert!(matches!(
            unzip_ifc(&zip, &small),
            Err(ZipError::TooLarge { limit: 16, .. })
        ));
        // A directory that lies about the size: declared small, inflates large.
        let mut lying = zip.clone();
        let central = lying
            .windows(4)
            .position(|w| w == CENTRAL_SIGNATURE)
            .unwrap();
        lying[central + 24..central + 28].copy_from_slice(&8u32.to_le_bytes());
        let limit = UnzipOptions {
            max_uncompressed_bytes: 32,
            ..UnzipOptions::default()
        };
        assert!(matches!(
            unzip_ifc(&lying, &limit),
            Err(ZipError::TooLarge { .. })
        ));
        assert!(matches!(
            unzip_ifc(&lying, &UnzipOptions::default()),
            Err(ZipError::Malformed(what)) if what.contains("declared")
        ));
        let image = open(
            &zip,
            &ParseOptions {
                max_ifczip_bytes: 16,
                ..ParseOptions::default()
            },
        );
        assert_eq!(image.len(), 0);
        assert_eq!(
            image.diagnostics.items()[0].code,
            DiagCode::IFCZIP_TOO_LARGE
        );
    }

    #[test]
    fn truncated_and_foreign_archives_are_refused() {
        let zip = archive(&[("model.ifc", WALL, true)]);
        for cut in [zip.len() - 1, zip.len() - 30, 40, 4] {
            let short = &zip[..cut];
            assert!(
                unzip_ifc(short, &UnzipOptions::default()).is_err(),
                "cut at {cut}"
            );
        }
        let image = open(&zip[..zip.len() - 1], &ParseOptions::default());
        assert_eq!(
            image.diagnostics.items()[0].code,
            DiagCode::IFCZIP_MALFORMED
        );
        // An empty archive has a record and nothing else.
        assert_eq!(
            unzip_ifc(&archive(&[]), &UnzipOptions::default()).unwrap_err(),
            ZipError::NoEntry
        );
        // Encryption, an unknown method and ZIP64 markers.
        let mut encrypted = zip.clone();
        let central = encrypted
            .windows(4)
            .position(|w| w == CENTRAL_SIGNATURE)
            .unwrap();
        encrypted[central + 8] |= 1;
        assert!(unzip_ifc(&encrypted, &UnzipOptions::default()).is_err());
        let mut lzma = zip.clone();
        lzma[central + 10] = 14;
        assert!(unzip_ifc(&lzma, &UnzipOptions::default()).is_err());
        let mut zip64 = zip.clone();
        let eocd = zip64.len() - 22;
        zip64[eocd + 16..eocd + 20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(unzip_ifc(&zip64, &UnzipOptions::default()).is_err());
        let many = UnzipOptions {
            max_entries: 1,
            ..UnzipOptions::default()
        };
        let two = archive(&[("a.ifc", WALL, true), ("b.ifc", WALL, true)]);
        assert!(unzip_ifc(&two, &many).is_err());
    }
}
