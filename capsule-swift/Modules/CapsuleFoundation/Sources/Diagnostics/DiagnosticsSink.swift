import Foundation

/// A destination for ``DiagnosticEvent``s.
///
/// `record` must be cheap and non-blocking: it is called synchronously from the
/// emitting isolation domain (often the main actor on a hot path), so any IO,
/// networking, or contended work belongs on the implementation's own
/// queue/actor, hopped to from inside `record`.
public protocol DiagnosticsSink: Sendable {
    func record(_ event: DiagnosticEvent, at time: Date)
}
