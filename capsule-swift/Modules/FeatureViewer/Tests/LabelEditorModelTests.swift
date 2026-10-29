import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsulePorts
import Testing

@testable import FeatureViewer

/// The editor writes through the port, redraws from its answers, and leaves
/// its state as it was when a write is refused.
@MainActor
@Suite("Label editor model")
struct LabelEditorModelTests {
    private let asset = AssetID.managed(uuid: "asset-1")
    private let stack = StackID("stack-1")

    private func port() -> MockLabelStore {
        MockLabelStore(existence: .everything, seed: 3)
    }

    @Test("On a stack, labels are added and removed as the port stores them")
    func stackLabels() async {
        let model = LabelEditorModel(subject: .stack(stack), port: port())
        await model.load()
        #expect(model.labelTarget == .stack(stack))
        await model.addLabel("  Holiday ")
        await model.addLabel("beach")
        #expect(model.labels == ["Holiday", "beach"])
        await model.removeLabel("Holiday")
        #expect(model.labels == ["beach"])
        #expect(model.failure == nil)
    }

    @Test("On an asset with no region selected, a typed label goes nowhere")
    func assetWithoutRegion() async {
        let model = LabelEditorModel(subject: .asset(asset), port: port())
        await model.load()
        #expect(model.labelTarget == nil)
        await model.addLabel("ignored")
        #expect(model.labels.isEmpty)
    }

    @Test("Drawing writes the draft as a region, selects it, and labels go to it")
    func drawAndLabel() async throws {
        let model = LabelEditorModel(subject: .asset(asset), port: port())
        await model.load()
        model.startDrawing(.circle)
        model.draft = model.draft?.moved(dx: 0.1, dy: 0)
        await model.finishDrawing(labels: ["face"])
        #expect(model.draft == nil)
        let region = try #require(model.regions.first)
        #expect(model.selectedRegionID == region.id)
        #expect(model.labels == ["face"])
        await model.addLabel("Alice")
        #expect(model.labels == ["Alice", "face"])
        #expect(model.regions.first?.labels == ["Alice", "face"])
    }

    @Test("Reloading keeps the selection and reads the store's regions")
    func reload() async throws {
        let store = port()
        let first = try await store.addRegion(.rect(left: 0, top: 0, width: 0.5, height: 0.5), labels: ["a"], to: asset)
        let second = try await store.addRegion(.rect(left: 0.5, top: 0.5, width: 0.5, height: 0.5), labels: ["b"], to: asset)
        let model = LabelEditorModel(subject: .asset(asset), port: store)
        await model.load()
        #expect(model.selectedRegionID == first.id)
        model.selectRegion(second.id)
        #expect(model.labels == ["b"])
        await model.load()
        #expect(model.selectedRegionID == second.id)
    }

    @Test("Deleting the selected region selects the next one")
    func deleteRegion() async throws {
        let store = port()
        let first = try await store.addRegion(.rect(left: 0, top: 0, width: 0.5, height: 0.5), labels: [], to: asset)
        let second = try await store.addRegion(.rect(left: 0.5, top: 0.5, width: 0.5, height: 0.5), labels: ["b"], to: asset)
        let model = LabelEditorModel(subject: .asset(asset), port: store)
        await model.load()
        #expect(model.selectedRegionID == first.id)
        await model.deleteSelectedRegion()
        #expect(model.regions.map(\.id) == [second.id])
        #expect(model.selectedRegionID == second.id)
        #expect(model.labels == ["b"])
    }

    @Test("A refused label is named and changes nothing")
    func refusedLabel() async {
        let model = LabelEditorModel(subject: .stack(stack), port: port())
        await model.addLabel("kept")
        await model.addLabel("   ")
        #expect(model.failure == .labelEmpty)
        #expect(model.labels == ["kept"])
        await model.addLabel("next")
        #expect(model.failure == nil)
    }

    @Test("A missing subject is named as not found")
    func missingSubject() async {
        let store = MockLabelStore(
            existence: .init(asset: { _ in false }, stack: { _ in false }, album: { _ in false }),
            seed: 3
        )
        let model = LabelEditorModel(subject: .stack(stack), port: store)
        await model.load()
        #expect(model.failure == .notFound)
    }

    @Test("Errors map onto the editor's failures")
    func failureMapping() {
        #expect(LabelEditorModel.failure(for: UserLabel.Refusal.tooLong(bytes: 300)) == .labelTooLong(bytes: 300))
        #expect(LabelEditorModel.failure(for: RegionShape.Refusal.pastEdge) == .shapeRefused(.pastEdge))
        #expect(LabelEditorModel.failure(for: CancellationError()) == .unavailable)
    }

    @Test("A stack or album has no regions to draw")
    func noDrawingOffAsset() {
        let model = LabelEditorModel(subject: .stack(stack), port: port())
        model.startDrawing(.rect)
        #expect(model.draft == nil)
    }
}
