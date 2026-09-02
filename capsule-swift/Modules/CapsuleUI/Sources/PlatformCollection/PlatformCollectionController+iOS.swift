#if os(iOS)

    import SwiftUI
    import UIKit

    // MARK: - Representable

    extension PlatformCollectionView: UIViewControllerRepresentable {
        public func makeUIViewController(context _: Context) -> PlatformCollectionController<
            SectionID, Item, ItemContent, HeaderContent
        > {
            PlatformCollectionController(itemContent: itemContent, headerContent: headerContent)
        }

        public func updateUIViewController(
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

    /// The `UICollectionView` half of ``PlatformCollectionView``.
    ///
    /// Generic over the identifiers *and* the hosted SwiftUI content, so cells
    /// carry their concrete content type all the way down and SwiftUI can diff
    /// them structurally instead of rebuilding an `AnyView` per configuration.
    /// A generic `NSObject` subclass cannot vend `@objc` entry points, so every
    /// delegate callback and gesture action arrives through the non-generic
    /// ``PlatformCollectionBridge``.
    public final class PlatformCollectionController<SectionID, Item, ItemContent, HeaderContent>:
        UIViewController
        where SectionID: Hashable & Sendable, Item: Hashable & Sendable,
        ItemContent: View, HeaderContent: View {
        var itemContent: (SectionID, Item) -> ItemContent
        var headerContent: (SectionID) -> HeaderContent
        var onSelect: ((SectionID, Item) -> Void)?
        var onPrefetch: (([Item]) -> Void)?
        var onCancelPrefetch: (([Item]) -> Void)?
        var onMagnify: ((Bool) -> Void)?
        var onLeadingVisibleItem: ((SectionID, Item) -> Void)?
        /// Report a new resting column count chosen by a pinch.
        var onColumnsChange: ((Int) -> Void)?
        /// The column count the caller currently renders — the base every pinch
        /// measures from, and the value `onColumnsChange` asks it to replace.
        var columns: Int = PhotoGridZoom.defaultColumns

        /// The column count in force when the current pinch began, and the item
        /// under its centroid. Both are `nil` outside a gesture.
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

        private lazy var collectionView: UICollectionView = {
            let view = UICollectionView(
                frame: .zero,
                collectionViewLayout: PlatformCollectionLayoutBuilder.make(layout)
            )
            view.backgroundColor = .systemBackground
            view.alwaysBounceVertical = true
            view.delegate = bridge
            view.prefetchDataSource = bridge
            return view
        }()

        private lazy var cellRegistration = UICollectionView
            .CellRegistration<UICollectionViewCell, Item> { [weak self] cell, indexPath, item in
                guard let self, let sectionID = sectionID(at: indexPath.section) else { return }
                // `UIHostingConfiguration` is what makes a cell's content plain
                // SwiftUI: reuse, sizing, and teardown stay UIKit's job, while the
                // pixels are the same view the Mac renders. Margins are zeroed
                // because a photo tile is edge-to-edge by definition.
                cell.contentConfiguration = UIHostingConfiguration {
                    itemContent(sectionID, item)
                }
                .margins(.all, 0)
            }

        private lazy var headerRegistration = UICollectionView
            .SupplementaryRegistration<UICollectionViewCell>(
                elementKind: PlatformCollectionLayoutBuilder.headerElementKind
            ) { [weak self] view, _, indexPath in
                guard let self, let sectionID = sectionID(at: indexPath.section) else { return }
                // A `UICollectionViewCell` is used as the supplementary view purely
                // because it is the reusable view that accepts a content
                // configuration; nothing about it behaves like a cell here.
                view.contentConfiguration = UIHostingConfiguration {
                    headerContent(sectionID)
                }
                .margins(.all, 0)
            }

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

        override public func viewDidLoad() {
            super.viewDidLoad()
            collectionView.frame = view.bounds
            collectionView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            view.addSubview(collectionView)

            bridge.onSelect = { [weak self] indexPath in self?.handleSelection(at: indexPath) }
            bridge.onPrefetch = { [weak self] indexPaths in
                self?.forwardPrefetch(indexPaths, cancel: false)
            }
            bridge.onCancelPrefetch = { [weak self] indexPaths in
                self?.forwardPrefetch(indexPaths, cancel: true)
            }
            bridge.onPinch = { [weak self] recognizer in self?.handlePinch(recognizer) }
            bridge.onScroll = { [weak self] in self?.reportLeadingVisibleItem() }

            let pinch = UIPinchGestureRecognizer(
                target: bridge,
                action: #selector(PlatformCollectionBridge.handlePinch)
            )
            collectionView.addGestureRecognizer(pinch)

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
            bridge.deselectsAnimated = !allowsMultipleSelection
            // A cleared request must re-arm the target, or drilling back into the
            // same section a second time would silently not scroll.
            if newTarget == nil { appliedScrollTarget = nil }
            scrollToSectionID = newTarget
            if newItemTarget == nil { appliedScrollItem = nil }
            scrollToItem = newItemTarget
            if layoutChanged {
                collectionView.setCollectionViewLayout(
                    PlatformCollectionLayoutBuilder.make(newLayout),
                    animated: hasAppliedSnapshot
                ) { [weak self] _ in
                    self?.restoreAnchorAfterLayoutChange()
                }
            }
            applySnapshot(animated: hasAppliedSnapshot && !layoutChanged)
            applyScrollTargetIfNeeded()
            reportLeadingVisibleItem()
        }

        // MARK: Pinch

        /// Drive the column count from a live pinch.
        ///
        /// The grid responds *during* the gesture rather than at the end of it,
        /// which is the difference between a zoom and a toggle. What it does not
        /// do is draw fractional columns: half a tile cannot be rendered, so the
        /// gesture maps continuously onto a ladder of rungs and the layout
        /// animates between them. `UICollectionView` interpolates the item
        /// frames itself, so crossing a rung reads as the tiles growing rather
        /// than as a jump.
        ///
        /// The caller owns the column count — it is SwiftUI state, and the
        /// layout arrives back through `update(...)`. Changing it here as well
        /// would give one value two owners and a fight on the next render.
        private func handlePinch(_ recognizer: UIPinchGestureRecognizer) {
            switch recognizer.state {
            case .began:
                pinchBaseColumns = columns
                pinchAnchor = anchorIndexPath(near: recognizer.location(in: collectionView))
            case .changed:
                guard let base = pinchBaseColumns else { return }
                if let step = PhotoGridZoom.levelStep(base: base, scale: recognizer.scale) {
                    // Past the end of the ladder: this is a level change, and
                    // continuing to report columns would fight it.
                    recognizer.state = .ended
                    pinchBaseColumns = nil
                    onMagnify?(step)
                    return
                }
                let settled = PhotoGridZoom.settle(
                    PhotoGridZoom.continuousColumns(base: base, scale: recognizer.scale)
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

        /// The item under a point, or the topmost visible one if the pinch
        /// landed in a gap between tiles.
        private func anchorIndexPath(near point: CGPoint) -> IndexPath? {
            collectionView.indexPathForItem(at: point)
                ?? collectionView.indexPathsForVisibleItems.min()
        }

        /// Keep the pinched-on photo under the fingers across a column change.
        ///
        /// Without this the grid keeps its *scroll offset*, which means a
        /// different number of rows now sits above it and the reader is
        /// somewhere else in the library — the further down they were, the
        /// further they are thrown. Re-finding the anchor is what makes a pinch
        /// feel like zooming a photograph rather than resizing a list.
        private func restoreAnchorAfterLayoutChange() {
            guard let anchor = pinchAnchor,
                  anchor.section < collectionView.numberOfSections,
                  anchor.item < collectionView.numberOfItems(inSection: anchor.section)
            else { return }
            collectionView.scrollToItem(at: anchor, at: .centeredVertically, animated: false)
        }

        /// Report the topmost visible item, when it is not the one last
        /// reported.
        ///
        /// `indexPathsForVisibleItems` is unordered, so the minimum is taken
        /// rather than the first. Reporting on *change* rather than on every
        /// scroll callback is what keeps this from driving a SwiftUI update per
        /// frame during a fling — a tile spans many frames of scrolling.
        private func reportLeadingVisibleItem() {
            guard let onLeadingVisibleItem else { return }
            guard let indexPath = collectionView.indexPathsForVisibleItems.min(),
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
        /// distinct request.
        ///
        /// A section target resolves through the controller's own `sections`; an
        /// item target resolves through the data source, which is the only thing
        /// that knows an item's index path and answers in constant time. Both
        /// scroll one runloop turn later, because the snapshot has to land
        /// before there is anything to scroll to.
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
                collectionView.scrollToItem(at: indexPath, at: .top, animated: false)
            }
        }

        private func makeDataSource() -> UICollectionViewDiffableDataSource<SectionID, Item> {
            let cells = cellRegistration
            let headers = headerRegistration
            let source = UICollectionViewDiffableDataSource<SectionID, Item>(
                collectionView: collectionView
            ) { view, indexPath, item in
                view.dequeueConfiguredReusableCell(using: cells, for: indexPath, item: item)
            }
            source.supplementaryViewProvider = { view, _, indexPath in
                view.dequeueConfiguredReusableSupplementary(using: headers, for: indexPath)
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
    /// the pinch action.
    final class PlatformCollectionBridge: NSObject, UICollectionViewDelegate,
        UICollectionViewDataSourcePrefetching {
        var onSelect: ((IndexPath) -> Void)?
        var onPrefetch: (([IndexPath]) -> Void)?
        var onCancelPrefetch: (([IndexPath]) -> Void)?
        var onMagnify: ((CGFloat) -> Void)?
        var onPinch: ((UIPinchGestureRecognizer) -> Void)?
        var onScroll: (() -> Void)?
        /// Whether the deselect that immediately follows a tap animates — it
        /// should not while multi-select is driving its own selection visuals.
        var deselectsAnimated = true

        func scrollViewDidScroll(_: UIScrollView) {
            onScroll?()
        }

        func collectionView(_ collectionView: UICollectionView, didSelectItemAt indexPath: IndexPath) {
            // Selection visuals are owned by the hosted SwiftUI content, so the
            // collection's own selection is released straight away.
            collectionView.deselectItem(at: indexPath, animated: deselectsAnimated)
            onSelect?(indexPath)
        }

        func collectionView(_: UICollectionView, prefetchItemsAt indexPaths: [IndexPath]) {
            onPrefetch?(indexPaths)
        }

        func collectionView(_: UICollectionView, cancelPrefetchingForItemsAt indexPaths: [IndexPath]) {
            onCancelPrefetch?(indexPaths)
        }

        @objc func handlePinch(_ recognizer: UIPinchGestureRecognizer) {
            onPinch?(recognizer)
        }
    }

#endif
