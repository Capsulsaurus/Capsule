import AssetKit
import CapsuleFoundation
import ImagePipeline
import SwiftUI
import UIKit

// MARK: - PhotoGridView

/// A high-performance photo grid for SwiftUI, backed by `UICollectionView` with
/// a compositional layout and a diffable data source.
///
/// A `UICollectionView` is used over `LazyVGrid` for true cell reuse, first-class
/// prefetch/cancel, and pinned section headers — the properties a fast,
/// large-library timeline needs. The grid is source-agnostic: it renders
/// ``PhotoGridSection`` values as uniform tiles or representative cards, and is
/// reused by the timeline, album, and aggregation screens.
public struct PhotoGridView: UIViewControllerRepresentable {
    private let sections: [PhotoGridSection]
    private let style: PhotoGridStyle
    private let thumbnails: any ThumbnailProvider
    private let showsSectionHeaders: Bool
    private let scrollToSectionID: String?
    private let isSelecting: Bool
    private let selectedIDs: Set<AssetID>
    private let onSelect: (Asset) -> Void
    private let onSelectSection: ((PhotoGridSection) -> Void)?
    private let onZoomLevelChange: ((Bool) -> Void)?
    private let onToggleSelection: ((AssetID) -> Void)?

    /// The full grid surface: choose a ``PhotoGridStyle``, and — for the
    /// aggregation levels — handle card taps, pinch-to-zoom level changes, and an
    /// optional section to scroll into view after a level switch. Pass
    /// `isSelecting` / `selectedIDs` / `onToggleSelection` to drive multi-select.
    public init(
        sections: [PhotoGridSection],
        style: PhotoGridStyle,
        thumbnails: any ThumbnailProvider,
        showsSectionHeaders: Bool = true,
        scrollToSectionID: String? = nil,
        isSelecting: Bool = false,
        selectedIDs: Set<AssetID> = [],
        onSelect: @escaping (Asset) -> Void,
        onSelectSection: ((PhotoGridSection) -> Void)? = nil,
        onZoomLevelChange: ((Bool) -> Void)? = nil,
        onToggleSelection: ((AssetID) -> Void)? = nil
    ) {
        self.sections = sections
        self.style = style
        self.thumbnails = thumbnails
        self.showsSectionHeaders = showsSectionHeaders
        self.scrollToSectionID = scrollToSectionID
        self.isSelecting = isSelecting
        self.selectedIDs = selectedIDs
        self.onSelect = onSelect
        self.onSelectSection = onSelectSection
        self.onZoomLevelChange = onZoomLevelChange
        self.onToggleSelection = onToggleSelection
    }

    /// Convenience initializer for the common uniform-tile grid.
    public init(
        sections: [PhotoGridSection],
        columnCount: Int,
        thumbnails: any ThumbnailProvider,
        showsSectionHeaders: Bool = true,
        onSelect: @escaping (Asset) -> Void
    ) {
        self.init(
            sections: sections,
            style: .uniform(columns: columnCount),
            thumbnails: thumbnails,
            showsSectionHeaders: showsSectionHeaders,
            onSelect: onSelect
        )
    }

    public func makeUIViewController(context _: Context) -> PhotoGridViewController {
        PhotoGridViewController(thumbnails: thumbnails, showsSectionHeaders: showsSectionHeaders)
    }

    public func updateUIViewController(_ controller: PhotoGridViewController, context _: Context) {
        controller.onSelect = onSelect
        controller.onSelectSection = onSelectSection
        controller.onZoomLevelChange = onZoomLevelChange
        controller.onToggleSelection = onToggleSelection
        controller.update(
            sections: sections,
            style: style,
            scrollToSectionID: scrollToSectionID,
            isSelecting: isSelecting,
            selectedIDs: selectedIDs
        )
    }
}

// MARK: - PhotoGridViewController

/// The `UICollectionView` controller behind ``PhotoGridView``.
public final class PhotoGridViewController: UIViewController, UICollectionViewDelegate,
    UICollectionViewDataSourcePrefetching {
    /// Called when the user taps an asset (uniform style).
    public var onSelect: ((Asset) -> Void)?
    /// Called when the user taps a representative card (cards style).
    public var onSelectSection: ((PhotoGridSection) -> Void)?
    /// Called when the user pinches to change aggregation level — `true` to zoom
    /// in (finer), `false` to zoom out (coarser).
    public var onZoomLevelChange: ((Bool) -> Void)?
    /// Called when the user toggles an asset's selection (select mode).
    public var onToggleSelection: ((AssetID) -> Void)?

    private let thumbnails: any ThumbnailProvider
    private let showsSectionHeaders: Bool
    private var sections: [PhotoGridSection] = []
    private var style: PhotoGridStyle = .uniform(columns: 3)
    private var scrollToSectionID: String?
    private var appliedFocusID: String?
    private var isSelecting = false
    private var selectedIDs: Set<AssetID> = []
    private var hasAppliedSnapshot = false

    private lazy var collectionView: UICollectionView = {
        let collectionView = UICollectionView(
            frame: .zero,
            collectionViewLayout: Self.makeLayout(style: style, showsHeaders: showsSectionHeaders)
        )
        collectionView.backgroundColor = .systemBackground
        collectionView.alwaysBounceVertical = true
        collectionView.delegate = self
        collectionView.prefetchDataSource = self
        return collectionView
    }()

    private lazy var dataSource = makeDataSource()

    init(thumbnails: any ThumbnailProvider, showsSectionHeaders: Bool) {
        self.thumbnails = thumbnails
        self.showsSectionHeaders = showsSectionHeaders
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) {
        fatalError("PhotoGridViewController is not loaded from a nib")
    }

    override public func viewDidLoad() {
        super.viewDidLoad()
        collectionView.frame = view.bounds
        collectionView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.addSubview(collectionView)

        let pinch = UIPinchGestureRecognizer(target: self, action: #selector(handlePinch))
        collectionView.addGestureRecognizer(pinch)

        applyState(animated: false)
    }

    /// Push fresh sections, style, focus, and multi-select state into the grid.
    func update(
        sections newSections: [PhotoGridSection],
        style newStyle: PhotoGridStyle,
        scrollToSectionID newFocus: String?,
        isSelecting newSelecting: Bool,
        selectedIDs newSelected: Set<AssetID>
    ) {
        let styleChanged = newStyle != style
        let selectionChanged = newSelecting != isSelecting || newSelected != selectedIDs
        sections = newSections
        style = newStyle
        isSelecting = newSelecting
        selectedIDs = newSelected
        collectionView.allowsMultipleSelection = newSelecting
        if newFocus == nil { appliedFocusID = nil }
        scrollToSectionID = newFocus
        if styleChanged {
            collectionView.setCollectionViewLayout(
                Self.makeLayout(style: newStyle, showsHeaders: showsSectionHeaders),
                animated: hasAppliedSnapshot
            )
        }
        applyState(animated: hasAppliedSnapshot && !styleChanged)
        // A snapshot reload re-runs the registration (which sets selection); a
        // pure selection change does not, so refresh visible cells directly.
        if selectionChanged, !styleChanged {
            refreshSelectionAppearance()
        }
        applyFocusIfNeeded()
    }

    private func refreshSelectionAppearance() {
        for indexPath in collectionView.indexPathsForVisibleItems {
            guard let cell = collectionView.cellForItem(at: indexPath) as? PhotoGridCell,
                  let asset = dataSource.itemIdentifier(for: indexPath) else { continue }
            cell.setSelection(isSelecting: isSelecting, isSelected: selectedIDs.contains(asset.id))
        }
    }

    // MARK: Data source

    private func applyState(animated: Bool) {
        var snapshot = NSDiffableDataSourceSnapshot<String, Asset>()
        snapshot.appendSections(sections.map(\.id))
        for section in sections {
            snapshot.appendItems(section.assets, toSection: section.id)
        }
        dataSource.apply(snapshot, animatingDifferences: animated)
        hasAppliedSnapshot = true
    }

    /// Scroll a freshly-focused section to the top, once per distinct request.
    private func applyFocusIfNeeded() {
        guard let id = scrollToSectionID, id != appliedFocusID,
              let index = sections.firstIndex(where: { $0.id == id }) else { return }
        appliedFocusID = id
        let indexPath = IndexPath(item: 0, section: index)
        DispatchQueue.main.async { [weak self] in
            guard let self, indexPath.section < collectionView.numberOfSections else { return }
            collectionView.scrollToItem(at: indexPath, at: .top, animated: false)
        }
    }

    private func makeDataSource() -> UICollectionViewDiffableDataSource<String, Asset> {
        let tileRegistration = UICollectionView
            .CellRegistration<PhotoGridCell, Asset> { [weak self] cell, _, asset in
                guard let self else { return }
                cell.configure(with: asset, pixelSize: cellPixelSize, thumbnails: thumbnails)
                cell.setSelection(isSelecting: isSelecting, isSelected: selectedIDs.contains(asset.id))
            }
        let cardRegistration = UICollectionView
            .CellRegistration<PhotoGridCardCell, Asset> { [weak self] cell, indexPath, asset in
                guard let self, indexPath.section < sections.count else { return }
                cell.configure(
                    with: asset,
                    title: sections[indexPath.section].title,
                    pixelSize: cellPixelSize,
                    thumbnails: thumbnails
                )
            }
        let headerRegistration = UICollectionView
            .SupplementaryRegistration<PhotoGridHeaderView>(
                elementKind: UICollectionView.elementKindSectionHeader
            ) { [weak self] header, _, indexPath in
                guard let self, indexPath.section < sections.count else { return }
                header.title = sections[indexPath.section].title
            }
        let source = UICollectionViewDiffableDataSource<String, Asset>(
            collectionView: collectionView
        ) { [weak self] collectionView, indexPath, asset in
            if self?.style == .cards {
                return collectionView.dequeueConfiguredReusableCell(
                    using: cardRegistration, for: indexPath, item: asset
                )
            }
            return collectionView.dequeueConfiguredReusableCell(
                using: tileRegistration, for: indexPath, item: asset
            )
        }
        source.supplementaryViewProvider = { collectionView, _, indexPath in
            collectionView.dequeueConfiguredReusableSupplementary(
                using: headerRegistration, for: indexPath
            )
        }
        return source
    }

    /// The device-pixel size a cell decodes to at the current style.
    private var cellPixelSize: CGSize {
        let width = collectionView.bounds.width
        let scale = max(view.window?.screen.scale ?? traitCollection.displayScale, 1)
        switch style {
        case let .uniform(columns):
            let side = (width / CGFloat(columns)) * scale
            return CGSize(width: side, height: side)
        case .cards:
            let pixels = width * scale
            return CGSize(width: pixels, height: pixels * 0.7)
        }
    }

    // MARK: Gestures

    @objc private func handlePinch(_ recognizer: UIPinchGestureRecognizer) {
        guard recognizer.state == .ended, onZoomLevelChange != nil else { return }
        if recognizer.scale > 1.25 {
            onZoomLevelChange?(true)
        } else if recognizer.scale < 0.8 {
            onZoomLevelChange?(false)
        }
    }

    // MARK: UICollectionViewDelegate

    public func collectionView(_ collectionView: UICollectionView, didSelectItemAt indexPath: IndexPath) {
        collectionView.deselectItem(at: indexPath, animated: !isSelecting)
        switch style {
        case .cards:
            guard indexPath.section < sections.count else { return }
            onSelectSection?(sections[indexPath.section])
        case .uniform:
            guard let asset = dataSource.itemIdentifier(for: indexPath) else { return }
            if isSelecting {
                let willSelect = !selectedIDs.contains(asset.id)
                if willSelect { selectedIDs.insert(asset.id) } else { selectedIDs.remove(asset.id) }
                (collectionView.cellForItem(at: indexPath) as? PhotoGridCell)?
                    .setSelection(isSelecting: true, isSelected: willSelect)
                onToggleSelection?(asset.id)
            } else {
                onSelect?(asset)
            }
        }
    }

    // MARK: UICollectionViewDataSourcePrefetching

    public func collectionView(_: UICollectionView, prefetchItemsAt indexPaths: [IndexPath]) {
        let assets = indexPaths.compactMap { dataSource.itemIdentifier(for: $0) }
        guard !assets.isEmpty else { return }
        let size = cellPixelSize
        Task { await thumbnails.beginPrefetching(for: assets, pixelSize: size) }
    }

    public func collectionView(_: UICollectionView, cancelPrefetchingForItemsAt indexPaths: [IndexPath]) {
        let assets = indexPaths.compactMap { dataSource.itemIdentifier(for: $0) }
        guard !assets.isEmpty else { return }
        let size = cellPixelSize
        Task { await thumbnails.cancelPrefetching(for: assets, pixelSize: size) }
    }

    // MARK: Layout

    private static func makeLayout(style: PhotoGridStyle, showsHeaders: Bool) -> UICollectionViewLayout {
        switch style {
        case let .uniform(columns):
            return makeUniformLayout(columnCount: columns, showsHeaders: showsHeaders)
        case .cards:
            return makeCardsLayout()
        }
    }

    /// `columnCount` square tiles per row, with pinned section headers when set.
    private static func makeUniformLayout(columnCount: Int, showsHeaders: Bool) -> UICollectionViewLayout {
        let spacing: CGFloat = 0.75
        let item = NSCollectionLayoutItem(layoutSize: NSCollectionLayoutSize(
            widthDimension: .fractionalWidth(1),
            heightDimension: .fractionalHeight(1)
        ))
        item.contentInsets = NSDirectionalEdgeInsets(
            top: spacing, leading: spacing, bottom: spacing, trailing: spacing
        )
        let fraction = 1.0 / CGFloat(columnCount)
        let group = NSCollectionLayoutGroup.horizontal(
            layoutSize: NSCollectionLayoutSize(
                widthDimension: .fractionalWidth(1),
                heightDimension: .fractionalWidth(fraction)
            ),
            repeatingSubitem: item,
            count: columnCount
        )
        let section = NSCollectionLayoutSection(group: group)
        if showsHeaders {
            let header = NSCollectionLayoutBoundarySupplementaryItem(
                layoutSize: NSCollectionLayoutSize(
                    widthDimension: .fractionalWidth(1),
                    heightDimension: .estimated(44)
                ),
                elementKind: UICollectionView.elementKindSectionHeader,
                alignment: .top
            )
            header.pinToVisibleBounds = true
            section.boundarySupplementaryItems = [header]
        }
        return UICollectionViewCompositionalLayout(section: section)
    }

    /// One full-width representative card per section, for Years and Months.
    private static func makeCardsLayout() -> UICollectionViewLayout {
        let item = NSCollectionLayoutItem(layoutSize: NSCollectionLayoutSize(
            widthDimension: .fractionalWidth(1),
            heightDimension: .fractionalHeight(1)
        ))
        let group = NSCollectionLayoutGroup.horizontal(
            layoutSize: NSCollectionLayoutSize(
                widthDimension: .fractionalWidth(1),
                heightDimension: .fractionalWidth(0.62)
            ),
            repeatingSubitem: item,
            count: 1
        )
        let section = NSCollectionLayoutSection(group: group)
        section.contentInsets = NSDirectionalEdgeInsets(top: 6, leading: 12, bottom: 6, trailing: 12)
        return UICollectionViewCompositionalLayout(section: section)
    }
}
