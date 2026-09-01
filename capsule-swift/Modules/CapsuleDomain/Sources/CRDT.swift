import Foundation

// MARK: - Stamped

/// A value with the signature of who wrote it and when — the unit every LWW
/// register is ordered by (*Metadata — Collaborative Metadata*).
///
/// The total order is `(timestamp, author)`: later timestamp wins, ties broken on
/// the larger device id. Both halves are required. Without the timestamp there
/// is no order; without the device-id tiebreak two devices writing in the same
/// millisecond would converge to different states, which is precisely the
/// divergence the CRDT exists to prevent.
public struct Stamped<Value: Sendable & Equatable>: Sendable, Equatable {
    /// The written value.
    public var value: Value
    /// RFC 3339 write time, preserved verbatim (it is inside the signed bytes).
    public var timestamp: CapsuleTimestamp
    /// The writing device — the lexicographic tiebreaker.
    public var author: DeviceID

    public init(value: Value, timestamp: CapsuleTimestamp, author: DeviceID) {
        self.value = value
        self.timestamp = timestamp
        self.author = author
    }

    /// Whether this write beats `other` under the documented total order.
    public func beats(_ other: Stamped<Value>) -> Bool {
        if timestamp != other.timestamp { return timestamp > other.timestamp }
        return author > other.author
    }
}

extension Stamped: Hashable where Value: Hashable {}

// MARK: - Lww

/// A last-writer-wins register that **also keeps what it displaced**
/// (*Metadata — Surfacing Concurrent Edits*).
///
/// A plain LWW register silently loses one side of a tied edit. That is a real
/// data-loss surface when two people caption the same photo seconds apart from
/// different devices, so Capsule keeps the winner authoritative *and* preserves
/// the losers, newest-superseded first, capped at ``supersededCap`` with the
/// oldest evicted.
///
/// This is load-bearing for the UI, not bookkeeping: the viewer surfaces "this
/// caption replaced another" and offers restoring the earlier value. A client
/// that strips the superseded log is in violation of *Threat Model — Forbidden
/// Client Behaviors*.
public struct Lww<Value: Sendable & Equatable>: Sendable, Equatable {
    /// The cap on the superseded log — 16, matching `superseded_captions` in
    /// the sidecar schema.
    public static var supersededCap: Int { 16 }

    /// The current authoritative write, or `nil` for a never-written register.
    ///
    /// A never-written register is **not** the same as a register stamped with
    /// an empty value: the former is wire-absent, the latter is a real edit.
    public var current: Stamped<Value>?

    /// Displaced writes, most recently superseded first.
    public var superseded: [Stamped<Value>]

    public init(current: Stamped<Value>? = nil, superseded: [Stamped<Value>] = []) {
        self.current = current
        self.superseded = Array(superseded.prefix(Self.supersededCap))
    }

    /// The current value, or `nil` if never written.
    public var value: Value? {
        current?.value
    }

    /// Whether anything has ever been written to this register. Distinguishes
    /// "never flagged" from "explicitly set back to the default", which the
    /// wire-absent defaults (`cull`, `hidden`, `stack_membership`) depend on.
    public var hasBeenWritten: Bool {
        current != nil
    }

    /// Apply a write, returning the resulting register.
    ///
    /// The higher `(timestamp, author)` becomes current; the loser lands in
    /// ``superseded`` — including a *late-arriving* write that loses, because
    /// the user should still be able to see and restore it. A write that merely
    /// repeats the current value is not recorded as superseded; it is not a
    /// displaced edit.
    public func applying(_ incoming: Stamped<Value>) -> Lww<Value> {
        guard let existing = current else {
            return Lww(current: incoming, superseded: superseded)
        }
        if incoming.beats(existing) {
            return Lww(current: incoming, superseded: [existing] + superseded)
        }
        guard incoming.value != existing.value else { return self }
        // A displaced write that is already logged is not a second displacement.
        // Without this, `merging` a register with itself would grow the log on
        // every call, and merge would stop being idempotent — the one property
        // the grouping-convergence requirement is not allowed to lose.
        guard !superseded.contains(incoming) else { return self }
        return Lww(current: existing, superseded: [incoming] + superseded)
    }

    /// Merge another replica of the same register.
    ///
    /// Commutative, associative, and idempotent by construction — it is a fold
    /// of ``applying(_:)`` over the other side's writes — which is what the
    /// grouping-convergence requirement demands of every collaborative field.
    public func merging(_ other: Lww<Value>) -> Lww<Value> {
        var result = self
        if let incoming = other.current {
            result = result.applying(incoming)
        }
        for displaced in other.superseded {
            result = result.applying(displaced)
        }
        return result
    }
}

extension Lww: Hashable where Value: Hashable {}

// MARK: - AddID

/// The identity of one OR-set insertion (*Metadata — Add-id Binding*).
///
/// `(device_id, monotonic_counter)`, the counter unique per
/// `(device, asset, OR-set)`. Reusing a counter would alias two distinct adds,
/// so removing one would silently delete the other — which is why the counter
/// is reseeded past the maximum this device has *ever* issued after a restart,
/// never merely reset.
public struct AddID: Sendable, Hashable, Comparable, Codable {
    /// The issuing device (UUIDv4).
    public var deviceID: DeviceID
    /// The per-device, per-set monotonic counter.
    public var counter: UInt64

    public init(deviceID: DeviceID, counter: UInt64) {
        self.deviceID = deviceID
        self.counter = counter
    }

    public static func < (lhs: AddID, rhs: AddID) -> Bool {
        (lhs.deviceID, lhs.counter) < (rhs.deviceID, rhs.counter)
    }
}

/// A remove named an `add_id` this replica never observed as an add.
///
/// **Rejected, never a silent no-op** — tolerating it is the "remove an element
/// you never added" attack from the threat model.
public struct UnobservedRemove: Error, Sendable, Equatable, Hashable {
    /// The add that was never observed.
    public var addID: AddID

    public init(addID: AddID) {
        self.addID = addID
    }
}

// MARK: - OrSet

/// An observed-remove set — the CRDT behind `tags_user` and `tags_ai`
/// (*Metadata — Collaborative Metadata*).
///
/// Adds are keyed by ``AddID`` and removes target a specific add, so a tag
/// added on one device and removed on another converges regardless of arrival
/// order, and re-adding a removed tag is a genuinely new element rather than a
/// resurrection of the tombstoned one.
public struct OrSet<Element: Sendable & Hashable>: Sendable, Equatable {
    /// Observed adds, keyed by their add id.
    public private(set) var adds: [AddID: Element]
    /// Tombstoned add ids.
    public private(set) var removes: Set<AddID>

    public init(adds: [AddID: Element] = [:], removes: Set<AddID> = []) {
        self.adds = adds
        self.removes = removes
    }

    /// The live elements — every add whose id has not been tombstoned.
    public var value: Set<Element> {
        Set(adds.filter { !removes.contains($0.key) }.values)
    }

    /// The live entries with their add ids, so a remove can name the right one.
    /// A UI dismissing an AI tag must pass the *original* add id.
    public var entries: [(addID: AddID, element: Element)] {
        adds
            .filter { !removes.contains($0.key) }
            .map { (addID: $0.key, element: $0.value) }
            .sorted { $0.addID < $1.addID }
    }

    /// Insert an element under a fresh add id.
    public func adding(_ element: Element, addID: AddID) -> OrSet<Element> {
        var result = self
        result.adds[addID] = element
        return result
    }

    /// Tombstone a specific add.
    ///
    /// - Throws: ``UnobservedRemove`` when the add was never observed here.
    public func removing(_ addID: AddID) throws -> OrSet<Element> {
        guard adds[addID] != nil else { throw UnobservedRemove(addID: addID) }
        var result = self
        result.removes.insert(addID)
        return result
    }

    /// Merge another replica: the union of adds and the union of removes.
    /// Commutative, associative, idempotent.
    public func merging(_ other: OrSet<Element>) -> OrSet<Element> {
        var result = self
        result.adds.merge(other.adds) { existing, _ in existing }
        result.removes.formUnion(other.removes)
        return result
    }

    /// Whether this replica has observed the given add — the precondition a
    /// remove is validated against before it is issued.
    public func hasObserved(_ addID: AddID) -> Bool {
        adds[addID] != nil
    }
}
