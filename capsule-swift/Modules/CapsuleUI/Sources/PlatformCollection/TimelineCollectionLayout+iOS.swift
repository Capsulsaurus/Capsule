#if os(iOS)

    import CoreGraphics
    import UIKit

    // MARK: - TimelineCollectionLayout

    /// A `UICollectionViewLayout` whose every answer comes from
    /// ``TimelineLayout``'s prefix sums.
    ///
    /// The conventional subclass builds a dictionary of
    /// `UICollectionViewLayoutAttributes` in `prepare()` by walking every item,
    /// then serves queries from it. At a quarter of a million assets that is a
    /// quarter of a million objects allocated before the first row is visible,
    /// tens of megabytes held for the life of the screen, and the whole thing
    /// rebuilt on every rotation.
    ///
    /// This one caches nothing per item and iterates nothing per item:
    ///
    /// - ``collectionViewContentSize`` is a prefix sum. It is **exact from the
    ///   first frame**, before a single asset has loaded, so the scrollbar never
    ///   lies and the content never jumps under the user's thumb as pages arrive.
    /// - ``layoutAttributesForElements(in:)`` is a binary search plus one frame
    ///   computation per *visible* item — a few dozen, whether the user is at the
    ///   top of the grid or ten years down it.
    /// - `prepare()` rebuilds only when the width or the day aggregate actually
    ///   changed, and that rebuild is O(sections): a few thousand rows for a
    ///   decade of photos, not a few hundred thousand.
    ///
    /// The maths itself lives in ``TimelineAttributeSolver``, outside the island
    /// and free of UIKit, so it is verified by tests on either platform. This
    /// class only copies placements into UIKit's attribute objects.
    final class TimelineCollectionLayout: UICollectionViewLayout {
        /// The pinned header's z-order. Anything above the cells' default `0`
        /// works; a named constant keeps the two platforms in step.
        private static let headerZIndex = 10

        /// The day aggregate and metrics. Assigning a different value schedules a
        /// geometry rebuild — the *only* thing that costs O(sections).
        var geometry: TimelineGridGeometry {
            didSet {
                guard geometry != oldValue else { return }
                needsRebuild = true
                invalidateLayout()
            }
        }

        /// The resolved geometry every query is answered from.
        private(set) var resolved: TimelineLayout

        private var resolvedWidth: CGFloat = -1
        private var needsRebuild = true

        init(geometry: TimelineGridGeometry) {
            self.geometry = geometry
            resolved = geometry.layout(forWidth: 0)
            super.init()
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("TimelineCollectionLayout is not loaded from a nib")
        }

        // MARK: Geometry

        override func prepare() {
            super.prepare()
            let width = collectionView?.bounds.width ?? 0
            guard needsRebuild || width != resolvedWidth else { return }
            resolvedWidth = width
            resolved = geometry.layout(forWidth: width)
            needsRebuild = false
        }

        override var collectionViewContentSize: CGSize {
            CGSize(width: max(0, resolvedWidth), height: resolved.totalContentHeight)
        }

        // MARK: Attributes

        override func layoutAttributesForElements(in rect: CGRect) -> [UICollectionViewLayoutAttributes]? {
            var attributes = TimelineAttributeSolver
                .itemPlacements(in: rect, geometry: resolved)
                .map(Self.attributes(forItem:))
            attributes.append(contentsOf: TimelineAttributeSolver.headerPlacements(
                in: rect,
                width: max(0, resolvedWidth),
                contentTop: contentTop,
                pinsHeaders: geometry.showsHeaders,
                geometry: resolved
            ).map(Self.attributes(forHeader:)))
            return attributes
        }

        override func layoutAttributesForItem(at indexPath: IndexPath) -> UICollectionViewLayoutAttributes? {
            TimelineAttributeSolver
                .itemPlacement(section: indexPath.section, item: indexPath.item, geometry: resolved)
                .map(Self.attributes(forItem:))
        }

        override func layoutAttributesForSupplementaryView(
            ofKind elementKind: String,
            at indexPath: IndexPath
        ) -> UICollectionViewLayoutAttributes? {
            guard elementKind == UICollectionView.elementKindSectionHeader else { return nil }
            return TimelineAttributeSolver.headerPlacement(
                section: indexPath.section,
                width: max(0, resolvedWidth),
                contentTop: contentTop,
                pinsHeaders: geometry.showsHeaders,
                geometry: resolved
            ).map(Self.attributes(forHeader:))
        }

        // MARK: Invalidation

        override func shouldInvalidateLayout(forBoundsChange newBounds: CGRect) -> Bool {
            // A width change moves every tile: the tile side is *derived* from the
            // width so the trailing column stays flush, so a rotation or a window
            // resize is a genuine geometry change and not a reflow.
            if abs(newBounds.width - resolvedWidth) > 0.5 { return true }
            // A scroll moves nothing except a pinned header — but it does move
            // that, on every frame, which is what pinning means.
            return geometry.showsHeaders
        }

        override func invalidationContext(
            forBoundsChange newBounds: CGRect
        ) -> UICollectionViewLayoutInvalidationContext {
            let context = super.invalidationContext(forBoundsChange: newBounds)
            guard abs(newBounds.width - resolvedWidth) <= 0.5 else {
                needsRebuild = true
                return context
            }
            // Scroll only. Item frames are unchanged, so invalidating them would
            // throw away correct attributes every frame; only the headers re-pin.
            let sections = TimelineAttributeSolver.sectionRange(intersecting: newBounds, geometry: resolved)
            guard !sections.isEmpty else { return context }
            context.invalidateSupplementaryElements(
                ofKind: UICollectionView.elementKindSectionHeader,
                at: sections.map { IndexPath(item: 0, section: $0) }
            )
            return context
        }

        // MARK: Helpers

        /// The top of the visible content, in content coordinates — where a
        /// pinned header comes to rest.
        private var contentTop: CGFloat {
            guard let collectionView else { return 0 }
            return collectionView.bounds.minY + collectionView.adjustedContentInset.top
        }

        private static func attributes(forItem placement: TimelineItemPlacement) -> UICollectionViewLayoutAttributes {
            let attributes = UICollectionViewLayoutAttributes(
                forCellWith: IndexPath(item: placement.item, section: placement.section)
            )
            attributes.frame = placement.frame
            return attributes
        }

        private static func attributes(
            forHeader placement: TimelineHeaderPlacement
        ) -> UICollectionViewLayoutAttributes {
            let attributes = UICollectionViewLayoutAttributes(
                forSupplementaryViewOfKind: UICollectionView.elementKindSectionHeader,
                with: IndexPath(item: 0, section: placement.section)
            )
            attributes.frame = placement.frame
            attributes.zIndex = headerZIndex
            return attributes
        }
    }

#endif
