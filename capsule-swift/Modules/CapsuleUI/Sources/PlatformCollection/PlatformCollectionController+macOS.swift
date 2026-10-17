#if os(macOS)

    import AppKit
    import SwiftUI

    // MARK: - Representable

    extension PlatformCollectionView: NSViewControllerRepresentable {
        public func makeNSViewController(context _: Context) -> PlatformCollectionController<
            SectionID, Item, ItemContent, HeaderContent
        > {
            PlatformCollectionController(itemContent: itemContent, headerContent: headerContent)
        }

        public func updateNSViewController(
            _ controller: PlatformCollectionController<SectionID, Item, ItemContent, HeaderContent>,
            context _: Context
        ) {
            controller.itemContent = itemContent
            controller.headerContent = headerContent
            controller.onSelect = onSelect
            controller.onPrefetch = onPrefetch
            controller.onCancelPrefetch = onCancelPrefetch
            controller.onMagnify = onMagnify
            controller.onLeadingVisibleItem = onLeadingVisibleItem
            controller.onColumnsChange = onColumnsChange
            controller.columns = columns
            controller.update(
                sections: sections,
                layout: layout,
                scrollToSectionID: scrollToSectionID,
                scrollToItem: scrollToItem,
                allowsMultipleSelection: allowsMultipleSelection
            )
        }
    }

    // MARK: - Controller

    /// The `NSCollectionView` half of ``PlatformCollectionView``.
    ///
    /// Mirrors the iOS controller callback for callback: same diffable data
    /// source, same prefetch hooks, same "apply the scroll target once" rule.
    /// Two differences are forced by AppKit and are deliberate:
    ///
    /// * There is no `NSHostingConfiguration`, so each item hosts an
    ///   `NSHostingView` and the SwiftUI content is type-erased through
    ///   `AnyView`. The content itself is still the same view the iPhone renders.
    /// * `NSCollectionView` is not a scroll view, so it is installed as the
    ///   document view of an `NSScrollView` the controller owns.
    public final class PlatformCollectionController<SectionID, Item, ItemContent, HeaderContent>:
        NSViewController
        where SectionID: Hashable & Sendable, Item: Hashable & Sendable,
        ItemContent: View, HeaderContent: View {
        var itemContent: (SectionID, Item) -> ItemContent
        var headerContent: (SectionID) -> HeaderContent
        var onSelect: ((SectionID, Item) -> Void)?
        var onPrefetch: (([Item]) -> Void)?
        var onCancelPrefetch: (([Item]) -> Void)?
        var onMagnify: ((Bool) -> Void)?
        var onLeadingVisibleItem: ((SectionID, Item) -> Void)?
        /// Report a new resting column count chosen by a trackpad magnification.
        var onColumnsChange: ((Int) -> Void)?
        /// The density the caller currently renders — the base a magnification
        /// measures from.
        var columns: Int = PhotoGridZoom.defaultColumns

        private var pinchBaseColumns: Int?
        private var pinchAnchor: IndexPath?

        /// The last item reported through ``onLeadingVisibleItem``, so a scroll
        /// that stays inside one tile reports nothing.
        private var reportedLeadingItem: Item?
        private var layout: PlatformCollectionLayout
        private var sections: [PlatformCollectionSection<SectionID, Item>] = []
        private var scrollToSectionID: SectionID?
        private var appliedScrollTarget: SectionID?
        private var scrollToItem: Item?
        private var appliedScrollItem: Item?
        private var hasAppliedSnapshot = false
        private let bridge = PlatformCollectionBridge()

        private lazy var collectionView: NSCollectionView = {
            let view = NSCollectionView()
            view.collectionViewLayout = PlatformCollectionLayoutBuilder.make(layout)
            view.backgroundColors = [.clear]
            view.isSelectable = true
            view.allowsEmptySelection = true
            view.delegate = bridge
            view.prefetchDataSource = bridge
            view.register(
                PlatformHostingItem.self,
                forItemWithIdentifier: PlatformHostingItem.identifier
            )
            view.register(
                PlatformHostingSupplementaryView.self,
                forSupplementaryViewOfKind: PlatformCollectionLayoutBuilder.headerElementKind,
                withIdentifier: PlatformHostingSupplementaryView.identifier
            )
            return view
        }()

        private lazy var dataSource = makeDataSource()

        init(
            itemContent: @escaping (SectionID, Item) -> ItemContent,
            headerContent: @escaping (SectionID) -> HeaderContent
        ) {
            self.itemContent = itemContent
            self.headerContent = headerContent
            layout = .uniformGrid(columns: 3, itemSpacing: 0, pinnedHeaders: false)
            super.init(nibName: nil, bundle: nil)
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("PlatformCollectionController is not loaded from a nib")
        }

        override public func loadView() {
            let scrollView = NSScrollView()
            scrollView.hasVerticalScroller = true
            scrollView.autohidesScrollers = true
            scrollView.drawsBackground = true
            scrollView.backgroundColor = .windowBackgroundColor
            // A flexible width lets the compositional layout keep resolving its
            // fractional dimensions against the visible width as the window
            // resizes; the height is content-driven, so it stays rigid.
            collectionView.autoresizingMask = [.width]
            scrollView.documentView = collectionView
            view = scrollView
        }

        override public func viewDidLoad() {
            super.viewDidLoad()

            bridge.onSelect = { [weak self] indexPath in self?.handleSelection(at: indexPath) }
            bridge.onPrefetch = { [weak self] indexPaths in
                self?.forwardPrefetch(indexPaths, cancel: false)
            }
            bridge.onCancelPrefetch = { [weak self] indexPaths in
                self?.forwardPrefetch(indexPaths, cancel: true)
            }
            bridge.onMagnifyGesture = { [weak self] recognizer in
                self?.handleMagnification(recognizer)
            }

            // The Mac's pinch is a trackpad magnification, reported as a delta
            // around zero rather than UIKit's scale around one.
            let magnify = NSMagnificationGestureRecognizer(
                target: bridge,
                action: #selector(PlatformCollectionBridge.handleMagnify)
            )
            collectionView.addGestureRecognizer(magnify)

            // AppKit has no scroll-view delegate callback: a scroll is a bounds
            // change on the clip view, and the notification only fires once the
            // clip view is told to post it.
            if let clipView = (view as? NSScrollView)?.contentView {
                clipView.postsBoundsChangedNotifications = true
                NotificationCenter.default.addObserver(
                    forName: NSView.boundsDidChangeNotification,
                    object: clipView,
                    queue: .main
                ) { [weak self] _ in
                    MainActor.assumeIsolated { self?.reportLeadingVisibleItem() }
                }
            }

            applySnapshot(animated: false)
        }

        // MARK: Update

        /// Push fresh content, layout, focus, and selection mode into the grid.
        func update(
            sections newSections: [PlatformCollectionSection<SectionID, Item>],
            layout newLayout: PlatformCollectionLayout,
            scrollToSectionID newTarget: SectionID?,
            scrollToItem newItemTarget: Item?,
            allowsMultipleSelection: Bool
        ) {
            let layoutChanged = newLayout != layout
            sections = newSections
            layout = newLayout
            collectionView.allowsMultipleSelection = allowsMultipleSelection
            // A cleared request must re-arm the target, or drilling back into the
            // same section a second time would silently not scroll.
            if newTarget == nil { appliedScrollTarget = nil }
            scrollToSectionID = newTarget
            if newItemTarget == nil { appliedScrollItem = nil }
            scrollToItem = newItemTarget
            if layoutChanged {
                collectionView.collectionViewLayout = PlatformCollectionLayoutBuilder.make(newLayout)
                restoreAnchorAfterLayoutChange()
            }
            applySnapshot(animated: hasAppliedSnapshot && !layoutChanged)
            applyScrollTargetIfNeeded()
            reportLeadingVisibleItem()
        }

        // MARK: Magnification

        /// Drive the column count from a live trackpad magnification.
        ///
        /// The iOS twin of `handlePinch`, with one spelling difference that
        /// matters: AppKit reports magnification as a *delta around zero* while
        /// UIKit reports a *scale around one*, so the value is normalised before
        /// the shared arithmetic sees it. Getting that wrong makes the Mac zoom
        /// the wrong way, which is why the conversion is written once, here.
        private func handleMagnification(_ recognizer: NSMagnificationGestureRecognizer) {
            let scale = 1 + recognizer.magnification
            switch recognizer.state {
            case .began:
                pinchBaseColumns = columns
                pinchAnchor = anchorIndexPath(near: recognizer.location(in: collectionView))
            case .changed:
                guard let base = pinchBaseColumns else { return }
                if let step = PhotoGridZoom.levelStep(base: base, scale: scale) {
                    recognizer.state = .ended
                    pinchBaseColumns = nil
                    onMagnify?(step)
                    return
                }
                let settled = PhotoGridZoom.settle(
                    PhotoGridZoom.continuousColumns(base: base, scale: scale)
                )
                guard settled != columns else { return }
                onColumnsChange?(settled)
            case .ended, .cancelled, .failed:
                pinchBaseColumns = nil
                pinchAnchor = nil
            default:
                break
            }
        }

        private func anchorIndexPath(near point: CGPoint) -> IndexPath? {
            collectionView.indexPathForItem(at: point)
                ?? collectionView.indexPathsForVisibleItems().min()
        }

        /// Keep the magnified-on photo in view across a column change, so the
        /// reader stays where they were in the library rather than wherever the
        /// old scroll offset now points.
        private func restoreAnchorAfterLayoutChange() {
            guard let anchor = pinchAnchor,
                  anchor.section < collectionView.numberOfSections,
                  anchor.item < collectionView.numberOfItems(inSection: anchor.section)
            else { return }
            collectionView.scrollToItems(at: [anchor], scrollPosition: .centeredVertically)
        }

        /// Report the topmost visible item, when it is not the one last
        /// reported. The iOS twin of this, callback for callback.
        private func reportLeadingVisibleItem() {
            guard let onLeadingVisibleItem else { return }
            guard let indexPath = collectionView.indexPathsForVisibleItems().min(),
                  let item = dataSource.itemIdentifier(for: indexPath),
                  let sectionID = sectionID(at: indexPath.section)
            else { return }
            guard item != reportedLeadingItem else { return }
            reportedLeadingItem = item
            onLeadingVisibleItem(sectionID, item)
        }

        private func applySnapshot(animated: Bool) {
            var snapshot = NSDiffableDataSourceSnapshot<SectionID, Item>()
            snapshot.appendSections(sections.map(\.id))
            for section in sections {
                snapshot.appendItems(section.items, toSection: section.id)
            }
            dataSource.apply(snapshot, animatingDifferences: animated)
            hasAppliedSnapshot = true
        }

        /// Scroll a freshly-targeted section or item to the top, once per
        /// distinct request. The iOS twin of this, resolution rule for
        /// resolution rule.
        private func applyScrollTargetIfNeeded() {
            if let target = scrollToSectionID, target != appliedScrollTarget,
               let index = sections.firstIndex(where: { $0.id == target }) {
                appliedScrollTarget = target
                scroll(to: IndexPath(item: 0, section: index))
                return
            }
            if let item = scrollToItem, item != appliedScrollItem {
                appliedScrollItem = item
                guard let indexPath = dataSource.indexPath(for: item) else { return }
                scroll(to: indexPath)
            }
        }

        private func scroll(to indexPath: IndexPath) {
            Task { @MainActor [weak self] in
                guard let self, indexPath.section < collectionView.numberOfSections else { return }
                collectionView.scrollToItems(at: [indexPath], scrollPosition: .top)
            }
        }

        private func makeDataSource() -> NSCollectionViewDiffableDataSource<SectionID, Item> {
            let source = NSCollectionViewDiffableDataSource<SectionID, Item>(
                collectionView: collectionView
            ) { [weak self] view, indexPath, item in
                let element = view.makeItem(
                    withIdentifier: PlatformHostingItem.identifier, for: indexPath
                )
                guard let self, let hosting = element as? PlatformHostingItem,
                      let sectionID = sectionID(at: indexPath.section)
                else {
                    return element
                }
                hosting.host(AnyView(itemContent(sectionID, item)))
                return element
            }
            source.supplementaryViewProvider = { [weak self] view, kind, indexPath in
                let element = view.makeSupplementaryView(
                    ofKind: kind,
                    withIdentifier: PlatformHostingSupplementaryView.identifier,
                    for: indexPath
                )
                guard let hosting = element as? PlatformHostingSupplementaryView else {
                    // The registered class is ours, so this cannot happen in
                    // practice; returning nil is still better than trapping in a
                    // scroll-time callback.
                    return element as? (NSView & NSCollectionViewElement)
                }
                if let self, let sectionID = sectionID(at: indexPath.section) {
                    hosting.host(AnyView(headerContent(sectionID)))
                }
                return hosting
            }
            return source
        }

        // MARK: Callbacks

        private func handleSelection(at indexPath: IndexPath) {
            guard let item = dataSource.itemIdentifier(for: indexPath),
                  let sectionID = sectionID(at: indexPath.section) else { return }
            onSelect?(sectionID, item)
        }

        /// The section identifier at `index`.
        ///
        /// Resolved from the controller's own copy of the sections rather than
        /// from the data source: `sections` is assigned before every snapshot
        /// apply, so it always agrees with the index paths the collection asks
        /// about, and it needs no API that only one of the two frameworks has.
        private func sectionID(at index: Int) -> SectionID? {
            guard index >= 0, index < sections.count else { return nil }
            return sections[index].id
        }

        private func forwardPrefetch(_ indexPaths: [IndexPath], cancel: Bool) {
            let items = indexPaths.compactMap { dataSource.itemIdentifier(for: $0) }
            guard !items.isEmpty else { return }
            if cancel { onCancelPrefetch?(items) } else { onPrefetch?(items) }
        }
    }

    // MARK: - Bridge

    /// The non-generic `NSObject` that carries every `@objc` entry point the
    /// generic controller cannot declare: the delegate, the prefetch source, and
    /// the magnification action.
    final class PlatformCollectionBridge: NSObject, NSCollectionViewDelegate,
        NSCollectionViewPrefetching {
        var onSelect: ((IndexPath) -> Void)?
        var onPrefetch: (([IndexPath]) -> Void)?
        var onCancelPrefetch: (([IndexPath]) -> Void)?
        var onMagnify: ((CGFloat) -> Void)?
        var onMagnifyGesture: ((NSMagnificationGestureRecognizer) -> Void)?

        func collectionView(_ collectionView: NSCollectionView, didSelectItemsAt indexPaths: Set<IndexPath>) {
            // Selection visuals are owned by the hosted SwiftUI content, so the
            // collection's own selection is released straight away.
            collectionView.deselectItems(at: indexPaths)
            for indexPath in indexPaths.sorted() {
                onSelect?(indexPath)
            }
        }

        func collectionView(_: NSCollectionView, prefetchItemsAt indexPaths: [IndexPath]) {
            onPrefetch?(indexPaths)
        }

        func collectionView(_: NSCollectionView, cancelPrefetchingForItemsAt indexPaths: [IndexPath]) {
            onCancelPrefetch?(indexPaths)
        }

        @objc func handleMagnify(_ recognizer: NSMagnificationGestureRecognizer) {
            onMagnifyGesture?(recognizer)
        }
    }

#endif
