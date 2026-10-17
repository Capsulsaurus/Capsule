import CapsuleFoundation
import SwiftUI

/// A pinch- and double-tap-zoomable image view for SwiftUI — the full-screen
/// viewer's photo pages. The image is shown aspect-fit at rest and zooms to 4×
/// that scale.
///
/// Pure SwiftUI, replacing the `UIScrollView` subclass this used to be. That is
/// not a stylistic preference: `NSScrollView`'s magnification is a different
/// interaction model (no bounce-back zoom, no zoom-to-rect on double click), so
/// matching the iOS feel on the Mac meant either two implementations or one
/// gesture-driven view. The maths lives in ``ZoomableImageMath`` and is tested.
///
/// Interaction differences worth knowing: on iOS the pinch is two fingers on
/// glass and the double-tap is a tap; on macOS the same `MagnifyGesture` is a
/// trackpad pinch and the double-tap is a double-click. A mouse without a
/// trackpad can still double-click to toggle between fit and 4×.
public struct ZoomableImageView: View {
    private let image: PlatformImage

    /// The live zoom / offset, and the values the last gesture committed. The
    /// committed pair is what a new gesture measures its delta against, which is
    /// what keeps successive pinches and drags additive.
    @State private var zoom: CGFloat = 1
    @State private var committedZoom: CGFloat = 1
    @State private var offset: CGSize = .zero
    @State private var committedOffset: CGSize = .zero

    public init(image: PlatformImage) {
        self.image = image
    }

    public var body: some View {
        GeometryReader { proxy in
            let container = proxy.size
            let fitted = ZoomableImageMath.fittedSize(aspectRatio: aspectRatio, in: container)

            Image(platformImage: image)
                .resizable()
                .interpolation(.high)
                .scaledToFit()
                .frame(width: container.width, height: container.height)
                .scaleEffect(zoom)
                .offset(offset)
                .contentShape(Rectangle())
                .gesture(magnifyGesture(fitted: fitted, container: container))
                // Simultaneous and masked off at rest: while the photo fits, the
                // horizontal drag belongs to the viewer's pager, and a gesture
                // that is merely ignored would still swallow it.
                .simultaneousGesture(
                    panGesture(fitted: fitted, container: container),
                    including: zoom > ZoomableImageMath.minZoom ? .all : .none
                )
                .onTapGesture(count: 2, coordinateSpace: .local) { location in
                    handleDoubleTap(at: location, fitted: fitted, container: container)
                }
                .onChange(of: container) { _, newContainer in
                    // A rotation or window resize can leave the image parked
                    // outside its new bounds.
                    let newFitted = ZoomableImageMath.fittedSize(
                        aspectRatio: aspectRatio, in: newContainer
                    )
                    commit(zoom: zoom, offset: offset, fitted: newFitted, container: newContainer)
                }
        }
        .clipped()
        .onChange(of: ObjectIdentifier(image)) { _, _ in reset() }
    }

    // MARK: Gestures

    private func magnifyGesture(fitted: CGSize, container: CGSize) -> some Gesture {
        MagnifyGesture()
            .onChanged { value in
                let newZoom = ZoomableImageMath.clampedZoom(committedZoom * value.magnification)
                let anchor = CGPoint(
                    x: value.startAnchor.x * container.width,
                    y: value.startAnchor.y * container.height
                )
                let anchored = ZoomableImageMath.anchoredOffset(
                    current: committedOffset,
                    from: committedZoom,
                    to: newZoom,
                    anchor: anchor,
                    container: container
                )
                zoom = newZoom
                offset = ZoomableImageMath.rubberBanded(
                    anchored,
                    limit: ZoomableImageMath.offsetLimit(
                        fitted: fitted, container: container, zoom: newZoom
                    )
                )
            }
            .onEnded { _ in
                // Snapping the rubber band back is the only place this view
                // animates by itself; everything during a gesture tracks the
                // fingers one-to-one.
                withAnimation(.spring(duration: 0.3, bounce: 0.1)) {
                    commit(zoom: zoom, offset: offset, fitted: fitted, container: container)
                }
            }
    }

    private func panGesture(fitted: CGSize, container: CGSize) -> some Gesture {
        DragGesture()
            .onChanged { value in
                let proposed = CGSize(
                    width: committedOffset.width + value.translation.width,
                    height: committedOffset.height + value.translation.height
                )
                offset = ZoomableImageMath.rubberBanded(
                    proposed,
                    limit: ZoomableImageMath.offsetLimit(
                        fitted: fitted, container: container, zoom: zoom
                    )
                )
            }
            .onEnded { _ in
                withAnimation(.spring(duration: 0.3, bounce: 0.1)) {
                    commit(zoom: zoom, offset: offset, fitted: fitted, container: container)
                }
            }
    }

    /// Double-tap toggles between the resting fit and full zoom, centred on the
    /// point that was tapped — the same behaviour the `UIScrollView`'s
    /// `zoom(to:animated:)` gave.
    private func handleDoubleTap(at location: CGPoint, fitted: CGSize, container: CGSize) {
        let isZoomed = zoom > ZoomableImageMath.minZoom
        let targetZoom = isZoomed ? ZoomableImageMath.minZoom : ZoomableImageMath.maxZoom
        let targetOffset = isZoomed
            ? CGSize.zero
            : ZoomableImageMath.anchoredOffset(
                current: offset,
                from: zoom,
                to: targetZoom,
                anchor: location,
                container: container
            )
        withAnimation(.easeInOut(duration: 0.28)) {
            commit(zoom: targetZoom, offset: targetOffset, fitted: fitted, container: container)
        }
    }

    // MARK: State

    /// Settle on a zoom / offset pair that is inside the bounds, and remember it
    /// as the base for the next gesture.
    private func commit(zoom newZoom: CGFloat, offset newOffset: CGSize, fitted: CGSize, container: CGSize) {
        let settledZoom = ZoomableImageMath.clampedZoom(newZoom)
        let settledOffset = ZoomableImageMath.clamped(
            newOffset,
            limit: ZoomableImageMath.offsetLimit(
                fitted: fitted, container: container, zoom: settledZoom
            )
        )
        zoom = settledZoom
        offset = settledOffset
        committedZoom = settledZoom
        committedOffset = settledOffset
    }

    private func reset() {
        zoom = 1
        committedZoom = 1
        offset = .zero
        committedOffset = .zero
    }

    /// The displayed aspect ratio, falling back to square when the image cannot
    /// report a size.
    private var aspectRatio: CGFloat {
        let size = image.displaySize
        guard size.width > 0, size.height > 0 else { return 1 }
        return size.width / size.height
    }
}
