import Foundation

// MARK: - MockCalendar

/// Civil-date arithmetic over UTC day numbers, with no `Calendar` and no
/// `DateFormatter`.
///
/// The synthetic library's shape depends on the calendar — more photos at
/// weekends and in summer — so the derivation has to know what day of the week
/// and what month an epoch day falls on. Going through `Calendar` would make
/// that answer depend on the *runner's* locale and time zone, and a fixture
/// whose contents change when a test machine is set to Tokyo is not a fixture.
///
/// The conversion is Howard Hinnant's `civil_from_days`, which is exact for the
/// proleptic Gregorian calendar over the whole `Int64` range and is pure integer
/// arithmetic.
public enum MockCalendar {
    /// A proleptic-Gregorian calendar date.
    ///
    /// A named type rather than a tuple because three anonymous `Int`s at a call
    /// site is exactly the shape that gets destructured in the wrong order once.
    public struct CivilDate: Sendable, Equatable, Hashable {
        public var year: Int
        public var month: Int
        public var day: Int

        public init(year: Int, month: Int, day: Int) {
            self.year = year
            self.month = month
            self.day = day
        }
    }

    /// Seconds in a UTC day. No leap seconds anywhere in this layer — the
    /// domain's own ``DayKey`` floors arithmetically for the same reason.
    public static let secondsPerDay: Int64 = 86400

    /// The UTC day number containing an instant, floored toward negative
    /// infinity so pre-1970 instants land on the right day.
    public static func dayNumber(epochSeconds: Int64) -> Int64 {
        let quotient = epochSeconds / secondsPerDay
        return epochSeconds % secondsPerDay < 0 ? quotient - 1 : quotient
    }

    /// Midnight UTC of a day number.
    public static func startOfDay(dayNumber: Int64) -> Int64 {
        dayNumber * secondsPerDay
    }

    /// The `(year, month, day)` of a UTC day number.
    public static func civil(dayNumber: Int64) -> CivilDate {
        let shifted = dayNumber + 719_468
        let era = (shifted >= 0 ? shifted : shifted - 146_096) / 146_097
        let dayOfEra = shifted - era * 146_097
        let yearOfEra = (dayOfEra - dayOfEra / 1460 + dayOfEra / 36524 - dayOfEra / 146_096) / 365
        let year = yearOfEra + era * 400
        let dayOfYear = dayOfEra - (365 * yearOfEra + yearOfEra / 4 - yearOfEra / 100)
        let monthProxy = (5 * dayOfYear + 2) / 153
        let day = dayOfYear - (153 * monthProxy + 2) / 5 + 1
        let month = monthProxy < 10 ? monthProxy + 3 : monthProxy - 9
        return CivilDate(year: Int(month <= 2 ? year + 1 : year), month: Int(month), day: Int(day))
    }

    /// Day of the week, `0` = Sunday. 1970-01-01 was a Thursday, hence the `+4`.
    public static func weekday(dayNumber: Int64) -> Int {
        Int(((dayNumber % 7) + 11) % 7)
    }

    /// Whether the day is a Saturday or a Sunday — when people take more
    /// photographs, which is the whole reason this file exists.
    public static func isWeekend(dayNumber: Int64) -> Bool {
        let weekdayIndex = weekday(dayNumber: dayNumber)
        return weekdayIndex == 0 || weekdayIndex == 6
    }

    /// `YYYY-MM-DD` for a day number, rendered without a formatter so it cannot
    /// pick up a locale's numbering system.
    public static func isoDate(dayNumber: Int64) -> String {
        let parts = civil(dayNumber: dayNumber)
        return "\(padded(parts.year, width: 4))-\(padded(parts.month, width: 2))-\(padded(parts.day, width: 2))"
    }

    private static func padded(_ value: Int, width: Int) -> String {
        let text = String(value)
        guard text.count < width else { return text }
        return String(repeating: "0", count: width - text.count) + text
    }
}
