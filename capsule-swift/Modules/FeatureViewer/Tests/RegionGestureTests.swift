import Testing

@testable import FeatureViewer

/// A simultaneous drag and pinch compose from one base, so neither erases the
/// other and neither compounds when the other ends.
@Suite("Region gesture composition")
struct RegionGestureTests {
    private let base = RegionDraft.rect(left: 0.4, top: 0.4, width: 0.2, height: 0.2)

    private func isClose(_ draft: RegionDraft, _ expected: RegionDraft) -> Bool {
        switch (draft, expected) {
        case let (.rect(l1, t1, w1, h1), .rect(l2, t2, w2, h2)):
            [l1 - l2, t1 - t2, w1 - w2, h1 - h2].allSatisfy { abs($0) < 1e-9 }
        case let (.circle(x1, y1, r1), .circle(x2, y2, r2)):
            [x1 - x2, y1 - y2, r1 - r2].allSatisfy { abs($0) < 1e-9 }
        default:
            false
        }
    }

    @Test("A drag alone moves the base by its cumulative translation")
    func dragAlone() {
        var gesture = RegionGesture(base: base)
        gesture.drag(dx: 0.05, dy: 0)
        gesture.drag(dx: 0.1, dy: 0.05)
        #expect(isClose(gesture.draft, base.moved(dx: 0.1, dy: 0.05)))
        #expect(gesture.isActive)
        gesture.endDrag()
        #expect(!gesture.isActive)
        #expect(isClose(gesture.draft, base.moved(dx: 0.1, dy: 0.05)))
    }

    @Test("A drag and a pinch together apply both changes on every update")
    func bothApply() {
        var gesture = RegionGesture(base: base)
        gesture.drag(dx: 0.1, dy: 0)
        gesture.pinch(by: 1.5)
        let both = base.scaled(by: 1.5).moved(dx: 0.1, dy: 0)
        #expect(isClose(gesture.draft, both))
        // A later drag update keeps the pinch, and a later pinch keeps the drag.
        gesture.drag(dx: 0.1, dy: 0)
        #expect(isClose(gesture.draft, both))
        gesture.pinch(by: 1.5)
        #expect(isClose(gesture.draft, both))
    }

    @Test("Ending one gesture neither undoes nor compounds the other")
    func endingOneKeepsTheOther() {
        var gesture = RegionGesture(base: base)
        gesture.drag(dx: 0.1, dy: 0)
        gesture.pinch(by: 1.5)
        gesture.endDrag()
        #expect(gesture.isActive)
        gesture.pinch(by: 1.5)
        #expect(isClose(gesture.draft, base.scaled(by: 1.5).moved(dx: 0.1, dy: 0)))
        gesture.pinch(by: 2)
        #expect(isClose(gesture.draft, base.scaled(by: 2).moved(dx: 0.1, dy: 0)))
        gesture.endPinch()
        #expect(!gesture.isActive)
        #expect(isClose(gesture.draft, base.scaled(by: 2).moved(dx: 0.1, dy: 0)))
    }

    @Test("A gesture that begins again while the other runs adds to what it left")
    func restartAccumulates() {
        var gesture = RegionGesture(base: base)
        gesture.pinch(by: 1.2)
        gesture.drag(dx: 0.05, dy: 0)
        gesture.endDrag()
        gesture.drag(dx: 0.05, dy: 0.02)
        #expect(isClose(gesture.draft, base.scaled(by: 1.2).moved(dx: 0.1, dy: 0.02)))
    }

    @Test("An unusable magnification is ignored")
    func invalidPinch() {
        var gesture = RegionGesture(base: base)
        gesture.pinch(by: .nan)
        gesture.pinch(by: 0)
        #expect(!gesture.isActive)
        #expect(gesture.draft == base)
    }
}
