import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - QuarantineDetailModel

/// Drives ``QuarantineDetailView``.
///
/// The invariant this screen serves: a quarantined item is **never silently
/// dropped and never silently applied**. Every path off this screen is an
/// explicit choice the user made, and the destructive one confirms.
@MainActor
@Observable
public final class QuarantineDetailModel {
    public private(set) var phase: ScreenPhase = .ready
    public private(set) var item: QuarantineItem
    /// A sample of the preserved bytes, when the holding area preserves any.
    public private(set) var inspectedBytes: Data?
    /// Set once the item has been resolved, so the screen can dismiss itself
    /// rather than showing actions for something that is gone.
    public private(set) var isResolved = false
    public private(set) var isBusy = false

    private let quarantine: any QuarantinePort
    private let sync: any SyncPort
    private var connection: ConnectionClass = .unmetered

    public init(item: QuarantineItem, quarantine: any QuarantinePort, sync: any SyncPort) {
        self.item = item
        self.quarantine = quarantine
        self.sync = sync
    }

    // MARK: Derived

    /// Exactly three options, in a fixed order, none of them a default.
    public var options: [QuarantineActionOption] { QuarantineActionOption.options(for: item) }

    /// Whether the original bytes are still recoverable.
    public var preservesOriginalBytes: Bool { item.surface.storage.preservesOriginalBytes }

    /// How many bytes are held, when any are.
    public var preservedBytes: UInt64? { item.preservedBytes }

    // MARK: Loading

    public func load() async {
        connection = await (try? sync.status().connectionClass) ?? connection
        phase = connection.isUsable ? .ready : .offline
    }

    // MARK: Actions

    /// Examine the preserved bytes. Changes nothing.
    ///
    /// A `nil` result is a fact, not a failure: an audit-log entry records that
    /// something happened without keeping the bytes, and returning plausible
    /// bytes for it would be inventing evidence.
    public func inspect() async {
        isBusy = true
        defer { isBusy = false }
        do {
            inspectedBytes = try await quarantine.inspect(item.id)
        } catch let error as CapsuleError {
            phase = .failed(error)
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    /// Attempt recovery — re-fetch, re-derive, re-run the ceremony, adopt.
    ///
    /// Refused where the holding area preserves no state repair could act on,
    /// which is a different answer from "repair failed".
    public func repair() async {
        guard item.isRecoverable else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            try await quarantine.repair(item.id)
            isResolved = true
        } catch let error as CapsuleError {
            phase = .failed(error)
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    /// Discard the item and its preserved bytes.
    ///
    /// **Irreversible**, never the default, and never bundled with another
    /// action — which is why it is its own call with its own confirmation
    /// rather than a flag on ``repair()``.
    public func discard() async {
        isBusy = true
        defer { isBusy = false }
        do {
            try await quarantine.discard(item.id)
            isResolved = true
        } catch let error as CapsuleError {
            phase = .failed(error)
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }
}
