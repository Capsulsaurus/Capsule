#if os(iOS)

    import SwiftUI
    import UIKit

    // MARK: - Representable

    extension TimelineCollectionView: UIViewControllerRepresentable {
        func makeUIViewController(context _: Context) -> TimelineCollectionController<ItemContent, HeaderContent> {
            TimelineCollectionController(
                geometry: geometry,
                itemContent: itemContent,
                headerContent: headerContent
            )
        }

        func updateUIViewController(
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

    /// The `UICollectionView` half of ``TimelineCollectionView``.
    ///
    /// Deliberately **not** diffable-data-source-backed. A diffable snapshot is a
    /// list of every item identity in the collection, which is the one structure
    /// a 250 000-asset timeline cannot afford to build — and would have to
    /// rebuild on every change. A plain `UICollectionViewDataSource` answers
    /// `numberOfItemsInSection:` straight from the day aggregate, so the data
    /// source knows the library's *shape* without ever holding its contents.
    ///
    /// Cells are addressed by **global index** rather than by index path, because
    /// that is the coordinate ``TimelineLayout`` and ``AssetWindowStore`` both
    /// speak. Converting is one prefix-sum lookup, and doing it here means no
    /// view above the reuse boundary ever sees an `IndexPath`.
    ///
    /// A generic `NSObject` subclass cannot vend `@objc` entry points, so every
    /// data-source, delegate, and gesture callback arrives through the
    /// non-generic ``TimelineCollectionBridge``.
    final class TimelineCollectionController<ItemContent, HeaderContent>: UIViewController
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

        private lazy var collectionView: UICollectionView = {
            let view = UICollectionView(frame: .zero, collectionViewLayout: timelineLayout)
            view.backgroundColor = .systemBackground
            view.alwaysBounceVertical = true
            view.dataSource = bridge
            view.delegate = bridge
            view.prefetchDataSource = bridge
            return view
        }()

        private lazy var cellRegistration = UICollectionView
            .CellRegistration<UICollectionViewCell, Int> { [weak self] cell, _, globalIndex in
                guard let self else { return }
                // `UIHostingConfiguration` is what keeps everything above the
                // reuse boundary in plain SwiftUI: reuse, sizing, and teardown
                // stay UIKit's job, while the pixels are the same view the Mac
                // renders. Margins are zeroed — a photo tile is edge-to-edge.
                cell.contentConfiguration = UIHostingConfiguration {
                    itemContent(globalIndex)
                }
                .margins(.all, 0)
            }

        private lazy var headerRegistration = UICollectionView
            .SupplementaryRegistration<UICollectionViewCell>(
                elementKind: UICollectionView.elementKindSectionHeader
            ) { [weak self] view, _, indexPath in
                guard let self else { return }
                view.contentConfiguration = UIHostingConfiguration {
                    headerContent(indexPath.section)
                }
                .margins(.all, 0)
            }

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

        override func viewDidLoad() {
            super.viewDidLoad()
            collectionView.frame = view.bounds
            collectionView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            view.addSubview(collectionView)
            wireBridge()

            let pinch = UIPinchGestureRecognizer(
                target: bridge,
                action: #selector(TimelineCollectionBridge.handlePinch)
            )
            collectionView.addGestureRecognizer(pinch)
        }

        override func viewDidLayoutSubviews() {
            super.viewDidLayoutSubviews()
            // A rotation or split-view resize changes both the tile side and how
            // many items a screenful holds, so the store's window is stale until
            // this runs.
            reportVisibleRange()
        }

        // MARK: Update

        func update(geometry newGeometry: TimelineGridGeometry, allowsMultipleSelection: Bool) {
            collectionView.allowsMultipleSelection = allowsMultipleSelection
            bridge.deselectsAnimated = !allowsMultipleSelection
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
            bridge.makeCell = { [weak self] view, indexPath in
                guard let self, let globalIndex = globalIndex(for: indexPath) else {
                    return UICollectionViewCell()
                }
                return view.dequeueConfiguredReusableCell(
                    using: cellRegistration, for: indexPath, item: globalIndex
                )
            }
            bridge.makeSupplementary = { [weak self] view, _, indexPath in
                guard let self else { return UICollectionReusableView() }
                return view.dequeueConfiguredReusableSupplementary(using: headerRegistration, for: indexPath)
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
            // A scroll view's `bounds` *is* its visible content rect, so no
            // conversion is needed between what the user sees and what the
            // timeline's binary search takes.
            let visible = collectionView.bounds
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
    /// source, and the pinch action.
    @MainActor
    final class TimelineCollectionBridge: NSObject, UICollectionViewDataSource,
        UICollectionViewDelegate, UICollectionViewDataSourcePrefetching {
        var sectionCount: (() -> Int)?
        var itemCount: ((Int) -> Int)?
        var makeCell: ((UICollectionView, IndexPath) -> UICollectionViewCell)?
        var makeSupplementary: ((UICollectionView, String, IndexPath) -> UICollectionReusableView)?
        var onSelect: ((IndexPath) -> Void)?
        var onPrefetch: (([IndexPath]) -> Void)?
        var onCancelPrefetch: (([IndexPath]) -> Void)?
        var onScroll: (() -> Void)?
        var onMagnify: ((CGFloat) -> Void)?
        /// Whether the deselect that immediately follows a tap animates — it
        /// should not while multi-select is driving its own selection visuals.
        var deselectsAnimated = true

        func numberOfSections(in _: UICollectionView) -> Int {
            sectionCount?() ?? 0
        }

        func collectionView(_: UICollectionView, numberOfItemsInSection section: Int) -> Int {
            itemCount?(section) ?? 0
        }

        func collectionView(
            _ collectionView: UICollectionView,
            cellForItemAt indexPath: IndexPath
        ) -> UICollectionViewCell {
            makeCell?(collectionView, indexPath) ?? UICollectionViewCell()
        }

        func collectionView(
            _ collectionView: UICollectionView,
            viewForSupplementaryElementOfKind kind: String,
            at indexPath: IndexPath
        ) -> UICollectionReusableView {
            makeSupplementary?(collectionView, kind, indexPath) ?? UICollectionReusableView()
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

        func scrollViewDidScroll(_: UIScrollView) {
            onScroll?()
        }

        @objc func handlePinch(_ recognizer: UIPinchGestureRecognizer) {
            guard recognizer.state == .ended else { return }
            onMagnify?(recognizer.scale)
        }
    }

#endif
