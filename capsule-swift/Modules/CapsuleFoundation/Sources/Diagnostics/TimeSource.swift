import Foundation

/// A source of wall-clock time, injected so time-stamped diagnostics are
/// deterministic under test.
public protocol TimeSource: Sendable {
    /// The current wall-clock instant.
    var now: Date { get }
}

/// The production clock, reading the system wall clock.
public struct SystemTimeSource: TimeSource {
    public init() {}
    public var now: Date { Date() }
}
