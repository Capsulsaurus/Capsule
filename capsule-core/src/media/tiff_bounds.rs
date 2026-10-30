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
//! Where it lives: here rather than upstream, for now. Capsule consumes `rawshift-image` from
//! crates.io, so an upstream bound in `read_value` reaches this tree only with a release; this
//! walker is the guard until then, and becomes redundant (not wrong) once that release lands.

use std::collections::HashMap;
use std::collections::hash_map::Entry as Slot;
use std::fmt;

use rawshift_image::tiff::{BIGTIFF_MAGIC, TIFF_MAGIC, TiffTag, TiffType};

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
    /// read the way `RawFile::thumbnail` reads it (the first `SHORT` or `LONG` value), `0` where
    /// none does. The thumbnail reader allocates that many bytes before it reads one, so
    /// [`crate::media::extract_embedded_preview`] holds it to the file's length first.
    pub max_jpeg_length: u64,
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
    /// `remaining` more `SubIFDs` offsets, the next one stored at `at`, each naming an IFD at
    /// `depth`.
    SubIfds { at: u64, remaining: u64, depth: u32 },
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
        TiffType::from_u16(self.data_type).map(|t| t.size() as u64)
    }

    /// Whether the reader treats the value as stored in the entry itself. An unknown type is
    /// never inline, as upstream's `IfdEntry::is_inline` has it.
    fn is_inline(&self, tiff: &Tiff<'_>) -> bool {
        self.type_size()
            .and_then(|size| size.checked_mul(self.count))
            .is_some_and(|bytes| bytes <= tiff.inline_size())
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
                depth,
            } => {
                // The reader fails the whole list at the first unreadable element, so the rest
                // are never parsed.
                let Some(offset) = tiff.offset_at(at) else {
                    continue;
                };
                if remaining > 1 {
                    stack.push(Work::SubIfds {
                        at: at + tiff.offset_size(),
                        remaining: remaining - 1,
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
        if tag == TiffTag::JPEGInterchangeFormatLength.as_u16()
            && let Some(length) = first_short_or_long(tiff, &entry)
        {
            walk.max_jpeg_length = walk.max_jpeg_length.max(length);
        }
        follow(tiff, &entry, depth + 1, first, stack);
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
    let element = if entry.tag == TiffTag::SubIFDs.as_u16() {
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

/// Push the IFDs the reader parses from `entry`, read the way the reader reads them, each at
/// `depth`.
fn follow(tiff: &Tiff<'_>, entry: &Entry, depth: u32, first: bool, stack: &mut Vec<Work>) {
    let child = |offset: Option<u64>| {
        offset.map(|offset| Work::Ifd {
            offset,
            depth,
            chain: false,
            first: false,
        })
    };
    let work = if entry.tag == TiffTag::SubIFDs.as_u16() {
        if entry.count == 0 {
            None
        } else if entry.is_inline(tiff) && entry.count == 1 {
            child(tiff.offset_at(entry.field))
        } else {
            tiff.offset_at(entry.field).map(|at| Work::SubIfds {
                at,
                remaining: entry.count,
                depth,
            })
        }
    } else if entry.tag == TiffTag::ExifIFDPointer.as_u16()
        || entry.tag == TiffTag::GPSInfoIFDPointer.as_u16()
    {
        // `read_value_as_u64`: the field itself where inline, else a `u32` at the offset it
        // holds — a `u32` even in BigTIFF, as upstream reads it.
        if entry.is_inline(tiff) {
            child(tiff.offset_at(entry.field))
        } else {
            child(
                tiff.offset_at(entry.field)
                    .and_then(|at| tiff.u32_at(at))
                    .map(u64::from),
            )
        }
    } else if first && entry.tag == TAG_SONY_SR2_IFD {
        child(first_short_or_long(tiff, entry))
    } else {
        None
    };
    stack.extend(work);
}

/// The first value of a `SHORT` or `LONG`/`IFD` entry, and nothing for any other type: how the
/// ARW reader takes the SR2 IFD offset from IFD0's tag `0x02BC`, and how both RAW readers take an
/// embedded JPEG's length from `JPEGInterchangeFormatLength`. Reading an `IFD`-typed length too
/// only widens the bound, since the reader takes no length from one.
fn first_short_or_long(tiff: &Tiff<'_>, entry: &Entry) -> Option<u64> {
    if entry.count == 0 {
        return None;
    }
    let at = if entry.is_inline(tiff) {
        entry.field
    } else {
        tiff.offset_at(entry.field)?
    };
    match TiffType::from_u16(entry.data_type)? {
        TiffType::Short => tiff.u16_at(at).map(u64::from),
        TiffType::Long | TiffType::Ifd => tiff.u32_at(at).map(u64::from),
        _ => None,
    }
}
