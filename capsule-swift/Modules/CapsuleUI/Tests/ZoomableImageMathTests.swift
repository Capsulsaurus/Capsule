import CoreGraphics
import Foundation
import Testing

@testable import CapsuleUI

@Suite("ZoomableImageMath fitting")
struct ZoomableImageFittingTests {
    private let container = CGSize(width: 100, height: 100)

    @Test("a wide image fits to the container's width")
    func wideImageFitsWidth() {
        let fitted = ZoomableImageMath.fittedSize(aspectRatio: 2, in: container)
        #expect(fitted == CGSize(width: 100, height: 50))
    }

    @Test("a tall image fits to the container's height")
    func tallImageFitsHeight() {
        let fitted = ZoomableImageMath.fittedSize(aspectRatio: 0.5, in: container)
        #expect(fitted == CGSize(width: 50, height: 100))
    }

    @Test("a degenerate aspect ratio or container yields nothing to lay out")
    func degenerateInputsAreSafe() {
        #expect(ZoomableImageMath.fittedSize(aspectRatio: 0, in: container) == .zero)
        #expect(ZoomableImageMath.fittedSize(aspectRatio: 1, in: .zero) == .zero)
    }
}

@Suite("ZoomableImageMath bounds")
struct ZoomableImageBoundsTests {
    private let container = CGSize(width: 100, height: 100)

    @Test("an image that fits cannot be panned at all")
    func restingImageHasNoSlack() {
        let fitted = ZoomableImageMath.fittedSize(aspectRatio: 2, in: container)
        let limit = ZoomableImageMath.offsetLimit(fitted: fitted, container: container, zoom: 1)
        #expect(limit == .zero)
    }

    @Test("zooming in opens up exactly the overflow, halved on each side")
    func zoomOpensSlack() {
        let fitted = ZoomableImageMath.fittedSize(aspectRatio: 2, in: container)
        let limit = ZoomableImageMath.offsetLimit(fitted: fitted, container: container, zoom: 2)
        #expect(limit == CGSize(width: 50, height: 0))
    }

    @Test("clamping keeps an edge from being dragged into view")
    func clampingHoldsTheEdge() {
        let limit = CGSize(width: 50, height: 0)
        let clamped = ZoomableImageMath.clamped(CGSize(width: 200, height: -30), limit: limit)
        #expect(clamped == CGSize(width: 50, height: 0))
    }

    @Test("an in-bounds offset survives clamping untouched")
    func inBoundsOffsetIsUntouched() {
        let limit = CGSize(width: 50, height: 20)
        let offset = CGSize(width: -10, height: 5)
        #expect(ZoomableImageMath.clamped(offset, limit: limit) == offset)
    }

    @Test("the rubber band damps overshoot instead of stopping dead")
    func rubberBandDampsOvershoot() {
        let limit = CGSize(width: 50, height: 50)
        let banded = ZoomableImageMath.rubberBanded(CGSize(width: 100, height: -100), limit: limit)
        let expected = 50 + 50 * ZoomableImageMath.rubberBandFactor
        #expect(banded.width == expected)
        #expect(banded.height == -expected)
        // Still moving, but less than the finger did.
        #expect(banded.width > limit.width)
        #expect(banded.width < 100)
    }

    @Test("the rubber band leaves in-bounds movement one-to-one")
    func rubberBandIsTransparentInBounds() {
        let limit = CGSize(width: 50, height: 50)
        let offset = CGSize(width: 20, height: -50)
        #expect(ZoomableImageMath.rubberBanded(offset, limit: limit) == offset)
    }

    @Test("zoom is clamped to the fit-to-4x range")
    func zoomIsClamped() {
        #expect(ZoomableImageMath.clampedZoom(0.2) == ZoomableImageMath.minZoom)
        #expect(ZoomableImageMath.clampedZoom(9) == ZoomableImageMath.maxZoom)
        #expect(ZoomableImageMath.clampedZoom(2) == 2)
    }
}

@Suite("ZoomableImageMath anchoring")
struct ZoomableImageAnchoringTests {
    private let container = CGSize(width: 100, height: 100)

    @Test("zooming about the centre never shifts the image")
    func centreAnchorDoesNotShift() {
        let offset = ZoomableImageMath.anchoredOffset(
            current: .zero,
            from: 1,
            to: 3,
            anchor: CGPoint(x: 50, y: 50),
            container: container
        )
        #expect(offset == .zero)
    }

    @Test("zooming about a corner pulls that corner back under the finger")
    func cornerAnchorPullsContent() {
        let offset = ZoomableImageMath.anchoredOffset(
            current: .zero,
            from: 1,
            to: 2,
            anchor: CGPoint(x: 100, y: 100),
            container: container
        )
        #expect(offset == CGSize(width: -50, height: -50))
    }

    @Test("an existing offset is carried through the zoom change")
    func existingOffsetIsScaled() {
        let offset = ZoomableImageMath.anchoredOffset(
            current: CGSize(width: 10, height: 0),
            from: 1,
            to: 2,
            anchor: CGPoint(x: 50, y: 50),
            container: container
        )
        #expect(offset == CGSize(width: 20, height: 0))
    }

    @Test("zooming out about a point is the exact inverse of zooming in")
    func anchoringIsReversible() {
        let anchor = CGPoint(x: 80, y: 20)
        let zoomedIn = ZoomableImageMath.anchoredOffset(
            current: .zero, from: 1, to: 4, anchor: anchor, container: container
        )
        let backOut = ZoomableImageMath.anchoredOffset(
            current: zoomedIn, from: 4, to: 1, anchor: anchor, container: container
        )
        #expect(abs(backOut.width) < 0.0001)
        #expect(abs(backOut.height) < 0.0001)
    }

    @Test("a zero starting zoom cannot divide by zero")
    func zeroZoomIsSafe() {
        let current = CGSize(width: 5, height: 5)
        let offset = ZoomableImageMath.anchoredOffset(
            current: current, from: 0, to: 2, anchor: .zero, container: container
        )
        #expect(offset == current)
    }
}
