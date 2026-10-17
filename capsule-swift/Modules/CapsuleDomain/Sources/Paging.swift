import Foundation

// MARK: - PageRequest

/// One window into an ordered collection.
///
/// **Everything is paged.** A library is not an array — it is a query result
/// that can be a hundred thousand rows long, and a port that returns
/// `[LibraryAsset]` is a port that will one day materialise a hundred thousand
/// structs to draw twenty of them. Modelling reads as windows makes that
/// mistake unrepresentable at the boundary rather than a discipline every
/// caller has to remember.
public struct PageRequest: Sendable, Equatable, Hashable {
    /// A sensible window for a grid: enough to fill a screen and prefetch a
    /// little past it, small enough to decode quickly.
    public static let defaultLimit = 200

    /// Rows to skip.
    public var offset: Int
    /// Maximum rows to return.
    public var limit: Int

    public init(offset: Int = 0, limit: Int = PageRequest.defaultLimit) {
        self.offset = max(0, offset)
        self.limit = max(0, limit)
    }

    /// The window immediately after this one.
    public var next: PageRequest {
        PageRequest(offset: offset + limit, limit: limit)
    }

    /// The half-open index range this window covers.
    public var range: Range<Int> {
        offset ..< (offset + limit)
    }
}

// MARK: - Page

/// A window of results plus the context needed to ask for the next one.
///
/// ``totalCount`` is optional because it is not always cheap: a smart-album
/// membership count is a full evaluation, and forcing every read to produce one
/// would make paging pointless. A `nil` total means "unknown", never "zero", and
/// a UI must render a count-less page rather than an empty one.
public struct Page<Element: Sendable & Equatable>: Sendable, Equatable {
    /// The rows in this window, in query order.
    public var items: [Element]
    /// The window that produced them.
    public var request: PageRequest
    /// Total rows across every window, when it is cheap to know.
    public var totalCount: Int?

    public init(items: [Element], request: PageRequest, totalCount: Int? = nil) {
        self.items = items
        self.request = request
        self.totalCount = totalCount
    }

    /// An empty final page.
    public static func empty(request: PageRequest, totalCount: Int? = nil) -> Page<Element> {
        Page(items: [], request: request, totalCount: totalCount)
    }

    /// Whether another window is worth requesting.
    ///
    /// A short page is the end of the collection: a source that returns fewer
    /// rows than requested has no more to give. When the total is known it is
    /// used instead, which also covers a source that pads.
    public var hasMore: Bool {
        if let totalCount { return request.offset + items.count < totalCount }
        return items.count == request.limit
    }

    /// The next window, or `nil` at the end.
    public var nextRequest: PageRequest? {
        hasMore ? PageRequest(offset: request.offset + items.count, limit: request.limit) : nil
    }

    /// Map the elements, keeping the window context.
    public func map<T: Sendable & Equatable>(_ transform: (Element) throws -> T) rethrows -> Page<T> {
        try Page<T>(items: items.map(transform), request: request, totalCount: totalCount)
    }
}

// MARK: - DayCount

/// How many assets fall on one UTC day.
///
/// This is the aggregate a **virtualized grid cannot work without**. To place a
/// scroll indicator, size a scrubber, or jump to a date, the grid needs section
/// sizes for the whole library — which it must get without loading the whole
/// library. One small array of `(day, count)` answers all three, and is the
/// reason ``LibraryPort`` exposes it as a first-class read rather than making
/// callers derive it from pages they would have to fetch anyway.
public struct DayCount: Sendable, Equatable, Hashable, Identifiable {
    /// The UTC day.
    public var dayKey: DayKey
    /// Assets captured on it, after the query's filters.
    public var count: Int

    public var id: DayKey { dayKey }

    public init(dayKey: DayKey, count: Int) {
        self.dayKey = dayKey
        self.count = count
    }
}

public extension [DayCount] {
    /// The running offset of each day's first row, in the same order.
    ///
    /// The exact mapping a grid needs to translate a section index into a flat
    /// row offset it can page against.
    var sectionOffsets: [Int] {
        var running = 0
        return map { day in
            defer { running += day.count }
            return running
        }
    }

    /// Total assets across every day.
    var totalCount: Int {
        reduce(0) { $0 + $1.count }
    }
}
