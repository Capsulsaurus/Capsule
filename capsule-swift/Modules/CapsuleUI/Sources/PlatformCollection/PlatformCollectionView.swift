import CoreGraphics
import SwiftUI

// MARK: - PlatformCollectionSection

/// One titled run of items in a ``PlatformCollectionView``.
///
/// Deliberately anaemic: the collection only needs a stable section identity
/// and the item identities inside it. Everything a *view* needs (titles, dates,
/// thumbnails) is looked up by the caller's content closures, which keeps the
/// island free of any knowledge about photos.
public struct PlatformCollectionSection<SectionID: Hashable & Sendable, Item: Hashable & Sendable>:
    Identifiable, Sendable {
    public let id: SectionID
    public let items: [Item]

    public init(id: SectionID, items: [Item]) {
        self.id = id
        self.items = items
    }
}

// MARK: - PlatformCollectionView

/// A SwiftUI collection view backed by `UICollectionView` on iOS/iPadOS and
/// `NSCollectionView` on macOS, with SwiftUI content in every cell.
///
/// This type is the app's single sanctioned UIKit/AppKit island. It exists
/// because SwiftUI's own containers still do not give a photo library what it
/// needs: true cell reuse, first-class prefetch **and cancel** hooks, pinned
/// section headers, and a diffable data source that animates large snapshot
/// diffs without rebuilding the world. Everything above it — including the
/// cells' own contents — is ordinary SwiftUI written once for both platforms.
///
/// The two frameworks are wired to the *same* callbacks:
/// `UICollectionViewDataSourcePrefetching` and `NSCollectionViewPrefetching`
/// both land in ``onPrefetch`` / ``onCancelPrefetch``, and pinch
/// (`UIPinchGestureRecognizer`) and trackpad magnification
/// (`NSMagnificationGestureRecognizer`) both land in ``onMagnify``.
///
/// - Note: the representable conformance is declared per platform, in
///   `PlatformCollectionController+iOS.swift` and
///   `PlatformCollectionController+macOS.swift`, so only those files ever name
///   a UIKit or AppKit type.
public struct PlatformCollectionView<SectionID, Item, ItemContent, HeaderContent>
    where SectionID: Hashable & Sendable, Item: Hashable & Sendable,
    ItemContent: View, HeaderContent: View {
    let sections: [PlatformCollectionSection<SectionID, Item>]
    let layout: PlatformCollectionLayout
    let scrollToSectionID: SectionID?
    let allowsMultipleSelection: Bool
    let onSelect: (SectionID, Item) -> Void
    let onPrefetch: ([Item]) -> Void
    let onCancelPrefetch: ([Item]) -> Void
    let onMagnify: ((Bool) -> Void)?
    let itemContent: (SectionID, Item) -> ItemContent
    let headerContent: (SectionID) -> HeaderContent

    /// - Parameters:
    ///   - sections: the content, in display order. Item identities must be
    ///     unique across the whole snapshot — that is a diffable data source
    ///     requirement, not a Capsule one.
    ///   - layout: how rows are arranged; a change re-lays-out in place.
    ///   - scrollToSectionID: a section to bring to the top, applied once per
    ///     distinct request so a re-render never re-scrolls the user.
    ///   - allowsMultipleSelection: whether the collection tracks more than one
    ///     selected index path (multi-select mode).
    ///   - onSelect: the section and item the user activated.
    ///   - onPrefetch: items about to scroll on screen — warm their caches.
    ///   - onCancelPrefetch: items that scrolled away unseen — drop the warm-up.
    ///   - onMagnify: a discrete zoom step: `true` to zoom in (finer), `false`
    ///     to zoom out (coarser). `nil` disables the gesture entirely.
    ///   - item: the SwiftUI content hosted by each cell.
    ///   - header: the SwiftUI content hosted by each pinned section header;
    ///     never dequeued when the layout has no header.
    public init(
        sections: [PlatformCollectionSection<SectionID, Item>],
        layout: PlatformCollectionLayout,
        scrollToSectionID: SectionID? = nil,
        allowsMultipleSelection: Bool = false,
        onSelect: @escaping (SectionID, Item) -> Void,
        onPrefetch: @escaping ([Item]) -> Void = { _ in },
        onCancelPrefetch: @escaping ([Item]) -> Void = { _ in },
        onMagnify: ((Bool) -> Void)? = nil,
        @ViewBuilder item: @escaping (SectionID, Item) -> ItemContent,
        @ViewBuilder header: @escaping (SectionID) -> HeaderContent
    ) {
        self.sections = sections
        self.layout = layout
        self.scrollToSectionID = scrollToSectionID
        self.allowsMultipleSelection = allowsMultipleSelection
        self.onSelect = onSelect
        self.onPrefetch = onPrefetch
        self.onCancelPrefetch = onCancelPrefetch
        self.onMagnify = onMagnify
        itemContent = item
        headerContent = header
    }
}

// MARK: - PlatformCollectionMagnification

/// Turns a continuous pinch / magnification value into the discrete "one level
/// in, one level out" step the aggregation grid actually reacts to.
///
/// Shared by both platforms so the gesture feels the same, and pure so the
/// thresholds are covered by tests rather than by pinching a simulator.
/// AppKit reports magnification as a *delta* around zero while UIKit reports a
/// *scale* around one; callers normalise to UIKit's spelling before asking.
enum PlatformCollectionMagnification {
    /// The scale above which a pinch counts as "zoom in".
    static let zoomInThreshold: CGFloat = 1.25
    /// The scale below which a pinch counts as "zoom out".
    static let zoomOutThreshold: CGFloat = 0.8

    /// `true` to zoom in, `false` to zoom out, `nil` when the pinch was too
    /// small to mean anything.
    static func step(forScale scale: CGFloat) -> Bool? {
        if scale > zoomInThreshold { return true }
        if scale < zoomOutThreshold { return false }
        return nil
    }
}
