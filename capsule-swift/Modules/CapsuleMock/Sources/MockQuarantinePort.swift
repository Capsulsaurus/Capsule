import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - QuarantinePort

/// The inventory of things that need a human to look at.
///
/// The invariant: a quarantined item is **never silently dropped and never
/// silently applied**. That is why there is no `resolveAll()` here and no
/// automatic resolution — automatic resolution is the same thing as silently
/// applying or silently dropping, which is what the surface exists to prevent.
extension MockSystemStore: QuarantinePort {
    public func items(offset: Int, limit: Int) async throws -> Page<QuarantineItem> {
        page(quarantineItems, offset: offset, limit: limit)
    }

    public func items(on surface: QuarantineSurface, offset: Int, limit: Int) async throws -> Page<QuarantineItem> {
        page(quarantineItems.filter { $0.surface == surface }, offset: offset, limit: limit)
    }

    public func itemCount() async throws -> Int {
        quarantineItems.count
    }

    /// Read the preserved bytes without changing anything.
    ///
    /// `nil` when the holding area records the event but not the bytes — an
    /// audit-log entry has nothing to inspect, and returning plausible bytes for
    /// it would be inventing evidence.
    public func inspect(_ identifier: QuarantineID) async throws -> Data? {
        guard let item = quarantineItems.first(where: { $0.id == identifier }),
              item.surface.storage.preservesOriginalBytes,
              let byteCount = item.preservedBytes
        else { return nil }
        // A sample rather than the whole blob: an inspector shows a header, and
        // materializing megabytes to display a few hundred bytes would be the
        // same mistake this module exists to avoid elsewhere.
        let sample = min(Int(byteCount), 256)
        var bytes: [UInt8] = []
        bytes.reserveCapacity(sample)
        var hash = MockHash.mix(configuration.seed &+ UInt64(identifier.rawValue.utf8.count))
        while bytes.count < sample {
            hash = MockHash.mix(hash)
            withUnsafeBytes(of: hash.bigEndian) { bytes.append(contentsOf: $0) }
        }
        return Data(bytes.prefix(sample))
    }

    /// Attempt recovery — re-fetch, re-derive, re-run the ceremony, adopt.
    ///
    /// Throws when the holding area does not preserve enough state for repair to
    /// mean anything, which is a different answer from "repair failed": there is
    /// nothing here to repair, and offering the button anyway would be a lie the
    /// user only discovers by pressing it.
    public func repair(_ identifier: QuarantineID) async throws {
        guard let item = quarantineItems.first(where: { $0.id == identifier }) else {
            throw CapsuleError(code: .albumInvalidID, detail: "CapsuleMock: no such quarantine item")
        }
        guard item.isRecoverable else {
            throw CapsuleError(
                code: .uploadInvalidAction,
                detail: "CapsuleMock: \(item.surface.rawValue) preserves no state repair could act on"
            )
        }
        setItems(quarantineItems.filter { $0.id != identifier })
        await quarantineChanges.send(())
    }

    /// Discard an item and its preserved bytes.
    ///
    /// **Irreversible.** Never the default, and never bundled with another
    /// action — which is why it is its own call rather than a flag on
    /// ``repair(_:)``.
    public func discard(_ identifier: QuarantineID) async throws {
        setItems(quarantineItems.filter { $0.id != identifier })
        await quarantineChanges.send(())
    }

    public nonisolated func changes() -> AsyncStream<Void> {
        quarantineChanges.subscribe()
    }

    private func page(_ items: [QuarantineItem], offset: Int, limit: Int) -> Page<QuarantineItem> {
        let request = PageRequest(offset: offset, limit: limit)
        return Page(
            items: MockQueryEngine.window(items, request: request),
            request: request,
            totalCount: items.count
        )
    }
}

// MARK: - MaintenancePort

extension MockSystemStore: MaintenancePort {
    public func tasks() async throws -> [MaintenanceTask] {
        taskList
    }

    /// Run a job now, bypassing its schedule but **not** its gating.
    ///
    /// A destructive job still runs behind verify-before-destroy: asking for it
    /// explicitly does not waive the gate that stops it deleting an only copy.
    /// So the stream reports what it found rather than what it removed.
    public nonisolated func run(_ kind: MaintenanceTaskKind) -> AsyncStream<MaintenanceTask> {
        AsyncStream { continuation in
            Task {
                await self.clearCancelled(kind)
                for step in 1 ... 5 {
                    if await self.isCancelled(kind) {
                        // Cancellation is a stop, not a rollback: partial
                        // progress stands, so the task returns to idle with its
                        // last-run stamp intact rather than reverting.
                        let stopped = await self.update(kind, state: .idle)
                        continuation.yield(stopped)
                        continuation.finish()
                        return
                    }
                    let running = await self.update(kind, state: .running(fractionComplete: Double(step) / 5))
                    continuation.yield(running)
                }
                let finished = await self.finish(kind)
                continuation.yield(finished)
                continuation.finish()
            }
        }
    }

    public func cancel(_ kind: MaintenanceTaskKind) async throws {
        markCancelled(kind)
    }

    public nonisolated func changes() -> AsyncStream<[MaintenanceTask]> {
        maintenanceChanges.subscribe()
    }

    private func update(_ kind: MaintenanceTaskKind, state: MaintenanceTask.State) async -> MaintenanceTask {
        let existing = taskList.first { $0.kind == kind }
        let task = MaintenanceTask(kind: kind, state: state, lastRunAt: existing?.lastRunAt)
        setTask(task)
        await maintenanceChanges.send(taskList)
        return task
    }

    /// Finish a run. Zero findings is the good answer, and a job that reports it
    /// has done its work — a UI that only shows a result when something is wrong
    /// gives the user no way to tell "checked and fine" from "never ran".
    private func finish(_ kind: MaintenanceTaskKind) async -> MaintenanceTask {
        let findings = kind.isDestructive ? 0 : Int(MockHash.value(
            seed: configuration.seed,
            index: kind.rawValue.utf8.count,
            salt: .schemaAhead
        ) % 5)
        let task = MaintenanceTask(
            kind: kind,
            state: .completed(occurredAt: configuration.clock.now, findingCount: findings),
            lastRunAt: configuration.clock.now
        )
        setTask(task)
        await maintenanceChanges.send(taskList)
        return task
    }
}
