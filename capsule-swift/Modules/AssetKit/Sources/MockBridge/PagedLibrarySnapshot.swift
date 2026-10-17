import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - PagedLibrarySnapshot

/// An ``AssetSnapshot`` over a paged ``LibraryPort``.
///
/// ## The shape mismatch this type exists to absorb
///
/// `AssetSnapshot` promises `count` and synchronous `asset(at:)`. `LibraryPort`
/// promises `async` windows and, deliberately, **no `allAssets()`** — the docs
/// say the one call that materialises the library is the one call that will be
/// made from a view body. So the honest bridge is not "load everything into an
/// array"; it is a snapshot whose size is known up front from a cheap aggregate
/// and whose rows arrive a window at a time.
///
/// Construction therefore costs exactly two aggregate reads —
/// ``LibraryPort/assetCount(matching:)`` and
/// ``LibraryPort/dayCounts(matching:)`` — plus however many windows the caller
/// asks to warm. A 250 000-asset library reports the right `count` after
/// reading no rows at all.
///
/// ## An index whose window has not arrived
///
/// `asset(at:)` is synchronous and non-optional, so there is no way to answer
/// "not yet" in the protocol's vocabulary. Rather than block a scroll on a
/// fetch or return a neighbouring row and lie, the snapshot returns an
/// **explicitly provisional** `Asset` and schedules the window that would
/// answer properly, so the next read of the same index is real.
///
/// A provisional row is provisional in a way the caller can detect and a viewer
/// cannot mistake for content:
///
/// - Its identifier is `.managed(uuid:)` under the reserved
///   ``provisionalIdentifierPrefix``, which no real asset carries and
///   ``isProvisional(_:)`` recognises. It resolves through no provider, so a
///   thumbnail request for it returns `nil` and the tile stays a placeholder.
/// - Its ``Asset/pixelWidth`` and ``Asset/pixelHeight`` are `0`, which
///   ``Asset/aspectRatio`` already reads as "unknown — lay it out square".
/// - Its ``Asset/captureDate`` is **correct**, because the day histogram read
///   at construction knows which day any index falls on. That is the one field
///   worth being right about: a timeline sections on it, so a wrong date would
///   put the row in the wrong month and make the section jump when the real row
///   landed.
///
/// The identifier is unique per index rather than shared, so a `ForEach` over a
/// partially-loaded window does not collapse a screenful of rows into one.
public struct PagedLibrarySnapshot: AssetSnapshot {
    /// Reserved identifier prefix for a row whose window has not arrived.
    public static let provisionalIdentifierPrefix = "capsule.provisional."

    public let count: Int

    private let cache: LibraryPageCache
    private let days: ProvisionalDayIndex

    /// Build a snapshot over an already-read count and histogram.
    ///
    /// Takes them as parameters rather than reading them itself so the type
    /// stays synchronously constructible — which is what lets a test build one
    /// over a fake port without an `async` factory, and lets the provider read
    /// both aggregates concurrently if it ever wants to.
    public init(
        library: any LibraryPort,
        query: TimelineQuery = .default,
        count: Int,
        dayCounts: [DayCount],
        pageSize: Int = PageRequest.defaultLimit,
        residentPageLimit: Int = 40
    ) {
        self.count = max(0, count)
        cache = LibraryPageCache(
            library: library,
            query: query,
            pageSize: pageSize,
            residentPageLimit: residentPageLimit
        )
        days = ProvisionalDayIndex(dayCounts: dayCounts)
    }

    /// The row at `index`, or a provisional stand-in while its window loads.
    ///
    /// - Precondition: `index` is in `0 ..< count`, per the protocol. An index
    ///   outside it still answers rather than trapping, because a grid that
    ///   reads one row past a shrinking library should redraw, not crash.
    public func asset(at index: Int) -> Asset {
        if let loaded = cache.asset(at: index) { return loaded }
        cache.scheduleLoad(containing: index)
        return provisionalAsset(at: index)
    }

    /// Fetch the windows covering the first `assetLimit` rows before returning.
    ///
    /// The provider calls this once, so the first screens of a freshly-loaded
    /// timeline are real rows rather than placeholders that resolve a frame
    /// later. It is a **bounded prefix**, not the whole library: past the limit
    /// the snapshot pages on demand, which is the entire point.
    public func warm(assetLimit: Int) async {
        let rows = min(max(0, assetLimit), count)
        let pages = (rows + cache.pageSize - 1) / cache.pageSize
        await cache.warm(pageCount: min(pages, cache.residentPageLimit))
    }

    /// Whether the row at `index` has arrived — for a caller that wants to
    /// prefetch rather than draw a placeholder, and for tests.
    public func isLoaded(at index: Int) -> Bool {
        cache.isLoaded(containing: index)
    }

    /// Windows currently resident. A residency assertion, not a screen's
    /// business.
    public var loadedPageCount: Int {
        cache.loadedPageCount
    }

    /// Whether an asset is a stand-in rather than a real row.
    public static func isProvisional(_ asset: Asset) -> Bool {
        guard case let .managed(uuid) = asset.id else { return false }
        return uuid.hasPrefix(provisionalIdentifierPrefix)
    }

    // MARK: Private

    private func provisionalAsset(at index: Int) -> Asset {
        Asset(
            id: .managed(uuid: "\(Self.provisionalIdentifierPrefix)\(index)"),
            mediaType: .photo,
            captureDate: days.captureDate(at: index) ?? Date(timeIntervalSince1970: 0)
        )
    }
}

// MARK: - ProvisionalDayIndex

/// Maps a timeline index to the UTC day it falls on, from the day histogram.
///
/// The histogram is the aggregate ``LibraryPort`` already exposes so a
/// virtualized grid can size its sections without loading rows; the same array
/// answers "which day is row 90 000 on?" by prefix sum. That is what lets a
/// provisional row carry a truthful date instead of an epoch sentinel that
/// would section it into 1970.
///
/// Built newest-day-first to match the timeline's own order, since
/// ``LibraryPort/dayCounts(matching:)`` returns oldest first.
struct ProvisionalDayIndex: Sendable {
    /// Exclusive upper row bound of each day, newest day first.
    private let upperBounds: [Int]
    /// Midday UTC on each corresponding day.
    private let dates: [Date]

    init(dayCounts: [DayCount]) {
        var bounds: [Int] = []
        var days: [Date] = []
        var running = 0
        for day in dayCounts.reversed() {
            guard day.count >= 1, let date = Self.middayUTC(of: day.dayKey) else { continue }
            running += day.count
            bounds.append(running)
            days.append(date)
        }
        upperBounds = bounds
        dates = days
    }

    /// The capture date a row at `index` most likely carries, or `nil` when the
    /// histogram does not cover it.
    func captureDate(at index: Int) -> Date? {
        guard index >= 0, let position = position(of: index) else { return nil }
        return dates[position]
    }

    /// Binary search for the first day whose exclusive upper bound passes
    /// `index` — the day that row sections into.
    private func position(of index: Int) -> Int? {
        var low = 0
        var high = upperBounds.count
        while low < high {
            let middle = low + (high - low) / 2
            if upperBounds[middle] <= index {
                low = middle + 1
            } else {
                high = middle
            }
        }
        return low < upperBounds.count ? low : nil
    }

    /// Midday rather than midnight on the day, so a consumer that re-derives a
    /// day key in the *viewer's* timezone still lands on the same date for
    /// every offset short of twelve hours.
    private static func middayUTC(of dayKey: DayKey) -> Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withFullDate, .withDashSeparatorInDate]
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        return formatter.date(from: dayKey.rawValue).map { $0.addingTimeInterval(43200) }
    }
}
