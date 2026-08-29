import CoreGraphics
import Foundation
import Testing

@testable import CapsuleUI

@Suite("Photo grid pinch zoom")
struct PhotoGridZoomTests {
    // MARK: Direction

    /// The assertion that catches an inverted gesture, which is the defect this
    /// arithmetic exists to prevent and the one nobody notices in code review.
    @Test("spreading the fingers means fewer, larger tiles")
    func spreadingReducesColumns() {
        let columns = PhotoGridZoom.continuousColumns(base: 5, scale: 2)
        #expect(columns < 5)
    }

    @Test("pinching the fingers together means more, smaller tiles")
    func pinchingIncreasesColumns() {
        let columns = PhotoGridZoom.continuousColumns(base: 5, scale: 0.5)
        #expect(columns > 5)
    }

    @Test("a gesture that has not moved changes nothing")
    func identityScaleIsIdentity() {
        #expect(PhotoGridZoom.continuousColumns(base: 5, scale: 1) == 5)
        #expect(PhotoGridZoom.settle(PhotoGridZoom.continuousColumns(base: 5, scale: 1)) == 5)
    }

    /// A pinch recognizer can report zero or a negative scale on the frame the
    /// gesture is cancelled. Dividing by it would produce an infinity that
    /// `settle` then has to defend against; refusing here is cheaper.
    @Test("a degenerate scale leaves the base untouched", arguments: [CGFloat(0), -1, -0.5])
    func degenerateScaleIsRefused(scale: CGFloat) {
        #expect(PhotoGridZoom.continuousColumns(base: 7, scale: scale) == 7)
    }

    // MARK: Settling

    @Test("every rung settles to itself")
    func rungsAreFixedPoints() {
        for rung in PhotoGridZoom.ladder {
            #expect(PhotoGridZoom.settle(CGFloat(rung)) == rung)
        }
    }

    @Test("settling always lands on a rung, never between them")
    func settlingAlwaysLandsOnARung() {
        for step in stride(from: CGFloat(0.5), through: 20, by: 0.1) {
            #expect(PhotoGridZoom.ladder.contains(PhotoGridZoom.settle(step)))
        }
    }

    @Test("a value past either end clamps to the nearest rung")
    func outOfRangeClamps() {
        #expect(PhotoGridZoom.settle(0.2) == PhotoGridZoom.ladder.first)
        #expect(PhotoGridZoom.settle(500) == PhotoGridZoom.ladder.last)
    }

    @Test("a NaN scale settles to the default rather than trapping")
    func nonFiniteSettlesToDefault() {
        #expect(PhotoGridZoom.settle(.nan) == PhotoGridZoom.defaultColumns)
    }

    /// Settling is proportional, not linear, and the two genuinely disagree.
    ///
    /// The linear midpoint between the 7 and 10 rungs is 8.5; the geometric one
    /// is √70 ≈ 8.37. Anything in between — 8.4 here — settles to 7 under
    /// subtraction and to 10 under ratio. Ratio is the right answer because it
    /// is what a reader perceives: 7→10 columns shrinks a tile by 30%, the same
    /// proportional step as 5→7, even though one gap is 3 and the other is 2.
    @Test("settling is nearest by ratio, not by subtraction")
    func settlingIsProportional() {
        #expect(abs(CGFloat(7) - 8.4) < abs(CGFloat(10) - 8.4)) // subtraction says 7
        #expect(PhotoGridZoom.settle(8.4) == 10) // ratio says 10
        #expect(PhotoGridZoom.settle(8.3) == 7) // just below the geometric mid
    }

    /// The whole ladder has to be reachable by pinching, or a rung is decoration.
    @Test("every rung is reachable from the default by some pinch")
    func everyRungIsReachable() {
        var reached: Set<Int> = []
        for scale in stride(from: CGFloat(0.2), through: 4, by: 0.01) {
            let continuous = PhotoGridZoom.continuousColumns(
                base: PhotoGridZoom.defaultColumns, scale: scale
            )
            reached.insert(PhotoGridZoom.settle(continuous))
        }
        #expect(reached == Set(PhotoGridZoom.ladder))
    }

    @Test("settling is monotonic in the gesture")
    func settlingIsMonotonic() {
        var previous = Int.max
        for scale in stride(from: CGFloat(0.2), through: 4, by: 0.01) {
            let settled = PhotoGridZoom.settle(
                PhotoGridZoom.continuousColumns(base: 5, scale: scale)
            )
            #expect(settled <= previous)
            previous = settled
        }
    }

    // MARK: Handing off to another level

    @Test("a pinch inside the ladder never changes level")
    func insideTheLadderStaysPut() {
        #expect(PhotoGridZoom.levelStep(base: 5, scale: 1) == nil)
        #expect(PhotoGridZoom.levelStep(base: 5, scale: 0.6) == nil)
        #expect(PhotoGridZoom.levelStep(base: 2, scale: 3) == nil)
    }

    /// Resting *at* the coarsest density must not tip into Months, or a hand
    /// that trembles at the end of a pinch changes the screen.
    @Test("resting at the coarsest rung does not change level")
    func coarsestRungIsStable() {
        guard let coarsest = PhotoGridZoom.ladder.last else { return }
        let scaleToCoarsest = CGFloat(5) / CGFloat(coarsest)
        #expect(PhotoGridZoom.levelStep(base: 5, scale: scaleToCoarsest) == nil)
    }

    @Test("pinching well past the coarsest rung goes coarser")
    func pastTheCoarsestRungHandsOff() {
        guard let coarsest = PhotoGridZoom.ladder.last else { return }
        let scale = CGFloat(5) / (CGFloat(coarsest) * PhotoGridZoom.levelHandoffMargin * 1.2)
        #expect(PhotoGridZoom.levelStep(base: 5, scale: scale) == false)
    }

    /// Spreading past the largest tile does *not* open the viewer. A stray
    /// gesture on the grid taking over the screen is worse than a gesture that
    /// stops.
    @Test("spreading past the finest rung never hands off")
    func pastTheFinestRungStops() {
        #expect(PhotoGridZoom.levelStep(base: 2, scale: 50) == nil)
    }

    // MARK: Stepping

    @Test("stepping walks the ladder and stops at both ends")
    func steppingWalksAndStops() {
        #expect(PhotoGridZoom.stepped(from: 5, finer: true) == 7)
        #expect(PhotoGridZoom.stepped(from: 5, finer: false) == 4)
        #expect(PhotoGridZoom.stepped(from: PhotoGridZoom.ladder.last ?? 10, finer: true)
            == PhotoGridZoom.ladder.last)
        #expect(PhotoGridZoom.stepped(from: PhotoGridZoom.ladder.first ?? 2, finer: false)
            == PhotoGridZoom.ladder.first)
    }

    /// A column count persisted before the ladder changed must not send the
    /// grid somewhere arbitrary.
    @Test("stepping from a value not on the ladder recovers to the default")
    func steppingFromAnUnknownValueRecovers() {
        #expect(PhotoGridZoom.ladder.contains(PhotoGridZoom.stepped(from: 6, finer: true)))
        #expect(PhotoGridZoom.ladder.contains(PhotoGridZoom.stepped(from: 99, finer: false)))
    }
}
