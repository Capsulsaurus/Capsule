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
            controller.update(
                sections: sections,
                layout: layout,
                scrollToSectionID: scrollToSectionID,
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

        private var layout: PlatformCollectionLayout
        private var sections: [PlatformCollectionSection<SectionID, Item>] = []
        private var scrollToSectionID: SectionID?
        private var appliedScrollTarget: SectionID?
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
            bridge.onMagnify = { [weak self] scale in
                guard let step = PlatformCollectionMagnification.step(forScale: scale) else { return }
                self?.onMagnify?(step)
            }

            // The Mac's pinch is a trackpad magnification, reported as a delta
            // around zero rather than UIKit's scale around one.
            let magnify = NSMagnificationGestureRecognizer(
                target: bridge,
                action: #selector(PlatformCollectionBridge.handleMagnify)
            )
            collectionView.addGestureRecognizer(magnify)

            applySnapshot(animated: false)
        }

        // MARK: Update

        /// Push fresh content, layout, focus, and selection mode into the grid.
        func update(
            sections newSections: [PlatformCollectionSection<SectionID, Item>],
            layout newLayout: PlatformCollectionLayout,
            scrollToSectionID newTarget: SectionID?,
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
            if layoutChanged {
                collectionView.collectionViewLayout = PlatformCollectionLayoutBuilder.make(newLayout)
            }
            applySnapshot(animated: hasAppliedSnapshot && !layoutChanged)
            applyScrollTargetIfNeeded()
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

        /// Scroll a freshly-targeted section to the top, once per distinct request.
        private func applyScrollTargetIfNeeded() {
            guard let target = scrollToSectionID, target != appliedScrollTarget,
                  let index = sections.firstIndex(where: { $0.id == target }) else { return }
            appliedScrollTarget = target
            let indexPath = IndexPath(item: 0, section: index)
            // One turn later: the snapshot above has to land before the section
            // exists to scroll to.
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
            guard recognizer.state == .ended else { return }
            // AppKit's magnification is a delta around zero; the shared threshold
            // logic speaks UIKit's scale around one.
            onMagnify?(1 + recognizer.magnification)
        }
    }

    // MARK: - Hosting item

    /// An `NSCollectionViewItem` whose entire body is a hosted SwiftUI view.
    ///
    /// Re-hosting on dequeue is what makes the SwiftUI content's own `task`
    /// cancellation work: the recycled item is handed the next item's content
    /// before it is shown, so the previous subtree is torn down and its
    /// in-flight thumbnail decode cancelled — exactly what `prepareForReuse`
    /// did by hand in the UIKit-only implementation this replaced.
    final class PlatformHostingItem: NSCollectionViewItem {
        static let identifier = NSUserInterfaceItemIdentifier("PlatformHostingItem")

        private let hostingView = NSHostingView(rootView: AnyView(EmptyView()))

        override func loadView() {
            let container = NSView()
            hostingView.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(hostingView)
            NSLayoutConstraint.activate([
                hostingView.topAnchor.constraint(equalTo: container.topAnchor),
                hostingView.bottomAnchor.constraint(equalTo: container.bottomAnchor),
                hostingView.leadingAnchor.constraint(equalTo: container.leadingAnchor),
                hostingView.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            ])
            view = container
        }

        func host(_ content: AnyView) {
            hostingView.rootView = content
        }
    }

    /// The supplementary-view twin of ``PlatformHostingItem``, used for pinned
    /// section headers.
    /// Conforms to `NSCollectionViewElement` because AppKit's
    /// `supplementaryViewProvider` is typed `(NSView & NSCollectionViewElement)?`,
    /// not plain `NSView` as the item provider is. The protocol has no required
    /// members — the conformance is what makes the view usable as a supplementary
    /// element at all.
    final class PlatformHostingSupplementaryView: NSView, NSCollectionViewElement {
        static let identifier = NSUserInterfaceItemIdentifier("PlatformHostingSupplementaryView")

        private let hostingView = NSHostingView(rootView: AnyView(EmptyView()))

        override init(frame frameRect: NSRect) {
            super.init(frame: frameRect)
            hostingView.translatesAutoresizingMaskIntoConstraints = false
            addSubview(hostingView)
            NSLayoutConstraint.activate([
                hostingView.topAnchor.constraint(equalTo: topAnchor),
                hostingView.bottomAnchor.constraint(equalTo: bottomAnchor),
                hostingView.leadingAnchor.constraint(equalTo: leadingAnchor),
                hostingView.trailingAnchor.constraint(equalTo: trailingAnchor),
            ])
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("PlatformHostingSupplementaryView is not loaded from a nib")
        }

        func host(_ content: AnyView) {
            hostingView.rootView = content
        }
    }

#endif
