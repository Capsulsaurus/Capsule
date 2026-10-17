#if os(macOS)

    import AppKit
    import CoreGraphics

    // MARK: - TimelineCollectionLayout

    /// The `NSCollectionViewLayout` twin of the iOS layout, answering from the
    /// same ``TimelineAttributeSolver``.
    ///
    /// Everything said about the iOS class holds here: no per-item preparation,
    /// no cached attribute dictionary, an exact content height before anything
    /// loads. Only AppKit's spellings differ, and they differ in four places —
    /// which is precisely why the arithmetic lives outside both files:
    ///
    /// * `NSCollectionViewLayoutAttributes` rather than the UIKit class.
    /// * ``layoutAttributesForElements(in:)`` returns a non-optional array.
    /// * `invalidateSupplementaryElements(ofKind:at:)` takes a `Set`, not an
    ///   `Array`.
    /// * There is no adjusted content inset; the Mac reads the scroll view's
    ///   `visibleRect` instead.
    final class TimelineCollectionLayout: NSCollectionViewLayout {
        /// The pinned header's z-order, matching iOS.
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

        override var collectionViewContentSize: NSSize {
            NSSize(width: max(0, resolvedWidth), height: resolved.totalContentHeight)
        }

        // MARK: Attributes

        override func layoutAttributesForElements(in rect: NSRect) -> [NSCollectionViewLayoutAttributes] {
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

        override func layoutAttributesForItem(at indexPath: IndexPath) -> NSCollectionViewLayoutAttributes? {
            TimelineAttributeSolver
                .itemPlacement(section: indexPath.section, item: indexPath.item, geometry: resolved)
                .map(Self.attributes(forItem:))
        }

        override func layoutAttributesForSupplementaryView(
            ofKind elementKind: NSCollectionView.SupplementaryElementKind,
            at indexPath: IndexPath
        ) -> NSCollectionViewLayoutAttributes? {
            guard elementKind == NSCollectionView.elementKindSectionHeader else { return nil }
            return TimelineAttributeSolver.headerPlacement(
                section: indexPath.section,
                width: max(0, resolvedWidth),
                contentTop: contentTop,
                pinsHeaders: geometry.showsHeaders,
                geometry: resolved
            ).map(Self.attributes(forHeader:))
        }

        // MARK: Invalidation

        override func shouldInvalidateLayout(forBoundsChange newBounds: NSRect) -> Bool {
            // A width change moves every tile: the tile side is *derived* from the
            // width so the trailing column stays flush, so a live window resize is
            // a genuine geometry change and not a reflow.
            if abs(newBounds.width - resolvedWidth) > 0.5 { return true }
            // A scroll moves nothing except a pinned header — but it does move
            // that, on every frame, which is what pinning means.
            return geometry.showsHeaders
        }

        override func invalidationContext(
            forBoundsChange newBounds: NSRect
        ) -> NSCollectionViewLayoutInvalidationContext {
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
                ofKind: NSCollectionView.elementKindSectionHeader,
                at: Set(sections.map { IndexPath(item: 0, section: $0) })
            )
            return context
        }

        // MARK: Helpers

        /// The top of the visible content, in content coordinates — where a
        /// pinned header comes to rest.
        ///
        /// `NSCollectionView` is the scroll view's *document view* and is flipped,
        /// so its own `visibleRect` is already in the coordinate space the
        /// timeline computes in.
        private var contentTop: CGFloat {
            collectionView?.visibleRect.minY ?? 0
        }

        private static func attributes(forItem placement: TimelineItemPlacement) -> NSCollectionViewLayoutAttributes {
            let attributes = NSCollectionViewLayoutAttributes(
                forItemWith: IndexPath(item: placement.item, section: placement.section)
            )
            attributes.frame = placement.frame
            return attributes
        }

        private static func attributes(
            forHeader placement: TimelineHeaderPlacement
        ) -> NSCollectionViewLayoutAttributes {
            let attributes = NSCollectionViewLayoutAttributes(
                forSupplementaryViewOfKind: NSCollectionView.elementKindSectionHeader,
                with: IndexPath(item: 0, section: placement.section)
            )
            attributes.frame = placement.frame
            attributes.zIndex = headerZIndex
            return attributes
        }
    }

#endif
