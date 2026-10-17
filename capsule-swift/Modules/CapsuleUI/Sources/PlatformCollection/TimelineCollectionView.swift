import CoreGraphics
import SwiftUI

// MARK: - TimelineCollectionView

/// The SwiftUI face of the **virtualized** collection island.
///
/// ``PlatformCollectionView`` next door is diffable-data-source-backed: every
/// item identity goes into a snapshot. That is the right shape for an album of
/// four hundred photos and the wrong shape for a library of 250 000, where
/// building the snapshot is itself the thing that must not happen. This one
/// therefore takes **no items at all** — only the day aggregate — and asks its
/// callbacks for content by global index as cells are dequeued.
///
/// What it gives the caller back is the one fact the window store needs: which
/// global indices are on screen, and how many fit in a screenful.
///
/// - Note: the representable conformance is declared per platform, in
///   `TimelineCollectionController+iOS.swift` and
///   `TimelineCollectionController+macOS.swift`, so only those files ever name a
///   UIKit or AppKit type.
struct TimelineCollectionView<ItemContent, HeaderContent>
    where ItemContent: View, HeaderContent: View {
    /// The day aggregate and the metrics to lay it out with.
    let geometry: TimelineGridGeometry
    /// Whether the collection tracks more than one selected index path.
    let allowsMultipleSelection: Bool
    /// The global index the user activated.
    let onSelect: (Int) -> Void
    /// The visible global index range, and how many items a screenful holds.
    /// Called on every scroll that changes the answer, and never when it does
    /// not.
    let onVisibleRangeChange: (Range<Int>, Int) -> Void
    /// Global indices about to scroll on screen — warm their caches.
    let onPrefetch: ([Int]) -> Void
    /// Global indices that scrolled away unseen — drop the warm-up.
    let onCancelPrefetch: ([Int]) -> Void
    /// A discrete zoom step: `true` finer, `false` coarser. `nil` disables the
    /// gesture.
    let onMagnify: ((Bool) -> Void)?
    /// The SwiftUI content for the cell at a global index.
    let itemContent: (Int) -> ItemContent
    /// The SwiftUI content for a section's pinned header.
    let headerContent: (Int) -> HeaderContent
}
