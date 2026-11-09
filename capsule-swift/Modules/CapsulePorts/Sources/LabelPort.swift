import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - LabelPort

/// Labels on image regions, stacks and albums, and the regions themselves
/// (*Asset Organization — Labels*).
///
/// Every operation is metadata-only: it rewrites a sidecar register and never
/// touches an original's bytes. Labels are added and removed **by text**: the
/// adapter applies ``UserLabel/normalize(_:)`` before anything is written, and
/// a refused label writes nothing. An asset's own labels are its user tags,
/// edited through ``OrganizePort``.
///
/// Every write answers with what its target holds afterwards, so an editor
/// redraws from the store's truth rather than from what it asked for.
public protocol LabelPort: Sendable {
    /// The labels the target carries, sorted.
    ///
    /// - Throws: ``LabelTargetRefusal/notFound`` when the target does not
    ///   exist.
    func labels(on target: LabelTarget) async throws -> [String]

    /// Add a label to the target, and return its labels afterwards.
    ///
    /// - Throws: ``UserLabel/Refusal`` for a label the rule refuses, or
    ///   ``LabelTargetRefusal/notFound``; nothing is written.
    func addLabel(_ label: String, to target: LabelTarget) async throws -> [String]

    /// Remove a label from the target, and return its labels afterwards.
    ///
    /// A label the target does not carry is not an error: the answer is the
    /// unchanged list, which is how an editor tells the user nothing changed.
    ///
    /// - Throws: ``UserLabel/Refusal`` for a label the rule refuses, or
    ///   ``LabelTargetRefusal/notFound``.
    func removeLabel(_ label: String, from target: LabelTarget) async throws -> [String]

    /// The asset's live regions, oldest first.
    ///
    /// - Throws: ``LabelTargetRefusal/notFound`` when the asset does not exist.
    func regions(of assetID: AssetID) async throws -> [AssetRegion]

    /// Mark a region on an asset, with its first labels.
    ///
    /// Every label is checked before the region is written, so a refused label
    /// leaves no unlabelled region behind.
    ///
    /// - Throws: ``RegionShape/Refusal`` for a shape outside the
    ///   normalized-coordinate contract, ``UserLabel/Refusal`` for a refused
    ///   label, or ``LabelTargetRefusal/notFound``; nothing is written.
    func addRegion(_ shape: RegionShape, labels: [String], to assetID: AssetID) async throws -> AssetRegion

    /// Delete a region. **Final**: a delete absorbs every concurrent edit of
    /// the region, and its id is never reused.
    ///
    /// - Throws: ``LabelTargetRefusal/notFound`` when the asset or the region
    ///   does not exist.
    func deleteRegion(_ regionID: RegionID, on assetID: AssetID) async throws
}
