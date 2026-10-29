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

    @Test("While a label write is in flight, selection and drawing are refused and the answer lands on its region")
    func selectionHeldWhileWriting() async throws {
        let store = port()
        let first = try await store.addRegion(.rect(left: 0, top: 0, width: 0.5, height: 0.5), labels: ["a"], to: asset)
        let second = try await store.addRegion(.rect(left: 0.5, top: 0.5, width: 0.5, height: 0.5), labels: ["b"], to: asset)
        let gate = Gate()
        let model = LabelEditorModel(subject: .asset(asset), port: GatedLabelPort(inner: store, gate: gate))
        await model.load()
        #expect(model.selectedRegionID == first.id)
        let write = Task { await model.addLabel("x") }
        await gate.waitUntilEntered()
        model.selectRegion(second.id)
        model.startDrawing(.rect)
        #expect(model.selectedRegionID == first.id)
        #expect(model.draft == nil)
        await gate.open()
        await write.value
        #expect(model.selectedRegionID == first.id)
        #expect(model.labels == ["a", "x"])
        #expect(model.regions.map(\.labels) == [["a", "x"], ["b"]])
        model.selectRegion(second.id)
        #expect(model.labels == ["b"])
    }

    @Test("Finishing a region keeps a draft that replaced it during the write")
    func finishKeepsNewerDraft() async throws {
        let gate = Gate()
        let model = LabelEditorModel(subject: .asset(asset), port: GatedLabelPort(inner: port(), gate: gate))
        await model.load()
        model.startDrawing(.rect)
        let write = Task { await model.finishDrawing(labels: ["face"]) }
        await gate.waitUntilEntered()
        let newer = RegionDraft(.circle)
        model.draft = newer
        await gate.open()
        await write.value
        #expect(model.regions.count == 1)
        #expect(model.draft == newer)
    }

    @Test("A stack or album has no regions to draw")
    func noDrawingOffAsset() {
        let model = LabelEditorModel(subject: .stack(stack), port: port())
        model.startDrawing(.rect)
        #expect(model.draft == nil)
    }
}

// MARK: - Gate

/// Holds the first write that reaches it until the test opens it, so the test
/// can act while that write is in flight.
private actor Gate {
    private var hasEntered = false
    private var isOpen = false
    private var entered: CheckedContinuation<Void, Never>?
    private var held: CheckedContinuation<Void, Never>?

    func pass() async {
        hasEntered = true
        entered?.resume()
        entered = nil
        guard !isOpen else { return }
        await withCheckedContinuation { held = $0 }
    }

    func waitUntilEntered() async {
        guard !hasEntered else { return }
        await withCheckedContinuation { entered = $0 }
    }

    func open() {
        isOpen = true
        held?.resume()
        held = nil
    }
}

// MARK: - GatedLabelPort

/// Forwards to a mock store, holding each write at the gate first.
private struct GatedLabelPort: LabelPort {
    let inner: MockLabelStore
    let gate: Gate

    func labels(on target: LabelTarget) async throws -> [String] {
        try await inner.labels(on: target)
    }

    func addLabel(_ label: String, to target: LabelTarget) async throws -> [String] {
        await gate.pass()
        return try await inner.addLabel(label, to: target)
    }

    func removeLabel(_ label: String, from target: LabelTarget) async throws -> [String] {
        await gate.pass()
        return try await inner.removeLabel(label, from: target)
    }

    func regions(of assetID: AssetID) async throws -> [AssetRegion] {
        try await inner.regions(of: assetID)
    }

    func addRegion(_ shape: RegionShape, labels: [String], to assetID: AssetID) async throws -> AssetRegion {
        await gate.pass()
        return try await inner.addRegion(shape, labels: labels, to: assetID)
    }

    func deleteRegion(_ regionID: RegionID, on assetID: AssetID) async throws {
        await gate.pass()
        try await inner.deleteRegion(regionID, on: assetID)
    }
}
