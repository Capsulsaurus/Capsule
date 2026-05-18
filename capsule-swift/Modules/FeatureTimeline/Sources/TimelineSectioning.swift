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
}
