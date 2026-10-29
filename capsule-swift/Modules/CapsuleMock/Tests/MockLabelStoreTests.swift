import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Testing

@testable import CapsuleMock

/// The mock's label store refuses what the core's workspace refuses, writes
/// nothing on a refusal, and answers every write with what its target holds.
@Suite("Mock label store")
struct MockLabelStoreTests {
    private let asset = AssetID.managed(uuid: "asset-1")
    private let stack = StackID("stack-1")
    private let album = AlbumID.managed(uuid: "album-1")
    private let square = RegionShape.rect(left: 0.1, top: 0.1, width: 0.3, height: 0.3)

    private func store(existing: Bool = true) -> MockLabelStore {
        MockLabelStore(
            existence: existing ? .everything : .init(asset: { _ in false }, stack: { _ in false }, album: { _ in false }),
            seed: 7
        )
    }

    @Test("Stack and album labels are normalized, deduplicated and sorted")
    func stackAndAlbumLabels() async throws {
        let store = store()
        _ = try await store.addLabel("  zebra ", to: .stack(stack))
        let labels = try await store.addLabel("apple", to: .stack(stack))
        #expect(labels == ["apple", "zebra"])
        let again = try await store.addLabel("apple", to: .stack(stack))
        #expect(again == ["apple", "zebra"])
        let albumLabels = try await store.addLabel("Trip", to: .album(album))
        #expect(albumLabels == ["Trip"])
    }

    @Test("Removing a label the target does not carry answers the unchanged list")
    func removeAbsent() async throws {
        let store = store()
        _ = try await store.addLabel("kept", to: .album(album))
        let labels = try await store.removeLabel("absent", from: .album(album))
        #expect(labels == ["kept"])
        let removed = try await store.removeLabel(" kept", from: .album(album))
        #expect(removed.isEmpty)
    }

    @Test("A refused label writes nothing")
    func refusedLabel() async throws {
        let store = store()
        await #expect(throws: UserLabel.Refusal.empty) { try await store.addLabel("   ", to: .stack(stack)) }
        let labels = try await store.labels(on: .stack(stack))
        #expect(labels.isEmpty)
    }

    @Test("A target the library does not hold is not found")
    func unknownTarget() async {
        let store = store(existing: false)
        await #expect(throws: LabelTargetRefusal.notFound) { try await store.addLabel("x", to: .stack(stack)) }
        await #expect(throws: LabelTargetRefusal.notFound) { try await store.regions(of: asset) }
        await #expect(throws: LabelTargetRefusal.notFound) {
            try await store.addRegion(square, labels: [], to: asset)
        }
    }

    @Test("A region is written with its first labels and takes labels of its own")
    func regionLabels() async throws {
        let store = store()
        let region = try await store.addRegion(square, labels: ["face", " Alice"], to: asset)
        #expect(region.labels == ["Alice", "face"])
        let target = LabelTarget.region(asset: asset, region: region.id)
        let labels = try await store.addLabel("smiling", to: target)
        #expect(labels == ["Alice", "face", "smiling"])
        let regions = try await store.regions(of: asset)
        #expect(regions.map(\.labels) == [["Alice", "face", "smiling"]])
    }

    @Test("A refused label or shape leaves no region behind")
    func refusedRegion() async throws {
        let store = store()
        await #expect(throws: UserLabel.Refusal.empty) { try await store.addRegion(square, labels: ["ok", " "], to: asset) }
        await #expect(throws: RegionShape.Refusal.pastEdge) {
            try await store.addRegion(.rect(left: 0.8, top: 0, width: 0.5, height: 0.5), labels: [], to: asset)
        }
        let regions = try await store.regions(of: asset)
        #expect(regions.isEmpty)
    }

    @Test("A delete is final: the region and its labels are gone, and its id is never minted again")
    func deleteIsFinal() async throws {
        let store = store()
        let first = try await store.addRegion(square, labels: ["a"], to: asset)
        try await store.deleteRegion(first.id, on: asset)
        let regions = try await store.regions(of: asset)
        #expect(regions.isEmpty)
        await #expect(throws: LabelTargetRefusal.notFound) {
            try await store.labels(on: .region(asset: asset, region: first.id))
        }
        await #expect(throws: LabelTargetRefusal.notFound) { try await store.deleteRegion(first.id, on: asset) }
        let second = try await store.addRegion(square, labels: [], to: asset)
        #expect(second.id != first.id)
    }

    @Test("Region ids are a function of the seed")
    func deterministicIDs() async throws {
        let first = try await store().addRegion(square, labels: [], to: asset)
        let second = try await store().addRegion(square, labels: [], to: asset)
        #expect(first.id == second.id)
        #expect(first.id == MockIdentifiers.regionID(seed: 7, ordinal: 0))
    }
}
