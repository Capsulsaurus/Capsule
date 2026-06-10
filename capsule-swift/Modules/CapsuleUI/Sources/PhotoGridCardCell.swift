import AssetKit
import CapsuleFoundation
import ImagePipeline
import UIKit

/// A large representative card for the Years / Months aggregation levels: one
/// photo standing in for a whole period, with the period title overlaid on a
/// legibility gradient.
///
/// Mirrors ``PhotoGridCell``'s discipline — one image layer, an owned thumbnail
/// `Task` cancelled on reuse — so the aggregate grid scrolls as smoothly as the
/// tile grid.
final class PhotoGridCardCell: UICollectionViewCell {
    private let imageView = UIImageView()
    private let scrim = GradientView()
    private let titleLabel = UILabel()
    private var thumbnailTask: Task<Void, Never>?
    private var representedID: AssetID?

    override init(frame: CGRect) {
        super.init(frame: frame)
        contentView.clipsToBounds = true
        contentView.layer.cornerRadius = CapsuleTheme.Radius.card
        contentView.backgroundColor = .secondarySystemBackground

        imageView.contentMode = .scaleAspectFill
        imageView.clipsToBounds = true
        imageView.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(imageView)

        scrim.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(scrim)

        titleLabel.font = .systemFont(ofSize: 22, weight: .bold)
        titleLabel.textColor = .white
        titleLabel.shadowColor = UIColor.black.withAlphaComponent(0.5)
        titleLabel.shadowOffset = CGSize(width: 0, height: 0.5)
        titleLabel.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(titleLabel)

        NSLayoutConstraint.activate([
            imageView.topAnchor.constraint(equalTo: contentView.topAnchor),
            imageView.bottomAnchor.constraint(equalTo: contentView.bottomAnchor),
            imageView.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            imageView.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
            scrim.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
            scrim.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
            scrim.bottomAnchor.constraint(equalTo: contentView.bottomAnchor),
            scrim.heightAnchor.constraint(equalTo: contentView.heightAnchor, multiplier: 0.5),
            titleLabel.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),
            titleLabel.trailingAnchor.constraint(lessThanOrEqualTo: contentView.trailingAnchor, constant: -16),
            titleLabel.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -14),
        ])
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) {
        fatalError("PhotoGridCardCell is not loaded from a nib")
    }

    /// Show `title` over `asset`'s thumbnail and start the decode.
    func configure(
        with asset: Asset,
        title: String,
        pixelSize: CGSize,
        thumbnails: any ThumbnailProvider
    ) {
        representedID = asset.id
        titleLabel.text = title

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
        titleLabel.text = nil
        representedID = nil
    }
}

/// A bottom-to-top dark gradient that keeps an overlaid title legible over any
/// photo. Backed by a `CAGradientLayer` so it costs one layer, no blending view.
private final class GradientView: UIView {
    // `layerClass` is an override and must stay a `class var`, so the
    // static-over-final-class lint does not apply.
    // swiftlint:disable:next static_over_final_class
    override class var layerClass: AnyClass { CAGradientLayer.self }

    override init(frame: CGRect) {
        super.init(frame: frame)
        isUserInteractionEnabled = false
        guard let gradient = layer as? CAGradientLayer else { return }
        gradient.colors = [
            UIColor.clear.cgColor,
            UIColor.black.withAlphaComponent(0.55).cgColor,
        ]
        gradient.locations = [0.0, 1.0]
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) {
        fatalError("GradientView is not loaded from a nib")
    }
}
