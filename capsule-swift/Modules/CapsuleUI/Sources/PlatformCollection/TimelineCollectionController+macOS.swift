#if os(macOS)

    import AppKit
    import SwiftUI

    // MARK: - Representable

    extension TimelineCollectionView: NSViewControllerRepresentable {
        func makeNSViewController(context _: Context) -> TimelineCollectionController<ItemContent, HeaderContent> {
            TimelineCollectionController(
                geometry: geometry,
                itemContent: itemContent,
                headerContent: headerContent
            )
        }

        func updateNSViewController(
            _ controller: TimelineCollectionController<ItemContent, HeaderContent>,
            context _: Context
        ) {
            controller.itemContent = itemContent
            controller.headerContent = headerContent
            controller.onSelect = onSelect
            controller.onVisibleRangeChange = onVisibleRangeChange
            controller.onPrefetch = onPrefetch
            controller.onCancelPrefetch = onCancelPrefetch
            controller.onMagnify = onMagnify
            controller.update(geometry: geometry, allowsMultipleSelection: allowsMultipleSelection)
        }
    }

    // MARK: - Controller

    /// The `NSCollectionView` half of ``TimelineCollectionView``.
    ///
    /// Mirrors the iOS controller callback for callback — same plain (non-
    /// diffable) data source over the day aggregate, same global-index
    /// addressing, same visible-range reporting. Three differences are forced by
    /// AppKit and are deliberate:
    ///
    /// * There is no `NSHostingConfiguration`, so items host an `NSHostingView`
    ///   and the SwiftUI content is type-erased through `AnyView`. The content
    ///   itself is the same view the iPhone renders. ``PlatformHostingItem`` and
    ///   ``PlatformHostingSupplementaryView`` are shared with the diffable island
    ///   next door rather than duplicated.
    /// * `NSCollectionView` is not a scroll view, so it is installed as the
    ///   document view of an `NSScrollView` the controller owns.
    /// * There is no `scrollViewDidScroll`, so the visible range is driven by the
    ///   clip view's bounds-changed notification instead.
    final class TimelineCollectionController<ItemContent, HeaderContent>: NSViewController
        where ItemContent: View, HeaderContent: View {
        var itemContent: (Int) -> ItemContent
        var headerContent: (Int) -> HeaderContent
        var onSelect: ((Int) -> Void)?
        var onVisibleRangeChange: ((Range<Int>, Int) -> Void)?
        var onPrefetch: (([Int]) -> Void)?
        var onCancelPrefetch: (([Int]) -> Void)?
        var onMagnify: ((Bool) -> Void)?

        private var geometry: TimelineGridGeometry
        private let bridge = TimelineCollectionBridge()
        private let timelineLayout: TimelineCollectionLayout
        /// The last range handed to the store, so a scroll that does not change
        /// the answer costs one comparison rather than a round trip.
        private var reportedRange: Range<Int>?

        private lazy var collectionView: NSCollectionView = {
            let view = NSCollectionView()
            view.collectionViewLayout = timelineLayout
            view.backgroundColors = [.clear]
            view.isSelectable = true
            view.allowsEmptySelection = true
            view.dataSource = bridge
            view.delegate = bridge
            view.prefetchDataSource = bridge
            view.register(
                PlatformHostingItem.self,
                forItemWithIdentifier: PlatformHostingItem.identifier
            )
            view.register(
                PlatformHostingSupplementaryView.self,
                forSupplementaryViewOfKind: NSCollectionView.elementKindSectionHeader,
                withIdentifier: PlatformHostingSupplementaryView.identifier
            )
            return view
        }()

        init(
            geometry: TimelineGridGeometry,
            itemContent: @escaping (Int) -> ItemContent,
            headerContent: @escaping (Int) -> HeaderContent
        ) {
            self.geometry = geometry
            self.itemContent = itemContent
            self.headerContent = headerContent
            timelineLayout = TimelineCollectionLayout(geometry: geometry)
            super.init(nibName: nil, bundle: nil)
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("TimelineCollectionController is not loaded from a nib")
        }

        override func loadView() {
            let scrollView = NSScrollView()
            scrollView.hasVerticalScroller = true
            scrollView.autohidesScrollers = true
            scrollView.drawsBackground = true
            scrollView.backgroundColor = .windowBackgroundColor
            // A flexible width keeps the layout resolving its tile side against
            // the visible width as the window resizes; the height is content-
            // driven and comes from the layout's exact content size.
            collectionView.autoresizingMask = [.width]
            scrollView.documentView = collectionView
            view = scrollView
        }

        override func viewDidLoad() {
            super.viewDidLoad()
            wireBridge()

            if let scrollView = view as? NSScrollView {
                bridge.observeBounds(of: scrollView.contentView)
            }

            // The Mac's pinch is a trackpad magnification, reported as a delta
            // around zero rather than UIKit's scale around one.
            let magnify = NSMagnificationGestureRecognizer(
                target: bridge,
                action: #selector(TimelineCollectionBridge.handleMagnify)
            )
            collectionView.addGestureRecognizer(magnify)
        }

        override func viewDidLayout() {
            super.viewDidLayout()
            // A live window resize changes both the tile side and how many items
            // a screenful holds, so the store's window is stale until this runs.
            reportVisibleRange()
        }

        // MARK: Update

        func update(geometry newGeometry: TimelineGridGeometry, allowsMultipleSelection: Bool) {
            collectionView.allowsMultipleSelection = allowsMultipleSelection
            guard newGeometry != geometry else { return }
            geometry = newGeometry
            timelineLayout.geometry = newGeometry
            collectionView.reloadData()
            // The aggregate moved, so the previously reported range describes a
            // library that no longer exists.
            reportedRange = nil
            reportVisibleRange()
        }

        // MARK: Wiring

        private func wireBridge() {
            bridge.sectionCount = { [weak self] in self?.timelineLayout.resolved.sectionCount ?? 0 }
            bridge.itemCount = { [weak self] section in
                self?.timelineLayout.resolved.itemCount(inSection: section) ?? 0
            }
            bridge.makeItem = { [weak self] view, indexPath in
                let element = view.makeItem(withIdentifier: PlatformHostingItem.identifier, for: indexPath)
                guard let self, let hosting = element as? PlatformHostingItem,
                      let globalIndex = globalIndex(for: indexPath)
                else {
                    return element
                }
                hosting.host(AnyView(itemContent(globalIndex)))
                return element
            }
            bridge.makeSupplementary = { [weak self] view, kind, indexPath in
                let element = view.makeSupplementaryView(
                    ofKind: kind,
                    withIdentifier: PlatformHostingSupplementaryView.identifier,
                    for: indexPath
                )
                guard let hosting = element as? PlatformHostingSupplementaryView else { return element }
                if let self {
                    hosting.host(AnyView(headerContent(indexPath.section)))
                }
                return hosting
            }
            bridge.onSelect = { [weak self] indexPath in
                guard let self, let globalIndex = globalIndex(for: indexPath) else { return }
                onSelect?(globalIndex)
            }
            bridge.onPrefetch = { [weak self] indexPaths in
                self?.forwardPrefetch(indexPaths, cancel: false)
            }
            bridge.onCancelPrefetch = { [weak self] indexPaths in
                self?.forwardPrefetch(indexPaths, cancel: true)
            }
            bridge.onScroll = { [weak self] in self?.reportVisibleRange() }
            bridge.onMagnify = { [weak self] scale in
                guard let step = PlatformCollectionMagnification.step(forScale: scale) else { return }
                self?.onMagnify?(step)
            }
        }

        // MARK: Callbacks

        /// The visible global index range, reported only when it changes.
        private func reportVisibleRange() {
            let layout = timelineLayout.resolved
            guard layout.itemCount > 0 else { return }
            // The collection view is the document view and is flipped, so its own
            // `visibleRect` is already in the coordinate space the timeline's
            // binary search takes.
            let visible = collectionView.visibleRect
            let range = layout.indexRange(intersecting: visible)
            guard range != reportedRange else { return }
            reportedRange = range
            onVisibleRangeChange?(
                range,
                geometry.viewportItemCount(height: visible.height, itemSide: layout.metrics.itemSide)
            )
        }

        private func forwardPrefetch(_ indexPaths: [IndexPath], cancel: Bool) {
            let indices = indexPaths.compactMap(globalIndex(for:))
            guard !indices.isEmpty else { return }
            if cancel { onCancelPrefetch?(indices) } else { onPrefetch?(indices) }
        }

        /// An index path as the coordinate the timeline and the store speak.
        private func globalIndex(for indexPath: IndexPath) -> Int? {
            guard let start = timelineLayout.resolved.firstGlobalIndex(inSection: indexPath.section) else {
                return nil
            }
            return start + indexPath.item
        }
    }

    // MARK: - Bridge

    /// The non-generic `NSObject` carrying every `@objc` entry point the generic
    /// controller cannot declare: the data source, the delegate, the prefetch
    /// source, the magnification action, and the clip view's bounds observation.
    @MainActor
    final class TimelineCollectionBridge: NSObject, NSCollectionViewDataSource,
        NSCollectionViewDelegate, NSCollectionViewPrefetching {
        var sectionCount: (() -> Int)?
        var itemCount: ((Int) -> Int)?
        var makeItem: ((NSCollectionView, IndexPath) -> NSCollectionViewItem)?
        var makeSupplementary: ((NSCollectionView, String, IndexPath) -> NSView)?
        var onSelect: ((IndexPath) -> Void)?
        var onPrefetch: (([IndexPath]) -> Void)?
        var onCancelPrefetch: (([IndexPath]) -> Void)?
        var onScroll: (() -> Void)?
        var onMagnify: ((CGFloat) -> Void)?

        /// Watch a clip view for scrolling.
        ///
        /// AppKit has no `scrollViewDidScroll`; the clip view's bounds moving *is*
        /// the scroll. The observer registration is zeroing-weak, so a
        /// deallocated bridge simply stops being notified.
        func observeBounds(of clipView: NSClipView) {
            clipView.postsBoundsChangedNotifications = true
            NotificationCenter.default.addObserver(
                self,
                selector: #selector(handleBoundsChange),
                name: NSView.boundsDidChangeNotification,
                object: clipView
            )
        }

        func numberOfSections(in _: NSCollectionView) -> Int {
            sectionCount?() ?? 0
        }

        func collectionView(_: NSCollectionView, numberOfItemsInSection section: Int) -> Int {
            itemCount?(section) ?? 0
        }

        func collectionView(
            _ collectionView: NSCollectionView,
            itemForRepresentedObjectAt indexPath: IndexPath
        ) -> NSCollectionViewItem {
            makeItem?(collectionView, indexPath) ?? NSCollectionViewItem()
        }

        func collectionView(
            _ collectionView: NSCollectionView,
            viewForSupplementaryElementOfKind kind: NSCollectionView.SupplementaryElementKind,
            at indexPath: IndexPath
        ) -> NSView {
            makeSupplementary?(collectionView, kind, indexPath) ?? NSView()
        }

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

        @objc func handleBoundsChange(_: Notification) {
            onScroll?()
        }

        @objc func handleMagnify(_ recognizer: NSMagnificationGestureRecognizer) {
            guard recognizer.state == .ended else { return }
            // AppKit's magnification is a delta around zero; the shared threshold
            // logic speaks UIKit's scale around one.
            onMagnify?(1 + recognizer.magnification)
        }
    }

#endif
