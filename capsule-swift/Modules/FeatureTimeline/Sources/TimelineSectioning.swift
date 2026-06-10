import AssetKit
import CapsuleUI
import Foundation

/// Groups a chronological asset list into the dated sections the grid renders.
///
/// Pure and deterministic — no I/O, no shared state — so the timeline's
/// day-bucketing is exhaustively unit-testable. Input assets are assumed to be
/// in newest-first order (the contract of ``AssetProvider/loadTimeline()``), so
/// a single linear pass groups each run of same-day assets.
public enum TimelineSectioning {
    /// Bucket `assets` into one ``PhotoGridSection`` per capture day.
    public static func sections(
        from assets: [Asset],
        calendar: Calendar = .current,
        referenceDate: Date = .now
    ) -> [PhotoGridSection] {
        var sections: [PhotoGridSection] = []
        var dayStart: Date?
        var bucket: [Asset] = []

        func flush() {
            guard let day = dayStart, !bucket.isEmpty else { return }
            sections.append(PhotoGridSection(
                id: dayKey(day, calendar: calendar),
                title: dayTitle(day, calendar: calendar, referenceDate: referenceDate),
                assets: bucket
            ))
            bucket.removeAll(keepingCapacity: true)
        }

        for asset in assets {
            let day = calendar.startOfDay(for: asset.captureDate)
            if day != dayStart {
                flush()
                dayStart = day
            }
            bucket.append(asset)
        }
        flush()
        return sections
    }

    /// A stable `yyyy-MM-dd` key for a day.
    static func dayKey(_ day: Date, calendar: Calendar) -> String {
        let parts = calendar.dateComponents([.year, .month, .day], from: day)
        return String(format: "%04d-%02d-%02d", parts.year ?? 0, parts.month ?? 0, parts.day ?? 0)
    }

    /// A human header for a day — `Today` / `Yesterday`, else a written date.
    static func dayTitle(_ day: Date, calendar: Calendar, referenceDate: Date) -> String {
        if calendar.isDate(day, inSameDayAs: referenceDate) {
            return "Today"
        }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: referenceDate),
           calendar.isDate(day, inSameDayAs: yesterday) {
            return "Yesterday"
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

    /// A single linear pass picking the newest asset of each contiguous period.
    /// Relies on the newest-first input contract, so a period's assets form one
    /// run and its first (newest) asset is the representative.
    private static func periodSections(
        from assets: [Asset],
        calendar: Calendar,
        granularity: Granularity
    ) -> [PhotoGridSection] {
        var sections: [PhotoGridSection] = []
        var currentKey: DateComponents?
        var representative: Asset?

        func flush() {
            guard let currentKey, let representative else { return }
            sections.append(PhotoGridSection(
                id: periodKey(currentKey, granularity: granularity),
                title: periodTitle(currentKey, granularity: granularity, calendar: calendar),
                assets: [representative]
            ))
        }

        for asset in assets {
            let key = calendar.dateComponents(granularity.components, from: asset.captureDate)
            if key != currentKey {
                flush()
                currentKey = key
                representative = asset
            }
        }
        flush()
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
