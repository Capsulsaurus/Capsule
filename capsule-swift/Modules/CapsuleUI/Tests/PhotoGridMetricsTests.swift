import CoreGraphics
import Foundation
import Testing

@testable import CapsuleUI

@Suite("PhotoGridMetrics decode sizing")
struct PhotoGridMetricsTests {
    @Test("an unmeasured grid asks for no decode at all")
    func unmeasuredGridDecodesNothing() {
        let size = PhotoGridMetrics.decodeSize(
            containerWidth: 0, style: .uniform(columns: 3), displayScale: 3
        )
        #expect(size == .zero)
    }

    @Test("a uniform tile decodes square, at the column width in device pixels")
    func uniformTileIsSquareAndScaled() {
        // 390 pt / 3 columns = 130 pt = 390 px at 3×, quantised up to 448.
        let size = PhotoGridMetrics.decodeSize(
            containerWidth: 390, style: .uniform(columns: 3), displayScale: 3
        )
        #expect(size.width == size.height)
        #expect(size.width >= 390)
        #expect(size.width == PhotoGridMetrics.quantized(390))
    }

    @Test("more columns means a smaller decode")
    func moreColumnsDecodeSmaller() {
        let three = PhotoGridMetrics.decodeSize(
            containerWidth: 1024, style: .uniform(columns: 3), displayScale: 2
        )
        let seven = PhotoGridMetrics.decodeSize(
            containerWidth: 1024, style: .uniform(columns: 7), displayScale: 2
        )
        #expect(seven.width < three.width)
    }

    @Test("a nonsensical column count still yields a usable decode")
    func columnCountIsFloored() {
        let size = PhotoGridMetrics.decodeSize(
            containerWidth: 500, style: .uniform(columns: 0), displayScale: 1
        )
        #expect(size.width > 0)
        #expect(size.width.isFinite)
    }

    @Test("a card decodes full width and taller than it displays")
    func cardDecodesWithCropHeadroom() {
        let size = PhotoGridMetrics.decodeSize(
            containerWidth: 400, style: .cards, displayScale: 2
        )
        #expect(size.width == PhotoGridMetrics.quantized(800))
        #expect(size.height > size.width * PhotoGridMetrics.cardHeightRatio)
    }

    @Test("nearby widths quantise to the same decode, so a resize is not a re-decode storm")
    func quantisationCollapsesNearbyWidths() {
        let style = PhotoGridStyle.uniform(columns: 3)
        let first = PhotoGridMetrics.decodeSize(containerWidth: 900, style: style, displayScale: 2)
        let second = PhotoGridMetrics.decodeSize(containerWidth: 901, style: style, displayScale: 2)
        #expect(first == second)
    }

    @Test("a decode is never smaller than one quantum")
    func quantisationHasAFloor() {
        #expect(PhotoGridMetrics.quantized(1) == PhotoGridMetrics.decodeQuantum)
        #expect(PhotoGridMetrics.quantized(0) == 0)
    }

    @Test("a display scale below 1 is treated as 1 rather than shrinking the decode")
    func displayScaleIsFloored() {
        let style = PhotoGridStyle.uniform(columns: 2)
        let unscaled = PhotoGridMetrics.decodeSize(containerWidth: 800, style: style, displayScale: 1)
        let bogus = PhotoGridMetrics.decodeSize(containerWidth: 800, style: style, displayScale: 0)
        #expect(unscaled == bogus)
    }
}

@Suite("PhotoGridStyle layout mapping")
struct PhotoGridStyleLayoutTests {
    @Test("a uniform style maps to a uniform grid, honouring the header request")
    func uniformMapsToGrid() {
        let layout = PhotoGridStyle.uniform(columns: 5).platformLayout(pinnedHeaders: true)
        #expect(layout == .uniformGrid(
            columns: 5, itemSpacing: PhotoGridMetrics.tileSpacing, pinnedHeaders: true
        ))
    }

    @Test("headers can be switched off")
    func headersAreOptional() {
        let layout = PhotoGridStyle.uniform(columns: 5).platformLayout(pinnedHeaders: false)
        #expect(layout == .uniformGrid(
            columns: 5, itemSpacing: PhotoGridMetrics.tileSpacing, pinnedHeaders: false
        ))
    }

    @Test("cards map to full-width rows and never carry a pinned header")
    func cardsMapToRows() {
        let layout = PhotoGridStyle.cards.platformLayout(pinnedHeaders: true)
        #expect(layout == .fullWidthRows(
            heightRatio: PhotoGridMetrics.cardHeightRatio,
            horizontalInset: PhotoGridMetrics.cardHorizontalInset,
            verticalInset: PhotoGridMetrics.cardVerticalInset
        ))
    }
}

@Suite("Pinch-to-zoom thresholds")
struct PlatformCollectionMagnificationTests {
    @Test("a spread past the threshold zooms in")
    func spreadZoomsIn() {
        #expect(PlatformCollectionMagnification.step(forScale: 1.5) == true)
    }

    @Test("a pinch past the threshold zooms out")
    func pinchZoomsOut() {
        #expect(PlatformCollectionMagnification.step(forScale: 0.5) == false)
    }

    @Test("a small pinch means nothing, so a level switch is never accidental")
    func smallPinchIsIgnored() {
        #expect(PlatformCollectionMagnification.step(forScale: 1) == nil)
        #expect(PlatformCollectionMagnification.step(forScale: 1.1) == nil)
        #expect(PlatformCollectionMagnification.step(forScale: 0.9) == nil)
    }

    @Test("AppKit's magnification delta reaches the same verdict as UIKit's scale")
    func appKitDeltaAgreesWithUIKitScale() {
        // AppKit reports magnification around zero; the controller adds one.
        #expect(PlatformCollectionMagnification.step(forScale: 1 + 0.4) == true)
        #expect(PlatformCollectionMagnification.step(forScale: 1 + -0.4) == false)
        #expect(PlatformCollectionMagnification.step(forScale: 1 + 0.05) == nil)
    }
}
