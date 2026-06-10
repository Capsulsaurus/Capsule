import SwiftUI
import UIKit

/// A pinch- and double-tap-zoomable image view for SwiftUI.
///
/// Backed by `UIScrollView` for native, fluid zoom and pan — the full-screen
/// viewer's photo pages. The image is shown aspect-fit at rest and zooms up to
/// 4× that scale.
public struct ZoomableImageView: UIViewRepresentable {
    private let image: UIImage

    public init(image: UIImage) {
        self.image = image
    }

    public func makeUIView(context _: Context) -> ZoomableScrollView {
        ZoomableScrollView()
    }

    public func updateUIView(_ scrollView: ZoomableScrollView, context _: Context) {
        scrollView.display(image)
    }
}

/// The `UIScrollView` subclass behind ``ZoomableImageView``.
public final class ZoomableScrollView: UIScrollView, UIScrollViewDelegate {
    private let imageView = UIImageView()
    private var hasSetInitialZoom = false

    override init(frame: CGRect) {
        super.init(frame: frame)
        delegate = self
        showsVerticalScrollIndicator = false
        showsHorizontalScrollIndicator = false
        contentInsetAdjustmentBehavior = .never
        bouncesZoom = true
        backgroundColor = .clear

        imageView.contentMode = .scaleAspectFit
        addSubview(imageView)

        let doubleTap = UITapGestureRecognizer(target: self, action: #selector(handleDoubleTap(_:)))
        doubleTap.numberOfTapsRequired = 2
        addGestureRecognizer(doubleTap)
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) {
        fatalError("ZoomableScrollView is not loaded from a nib")
    }

    /// Show `image`, resetting the zoom to the aspect-fit scale.
    func display(_ image: UIImage) {
        guard imageView.image !== image else { return }
        imageView.image = image
        imageView.frame = CGRect(origin: .zero, size: image.size)
        contentSize = image.size
        hasSetInitialZoom = false
        setNeedsLayout()
    }

    override public func layoutSubviews() {
        super.layoutSubviews()
        updateZoomScales()
        centerImage()
    }

    public func viewForZooming(in _: UIScrollView) -> UIView? {
        imageView
    }

    public func scrollViewDidZoom(_: UIScrollView) {
        centerImage()
    }

    /// Recompute the fit scale for the current bounds (e.g. after rotation).
    private func updateZoomScales() {
        guard let image = imageView.image,
              image.size.width > 0, image.size.height > 0,
              bounds.width > 0, bounds.height > 0
        else {
            return
        }
        let fitScale = min(bounds.width / image.size.width, bounds.height / image.size.height)
        minimumZoomScale = fitScale
        maximumZoomScale = fitScale * 4
        if !hasSetInitialZoom {
            zoomScale = fitScale
            hasSetInitialZoom = true
        }
    }

    /// Keep the image centred while it is smaller than the viewport.
    private func centerImage() {
        let horizontal = max(0, (bounds.width - imageView.frame.width) / 2)
        let vertical = max(0, (bounds.height - imageView.frame.height) / 2)
        contentInset = UIEdgeInsets(top: vertical, left: horizontal, bottom: vertical, right: horizontal)
    }

    @objc private func handleDoubleTap(_ gesture: UITapGestureRecognizer) {
        if zoomScale > minimumZoomScale {
            setZoomScale(minimumZoomScale, animated: true)
        } else {
            let point = gesture.location(in: imageView)
            let side = bounds.width / maximumZoomScale
            zoom(
                to: CGRect(x: point.x - side / 2, y: point.y - side / 2, width: side, height: side),
                animated: true
            )
        }
    }
}
