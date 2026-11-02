//! A session's committed set and the range scheduler's plan over it.
//!
//! The server holds the only truth about what a session has made durable: its **committed
//! set**, a list of coalesced byte ranges it spells on `X-Capsule-Committed` as comma-separated
//! inclusive ranges (`0-1048575,2097152-4194303`). The client re-plans from that set, never from
//! its own record of what it sent (transfer upload doc, §Idempotency and Resumption).
//!
//! [`next_range`] is the scheduler's one decision: the **lowest gap first**. It picks the lowest
//! byte neither committed nor in flight, and plans a range from it of at most the chunk size,
//! ending early where the next committed or in-flight range begins. Every start it picks is the
//! end of a range the server committed or the client planned, so a range is on the 4 KiB grid by
//! construction, and a sequential schedule is the case of a window of one.
//!
//! Everything here is a pure function over values, so the scheduler is tested without a server.

/// The protocol's block grid: every range starts on it, and every range but the one ending at the
/// declared size has a length on it.
pub const GRID: u64 = 4096;

/// The most fragments a committed set holds on the wire (the server's fragment bound). A header
/// naming more is malformed.
pub const MAX_FRAGMENTS: usize = 64;

/// A session's committed set: sorted, disjoint, coalesced, half-open byte ranges inside
/// `[0, size)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommittedSet {
    ranges: Vec<(u64, u64)>,
}

impl CommittedSet {
    /// Nothing committed.
    #[must_use]
    pub const fn empty() -> Self {
        Self { ranges: Vec::new() }
    }

    /// The prefix `[0, end)`: what an older server's bare `X-Capsule-Offset` says it holds.
    #[must_use]
    pub fn prefix(end: u64) -> Self {
        let mut set = Self::empty();
        set.insert(0, end);
        set
    }

    /// Parse `X-Capsule-Committed` for a session of `size` bytes.
    ///
    /// Every range must be well formed (`start-end`, inclusive, `start <= end`), ascending and
    /// disjoint, inside the declared size, start on the grid, and end on it or at the size. A
    /// value that breaks any rule is `None`, so a malformed header is ignored rather than
    /// trusted: planning from it could put a range off the grid or over committed bytes. The
    /// empty value is the empty set.
    #[must_use]
    pub fn parse(header: &str, size: u64) -> Option<Self> {
        let header = header.trim();
        if header.is_empty() {
            return Some(Self::empty());
        }
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for part in header.split(',') {
            let (first, last) = part.trim().split_once('-')?;
            let first: u64 = first.trim().parse().ok()?;
            let last: u64 = last.trim().parse().ok()?;
            let end = last.checked_add(1)?;
            let well_placed = first < end
                && end <= size
                && first.is_multiple_of(GRID)
                && (end.is_multiple_of(GRID) || end == size)
                && ranges.last().is_none_or(|&(_, prev)| prev <= first);
            if !well_placed {
                return None;
            }
            ranges.push((first, end));
        }
        if ranges.len() > MAX_FRAGMENTS {
            return None;
        }
        let mut set = Self::empty();
        for (start, end) in ranges {
            set.insert(start, end);
        }
        Some(set)
    }

    /// The set as `X-Capsule-Committed` spells it.
    #[must_use]
    pub fn to_header(&self) -> String {
        self.ranges
            .iter()
            .map(|(start, end)| format!("{start}-{}", end - 1))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The coalesced half-open ranges, ascending.
    #[must_use]
    pub fn ranges(&self) -> &[(u64, u64)] {
        &self.ranges
    }

    /// How many fragments the set holds.
    #[must_use]
    pub fn fragments(&self) -> usize {
        self.ranges.len()
    }

    /// The committed prefix: the end of the range starting at zero, or zero.
    #[must_use]
    pub fn prefix_end(&self) -> u64 {
        match self.ranges.first() {
            Some(&(0, end)) => end,
            _ => 0,
        }
    }

    /// The bytes the set holds.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.ranges.iter().map(|(start, end)| end - start).sum()
    }

    /// Whether the set holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Whether every byte of `[0, size)` is committed.
    #[must_use]
    pub fn covers(&self, size: u64) -> bool {
        size == 0 || self.ranges.first() == Some(&(0, size))
    }

    /// Whether every byte of `[start, end)` is committed.
    #[must_use]
    pub fn contains(&self, start: u64, end: u64) -> bool {
        start >= end
            || self
                .ranges
                .iter()
                .any(|&(from, to)| from <= start && end <= to)
    }

    /// Add `[start, end)`, coalescing with whatever it touches. An empty range adds nothing.
    pub fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let (mut start, mut end) = (start, end);
        let mut kept = Vec::with_capacity(self.ranges.len() + 1);
        for &(from, to) in &self.ranges {
            if to < start || end < from {
                kept.push((from, to));
            } else {
                start = start.min(from);
                end = end.max(to);
            }
        }
        let at = kept.partition_point(|&(from, _)| from < start);
        kept.insert(at, (start, end));
        self.ranges = kept;
    }

    /// Add every range of `other`.
    pub fn union(&mut self, other: &Self) {
        for &(start, end) in &other.ranges {
            self.insert(start, end);
        }
    }
}

/// The next range to send: the lowest byte of `[0, size)` that is neither committed nor in
/// `in_flight`, and from it at most `want` bytes, ending where the next committed or in-flight
/// range begins. `None` once every byte is committed or in flight.
///
/// `want` is the chunk size the adaptive strategy names, a multiple of the grid; a range shorter
/// than it either ends at the declared size or at a range that starts on the grid, so the plan
/// stays on the grid.
#[must_use]
pub fn next_range(
    committed: &CommittedSet,
    in_flight: &[(u64, u64)],
    size: u64,
    want: u64,
) -> Option<(u64, u64)> {
    let mut taken: Vec<(u64, u64)> = committed
        .ranges()
        .iter()
        .copied()
        .chain(in_flight.iter().copied().filter(|(start, end)| start < end))
        .collect();
    taken.sort_unstable();
    let mut start = 0;
    let mut limit = size;
    for &(from, to) in &taken {
        if from > start {
            limit = from;
            break;
        }
        start = start.max(to);
    }
    if start >= size {
        return None;
    }
    let end = start.saturating_add(want.max(1)).min(limit).min(size);
    Some((start, end))
}

#[cfg(test)]
mod tests;
