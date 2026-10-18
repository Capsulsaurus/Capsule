//! A no-allocation pre-check of a TIFF's IFD value counts, run before `RawFile::open` (issue #644).
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
//! [`check_ifd_value_counts`] walks exactly the directories `RawFile::open` parses — the IFD0
//! chain, every IFD's `SubIFDs` (recursively), EXIF and GPS IFDs, and the Sony SR2 IFD the ARW
//! reader follows from IFD0's tag `0x02BC` — and refuses the first entry whose value would need
//! more bytes than the whole file holds. Bounding by the file length is the right bound: every
//! byte a value occupies has to be in the file, so a real file never trips it, and a value the
//! reader allocates is then never larger than the bytes Capsule already holds.
//!
//! It reads the file in place and allocates nothing proportional to a count: the only memory it
//! grows is the set of IFD offsets it has visited and its work stack, both bounded by the number
//! of distinct IFDs, and a `SubIFDs` list is iterated lazily from the file rather than collected.
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

use std::collections::HashSet;
use std::fmt;

use rawshift_image::tiff::{BIGTIFF_MAGIC, TIFF_MAGIC, TiffTag, TiffType};

/// The Sony tag the ARW reader reads as the offset of an SR2 IFD and then parses.
const TAG_SONY_SR2_IFD: u16 = 0x02BC;

/// The most entries `TiffParser::parse_ifd_at` accepts in one IFD before it refuses the IFD.
const MAX_IFD_ENTRIES: u64 = 65_535;

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
    /// Entries checked across them.
    pub entries: u64,
}

/// One unit of pending work on the walk's stack.
#[derive(Debug, Clone, Copy)]
enum Work {
    /// Read the IFD at `offset`. `chain` marks the top-level IFD0 chain, the only place the
    /// reader follows a next-IFD pointer; `first` marks IFD0 itself, the only IFD whose SR2
    /// pointer the ARW reader follows.
    Ifd {
        offset: u64,
        chain: bool,
        first: bool,
    },
    /// `remaining` more `SubIFDs` offsets, the next one stored at `at`.
    SubIfds { at: u64, remaining: u64 },
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
/// allocate more than the file's length. Allocates nothing proportional to any count.
///
/// Bytes that are not a TIFF at all pass: the reader refuses them without allocating.
pub(super) fn check_ifd_value_counts(bytes: &[u8]) -> Result<IfdWalk, OversizedEntry> {
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
    let mut visited = HashSet::new();
    let mut stack = vec![Work::Ifd {
        offset: ifd0,
        chain: true,
        first: true,
    }];
    while let Some(work) = stack.pop() {
        match work {
            Work::SubIfds { at, remaining } => {
                // The reader fails the whole list at the first unreadable element, so the rest
                // are never parsed.
                let Some(offset) = tiff.offset_at(at) else {
                    continue;
                };
                if remaining > 1 {
                    stack.push(Work::SubIfds {
                        at: at + tiff.offset_size(),
                        remaining: remaining - 1,
                    });
                }
                if offset != 0 {
                    stack.push(Work::Ifd {
                        offset,
                        chain: false,
                        first: false,
                    });
                }
            }
            Work::Ifd {
                offset,
                chain,
                first,
            } => {
                if offset == 0 || !visited.insert(offset) {
                    continue;
                }
                walk_ifd(&tiff, offset, chain, first, &mut walk, &mut stack)?;
            }
        }
    }
    Ok(walk)
}

/// Check one IFD's entries and push the directories the reader would parse from it.
fn walk_ifd(
    tiff: &Tiff<'_>,
    offset: u64,
    chain: bool,
    first: bool,
    walk: &mut IfdWalk,
    stack: &mut Vec<Work>,
) -> Result<(), OversizedEntry> {
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
    walk.ifds += 1;
    walk.entries += entry_count;

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
        check_entry(tiff, offset, &entry, file_len)?;
        follow(tiff, &entry, first, stack);
    }
    // Pushed last, so popped first: the whole top-level chain is walked before any child, and an
    // IFD that is both a chain member and some IFD's child is visited as the chain member, whose
    // next pointer is then followed.
    if chain && next != 0 {
        stack.push(Work::Ifd {
            offset: next,
            chain: true,
            first: false,
        });
    }
    Ok(())
}

/// Refuse an entry whose value would need more bytes than the file holds.
///
/// A `SubIFDs` entry is held to the offset size as well as its type size, whatever type it
/// declares: the reader reads it through `read_value_as_u64_vec`, which ignores the type and reads
/// `count` offsets. An entry of a type the reader does not know is otherwise skipped, because
/// `read_value` refuses it before allocating.
fn check_entry(
    tiff: &Tiff<'_>,
    ifd_offset: u64,
    entry: &Entry,
    file_len: u64,
) -> Result<(), OversizedEntry> {
    let element = if entry.tag == TiffTag::SubIFDs.as_u16() {
        Some(entry.type_size().unwrap_or(0).max(tiff.offset_size()))
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

/// Push the IFDs the reader parses from `entry`, read the way the reader reads them.
fn follow(tiff: &Tiff<'_>, entry: &Entry, first: bool, stack: &mut Vec<Work>) {
    let child = |offset: Option<u64>| {
        offset.map(|offset| Work::Ifd {
            offset,
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
        child(sr2_offset(tiff, entry))
    } else {
        None
    };
    stack.extend(work);
}

/// The SR2 IFD offset the ARW reader takes from IFD0's tag `0x02BC`: the first value of a
/// `SHORT` or `LONG`/`IFD` entry, and nothing for any other type.
fn sr2_offset(tiff: &Tiff<'_>, entry: &Entry) -> Option<u64> {
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
