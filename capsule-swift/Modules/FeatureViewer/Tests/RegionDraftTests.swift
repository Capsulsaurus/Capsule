import CapsuleDomain
import Testing

@testable import FeatureViewer

/// Every edit keeps a draft inside the shape contract the core validates, so
/// whatever the user drew is writable.
@Suite("Region draft geometry")
struct RegionDraftTests {
    private func isValid(_ draft: RegionDraft) -> Bool {
        (try? draft.shape.validate()) != nil
    }

    @Test("A new draft sits in the middle of the image and is writable")
    func newDrafts() {
        #expect(RegionDraft(.circle) == .circle(centerX: 0.5, centerY: 0.5, radius: 0.16))
        #expect(RegionDraft(.rect) == .rect(left: 0.34, top: 0.34, width: 0.32, height: 0.32))
        #expect(RegionDraft.Kind.allCases.map { RegionDraft($0) }.allSatisfy { isValid($0) })
    }

    @Test("A drag past an edge stops at the edge")
    func moveClamps() {
        let rect = RegionDraft(.rect).moved(dx: 5, dy: -5)
        #expect(rect == .rect(left: 1 - 0.32, top: 0, width: 0.32, height: 0.32))
        #expect(isValid(rect))
        let circle = RegionDraft(.circle).moved(dx: -5, dy: 5)
        #expect(circle == .circle(centerX: 0.16, centerY: 0.84, radius: 0.16))
        #expect(isValid(circle))
    }

    @Test("A resize never shrinks below the minimum or grows past the image")
    func resizeClamps() {
        let small = RegionDraft(.rect).resized(dw: -1, dh: -1)
        #expect(small == .rect(left: 0.34, top: 0.34, width: RegionDraft.minimumExtent, height: RegionDraft.minimumExtent))
        let large = RegionDraft(.rect).resized(dw: 1, dh: 1)
        #expect(large == .rect(left: 0.34, top: 0.34, width: 1 - 0.34, height: 1 - 0.34))
        #expect(isValid(large))
    }

    @Test("A circle grows only until it meets the nearest edge on either axis")
    func circleGrowth() {
        let offCentre = RegionDraft.circle(centerX: 0.3, centerY: 0.5, radius: 0.1)
        #expect(offCentre.resized(dw: 1, dh: 0) == .circle(centerX: 0.3, centerY: 0.5, radius: 0.3))
        // The larger of the two steps applies.
        #expect(offCentre.resized(dw: 0.01, dh: -0.05) == .circle(centerX: 0.3, centerY: 0.5, radius: 0.05))
        #expect(isValid(offCentre.resized(dw: 1, dh: 1)))
    }

    @Test("A pinch scales about the centre and stays inside the image")
    func pinch() {
        let rect = RegionDraft.rect(left: 0.4, top: 0.4, width: 0.2, height: 0.2).scaled(by: 2)
        guard case let .rect(left, top, width, height) = rect else {
            Issue.record("a scaled rect is a rect")
            return
        }
        #expect(abs(left - 0.3) < 1e-9 && abs(top - 0.3) < 1e-9)
        #expect(abs(width - 0.4) < 1e-9 && abs(height - 0.4) < 1e-9)
        #expect(isValid(RegionDraft(.rect).scaled(by: 100)))
        #expect(isValid(RegionDraft(.circle).scaled(by: 100)))
        #expect(RegionDraft(.circle).scaled(by: .nan) == RegionDraft(.circle))
        #expect(RegionDraft(.circle).scaled(by: 0) == RegionDraft(.circle))
    }

    @Test("A shape meeting an edge exactly is shaved to fit in single precision")
    func edgeRounding() throws {
        // Fractions that sum to exactly 1 in Double can round past 1 in Float.
        for left in stride(from: 0.0, through: 0.98, by: 0.01) {
            let rect = RegionDraft.rect(left: left, top: left, width: 1 - left, height: 1 - left)
            try rect.shape.validate()
        }
        for centre in stride(from: 0.02, through: 0.98, by: 0.01) {
            let circle = RegionDraft.circle(centerX: centre, centerY: 1 - centre, radius: min(centre, 1 - centre))
            try circle.shape.validate()
        }
    }
}
