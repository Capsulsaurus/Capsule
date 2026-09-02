import AssetKit
import CapsuleFoundation
import Foundation

/// A preset capture-date window for the search screen.
public enum DateRangeOption: String, CaseIterable, Sendable, Equatable, Identifiable {
    case anytime
    case today
    case thisWeek
    case thisMonth
    case thisYear

    public var id: String { rawValue }

    /// The label shown in the filter control, localized. See
    /// ``SearchSuggestion/title`` for why this resolves rather than staying a
    /// key.
    public var title: String {
        String(localized: String.LocalizationValue(titleKey))
    }

    /// The catalog key for the label shown in the filter control.
    ///
    /// A key, not the text. Returned through a variable, English here was
    /// invisible to `i18n-guard` and shipped untranslated to every locale.
    public var titleKey: String {
        switch self {
        case .anytime: "app.search.filter.date.anytime"
        case .today: "app.search.filter.date.today"
        case .thisWeek: "app.search.filter.date.this_week"
        case .thisMonth: "app.search.filter.date.this_month"
        case .thisYear: "app.search.filter.date.this_year"
        }
    }

    /// The concrete date window, or `nil` for ``anytime``.
    func range(referenceDate: Date, calendar: Calendar) -> ClosedRange<Date>? {
        let component: Calendar.Component? = switch self {
        case .anytime: nil
        case .today: .day
        case .thisWeek: .weekOfYear
        case .thisMonth: .month
        case .thisYear: .year
        }
        guard let component,
              let interval = calendar.dateInterval(of: component, for: referenceDate)
        else {
            return nil
        }
        return interval.start ... max(interval.start, interval.end)
    }
}

/// The active search facets — media type and a capture-date window.
///
/// Pure and `Equatable`; ``matches(_:referenceDate:calendar:)`` is the whole
/// of search's filtering logic, so it is exhaustively unit-testable.
public struct SearchFilter: Equatable, Sendable {
    /// Restrict to a single media type, or `nil` for any.
    public var mediaType: MediaType?
    /// The capture-date window.
    public var dateRange: DateRangeOption

    public init(mediaType: MediaType? = nil, dateRange: DateRangeOption = .anytime) {
        self.mediaType = mediaType
        self.dateRange = dateRange
    }

    /// Whether any facet narrows the results.
    public var isActive: Bool {
        mediaType != nil || dateRange != .anytime
    }

    /// Whether `asset` passes every active facet.
    public func matches(
        _ asset: Asset,
        referenceDate: Date = .now,
        calendar: Calendar = .current
    ) -> Bool {
        if let mediaType, asset.mediaType != mediaType {
            return false
        }
        if let range = dateRange.range(referenceDate: referenceDate, calendar: calendar),
           !range.contains(asset.captureDate) {
            return false
        }
        return true
    }
}
