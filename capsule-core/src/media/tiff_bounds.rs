//! A no-allocation pre-check of a TIFF's IFD value counts (issue #644) and IFD nesting depth
//! (issue #647), run before `RawFile::open`.
//!
//! # Why this exists
//!
//! `rawshift-image` 0.1.1's `TiffParser` allocates from the file's own claims while it parses,
//! before Capsule sees anything: `read_value` sizes a buffer at an entry's stated `count` and only
//! then reads, and `parse_ifd_at` reads the `SubIFDs` entry through a `Vec::with_capacity(count)`.
//! `count` is a `u32` (a `u64` in BigTIFF), so a Double entry claiming `u32::MAX` values asks for
//! 32 GiB from a file of a few hundred bytes. An allocation failure aborts rather than unwinds, so
//! [`decode_guarded`](super::decode_guarded)'s `catch_unwind` cannot catch it.
//!
//! `TiffParser::validate_complete` does not help: it walks the chain through the same
//! `parse_ifd_at`, so it allocates for `SubIFDs` before it validates anything.
//!
//! # What it checks
//!
//! [`check_ifd_bounds`] walks exactly the directories `RawFile::open` parses — the IFD0
//! chain, every IFD's `SubIFDs` (recursively), EXIF and GPS IFDs, and the Sony SR2 IFD the ARW
//! reader follows from IFD0's tag `0x02BC` — and refuses the first entry whose value would need
//! more bytes than the whole file holds — a `SubIFDs` list counted at the 8-byte `u64` the reader
//! reserves per element, not the 4 bytes a classic-TIFF offset occupies. Bounding by the file length is the right bound: every
//! byte a value occupies has to be in the file, so a real file never trips it, and a value the
//! reader allocates is then never larger than the bytes Capsule already holds.
//!
//! It reads the file in place and allocates nothing proportional to a count: the only memory it
//! grows is the map of IFD offsets it has visited, bounded by the number of distinct IFDs, and
//! its work stack, bounded by the pointers it has read, and a `SubIFDs` list is iterated lazily
//! from the file rather than collected.
//!
//! It also records the largest length any walked `JPEGInterchangeFormatLength` entry states
//! ([`IfdWalk::max_jpeg_length`]). `RawFile::thumbnail` reads an embedded preview by allocating
//! that many bytes and only then reading them, so a few-byte file stating a 4 GiB preview would
//! abort the process the same way. That length is not refused here, because a broken preview
//! must not refuse the RAW decode: the embedded-preview extraction refuses it instead, before it
//! calls the reader.
//!
//! # The nesting bound
//!
//! `TiffParser::parse_ifd_at` follows each `SubIFDs` offset, the EXIF IFD and the GPS IFD by
//! calling itself, with no depth limit. Its visited-offset set stops a cycle but not a chain of
//! distinct IFDs, and one level costs a file about 18 bytes (a one-entry IFD holding an inline
//! `SubIFDs` offset), so a few hundred KB can nest tens of thousands of levels. Overflowing the
//! stack aborts the process rather than unwinding, which `decode_guarded` cannot catch either,
//! and an import worker's smaller stack overflows sooner.
//!
//! So the walk also refuses a TIFF whose directories nest deeper than [`MAX_IFD_DEPTH`] levels:
//! 16, where IFD0 and every other top-level chain member are level 0 and each SubIFD, EXIF, GPS or
//! SR2 IFD is one level below the IFD that points at it. Real ARW and DNG files nest two or three
//! levels deep; 16 leaves room for an unusual writer and keeps the reader's recursion to 17 nested
//! `parse_ifd_at` frames at worst, a negligible slice of any thread's stack.
//!
//! The depth is measured soundly rather than by imitating the reader's visiting order, which is
//! upstream's to change: an IFD is walked again whenever a pointer reaches it at a level deeper
//! than any it was walked at before, so every pointer path the reader could recurse along is
//! measured at its full length. Each IFD is then walked at most `MAX_IFD_DEPTH + 1` times. The
//! cost of that soundness is that a pointer loop among SubIFD, EXIF and GPS pointers — which the
//! reader cuts with its visited set — nests without end here and is refused. No camera writes one;
//! a next-IFD pointer loop along the top-level chain is iteration, not recursion, and still passes.
//!
//! # What it deliberately does not do
//!
//! It refuses only what would allocate. A directory it cannot read — a truncated entry table, an
//! out-of-range pointer, a non-TIFF header — is skipped rather than refused, because the reader
//! then fails on its own terms without allocating for it, and Capsule keeps reporting that
//! failure the way it already does (`MediaError::Decode` for a broken file,
//! `MediaError::UnsupportedFormat` for a variant upstream does not place). An entry whose value
//! fits in the file but whose offset runs past its end is left alone for the same reason: the
//! reader's allocation is bounded and its `read_exact` fails.
//!
//! Where it lives: here rather than upstream, for now. The bounds above were derived from
//! `rawshift-image` 0.1.1's own TIFF parser. The pinned git revision parses IFDs through gamut
//! (`gamut-ifd`), which no longer exports the TIFF constants this walk used, so they are spelled
//! out below from the TIFF 6.0 and BigTIFF specifications. The walk is kept as a conservative
//! guard over the new parser too: it refuses only values no real file can hold, so it is at worst
//! redundant, never wrong, and it is deleted once upstream's bounds are shown to cover it.
//!
//! It also records the largest `TileWidth` and `TileLength` any walked IFD states
//! ([`IfdWalk::max_tile`]) and the largest image any IFD states ([`IfdWalk::max_image`]).
//! gamut's DNG reader decodes the raw image as the file is opened, allocating for the image and
//! each tile at their stated sizes, and `rawshift-image` no longer reports a DNG's layout, so the
//! RAW budget holds a DNG's images and tiles to the budget through these before it opens it.
//!
//! The eager reader decodes more than the raw image and keeps all of it: every IFD that locates
//! strips or tiles is decoded at `width * height * SamplesPerPixel` `u16` samples, and the walk
//! records what that adds up to ([`IfdWalk::decode_pixels`], summed over every such IFD, and
//! [`IfdWalk::max_samples_per_pixel`]). Two of its compressions also allocate past any stated
//! size before checking one, so the walk records them too: a Deflate chunk inflates with no
//! output bound ([`IfdWalk::deflate`]), and a lossless-JPEG chunk is allocated at its own SOF3
//! header's size, which the walk reads from each chunk and compares with the strip or tile it
//! fills ([`IfdWalk::ljpeg_overrun`]). The walk refuses none of these itself; the DNG budget in
//! `decode` does, and the gaps are upstream's to close (visualcommons/rawshift#82).

use std::collections::HashMap;
use std::collections::hash_map::Entry as Slot;
use std::fmt;

/// The classic-TIFF header magic (TIFF 6.0, section 2).
const TIFF_MAGIC: u16 = 42;

/// The BigTIFF header magic.
const BIGTIFF_MAGIC: u16 = 43;

/// `JPEGInterchangeFormatLength`: an embedded JPEG's byte length.
const TAG_JPEG_INTERCHANGE_FORMAT_LENGTH: u16 = 0x0202;

/// `ImageWidth`, in pixels.
const TAG_IMAGE_WIDTH: u16 = 0x0100;

/// `ImageLength`, the image height in pixels.
const TAG_IMAGE_LENGTH: u16 = 0x0101;

/// `Compression`, the scheme an image's strips or tiles are stored in.
const TAG_COMPRESSION: u16 = 0x0103;

/// `StripOffsets`: where each strip of an image starts.
const TAG_STRIP_OFFSETS: u16 = 0x0111;

/// `SamplesPerPixel`.
const TAG_SAMPLES_PER_PIXEL: u16 = 0x0115;

/// `RowsPerStrip`.
const TAG_ROWS_PER_STRIP: u16 = 0x0116;

/// `StripByteCounts`: each strip's length in bytes.
const TAG_STRIP_BYTE_COUNTS: u16 = 0x0117;

/// `TileWidth`, in pixels.
const TAG_TILE_WIDTH: u16 = 0x0142;

/// `TileLength`, the tile height in pixels.
const TAG_TILE_LENGTH: u16 = 0x0143;

/// `TileOffsets`: where each tile of an image starts.
const TAG_TILE_OFFSETS: u16 = 0x0144;

/// `TileByteCounts`: each tile's length in bytes.
const TAG_TILE_BYTE_COUNTS: u16 = 0x0145;

/// `Compression` 7: JPEG, which for a DNG image is lossless JPEG (ITU-T T.81 process 14, SOF3).
pub(super) const COMPRESSION_JPEG: u64 = 7;

/// `Compression` 8: Deflate (zlib).
pub(super) const COMPRESSION_DEFLATE: u64 = 8;

/// `SubIFDs`: a list of child IFD offsets.
const TAG_SUB_IFDS: u16 = 0x014A;

/// The EXIF IFD pointer.
const TAG_EXIF_IFD: u16 = 0x8769;

/// The GPS IFD pointer.
const TAG_GPS_IFD: u16 = 0x8825;

/// TIFF field type `BYTE`.
const TYPE_BYTE: u16 = 1;

/// TIFF field type `SHORT`.
const TYPE_SHORT: u16 = 3;

/// TIFF field type `LONG`.
const TYPE_LONG: u16 = 4;

/// TIFF field type `IFD`.
const TYPE_IFD: u16 = 13;

/// BigTIFF field type `LONG8`, which gamut-ifd (built with `bigtiff`) accepts in classic TIFF too.
const TYPE_LONG8: u16 = 16;

/// BigTIFF field type `IFD8`, accepted like [`TYPE_LONG8`].
const TYPE_IFD8: u16 = 18;

/// The byte width of one element of an unsigned-integer type gamut-ifd coerces to a number:
/// `BYTE`, `SHORT`, `LONG`, `IFD`, `LONG8` and `IFD8`, the types `Value::as_u32` and
/// `Value::as_u64_vec` accept (the DNG reader's `get_u32` sizes and `get_u64_vec` `SubIFDs`)
/// and a superset of the `LONG`/`IFD`/`LONG8`/`IFD8` that `pointer_offsets` follows. `None` for
/// any other type, which no reader takes a size or a pointer from.
const fn unsigned_width(data_type: u16) -> Option<u64> {
    match data_type {
        TYPE_BYTE => Some(1),
        TYPE_SHORT => Some(2),
        TYPE_LONG | TYPE_IFD => Some(4),
        TYPE_LONG8 | TYPE_IFD8 => Some(8),
        _ => None,
    }
}

/// The byte size of one value of a TIFF (or BigTIFF) field type, and `None` for a type the
/// specifications do not define.
const fn type_size(data_type: u16) -> Option<u64> {
    match data_type {
        // BYTE, ASCII, SBYTE, UNDEFINED
        1 | 2 | 6 | 7 => Some(1),
        // SHORT, SSHORT
        3 | 8 => Some(2),
        // LONG, SLONG, FLOAT, IFD
        4 | 9 | 11 | 13 => Some(4),
        // RATIONAL, SRATIONAL, DOUBLE, and BigTIFF's LONG8, SLONG8, IFD8
        5 | 10 | 12 | 16 | 17 | 18 => Some(8),
        _ => None,
    }
}

/// The Sony tag the ARW reader reads as the offset of an SR2 IFD and then parses.
const TAG_SONY_SR2_IFD: u16 = 0x02BC;

/// The bytes `read_value_as_u64_vec` reserves per `SubIFDs` element: one `u64`, in classic TIFF
/// as in BigTIFF, which is also at least every TIFF type's own size.
const SUBIFD_ELEMENT_SIZE: u64 = std::mem::size_of::<u64>() as u64;

/// The most entries `TiffParser::parse_ifd_at` accepts in one IFD before it refuses the IFD.
const MAX_IFD_ENTRIES: u64 = 65_535;

/// The deepest level an IFD may sit at, IFD0 being level 0; see the module docs.
pub(super) const MAX_IFD_DEPTH: u32 = 16;

/// Why [`check_ifd_bounds`] refused a TIFF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum IfdRefusal {
    /// An entry's value needs more bytes than the file holds.
    Oversized(OversizedEntry),
    /// The directories nest deeper than [`MAX_IFD_DEPTH`].
    TooDeep(TooDeep),
}

impl fmt::Display for IfdRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oversized(entry) => entry.fmt(f),
            Self::TooDeep(deep) => deep.fmt(f),
        }
    }
}

/// The first IFD found deeper than [`MAX_IFD_DEPTH`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TooDeep {
    /// The offset of the IFD a pointer reached past the bound.
    pub ifd_offset: u64,
    /// The level it was reached at, one past [`MAX_IFD_DEPTH`].
    pub depth: u32,
}

impl fmt::Display for TooDeep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TIFF IFD at offset {} sits {} levels deep through SubIFD, EXIF, GPS and SR2 \
             pointers, past the limit of {MAX_IFD_DEPTH} (a pointer loop nests without end)",
            self.ifd_offset, self.depth
        )
    }
}

/// The first entry found whose value needs more bytes than the file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OversizedEntry {
    /// The offset of the IFD holding the entry.
    pub ifd_offset: u64,
    /// The entry's tag.
    pub tag: u16,
    /// The entry's TIFF data type.
    pub data_type: u16,
    /// The value count the entry claims.
    pub count: u64,
    /// The length of the whole file.
    pub file_len: u64,
}

impl fmt::Display for OversizedEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TIFF tag {:#06x} (type {}) in the IFD at offset {} claims {} values, more than the \
             {}-byte file holds",
            self.tag, self.data_type, self.ifd_offset, self.count, self.file_len
        )
    }
}

/// What a walk that found nothing oversized covered, for the trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct IfdWalk {
    /// Distinct IFDs whose entry table was read.
    pub ifds: usize,
    /// Entries checked across them, counting an IFD walked again at a deeper level again.
    pub entries: u64,
    /// The deepest level any IFD was reached at.
    pub depth: u32,
    /// The largest embedded-JPEG length any walked IFD's `JPEGInterchangeFormatLength` states,
    /// read as [`first_unsigned`] reads it (the first value of any unsigned-integer type, a
    /// superset of what `RawFile::thumbnail` takes), `0` where none does. The thumbnail reader allocates that many bytes before it reads one, so
    /// [`crate::media::extract_embedded_preview`] holds it to the file's length first.
    pub max_jpeg_length: u64,
    /// The largest `TileWidth` and the largest `TileLength` any walked IFD states (each read by
    /// [`first_unsigned`], which accepts every type gamut-ifd's `as_u32` does, `BYTE` and
    /// `LONG8` among them), `(0, 0)` where none does. Two maxima rather
    /// than one IFD's pair, so they bound every tile in the file, the RAW image's among them.
    pub max_tile: (u64, u64),
    /// The `ImageWidth` and `ImageLength` of the walked IFD that states the most pixels, read
    /// the same way, `(0, 0)` where none states both. gamut's DNG reader decodes the raw image
    /// when the file is opened, allocating for the size its IFD states, so the RAW budget holds
    /// a DNG's every stated image to the budget through this before it opens the file.
    pub max_image: (u64, u64),
    /// The pixels gamut's eager DNG reader decodes across every walked IFD that locates image
    /// data (a `StripOffsets` or `TileOffsets` entry) and states both its sizes, summed
    /// (saturating) over every walk of such an IFD. One IFD's figure is its `ImageWidth` by
    /// `ImageLength`, or, where it is tiled, its whole tile grid where that is larger: the
    /// reader decodes every tile at full size. The reader keeps every such IFD's samples (the
    /// raw image and each decoded sub-image), so it is the sum, not the largest, that bounds it.
    pub decode_pixels: u64,
    /// The largest `SamplesPerPixel` any IFD counted in [`Self::decode_pixels`] states (`1`
    /// where one leaves it out, as the reader defaults it), `0` where there is none. The reader
    /// allocates that many `u16` samples a pixel.
    pub max_samples_per_pixel: u64,
    /// Whether any IFD counted in [`Self::decode_pixels`] is Deflate-compressed. gamut's reader
    /// inflates each Deflate chunk with no bound on its output before it checks the chunk's
    /// size, so the size of what one inflates to cannot be known without inflating it.
    pub deflate: bool,
    /// The first lossless-JPEG chunk whose SOF3 header states more samples than its strip or
    /// tile holds, in an IFD counted in [`Self::decode_pixels`]. gamut's reader allocates each
    /// chunk at its own header's `width * height * components` (up to 65535 x 65535 x 4) and
    /// compares that with the chunk's size only after decoding it.
    pub ljpeg_overrun: Option<LjpegOverrun>,
}

/// A lossless-JPEG strip or tile whose header states more samples than the chunk holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LjpegOverrun {
    /// The offset of the IFD the chunk belongs to.
    pub ifd_offset: u64,
    /// The chunk's index in the IFD's strip or tile list.
    pub chunk: u64,
    /// The samples its SOF3 header states.
    pub samples: u64,
    /// The samples its strip or tile holds.
    pub capacity: u64,
}

impl fmt::Display for LjpegOverrun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "lossless-JPEG chunk {} of the IFD at offset {} states {} samples in its SOF3 \
             header, more than the {} its strip or tile holds",
            self.chunk, self.ifd_offset, self.samples, self.capacity
        )
    }
}

/// One unit of pending work on the walk's stack.
#[derive(Debug, Clone, Copy)]
enum Work {
    /// Read the IFD at `offset`, `depth` levels below the top-level chain. `chain` marks the
    /// top-level IFD0 chain, the only place the reader follows a next-IFD pointer; `first` marks
    /// IFD0 itself, the only IFD whose SR2 pointer the ARW reader follows.
    Ifd {
        offset: u64,
        depth: u32,
        chain: bool,
        first: bool,
    },
    /// `remaining` more pointer-list offsets (`SubIFDs`, EXIF or GPS), each `width` bytes, the
    /// next one stored at `at`, each naming an IFD at `depth`.
    SubIfds {
        at: u64,
        remaining: u64,
        width: u64,
        depth: u32,
    },
}

/// A TIFF read in place.
struct Tiff<'a> {
    bytes: &'a [u8],
    little: bool,
    big: bool,
}

impl Tiff<'_> {
    fn file_len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn slice<const N: usize>(&self, at: u64) -> Option<[u8; N]> {
        let start = usize::try_from(at).ok()?;
        let end = start.checked_add(N)?;
        self.bytes.get(start..end)?.try_into().ok()
    }

    /// The `len` bytes at `at`, where the file holds them all.
    fn range(&self, at: u64, len: u64) -> Option<&[u8]> {
        let start = usize::try_from(at).ok()?;
        let end = start.checked_add(usize::try_from(len).ok()?)?;
        self.bytes.get(start..end)
    }

    fn u16_at(&self, at: u64) -> Option<u16> {
        let b = self.slice::<2>(at)?;
        Some(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32_at(&self, at: u64) -> Option<u32> {
        let b = self.slice::<4>(at)?;
        Some(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn u64_at(&self, at: u64) -> Option<u64> {
        let b = self.slice::<8>(at)?;
        Some(if self.little {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        })
    }

    /// An unsigned integer `width` bytes wide (1, 2, 4 or 8), as [`unsigned_width`] sizes it.
    fn uint_at(&self, at: u64, width: u64) -> Option<u64> {
        match width {
            1 => self.slice::<1>(at).map(|[b]| u64::from(b)),
            2 => self.u16_at(at).map(u64::from),
            4 => self.u32_at(at).map(u64::from),
            8 => self.u64_at(at),
            _ => None,
        }
    }

    /// An offset-sized field: 4 bytes in classic TIFF, 8 in BigTIFF.
    fn offset_at(&self, at: u64) -> Option<u64> {
        if self.big {
            self.u64_at(at)
        } else {
            self.u32_at(at).map(u64::from)
        }
    }

    fn offset_size(&self) -> u64 {
        if self.big { 8 } else { 4 }
    }

    /// The size of an IFD entry's inline value field.
    fn inline_size(&self) -> u64 {
        self.offset_size()
    }
}

/// One IFD entry, read in place. `field` is the file offset of its value-or-offset field.
#[derive(Debug, Clone, Copy)]
struct Entry {
    tag: u16,
    data_type: u16,
    count: u64,
    field: u64,
}

impl Entry {
    /// The byte size of one value of the entry's type, where the type is one the reader knows.
    fn type_size(&self) -> Option<u64> {
        type_size(self.data_type)
    }

    /// Whether the reader treats the value as stored in the entry itself. An unknown type is
    /// never inline, as upstream's `IfdEntry::is_inline` has it.
    fn is_inline(&self, tiff: &Tiff<'_>) -> bool {
        self.type_size()
            .and_then(|size| size.checked_mul(self.count))
            .is_some_and(|bytes| bytes <= tiff.inline_size())
    }

    /// Where the entry's values start: the value field itself where they are inline, else the
    /// offset it holds.
    fn values_at(&self, tiff: &Tiff<'_>) -> Option<u64> {
        if self.is_inline(tiff) {
            Some(self.field)
        } else {
            tiff.offset_at(self.field)
        }
    }
}

/// Refuse a TIFF whose IFD chain holds an entry that would make `rawshift-image`'s parser
/// allocate more than the file's length, or whose IFDs nest deeper than [`MAX_IFD_DEPTH`].
/// Allocates nothing proportional to any count.
///
/// Bytes that are not a TIFF at all pass: the reader refuses them without allocating.
pub(super) fn check_ifd_bounds(bytes: &[u8]) -> Result<IfdWalk, IfdRefusal> {
    let little = match bytes.get(0..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Ok(IfdWalk::default()),
    };
    let mut tiff = Tiff {
        bytes,
        little,
        big: false,
    };
    let ifd0 = match tiff.u16_at(2) {
        Some(TIFF_MAGIC) => tiff.u32_at(4).map(u64::from),
        Some(BIGTIFF_MAGIC) => {
            tiff.big = true;
            tiff.u64_at(8)
        }
        _ => None,
    };
    let Some(ifd0) = ifd0 else {
        return Ok(IfdWalk::default());
    };

    let mut walk = IfdWalk::default();
    // Each IFD's deepest level walked so far: one reached deeper is walked again, so its
    // children are measured from the deepest path to it.
    let mut deepest = HashMap::new();
    let mut stack = vec![Work::Ifd {
        offset: ifd0,
        depth: 0,
        chain: true,
        first: true,
    }];
    while let Some(work) = stack.pop() {
        match work {
            Work::SubIfds {
                at,
                remaining,
                width,
                depth,
            } => {
                // The reader fails the whole list at the first unreadable element, so the rest
                // are never parsed.
                let Some(offset) = tiff.uint_at(at, width) else {
                    continue;
                };
                if remaining > 1 {
                    stack.push(Work::SubIfds {
                        at: at + width,
                        remaining: remaining - 1,
                        width,
                        depth,
                    });
                }
                if offset != 0 {
                    stack.push(Work::Ifd {
                        offset,
                        depth,
                        chain: false,
                        first: false,
                    });
                }
            }
            Work::Ifd {
                offset,
                depth,
                chain,
                first,
            } => {
                if offset == 0 {
                    continue;
                }
                // Refused on the pointer alone, before the IFD is read: the reader recurses into
                // the offset whether or not a table is there.
                if depth > MAX_IFD_DEPTH {
                    return Err(IfdRefusal::TooDeep(TooDeep {
                        ifd_offset: offset,
                        depth,
                    }));
                }
                let new = match deepest.entry(offset) {
                    Slot::Occupied(slot) if *slot.get() >= depth => continue,
                    Slot::Occupied(mut slot) => {
                        slot.insert(depth);
                        false
                    }
                    Slot::Vacant(slot) => {
                        slot.insert(depth);
                        true
                    }
                };
                let at = Visit {
                    offset,
                    depth,
                    chain,
                    first,
                    new,
                };
                walk_ifd(&tiff, at, &mut walk, &mut stack).map_err(IfdRefusal::Oversized)?;
            }
        }
    }
    Ok(walk)
}

/// One IFD about to be walked: [`Work::Ifd`]'s fields, and `new` where it was never walked
/// before at any level.
#[derive(Debug, Clone, Copy)]
struct Visit {
    offset: u64,
    depth: u32,
    chain: bool,
    first: bool,
    new: bool,
}

/// Check one IFD's entries and push the directories the reader would parse from it.
fn walk_ifd(
    tiff: &Tiff<'_>,
    visit: Visit,
    walk: &mut IfdWalk,
    stack: &mut Vec<Work>,
) -> Result<(), OversizedEntry> {
    let Visit {
        offset,
        depth,
        chain,
        first,
        new,
    } = visit;
    let (count_size, entry_size) = if tiff.big { (8, 20) } else { (2, 12) };
    let entry_count = if tiff.big {
        tiff.u64_at(offset)
    } else {
        tiff.u16_at(offset).map(u64::from)
    };
    let Some(entry_count) = entry_count.filter(|&n| n <= MAX_IFD_ENTRIES) else {
        return Ok(());
    };
    // Saturating: a BigTIFF offset is a full `u64` from the file. A saturated position reads
    // nothing, so the IFD is skipped like any other out-of-range one; every later position is
    // below `next_at`, which has been read, so it is inside the file.
    let table = offset.saturating_add(count_size);
    let next_at = table.saturating_add(entry_count * entry_size);
    // The reader parses the whole table and the next-IFD pointer before it reads any value, so a
    // table it cannot finish is one whose values it never reads.
    let Some(next) = tiff.offset_at(next_at) else {
        return Ok(());
    };
    walk.ifds += usize::from(new);
    walk.entries += entry_count;
    walk.depth = walk.depth.max(depth);

    let file_len = tiff.file_len();
    let (mut image_width, mut image_length) = (None, None);
    let mut image = ImageEntries::default();
    for index in 0..entry_count {
        let at = table + index * entry_size;
        let (Some(tag), Some(data_type)) = (tiff.u16_at(at), tiff.u16_at(at + 2)) else {
            return Ok(());
        };
        let count = if tiff.big {
            tiff.u64_at(at + 4)
        } else {
            tiff.u32_at(at + 4).map(u64::from)
        };
        let Some(count) = count else {
            return Ok(());
        };
        let entry = Entry {
            tag,
            data_type,
            count,
            field: at + 4 + if tiff.big { 8 } else { 4 },
        };
        check_entry(offset, &entry, file_len)?;
        if tag == TAG_JPEG_INTERCHANGE_FORMAT_LENGTH
            && let Some(length) = first_unsigned(tiff, &entry)
        {
            walk.max_jpeg_length = walk.max_jpeg_length.max(length);
        }
        if tag == TAG_TILE_WIDTH
            && let Some(width) = first_unsigned(tiff, &entry)
        {
            walk.max_tile.0 = walk.max_tile.0.max(width);
        }
        if tag == TAG_TILE_LENGTH
            && let Some(height) = first_unsigned(tiff, &entry)
        {
            walk.max_tile.1 = walk.max_tile.1.max(height);
        }
        if tag == TAG_IMAGE_WIDTH {
            image_width = first_unsigned(tiff, &entry);
        }
        if tag == TAG_IMAGE_LENGTH {
            image_length = first_unsigned(tiff, &entry);
        }
        image.take(tiff, &entry);
        follow(tiff, &entry, depth + 1, first, stack);
    }
    if let (Some(width), Some(length)) = (image_width, image_length) {
        account_image(tiff, offset, (width, length), &image, walk);
        let (max_w, max_l) = walk.max_image;
        // The first stated image is taken even at zero pixels, so a zero-sized one is reported
        // rather than mistaken for no image at all.
        if walk.max_image == (0, 0) || width.saturating_mul(length) > max_w.saturating_mul(max_l) {
            walk.max_image = (width, length);
        }
    }
    // Pushed last, so popped first: the whole top-level chain is walked before any child, and an
    // IFD that is both a chain member and some IFD's child is visited as the chain member first,
    // so its next pointer is followed; a later, deeper child visit walks its children again but
    // not its next pointer, which the reader follows only along the chain.
    if chain && next != 0 {
        stack.push(Work::Ifd {
            offset: next,
            depth: 0,
            chain: true,
            first: false,
        });
    }
    Ok(())
}

/// Refuse an entry whose value would need more bytes than the file holds.
///
/// A `SubIFDs` entry is held to [`SUBIFD_ELEMENT_SIZE`] bytes per element, whatever type it
/// declares and whichever TIFF variant holds it: the reader reads it through
/// `read_value_as_u64_vec`, which ignores the type and reserves `count` `u64`s before it reads a
/// single offset. A classic-TIFF offset is 4 bytes, so a list bounded at the offset size could
/// still reserve twice the file's length. An entry of a type the reader does not know is
/// otherwise skipped, because `read_value` refuses it before allocating.
fn check_entry(ifd_offset: u64, entry: &Entry, file_len: u64) -> Result<(), OversizedEntry> {
    let element = if entry.tag == TAG_SUB_IFDS {
        Some(SUBIFD_ELEMENT_SIZE)
    } else {
        entry.type_size()
    };
    let Some(element) = element else {
        return Ok(());
    };
    match element.checked_mul(entry.count) {
        Some(bytes) if bytes <= file_len => Ok(()),
        _ => Err(OversizedEntry {
            ifd_offset,
            tag: entry.tag,
            data_type: entry.data_type,
            count: entry.count,
            file_len,
        }),
    }
}

/// The entries of one IFD that say how gamut's DNG reader decodes its image, each size read by
/// [`first_unsigned`].
#[derive(Debug, Clone, Copy, Default)]
struct ImageEntries {
    compression: Option<u64>,
    samples_per_pixel: Option<u64>,
    rows_per_strip: Option<u64>,
    tile_width: Option<u64>,
    tile_length: Option<u64>,
    strip_offsets: Option<Entry>,
    strip_counts: Option<Entry>,
    tile_offsets: Option<Entry>,
    tile_counts: Option<Entry>,
}

impl ImageEntries {
    /// Keep `entry` where it is one of these.
    fn take(&mut self, tiff: &Tiff<'_>, entry: &Entry) {
        let value = || first_unsigned(tiff, entry);
        match entry.tag {
            TAG_COMPRESSION => self.compression = value(),
            TAG_SAMPLES_PER_PIXEL => self.samples_per_pixel = value(),
            TAG_ROWS_PER_STRIP => self.rows_per_strip = value(),
            TAG_TILE_WIDTH => self.tile_width = value(),
            TAG_TILE_LENGTH => self.tile_length = value(),
            TAG_STRIP_OFFSETS => self.strip_offsets = Some(*entry),
            TAG_STRIP_BYTE_COUNTS => self.strip_counts = Some(*entry),
            TAG_TILE_OFFSETS => self.tile_offsets = Some(*entry),
            TAG_TILE_BYTE_COUNTS => self.tile_counts = Some(*entry),
            _ => {}
        }
    }
}

/// Add what gamut's eager DNG reader decodes from one IFD stating a `width` by `length` image to
/// the walk: [`IfdWalk::decode_pixels`], [`IfdWalk::max_samples_per_pixel`],
/// [`IfdWalk::deflate`] and [`IfdWalk::ljpeg_overrun`].
///
/// The reader decodes every IFD that locates strips or tiles, the raw image and each sub-image
/// alike (gamut-dng's `decode_image_data`): it sizes the image at `width * height *
/// SamplesPerPixel` samples, lays strips or tiles out the way its `chunk_grid` does (tiled where
/// the IFD holds `TileOffsets` or `TileWidth`; `RowsPerStrip` defaulting to the height), and keeps
/// what it decoded. An IFD it would refuse on a tag read here is still counted, which only
/// widens the bound.
fn account_image(
    tiff: &Tiff<'_>,
    ifd_offset: u64,
    (width, length): (u64, u64),
    image: &ImageEntries,
    walk: &mut IfdWalk,
) {
    if image.strip_offsets.is_none() && image.tile_offsets.is_none() {
        return;
    }
    let samples_per_pixel = image.samples_per_pixel.unwrap_or(1);
    walk.max_samples_per_pixel = walk.max_samples_per_pixel.max(samples_per_pixel);
    let tiled = image.tile_offsets.is_some() || image.tile_width.is_some();
    let tile = match (image.tile_width, image.tile_length) {
        (Some(w), Some(l)) if tiled && w > 0 && l > 0 => Some((w, l)),
        _ => None,
    };
    let mut pixels = width.saturating_mul(length);
    if let Some((tile_w, tile_l)) = tile {
        let grid_w = width.div_ceil(tile_w).saturating_mul(tile_w);
        let grid_l = length.div_ceil(tile_l).saturating_mul(tile_l);
        pixels = pixels.max(grid_w.saturating_mul(grid_l));
    }
    walk.decode_pixels = walk.decode_pixels.saturating_add(pixels);

    match image.compression.unwrap_or(1) {
        COMPRESSION_DEFLATE => walk.deflate = true,
        COMPRESSION_JPEG if walk.ljpeg_overrun.is_none() => {
            // A chunk is a tile, or a strip of `RowsPerStrip` rows (the last one shorter). A
            // layout the reader refuses before decoding (no tile size, zero rows a strip) has
            // no chunk to bound.
            let (capacity, offsets, counts) = if tiled {
                (
                    tile.map(|(w, l)| w.saturating_mul(l)),
                    image.tile_offsets,
                    image.tile_counts,
                )
            } else {
                let rows = image.rows_per_strip.unwrap_or(length).min(length);
                (
                    (rows > 0).then(|| width.saturating_mul(rows)),
                    image.strip_offsets,
                    image.strip_counts,
                )
            };
            if let (Some(capacity), Some(offsets), Some(counts)) = (capacity, offsets, counts) {
                let capacity = capacity.saturating_mul(samples_per_pixel);
                walk.ljpeg_overrun = ljpeg_overrun(tiff, ifd_offset, (offsets, counts), capacity);
            }
        }
        _ => {}
    }
}

/// The first lossless-JPEG chunk listed by `offsets` and `counts` whose SOF3 header states more
/// than `capacity` samples.
///
/// The chunks are read the way gamut-dng's `grid_chunks` reads them (`get_u64_vec`, any
/// unsigned-integer type at its own width), and in its order. The scan stops at a chunk it
/// cannot place in the file or that has no SOF3 header ahead of any other frame, scan or end
/// marker: the reader decodes no chunk past one it fails on, so nothing past it is allocated.
/// Each chunk is scanned only up to its frame header, a prefix of what the reader reads itself.
fn ljpeg_overrun(
    tiff: &Tiff<'_>,
    ifd_offset: u64,
    (offsets, counts): (Entry, Entry),
    capacity: u64,
) -> Option<LjpegOverrun> {
    let (offset_width, count_width) = (
        unsigned_width(offsets.data_type)?,
        unsigned_width(counts.data_type)?,
    );
    let (offsets_at, counts_at) = (offsets.values_at(tiff)?, counts.values_at(tiff)?);
    for chunk in 0..offsets.count.min(counts.count) {
        let start = tiff.uint_at(
            offsets_at.saturating_add(chunk * offset_width),
            offset_width,
        )?;
        let len = tiff.uint_at(counts_at.saturating_add(chunk * count_width), count_width)?;
        let bytes = tiff.range(start, len)?;
        let samples = sof3_samples(bytes)?;
        if samples > capacity {
            return Some(LjpegOverrun {
                ifd_offset,
                chunk,
                samples,
                capacity,
            });
        }
    }
    None
}

/// The samples (`width * height * components`) a lossless-JPEG stream's SOF3 frame header
/// states, found the way gamut-dng's `lossless_jpeg::decode` walks the markers: after the SOI,
/// skip to each `FF`, past fill bytes, and skip each segment it does not act on by its length.
/// `None` where the reader refuses the stream before allocating for it: no SOI, a frame header
/// of another process, a scan or an end before any frame header, a precision outside 2..=16, a
/// deferred (zero) height or 0 or more than 4 components, or a marker past the end.
fn sof3_samples(stream: &[u8]) -> Option<u64> {
    const MARKER: u8 = 0xFF;
    if stream.get(0..2) != Some(&[MARKER, 0xD8]) {
        return None;
    }
    let be16 = |at: usize| {
        let b = stream.get(at..at.checked_add(2)?)?;
        Some(usize::from(u16::from_be_bytes([b[0], b[1]])))
    };
    let mut pos = 2usize;
    loop {
        pos += stream.get(pos..)?.iter().position(|&b| b == MARKER)?;
        while stream.get(pos) == Some(&MARKER) {
            pos += 1;
        }
        let marker = *stream.get(pos)?;
        pos += 1;
        match marker {
            0xC3 => {
                let precision = *stream.get(pos + 2)?;
                let height = be16(pos + 3)?;
                let width = be16(pos + 5)?;
                let components = *stream.get(pos + 7)?;
                if !(2..=16).contains(&precision) || height == 0 || !(1..=4).contains(&components) {
                    return None;
                }
                return Some(width as u64 * height as u64 * u64::from(components));
            }
            // Another frame process, a scan before the frame header, or the end of the image.
            0xC0..=0xC2 | 0xC5..=0xC7 | 0xC9..=0xCF | 0xDA | 0xD9 => return None,
            _ => pos = pos.checked_add(be16(pos)?)?,
        }
    }
}

/// Push the IFDs the reader parses from `entry`, read the way gamut-ifd reads them, each at
/// `depth`.
///
/// A `SubIFDs`, EXIF or GPS pointer is followed through every element, at the width of the
/// type the entry declares, whichever TIFF variant holds it: gamut-ifd decodes a `LONG8` or
/// `IFD8` list in classic TIFF as readily as in BigTIFF, the DNG reader takes `SubIFDs` through
/// `get_u64_vec` (`BYTE`, `SHORT`, `LONG`, `IFD`, `LONG8`, `IFD8`) and `read_tree` through
/// `pointer_offsets` (`LONG`, `IFD`, `LONG8`, `IFD8`). The walk follows the union, so every
/// directory either reader parses is walked. A list of any other type is a pointer to nothing.
fn follow(tiff: &Tiff<'_>, entry: &Entry, depth: u32, first: bool, stack: &mut Vec<Work>) {
    let work = if entry.tag == TAG_SUB_IFDS || entry.tag == TAG_EXIF_IFD || entry.tag == TAG_GPS_IFD
    {
        match (unsigned_width(entry.data_type), entry.values_at(tiff)) {
            (Some(width), Some(at)) if entry.count > 0 => Some(Work::SubIfds {
                at,
                remaining: entry.count,
                width,
                depth,
            }),
            _ => None,
        }
    } else if first && entry.tag == TAG_SONY_SR2_IFD {
        first_unsigned(tiff, entry).map(|offset| Work::Ifd {
            offset,
            depth,
            chain: false,
            first: false,
        })
    } else {
        None
    };
    stack.extend(work);
}

/// The first value of an unsigned-integer entry (`BYTE`, `SHORT`, `LONG`, `IFD`, `LONG8` or
/// `IFD8`, see [`unsigned_width`]), and nothing for any other type or an empty entry.
///
/// This is how a reader takes a size or an offset: gamut-ifd's `Value::as_u32`, behind the DNG
/// reader's `ImageWidth`, `ImageLength`, `TileWidth` and `TileLength`, accepts exactly these
/// types. It reads them only at a count of one, and refuses a `LONG8` past `u32::MAX`; taking the
/// first value at any count, at full width, only widens the bound, since a value the reader
/// would refuse is one it allocates nothing for. The same reading serves the ARW reader's SR2
/// IFD offset from IFD0's tag `0x02BC` and both RAW readers' embedded-JPEG length from
/// `JPEGInterchangeFormatLength`.
fn first_unsigned(tiff: &Tiff<'_>, entry: &Entry) -> Option<u64> {
    if entry.count == 0 {
        return None;
    }
    tiff.uint_at(entry.values_at(tiff)?, unsigned_width(entry.data_type)?)
}
