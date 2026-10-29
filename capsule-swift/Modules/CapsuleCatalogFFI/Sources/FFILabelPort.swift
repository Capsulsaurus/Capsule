import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import os

/// The label and region editor's log. Every write is an INFO line naming its
/// target; reads and refusals are DEBUG.
private let labelLog = Logger(subsystem: CapsuleLog.subsystem, category: "labels")

// MARK: - Workspace seam

/// The label and region verbs of `FfiWorkspace`, and nothing else of it.
///
/// Narrowed to a protocol so the port is tested over an in-memory double
/// rather than a passphrase-sealed workspace on disk; the generated object
/// conforms as declared.
protocol FFILabelWorkspace: Sendable {
    func labels(target: FfiLabelTarget) throws -> [String]
    func addLabel(target: FfiLabelTarget, label: String) throws -> [String]
    func removeLabel(target: FfiLabelTarget, label: String) throws -> [String]
    func regions(assetId: String) throws -> [FfiRegion]
    func addRegion(assetId: String, shape: FfiRegionShape, labels: [String]) throws -> FfiRegion
    func deleteRegion(assetId: String, regionId: String) throws
}

extension FfiWorkspace: FFILabelWorkspace {}

// MARK: - FFILabelPort

/// The Rust-backed ``LabelPort``: the workspace's label and region verbs, with
/// their refusals translated onto the domain's.
///
/// The workspace normalizes every label and checks every shape before it
/// writes, under the same rule ``UserLabel/normalize(_:)`` and
/// ``RegionShape/validate()`` state, so this adapter sends what the user typed
/// and reports what the core refused. `FfiError.Label` becomes
/// ``LabelTargetRefusal``, ``UserLabel/Refusal`` or ``RegionShape/Refusal``,
/// which is what lets an editor name the same refusal on this lane and the
/// mock one; any other failure is rethrown as it came.
///
/// Only the managed store's assets and albums live in the workspace, so a
/// system-Photos asset or smart album is ``LabelTargetRefusal/notFound``
/// without a call across the boundary.
public struct FFILabelPort: LabelPort {
    private let workspace: any FFILabelWorkspace

    /// Edit labels and regions in `workspace`.
    public init(workspace: FfiWorkspace) {
        self.init(source: workspace)
    }

    /// Edit labels and regions through `source`. The seam tests drive.
    init(source: any FFILabelWorkspace) {
        workspace = source
    }

    public func labels(on target: LabelTarget) async throws -> [String] {
        let ffiTarget = try FfiLabelTarget(target)
        labelLog.debug("labels on \(String(describing: target), privacy: .public)")
        return try translatingRefusals { try workspace.labels(target: ffiTarget) }
    }

    public func addLabel(_ label: String, to target: LabelTarget) async throws -> [String] {
        let ffiTarget = try FfiLabelTarget(target)
        let labels = try translatingRefusals { try workspace.addLabel(target: ffiTarget, label: label) }
        labelLog.info("label added to \(String(describing: target), privacy: .public)")
        return labels
    }

    public func removeLabel(_ label: String, from target: LabelTarget) async throws -> [String] {
        let ffiTarget = try FfiLabelTarget(target)
        let labels = try translatingRefusals { try workspace.removeLabel(target: ffiTarget, label: label) }
        labelLog.info("label removed from \(String(describing: target), privacy: .public)")
        return labels
    }

    public func regions(of assetID: AssetID) async throws -> [AssetRegion] {
        let asset = try workspaceAssetID(assetID)
        labelLog.debug("regions of \(asset, privacy: .public)")
        return try translatingRefusals { try workspace.regions(assetId: asset) }.map(AssetRegion.init)
    }

    public func addRegion(_ shape: RegionShape, labels: [String], to assetID: AssetID) async throws -> AssetRegion {
        let asset = try workspaceAssetID(assetID)
        let region = try translatingRefusals {
            try workspace.addRegion(assetId: asset, shape: FfiRegionShape(shape), labels: labels)
        }
        labelLog.info(
            "region \(region.regionId, privacy: .public) added to \(asset, privacy: .public), labels=\(labels.count)"
        )
        return AssetRegion(region)
    }

    public func deleteRegion(_ regionID: RegionID, on assetID: AssetID) async throws {
        let asset = try workspaceAssetID(assetID)
        try translatingRefusals { try workspace.deleteRegion(assetId: asset, regionId: regionID.rawValue) }
        labelLog.info("region \(regionID.rawValue, privacy: .public) deleted from \(asset, privacy: .public)")
    }
}

// MARK: - Refusals

/// Run one workspace verb, rethrowing an `FfiError.Label` as the domain
/// refusal it names.
private func translatingRefusals<T>(_ call: () throws -> T) throws -> T {
    do {
        return try call()
    } catch let FfiError.Label(refusal, detail) {
        labelLog.debug("label edit refused: \(detail, privacy: .public)")
        throw domainRefusal(refusal)
    }
}

/// The domain error an `FfiLabelRefusal` names.
func domainRefusal(_ refusal: FfiLabelRefusal) -> any Error {
    switch refusal {
    case .notFound: LabelTargetRefusal.notFound
    case .labelEmpty: UserLabel.Refusal.empty
    case let .labelTooLong(bytes): UserLabel.Refusal.tooLong(bytes: Int(clamping: bytes))
    case .regionOutOfRange: RegionShape.Refusal.outOfRange
    case .regionEmpty: RegionShape.Refusal.empty
    case .regionPastEdge: RegionShape.Refusal.pastEdge
    }
}

// MARK: - Mapping

/// The workspace's id for an asset: its catalog UUID. A system-Photos asset is
/// not in the workspace at all.
private func workspaceAssetID(_ assetID: AssetID) throws(LabelTargetRefusal) -> String {
    guard case let .managed(uuid) = assetID else { throw .notFound }
    return uuid
}

extension FfiLabelTarget {
    /// The workspace's name for a label target. A system smart album or
    /// system-Photos asset is not in the workspace, so it is not found.
    init(_ target: LabelTarget) throws(LabelTargetRefusal) {
        switch target {
        case let .region(asset, region):
            self = try .region(assetId: workspaceAssetID(asset), regionId: region.rawValue)
        case let .stack(stack):
            self = .stack(stackId: stack.rawValue)
        case let .album(album):
            guard case let .managed(uuid) = album else { throw .notFound }
            self = .album(albumId: uuid)
        }
    }
}

extension FfiRegionShape {
    /// The workspace's outline for a domain shape, coordinate for coordinate.
    init(_ shape: RegionShape) {
        switch shape {
        case let .circle(centerX, centerY, radius):
            self = .circle(cx: centerX, cy: centerY, r: radius)
        case let .rect(left, top, width, height):
            self = .rect(x: left, y: top, w: width, h: height)
        }
    }
}

extension RegionShape {
    /// The domain shape for a workspace outline, coordinate for coordinate.
    init(_ shape: FfiRegionShape) {
        switch shape {
        case let .circle(centerX, centerY, radius):
            self = .circle(centerX: centerX, centerY: centerY, radius: radius)
        case let .rect(left, top, width, height):
            self = .rect(left: left, top: top, width: width, height: height)
        }
    }
}

extension AssetRegion {
    /// The domain region for a workspace region.
    init(_ region: FfiRegion) {
        self.init(id: RegionID(region.regionId), shape: RegionShape(region.shape), labels: region.labels)
    }
}
