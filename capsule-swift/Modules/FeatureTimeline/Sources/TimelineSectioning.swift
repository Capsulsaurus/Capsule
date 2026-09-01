import AssetKit
import CapsuleUI
import Foundation

/// Groups a chronological asset list into the dated sections the grid renders.
///
/// Pure and deterministic — no I/O, no shared state — so the timeline's
/// day-bucketing is exhaustively unit-testable.
///
/// Assets are *expected* newest-first (the contract of
/// ``AssetProvider/loadTimeline()``), but bucketing does **not** assume it. An
/// earlier version grouped consecutive runs of the same day, which meant an
/// input where one day appeared in two non-adjacent runs produced two sections
/// with the same id — and `UICollectionViewDiffableDataSource` treats duplicate
/// section identifiers as a programmer error and raises, so a merely
/// out-of-order timeline crashed the app on launch rather than rendering
/// slightly oddly. Coalescing by key costs one dictionary and makes the failure
/// unrepresentable.
public enum TimelineSectioning {
    /// Bucket `assets` into one ``PhotoGridSection`` per capture day.
    ///
    /// Section order follows each day's **first** appearance in `assets`, so a
    /// correctly sorted input is unaffected; within a section, assets keep their
    /// input order.
    public static func sections(
        from assets: [Asset],
        calendar: Calendar = .current,
        referenceDate: Date = .now
    ) -> [PhotoGridSection] {
        var order: [String] = []
        var buckets: [String: (day: Date, assets: [Asset])] = [:]

        for asset in assets {
            let day = calendar.startOfDay(for: asset.captureDate)
            let key = dayKey(day, calendar: calendar)
            if buckets[key] == nil {
                order.append(key)
                buckets[key] = (day, [])
            }
            buckets[key]?.assets.append(asset)
        }

        return order.compactMap { key in
            guard let bucket = buckets[key], !bucket.assets.isEmpty else { return nil }
            return PhotoGridSection(
                id: key,
                title: dayTitle(bucket.day, calendar: calendar, referenceDate: referenceDate),
                assets: bucket.assets
            )
        }
    }

    /// The whole timeline as **one** unsectioned run — the All Photos level.
    ///
    /// Apple Photos' library grid is a single uninterrupted field of tiles: no
    /// day headers, no gaps, and no ragged last row where one day ends and the
    /// next begins. Sectioning by day is what the Days level is *for*, and doing
    /// it in All Photos as well meant the app had two views of the same shape
    /// and neither was the continuous one.
    ///
    /// It is also, measurably, the cheaper shape. Resolving a
    /// `UICollectionViewCompositionalLayout` over 250 000 assets costs 8.7 ms as
    /// one uniform section and 426 ms as 3 650 day sections — the boundaries are
    /// the expense, not the tiles. See `UniformGridLayoutTests`.
    ///
    /// The section keeps an empty title: nothing renders it, because the level
    /// that uses this run draws no headers. Where the reader is in time is
    /// reported from the topmost visible tile instead.
    public static func uniformSection(from assets: [Asset]) -> [PhotoGridSection] {
        guard !assets.isEmpty else { return [] }
        return [PhotoGridSection(id: allPhotosSectionID, title: "", assets: assets)]
    }

    /// The identity of the single All Photos section.
    ///
    /// Not a day key, and deliberately not one a day key could ever collide
    /// with: a diffable data source raises on duplicate section identifiers, and
    /// the drill-down path matches day sections by `hasPrefix`.
    public static let allPhotosSectionID = "all-photos"

    /// A stable `yyyy-MM-dd` key for a day.
    static func dayKey(_ day: Date, calendar: Calendar) -> String {
        let parts = calendar.dateComponents([.year, .month, .day], from: day)
        return String(format: "%04d-%02d-%02d", parts.year ?? 0, parts.month ?? 0, parts.day ?? 0)
    }

    /// A human header for a day — `Today` / `Yesterday`, else a written date.
    static func dayTitle(_ day: Date, calendar: Calendar, referenceDate: Date) -> String {
        if calendar.isDate(day, inSameDayAs: referenceDate) {
            return String(localized: "app.timeline.section.today")
        }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: referenceDate),
           calendar.isDate(day, inSameDayAs: yesterday) {
            return String(localized: "app.timeline.section.yesterday")
        }
        let sameYear = calendar.component(.year, from: day)
            == calendar.component(.year, from: referenceDate)
        return sameYear
            ? day.formatted(.dateTime.weekday(.abbreviated).month(.wide).day())
            : day.formatted(.dateTime.month(.wide).day().year())
    }

    // MARK: - Aggregation levels (Months / Years)

    /// Bucket `assets` into one section per capture **month**, newest first —
    /// the Months aggregation level. Each section carries a single representative
    /// asset (the newest of that month) and a `Month Year` title; the section id
    /// is `yyyy-MM`, so a day section's id (`yyyy-MM-dd`) is prefixed by it.
    public static func monthSections(
        from assets: [Asset],
        calendar: Calendar = .current
    ) -> [PhotoGridSection] {
        periodSections(from: assets, calendar: calendar, granularity: .month)
    }

    /// Bucket `assets` into one section per capture **year**, newest first — the
    /// Years aggregation level. Each section carries a representative asset and a
    /// `yyyy` title and id.
    public static func yearSections(
        from assets: [Asset],
        calendar: Calendar = .current
    ) -> [PhotoGridSection] {
        periodSections(from: assets, calendar: calendar, granularity: .year)
    }

    /// Coarse calendar buckets a representative card can stand in for.
    private enum Granularity {
        case month
        case year

        var components: Set<Calendar.Component> {
            switch self {
            case .month: [.year, .month]
            case .year: [.year]
            }
        }
    }

    /// One section per period, keeping each period's first-seen asset as its
    /// representative card.
    ///
    /// Coalesces by key rather than by contiguous run, for the same reason
    /// ``sections(from:calendar:referenceDate:)`` does: a period appearing in
    /// two non-adjacent runs would otherwise emit two sections sharing an id,
    /// and a diffable data source raises on duplicate section identifiers. The
    /// representative stays the *first* asset seen for the period, which under
    /// the newest-first input contract is the newest one.
    private static func periodSections(
        from assets: [Asset],
        calendar: Calendar,
        granularity: Granularity
    ) -> [PhotoGridSection] {
        var order: [String] = []
        var representatives: [String: (components: DateComponents, asset: Asset)] = [:]

        for asset in assets {
            let components = calendar.dateComponents(granularity.components, from: asset.captureDate)
            let key = periodKey(components, granularity: granularity)
            if representatives[key] == nil {
                order.append(key)
                representatives[key] = (components, asset)
            }
        }

        let sections: [PhotoGridSection] = order.compactMap { key in
            guard let entry = representatives[key] else { return nil }
            return PhotoGridSection(
                id: key,
                title: periodTitle(entry.components, granularity: granularity, calendar: calendar),
                assets: [entry.asset]
            )
        }
        return sections
    }

    /// A stable id for a period — `yyyy-MM` for months, `yyyy` for years.
    private static func periodKey(_ comps: DateComponents, granularity: Granularity) -> String {
        switch granularity {
        case .month: String(format: "%04d-%02d", comps.year ?? 0, comps.month ?? 0)
        case .year: String(format: "%04d", comps.year ?? 0)
        }
    }

    /// A human title for a period — `July 2024` for months, `2024` for years.
    private static func periodTitle(
        _ comps: DateComponents,
        granularity: Granularity,
        calendar: Calendar
    ) -> String {
        switch granularity {
        case .year:
            return comps.year.map { String($0) } ?? "—"
        case .month:
            var dateComponents = DateComponents()
            dateComponents.year = comps.year
            dateComponents.month = comps.month
            dateComponents.day = 1
            guard let date = calendar.date(from: dateComponents) else { return "—" }
            // Format in the calendar's own time zone so a UTC-midnight date is
            // never shifted back a day (and a month) by the local zone.
            let style = Date.FormatStyle(calendar: calendar, timeZone: calendar.timeZone)
                .month(.wide).year()
            return date.formatted(style)
        }
    }
}
