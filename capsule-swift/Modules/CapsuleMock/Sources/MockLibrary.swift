import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockLibrary

/// A synthetic photo library that is a **pure function of `(seed, index)`**.
///
/// Nothing here stores assets. `asset(at:)` derives every field from a keyed
/// hash of the index, so library size is a parameter rather than a memory cost:
/// the default scenario's 4 000 assets and ``MockScenario/hugeLibrary``'s
/// 250 000 cost exactly the same to construct.
///
/// That is not a micro-optimisation. The timeline is virtualized over a paged
/// port — ``LibraryPort/assets(matching:offset:limit:)`` plus
/// ``LibraryPort/dayCounts(matching:)`` — and `TimelineLayout` is tested against
/// 3 650 sections and 250 000 items. A mock that materialized an array would
/// make the one thing most worth proving about the UI unprovable.
///
/// ## How the day distribution stays cheap and exact
///
/// The only stored state is one prefix array of day boundaries, sized by *days*
/// (a few thousand) and never by *assets*. It is built by giving every day a
/// deterministic weight — heavier at weekends and in summer, occasionally zero
/// because nothing happened that day — and then slicing `assetCount` across
/// those weights. Two consequences that the whole design leans on:
///
/// - `dayCounts(...)` for an unfiltered query is O(days), read straight off the
///   array, and its total is *exactly* `assetCount`.
/// - Index → day is a binary search, and day → index range is a subscript, so
///   paging and the aggregate are two views of one arithmetic and cannot
///   disagree.
///
/// Assets are ordered **newest first**, matching
/// ``LibraryAsset/isOrderedNewestFirst(_:_:)``: index 0 is the most recent
/// photo, day 0 is the most recent day. Capture instants inside a day are
/// strictly decreasing with index, so the derived order and the domain's
/// comparator agree without relying on the identifier tiebreak.
public struct MockLibrary: Sendable {
    public let profile: MockLibraryProfile

    /// The index of each day's first asset, newest day first, with a trailing
    /// entry equal to ``assetCount``. Size is `spanDays + 1`.
    private let dayStartIndex: [Int]

    public init(profile: MockLibraryProfile) {
        self.profile = profile
        dayStartIndex = Self.makeDayStartIndex(profile: profile)
    }

    /// Assets in the default timeline.
    public var assetCount: Int { profile.assetCount }

    /// Days the library spans, including empty ones.
    public var dayCount: Int { profile.spanDays }

    // MARK: Day boundaries

    /// The UTC day number for a day index, `0` being the newest day.
    public func dayNumber(forDay dayIndex: Int) -> Int64 {
        profile.newestDayNumber - Int64(dayIndex)
    }

    /// The section key for a day index.
    public func dayKey(forDay dayIndex: Int) -> DayKey {
        DayKey(MockCalendar.isoDate(dayNumber: dayNumber(forDay: dayIndex)))
    }

    /// The index of a day's first (newest) asset.
    public func startIndex(forDay dayIndex: Int) -> Int {
        dayStartIndex[min(max(0, dayIndex), dayStartIndex.count - 1)]
    }

    /// How many assets fall on a day, before any query filter.
    public func count(forDay dayIndex: Int) -> Int {
        guard dayIndex >= 0, dayIndex < dayCount else { return 0 }
        return dayStartIndex[dayIndex + 1] - dayStartIndex[dayIndex]
    }

    /// The half-open index range a day occupies.
    public func indexRange(forDay dayIndex: Int) -> Range<Int> {
        guard dayIndex >= 0, dayIndex < dayCount else { return 0 ..< 0 }
        return dayStartIndex[dayIndex] ..< dayStartIndex[dayIndex + 1]
    }

    /// Which day an asset index falls on — a binary search over the boundary
    /// array, so it is O(log days) rather than a scan.
    public func dayIndex(forAsset assetIndex: Int) -> Int {
        guard assetIndex >= 0, assetIndex < assetCount else { return 0 }
        var low = 0
        var high = dayCount
        while low < high {
            let middle = (low + high) / 2
            if dayStartIndex[middle + 1] <= assetIndex {
                low = middle + 1
            } else {
                high = middle
            }
        }
        return low
    }

    /// Every non-empty day with its unfiltered count, **oldest day first** as
    /// ``LibraryPort/dayCounts(matching:)`` specifies.
    ///
    /// Empty days are omitted rather than reported as zero: a virtualized grid
    /// treats a section as a thing with a header, and a header over nothing is a
    /// rendering bug rather than information.
    public func unfilteredDayCounts() -> [DayCount] {
        var result: [DayCount] = []
        result.reserveCapacity(dayCount)
        for dayIndex in stride(from: dayCount - 1, through: 0, by: -1) {
            let dayTotal = count(forDay: dayIndex)
            guard dayTotal > 0 else { continue }
            result.append(DayCount(dayKey: dayKey(forDay: dayIndex), count: dayTotal))
        }
        return result
    }

    // MARK: Distribution

    /// Slice `assetCount` across the days in proportion to their weights.
    ///
    /// Integer arithmetic on running weights rather than repeated rounding, so
    /// the counts sum to `assetCount` exactly with no drift to reconcile — the
    /// property that lets the aggregate and the paged read agree by
    /// construction rather than by test.
    private static func makeDayStartIndex(profile: MockLibraryProfile) -> [Int] {
        let dayCount = profile.spanDays
        var runningWeight = [Int64](repeating: 0, count: dayCount + 1)
        var total: Int64 = 0
        for dayIndex in 0 ..< dayCount {
            total += Int64(dayWeight(profile: profile, dayIndex: dayIndex))
            runningWeight[dayIndex + 1] = total
        }
        guard total > 0, profile.assetCount > 0 else {
            return [Int](repeating: 0, count: dayCount + 1)
        }
        let assetTotal = Int64(profile.assetCount)
        return runningWeight.map { Int($0 * assetTotal / total) }
    }

    /// One day's relative weight.
    ///
    /// Three multipliers, all of which are visible in a real library's
    /// histogram: a base jitter, a weekend lift, and a summer-and-December
    /// season lift. Roughly one day in twenty-five gets nothing at all, because
    /// real people have days where they take no photographs and a grid that has
    /// never seen a gap has never been tested against one.
    private static func dayWeight(profile: MockLibraryProfile, dayIndex: Int) -> Int {
        let hash = MockHash.value(seed: profile.seed, index: dayIndex, salt: .dayWeight)
        guard !MockHash.occurs(hash, perMille: 40) else { return 0 }
        let base = MockHash.integer(MockHash.mix(hash), in: 30 ... 110)
        let dayNumber = profile.newestDayNumber - Int64(dayIndex)
        let weekendFactor = MockCalendar.isWeekend(dayNumber: dayNumber) ? 21 : 10
        let month = MockCalendar.civil(dayNumber: dayNumber).month
        let seasonFactor = seasonMultiplier(month: month)
        return max(1, base * weekendFactor * seasonFactor / 100)
    }

    /// Northern-hemisphere summer and the December holidays are when the
    /// library actually grows.
    private static func seasonMultiplier(month: Int) -> Int {
        switch month {
        case 6, 7, 8: 16
        case 12: 14
        case 4, 5, 9: 12
        case 1, 2: 8
        default: 10
        }
    }
}
