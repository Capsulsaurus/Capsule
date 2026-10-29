import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Testing

@testable import CapsuleCatalogFFI

/// A workspace double: records every call it receives, answers what it was
/// given, and throws `failure` from every verb when one is set.
private final class RecordingWorkspace: FFILabelWorkspace, @unchecked Sendable {
    enum Call: Equatable {
        case labels(FfiLabelTarget)
        case addLabel(FfiLabelTarget, String)
        case removeLabel(FfiLabelTarget, String)
        case regions(String)
        case addRegion(String, FfiRegionShape, [String])
        case deleteRegion(String, String)
    }

    private let lock = NSLock()
    private var recorded: [Call] = []
    var failure: (any Error)?
    var labelAnswer: [String] = []
    var regionAnswer: [FfiRegion] = []

    var calls: [Call] {
        lock.withLock { recorded }
    }

    private func record(_ call: Call) throws {
        try lock.withLock {
            recorded.append(call)
            if let failure { throw failure }
        }
    }

    func labels(target: FfiLabelTarget) throws -> [String] {
        try record(.labels(target))
        return labelAnswer
    }

    func addLabel(target: FfiLabelTarget, label: String) throws -> [String] {
        try record(.addLabel(target, label))
        return labelAnswer
    }

    func removeLabel(target: FfiLabelTarget, label: String) throws -> [String] {
        try record(.removeLabel(target, label))
        return labelAnswer
    }

    func regions(assetId: String) throws -> [FfiRegion] {
        try record(.regions(assetId))
        return regionAnswer
    }

    func addRegion(assetId: String, shape: FfiRegionShape, labels: [String]) throws -> FfiRegion {
        try record(.addRegion(assetId, shape, labels))
        return FfiRegion(regionId: "region-new", shape: shape, labels: labels.sorted())
    }

    func deleteRegion(assetId: String, regionId: String) throws {
        try record(.deleteRegion(assetId, regionId))
    }
}

private func refusal(_ refusal: FfiLabelRefusal) -> FfiError {
    .Label(refusal: refusal, detail: "refused")
}

@Suite("FFILabelPort — the workspace's label verbs as the LabelPort")
struct FFILabelPortTests {
    private static let asset = "0192f0c4-0000-7000-8000-00000000000a"
    private static let region = "0192f0c4-0000-7000-8000-00000000000b"
    private static let stack = "0192f0c4-0000-7000-8000-00000000000c"
    private static let album = "0192f0c4-0000-7000-8000-00000000000d"

    @Test(
        "each label target names the workspace's target by its UUID",
        arguments: [
            (
                LabelTarget.region(asset: .managed(uuid: asset), region: RegionID(region)),
                FfiLabelTarget.region(assetId: asset, regionId: region)
            ),
            (LabelTarget.stack(StackID(stack)), FfiLabelTarget.stack(stackId: stack)),
            (LabelTarget.album(.managed(uuid: album)), FfiLabelTarget.album(albumId: album)),
        ]
    )
    func mapsTargets(target: LabelTarget, expected: FfiLabelTarget) async throws {
        let workspace = RecordingWorkspace()
        workspace.labelAnswer = ["beach", "dog"]
        let port = FFILabelPort(source: workspace)

        let read = try await port.labels(on: target)
        let added = try await port.addLabel(" dog ", to: target)
        let removed = try await port.removeLabel("cat", from: target)

        #expect(read == ["beach", "dog"])
        #expect(added == ["beach", "dog"])
        #expect(removed == ["beach", "dog"])
        // The label goes across as typed: the workspace normalizes it.
        #expect(workspace.calls == [
            .labels(expected), .addLabel(expected, " dog "), .removeLabel(expected, "cat"),
        ])
    }

    @Test(
        "a system-Photos asset or smart album is not found without a call across the boundary",
        arguments: [
            LabelTarget.region(asset: .photoKit(localIdentifier: "ph-1"), region: RegionID(region)),
            LabelTarget.album(.smart(localIdentifier: "smart-1")),
        ]
    )
    func systemTargetsAreNotFound(target: LabelTarget) async {
        let workspace = RecordingWorkspace()
        let port = FFILabelPort(source: workspace)

        await #expect(throws: LabelTargetRefusal.notFound) { try await port.labels(on: target) }
        await #expect(throws: LabelTargetRefusal.notFound) { try await port.addLabel("x", to: target) }
        await #expect(throws: LabelTargetRefusal.notFound) { try await port.removeLabel("x", from: target) }
        #expect(workspace.calls.isEmpty)
    }

    @Test("region verbs on a system-Photos asset are not found without a call")
    func systemAssetRegionsAreNotFound() async {
        let workspace = RecordingWorkspace()
        let port = FFILabelPort(source: workspace)
        let asset = AssetID.photoKit(localIdentifier: "ph-1")

        await #expect(throws: LabelTargetRefusal.notFound) { try await port.regions(of: asset) }
        await #expect(throws: LabelTargetRefusal.notFound) {
            try await port.addRegion(.rect(left: 0, top: 0, width: 1, height: 1), labels: [], to: asset)
        }
        await #expect(throws: LabelTargetRefusal.notFound) { try await port.deleteRegion(RegionID("r"), on: asset) }
        #expect(workspace.calls.isEmpty)
    }

    @Test("regions come back with their ids, shapes and labels, in the workspace's order")
    func readsRegions() async throws {
        let workspace = RecordingWorkspace()
        workspace.regionAnswer = [
            FfiRegion(regionId: "r1", shape: .circle(cx: 0.5, cy: 0.4, r: 0.25), labels: ["face"]),
            FfiRegion(regionId: "r2", shape: .rect(x: 0.1, y: 0.2, w: 0.3, h: 0.4), labels: []),
        ]
        let port = FFILabelPort(source: workspace)

        let regions = try await port.regions(of: .managed(uuid: Self.asset))

        #expect(regions == [
            AssetRegion(id: RegionID("r1"), shape: .circle(centerX: 0.5, centerY: 0.4, radius: 0.25), labels: ["face"]),
            AssetRegion(id: RegionID("r2"), shape: .rect(left: 0.1, top: 0.2, width: 0.3, height: 0.4), labels: []),
        ])
        #expect(workspace.calls == [.regions(Self.asset)])
    }

    @Test(
        "a region is added with its shape coordinate for coordinate and its first labels",
        arguments: [
            (
                RegionShape.circle(centerX: 0.5, centerY: 0.5, radius: 0.125),
                FfiRegionShape.circle(cx: 0.5, cy: 0.5, r: 0.125)
            ),
            (
                RegionShape.rect(left: 0.25, top: 0.5, width: 0.5, height: 0.25),
                FfiRegionShape.rect(x: 0.25, y: 0.5, w: 0.5, h: 0.25)
            ),
        ]
    )
    func addsRegion(shape: RegionShape, expected: FfiRegionShape) async throws {
        let workspace = RecordingWorkspace()
        let port = FFILabelPort(source: workspace)

        let region = try await port.addRegion(shape, labels: ["b", "a"], to: .managed(uuid: Self.asset))

        #expect(region == AssetRegion(id: RegionID("region-new"), shape: shape, labels: ["a", "b"]))
        #expect(workspace.calls == [.addRegion(Self.asset, expected, ["b", "a"])])
    }

    @Test("a region is deleted by its asset and region ids")
    func deletesRegion() async throws {
        let workspace = RecordingWorkspace()
        let port = FFILabelPort(source: workspace)

        try await port.deleteRegion(RegionID(Self.region), on: .managed(uuid: Self.asset))

        #expect(workspace.calls == [.deleteRegion(Self.asset, Self.region)])
    }

    @Test("every workspace refusal becomes the domain refusal the editor names")
    func translatesRefusals() async {
        let workspace = RecordingWorkspace()
        let port = FFILabelPort(source: workspace)
        let target = LabelTarget.stack(StackID(Self.stack))
        let asset = AssetID.managed(uuid: Self.asset)
        let shape = RegionShape.rect(left: 0, top: 0, width: 0.5, height: 0.5)

        workspace.failure = refusal(.notFound)
        await #expect(throws: LabelTargetRefusal.notFound) { try await port.labels(on: target) }
        await #expect(throws: LabelTargetRefusal.notFound) { try await port.regions(of: asset) }
        await #expect(throws: LabelTargetRefusal.notFound) { try await port.deleteRegion(RegionID("r"), on: asset) }

        workspace.failure = refusal(.labelEmpty)
        await #expect(throws: UserLabel.Refusal.empty) { try await port.addLabel("  ", to: target) }

        workspace.failure = refusal(.labelTooLong(bytes: 300))
        await #expect(throws: UserLabel.Refusal.tooLong(bytes: 300)) { try await port.removeLabel("x", from: target) }

        workspace.failure = refusal(.regionOutOfRange)
        await #expect(throws: RegionShape.Refusal.outOfRange) {
            try await port.addRegion(shape, labels: [], to: asset)
        }
        workspace.failure = refusal(.regionEmpty)
        await #expect(throws: RegionShape.Refusal.empty) { try await port.addRegion(shape, labels: [], to: asset) }
        workspace.failure = refusal(.regionPastEdge)
        await #expect(throws: RegionShape.Refusal.pastEdge) { try await port.addRegion(shape, labels: [], to: asset) }
    }

    @Test("a byte count past Int's range clamps rather than traps")
    func clampsHugeByteCount() {
        let error = domainRefusal(.labelTooLong(bytes: .max))
        #expect(error as? UserLabel.Refusal == .tooLong(bytes: .max))
    }

    @Test("a failure that is not a label refusal is rethrown as it came")
    func rethrowsOtherFailures() async {
        let workspace = RecordingWorkspace()
        workspace.failure = FfiError.Workspace(detail: "disk full")
        let port = FFILabelPort(source: workspace)

        await #expect(throws: FfiError.Workspace(detail: "disk full")) {
            try await port.addLabel("x", to: .stack(StackID(Self.stack)))
        }
    }
}
