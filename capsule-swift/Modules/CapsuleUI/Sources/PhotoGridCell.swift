import AssetKit
import CapsuleFoundation
import ImagePipeline
import UIKit

/// A single photo-grid tile: one image layer plus a small media badge.
///
/// Kept deliberately minimal — one `UIImageView`, one badge label, one badge
/// glyph — so the layer tree is shallow and scrolling never triggers offscreen
/// rendering. Each cell owns its in-flight thumbnail `Task` and cancels it on
/// reuse, so a fast fling never applies a stale image.
final class PhotoGridCell: UICollectionViewCell {
    static let reuseIdentifier = "PhotoGridCell"

    private let imageView = UIImageView()
    private let durationLabel = UILabel()
    private let liveBadge = UIImageView()
    private var thumbnailTask: Task<Void, Never>?
    private var representedID: AssetID?

    override init(frame: CGRect) {
        super.init(frame: frame)
        contentView.clipsToBounds = true
        contentView.backgroundColor = .secondarySystemBackground

        imageView.contentMode = .scaleAspectFill
        imageView.clipsToBounds = true
        imageView.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(imageView)

        durationLabel.font = .systemFont(ofSize: 12, weight: .semibold)
        durationLabel.textColor = .white
        durationLabel.shadowColor = UIColor.black.withAlphaComponent(0.6)
        durationLabel.shadowOffset = CGSize(width: 0, height: 0.5)
        durationLabel.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(durationLabel)

        liveBadge.image = UIImage(systemName: "livephoto")
        liveBadge.tintColor = .white
        liveBadge.contentMode = .scaleAspectFit
        liveBadge.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(liveBadge)

        NSLayoutConstraint.activate([
            imageView.topAnchor.constraint(equalTo: contentView.topAnchor),
            imageView.bottomAnchor.constraint(equalTo: contentView.bottomAnchor),
            imageView.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            imageView.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
            durationLabel.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -4),
            durationLabel.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -3),
            liveBadge.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 4),
            liveBadge.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -4),
            liveBadge.widthAnchor.constraint(equalToConstant: 16),
            liveBadge.heightAnchor.constraint(equalToConstant: 16),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("PhotoGridCell is not loaded from a nib")
    }

    /// Apply an asset's static presentation (badges) and start its thumbnail load.
    func configure(with asset: Asset, pixelSize: CGSize, thumbnails: any ThumbnailProvider) {
        representedID = asset.id
        durationLabel.text = asset.mediaType == .video ? Self.durationText(asset.duration) : nil
        durationLabel.isHidden = asset.mediaType != .video
        liveBadge.isHidden = asset.mediaType != .livePhoto

        thumbnailTask?.cancel()
        let targetID = asset.id
        thumbnailTask = Task { [weak self] in
            let image = await thumbnails.thumbnail(for: asset, pixelSize: pixelSize)
            guard !Task.isCancelled else { return }
            guard let self, representedID == targetID else { return }
            imageView.image = image
        }
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        thumbnailTask?.cancel()
        thumbnailTask = nil
        imageView.image = nil
        representedID = nil
    }

    /// Formats a media duration as `m:ss`.
    private static func durationText(_ duration: TimeInterval) -> String {
        let total = Int(duration.rounded())
        return String(format: "%d:%02d", total / 60, total % 60)
    }
}
