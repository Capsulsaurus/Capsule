import AssetKit
import ImagePipeline
import SwiftUI
import UIKit

// MARK: - PhotoGridSection

/// One titled run of assets in the grid — typically a single capture day.
public struct PhotoGridSection: Identifiable, Sendable {
    /// A stable section identifier (e.g. the day key `2024-07-03`).
    public let id: String
    /// The header text shown for the section.
    public let title: String
    /// The section's assets, in display order.
    public let assets: [Asset]

    public init(id: String, title: String, assets: [Asset]) {
        self.id = id
        self.title = title
        self.assets = assets
    }
}

// MARK: - PhotoGridView

/// A high-performance photo grid for SwiftUI, backed by `UICollectionView` with
/// a compositional layout and a diffable data source.
///
/// A `UICollectionView` is used over `LazyVGrid` for true cell reuse, first-class
/// prefetch/cancel, and pinned section headers — the properties a fast,
/// large-library timeline needs. The grid is source-agnostic: it renders
/// ``PhotoGridSection`` values and is reused by the timeline and album screens.
public struct PhotoGridView: UIViewControllerRepresentable {
    private let sections: [PhotoGridSection]
    private let columnCount: Int
    private let thumbnails: any ThumbnailProvider
    private let onSelect: (Asset) -> Void

    public init(
        sections: [PhotoGridSection],
        columnCount: Int,
        thumbnails: any ThumbnailProvider,
        onSelect: @escaping (Asset) -> Void
    ) {
        self.sections = sections
        self.columnCount = columnCount
        self.thumbnails = thumbnails
        self.onSelect = onSelect
    }

    public func makeUIViewController(context _: Context) -> PhotoGridViewController {
        PhotoGridViewController(thumbnails: thumbnails)
    }

    public func updateUIViewController(_ controller: PhotoGridViewController, context _: Context) {
        controller.onSelect = onSelect
        controller.update(sections: sections, columnCount: columnCount)
    }
}

// MARK: - PhotoGridViewController

/// The `UICollectionView` controller behind ``PhotoGridView``.
public final class PhotoGridViewController: UIViewController, UICollectionViewDelegate,
    UICollectionViewDataSourcePrefetching {
    /// Called when the user taps an asset.
    public var onSelect: ((Asset) -> Void)?

    private let thumbnails: any ThumbnailProvider
    private var sections: [PhotoGridSection] = []
    private var columnCount = 3
    private var hasAppliedSnapshot = false

    private lazy var collectionView: UICollectionView = {
        let collectionView = UICollectionView(
            frame: .zero,
            collectionViewLayout: Self.makeLayout(columnCount: columnCount)
        )
        collectionView.backgroundColor = .systemBackground
        collectionView.alwaysBounceVertical = true
        collectionView.delegate = self
        collectionView.prefetchDataSource = self
        return collectionView
    }()

    private lazy var dataSource = makeDataSource()

    init(thumbnails: any ThumbnailProvider) {
        self.thumbnails = thumbnails
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
        applyState()
    }

    /// Push fresh sections and column density into the grid.
    func update(sections newSections: [PhotoGridSection], columnCount newColumnCount: Int) {
        let densityChanged = newColumnCount != columnCount
        sections = newSections
        columnCount = newColumnCount
        if densityChanged {
            collectionView.setCollectionViewLayout(
                Self.makeLayout(columnCount: newColumnCount),
                animated: hasAppliedSnapshot
            )
        }
        applyState()
    }

    // MARK: Data source

    private func applyState() {
        var snapshot = NSDiffableDataSourceSnapshot<String, Asset>()
        snapshot.appendSections(sections.map(\.id))
        for section in sections {
            snapshot.appendItems(section.assets, toSection: section.id)
        }
        dataSource.apply(snapshot, animatingDifferences: hasAppliedSnapshot)
        hasAppliedSnapshot = true
    }

    private func makeDataSource() -> UICollectionViewDiffableDataSource<String, Asset> {
        let cellRegistration = UICollectionView
            .CellRegistration<PhotoGridCell, Asset> { [weak self] cell, _, asset in
                guard let self else { return }
                cell.configure(with: asset, pixelSize: cellPixelSize, thumbnails: thumbnails)
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
        ) { collectionView, indexPath, asset in
            collectionView.dequeueConfiguredReusableCell(
                using: cellRegistration, for: indexPath, item: asset
            )
        }
        source.supplementaryViewProvider = { collectionView, _, indexPath in
            collectionView.dequeueConfiguredReusableSupplementary(
                using: headerRegistration, for: indexPath
            )
        }
        return source
    }

    /// The device-pixel size a grid tile decodes to at the current density.
    private var cellPixelSize: CGSize {
        let width = collectionView.bounds.width
        let scale = max(view.window?.screen.scale ?? traitCollection.displayScale, 1)
        let side = (width / CGFloat(columnCount)) * scale
        return CGSize(width: side, height: side)
    }

    // MARK: UICollectionViewDelegate

    public func collectionView(_ collectionView: UICollectionView, didSelectItemAt indexPath: IndexPath) {
        collectionView.deselectItem(at: indexPath, animated: true)
        if let asset = dataSource.itemIdentifier(for: indexPath) {
            onSelect?(asset)
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

    /// A compositional layout of `columnCount` square tiles per row, with
    /// pinned section headers.
    private static func makeLayout(columnCount: Int) -> UICollectionViewLayout {
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
        return UICollectionViewCompositionalLayout(section: section)
    }
}

// MARK: - PhotoGridHeaderView

/// A pinned section header — a date label over a chrome material so it stays
/// legible while photos scroll beneath it.
final class PhotoGridHeaderView: UICollectionReusableView {
    private let label = UILabel()

    var title: String? {
        get { label.text }
        set { label.text = newValue }
    }

    override init(frame: CGRect) {
        super.init(frame: frame)
        let background = UIVisualEffectView(effect: UIBlurEffect(style: .systemChromeMaterial))
        background.translatesAutoresizingMaskIntoConstraints = false
        addSubview(background)

        label.font = .systemFont(ofSize: 15, weight: .semibold)
        label.textColor = .label
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)

        NSLayoutConstraint.activate([
            background.topAnchor.constraint(equalTo: topAnchor),
            background.bottomAnchor.constraint(equalTo: bottomAnchor),
            background.leadingAnchor.constraint(equalTo: leadingAnchor),
            background.trailingAnchor.constraint(equalTo: trailingAnchor),
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            label.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -12),
            label.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            label.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -8),
        ])
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) {
        fatalError("PhotoGridHeaderView is not loaded from a nib")
    }
}
