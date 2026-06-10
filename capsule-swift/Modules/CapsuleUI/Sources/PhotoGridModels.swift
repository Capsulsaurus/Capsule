import AssetKit
import Foundation

/// One titled run of assets in the grid — typically a single capture day, or a
/// single representative asset for an aggregation level (a month or a year).
public struct PhotoGridSection: Identifiable, Sendable {
    /// A stable section identifier (e.g. the day key `2024-07-03`, the month key
    /// `2024-07`, or the year key `2024`).
    public let id: String
    /// The header text shown for the section.
    public let title: String
    /// The section's assets, in display order.
    public let assets: [Asset]

    public init(id: String, title: String, assets: [Asset]) {
        self.id = id
        self.title = title
        self.assets = assets
    }
}

/// How the grid lays out its sections.
public enum PhotoGridStyle: Equatable, Sendable {
    /// Uniform square tiles, `columns` per row — the All Photos / album grid.
    case uniform(columns: Int)
    /// One large representative card per section, full width, with the section
    /// title overlaid — the Years and Months aggregation levels.
    case cards
}
