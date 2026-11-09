import CapsuleDomain
import CoreGraphics
import Testing

@testable import FeatureViewer

/// Regions are fractions of the image; the editor draws the image aspect-fit.
/// These pin the conversions a drag, a pinch and every drawn outline rely on.
@Suite("Region geometry")
struct RegionGeometryTests {
    @Test("A wide image fits the width and is centred vertically")
    func aspectFitWide() {
        let frame = RegionGeometry.aspectFit(CGSize(width: 400, height: 200), in: CGRect(x: 0, y: 0, width: 200, height: 200))
        #expect(frame == CGRect(x: 0, y: 50, width: 200, height: 100))
    }

    @Test("A tall image fits the height and is centred horizontally")
    func aspectFitTall() {
        let frame = RegionGeometry.aspectFit(CGSize(width: 100, height: 400), in: CGRect(x: 10, y: 0, width: 300, height: 200))
        #expect(frame == CGRect(x: 135, y: 0, width: 50, height: 200))
    }

    @Test("Nothing to fit is an empty frame at the centre")
    func aspectFitEmpty() {
        let frame = RegionGeometry.aspectFit(.zero, in: CGRect(x: 0, y: 0, width: 100, height: 60))
        #expect(frame.size == .zero)
        #expect(frame.origin == CGPoint(x: 50, y: 30))
    }

    @Test("A circle's radius is a fraction of the shorter side, so it stays round")
    func circleFrame() {
        let image = CGRect(x: 0, y: 50, width: 200, height: 100)
        let frame = RegionGeometry.frame(of: RegionDraft.circle(centerX: 0.5, centerY: 0.5, radius: 0.25), in: image)
        #expect(frame == CGRect(x: 75, y: 75, width: 50, height: 50))
    }

    @Test("A rectangle scales each axis by its own side")
    func rectFrame() {
        let image = CGRect(x: 0, y: 50, width: 200, height: 100)
        let shape = RegionShape.rect(left: 0.25, top: 0.5, width: 0.5, height: 0.25)
        #expect(RegionGeometry.frame(of: shape, in: image) == CGRect(x: 50, y: 100, width: 100, height: 25))
    }

    @Test("A stored shape reads back as the draft that writes it")
    func draftRoundTrip() {
        let shapes: [RegionShape] = [
            .circle(centerX: 0.5, centerY: 0.25, radius: 0.125),
            .rect(left: 0.25, top: 0, width: 0.5, height: 1),
        ]
        for shape in shapes {
            #expect(RegionGeometry.draft(of: shape).shape == shape)
        }
    }

    @Test("A drag is a fraction of the image's width and height")
    func dragFractions() {
        let image = CGRect(x: 10, y: 10, width: 200, height: 100)
        let step = RegionGeometry.fractions(of: CGSize(width: 50, height: -25), in: image)
        #expect(step.dx == 0.25)
        #expect(step.dy == -0.25)
        let none = RegionGeometry.fractions(of: CGSize(width: 50, height: 50), in: .zero)
        #expect(none.dx == 0 && none.dy == 0)
    }

    @Test("Every refusal and every subject is named, each differently")
    func presentation() {
        let failures: [LabelEditorModel.Failure] = [
            .notFound, .labelEmpty, .labelTooLong(bytes: 300), .shapeRefused(.pastEdge), .unavailable,
        ]
        #expect(Set(failures.map(\.messageKey)).count == failures.count)
        let subjects: [LabelEditorModel.Subject] = [
            .asset(.managed(uuid: "a")), .stack(StackID("s")), .album(.managed(uuid: "b")),
        ]
        #expect(Set(subjects.map(\.titleKey)).count == subjects.count)
    }

    @Test("A region is named by its labels, or by nothing until it has one")
    func regionNames() {
        let unlabelled = AssetRegion(id: RegionID("r"), shape: .rect(left: 0, top: 0, width: 1, height: 1), labels: [])
        #expect(unlabelled.displayLabels == nil)
        var labelled = unlabelled
        labelled.labels = ["Ada"]
        #expect(labelled.displayLabels == "Ada")
    }
}
