import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Observation

// MARK: - LabelEditorModel

/// Drives the label and region editor: labels on a stack or an album, and on
/// an asset, regions drawn over its preview and labelled.
///
/// Every write goes through ``LabelPort`` and the editor redraws from what the
/// port answers, never from what it asked for, so a label the core normalizes
/// shows as stored. A refused write leaves the editor's state as it was and
/// names the refusal in ``failure``; nothing is retried behind the user's back.
///
/// An asset's own labels are its user tags, edited where the other
/// organization edits are, so on an asset this editor labels regions only.
@MainActor
@Observable
public final class LabelEditorModel {
    /// What the editor is open on.
    public enum Subject: Sendable, Equatable, Hashable {
        /// An asset: its regions and their labels.
        case asset(AssetID)
        /// A stack's labels.
        case stack(StackID)
        /// An album's labels.
        case album(AlbumID)
    }

    /// Why the last request was refused.
    public enum Failure: Sendable, Equatable, Hashable {
        /// The subject or region no longer exists.
        case notFound
        /// The label is empty once trimmed.
        case labelEmpty
        /// The label is past the byte bound.
        case labelTooLong(bytes: Int)
        /// The drawn shape fell outside the image contract.
        case shapeRefused(RegionShape.Refusal)
        /// The port failed for a reason the editor cannot name.
        case unavailable
    }

    public let subject: Subject
    /// The labels of the selected region, or of the stack or album.
    public private(set) var labels: [String] = []
    /// The asset's live regions, oldest first; empty for a stack or album.
    public private(set) var regions: [AssetRegion] = []
    /// The region whose labels are shown and edited.
    public private(set) var selectedRegionID: RegionID?
    /// The region being drawn, if one is.
    public var draft: RegionDraft?
    /// A request is in flight; the editor takes no other until it answers.
    public private(set) var isWriting = false
    /// Why the last request was refused, cleared by the next one.
    public private(set) var failure: Failure?

    private let port: any LabelPort

    public init(subject: Subject, port: any LabelPort) {
        self.subject = subject
        self.port = port
    }

    /// Where a typed label goes: the selected region on an asset, or the
    /// stack or album itself. `nil` on an asset with no region selected.
    public var labelTarget: LabelTarget? {
        switch subject {
        case let .asset(assetID): selectedRegionID.map { .region(asset: assetID, region: $0) }
        case let .stack(stackID): .stack(stackID)
        case let .album(albumID): .album(albumID)
        }
    }

    // MARK: Loading

    /// Read the subject's regions or labels from the port.
    public func load() async {
        await perform {
            switch subject {
            case let .asset(assetID):
                regions = try await port.regions(of: assetID)
                select(selectedRegionID.flatMap { id in regions.first { $0.id == id } } ?? regions.first)
            case .stack, .album:
                if let target = labelTarget { labels = try await port.labels(on: target) }
            }
        }
    }

    /// Show and edit the labels of `regionID`.
    public func selectRegion(_ regionID: RegionID?) {
        select(regionID.flatMap { id in regions.first { $0.id == id } })
    }

    // MARK: Labels

    /// Add a label to the current target.
    public func addLabel(_ text: String) async {
        guard let target = labelTarget else { return }
        await perform {
            labels = try await port.addLabel(text, to: target)
            storeRegionLabels(target)
        }
    }

    /// Remove a label from the current target.
    public func removeLabel(_ text: String) async {
        guard let target = labelTarget else { return }
        await perform {
            labels = try await port.removeLabel(text, from: target)
            storeRegionLabels(target)
        }
    }

    // MARK: Regions

    /// Start drawing a new region of `kind`, replacing any draft.
    public func startDrawing(_ kind: RegionDraft.Kind) {
        guard case .asset = subject else { return }
        draft = RegionDraft(kind)
        failure = nil
    }

    /// Drop the draft without writing anything.
    public func cancelDrawing() {
        draft = nil
    }

    /// Write the draft as a region with its first labels, and select it.
    public func finishDrawing(labels initial: [String]) async {
        guard case let .asset(assetID) = subject, let pending = draft else { return }
        await perform {
            let region = try await port.addRegion(pending.shape, labels: initial, to: assetID)
            regions.append(region)
            draft = nil
            select(region)
        }
    }

    /// Delete the selected region. Final: its id is never live again.
    public func deleteSelectedRegion() async {
        guard case let .asset(assetID) = subject, let regionID = selectedRegionID else { return }
        await perform {
            try await port.deleteRegion(regionID, on: assetID)
            regions.removeAll { $0.id == regionID }
            select(regions.first)
        }
    }

    // MARK: Plumbing

    private func select(_ region: AssetRegion?) {
        selectedRegionID = region?.id
        if case .asset = subject { labels = region?.labels ?? [] }
    }

    private func storeRegionLabels(_ target: LabelTarget) {
        guard case let .region(_, regionID) = target,
              let index = regions.firstIndex(where: { $0.id == regionID })
        else { return }
        regions[index].labels = labels
    }

    private func perform(_ work: () async throws -> Void) async {
        guard !isWriting else { return }
        isWriting = true
        failure = nil
        defer { isWriting = false }
        do {
            try await work()
        } catch {
            failure = Self.failure(for: error)
        }
    }

    static func failure(for error: any Error) -> Failure {
        switch error {
        case LabelTargetRefusal.notFound: .notFound
        case UserLabel.Refusal.empty: .labelEmpty
        case let UserLabel.Refusal.tooLong(bytes): .labelTooLong(bytes: bytes)
        case let refusal as RegionShape.Refusal: .shapeRefused(refusal)
        default: .unavailable
        }
    }
}
