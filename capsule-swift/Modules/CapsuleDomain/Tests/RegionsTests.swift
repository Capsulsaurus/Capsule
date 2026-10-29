import Foundation
import Testing

import CapsuleDomain

/// The label rule and the region shape contract, which mirror the core's
/// `normalize_label` and `RegionShape::validate` so an editor refuses before
/// it sends what the core would refuse on write.
@Suite("Labels and region shapes")
struct RegionsTests {
    @Test("A label is NFC-normalized and trimmed, and keeps its case")
    func labelNormalizes() throws {
        // "e" followed by a combining acute accent composes to one scalar.
        let decomposed = "  Cafe\u{0301} Trip\n"
        let label = try UserLabel.normalize(decomposed)
        #expect(label == "Caf\u{00E9} Trip")
        #expect(label.unicodeScalars.count == 9)
    }

    @Test("A label of only whitespace is refused as empty")
    func labelEmpty() {
        #expect(throws: UserLabel.Refusal.empty) { try UserLabel.normalize(" \t\n ") }
    }

    @Test("A label is bounded in UTF-8 bytes, not characters")
    func labelBound() throws {
        let atBound = String(repeating: "a", count: UserLabel.maxBytes)
        #expect(try UserLabel.normalize(atBound) == atBound)
        // 129 two-byte characters: 129 characters, 258 bytes.
        let pastBound = String(repeating: "\u{00E9}", count: 129)
        #expect(throws: UserLabel.Refusal.tooLong(bytes: 258)) { try UserLabel.normalize(pastBound) }
    }

    @Test("A shape inside the image validates")
    func shapesInside() throws {
        try RegionShape.circle(centerX: 0.5, centerY: 0.5, radius: 0.5).validate()
        try RegionShape.rect(left: 0, top: 0, width: 1, height: 1).validate()
        try RegionShape.rect(left: 0.25, top: 0.1, width: 0.5, height: 0.2).validate()
    }

    @Test("A coordinate outside [0, 1] or not finite is out of range")
    func shapesOutOfRange() {
        #expect(throws: RegionShape.Refusal.outOfRange) {
            try RegionShape.rect(left: -0.1, top: 0, width: 0.5, height: 0.5).validate()
        }
        #expect(throws: RegionShape.Refusal.outOfRange) {
            try RegionShape.circle(centerX: .nan, centerY: 0.5, radius: 0.1).validate()
        }
        #expect(throws: RegionShape.Refusal.outOfRange) {
            try RegionShape.circle(centerX: 0.5, centerY: 0.5, radius: .infinity).validate()
        }
    }

    @Test("A shape with no area is empty")
    func shapesEmpty() {
        #expect(throws: RegionShape.Refusal.empty) {
            try RegionShape.circle(centerX: 0.5, centerY: 0.5, radius: 0).validate()
        }
        #expect(throws: RegionShape.Refusal.empty) {
            try RegionShape.rect(left: 0.1, top: 0.1, width: 0.2, height: 0).validate()
        }
    }

    @Test("A shape past an edge is refused; a circle must fit its radius on both axes")
    func shapesPastEdge() {
        #expect(throws: RegionShape.Refusal.pastEdge) {
            try RegionShape.rect(left: 0.6, top: 0, width: 0.5, height: 0.5).validate()
        }
        #expect(throws: RegionShape.Refusal.pastEdge) {
            try RegionShape.circle(centerX: 0.2, centerY: 0.5, radius: 0.3).validate()
        }
        #expect(throws: RegionShape.Refusal.pastEdge) {
            try RegionShape.circle(centerX: 0.5, centerY: 0.9, radius: 0.2).validate()
        }
    }
}
