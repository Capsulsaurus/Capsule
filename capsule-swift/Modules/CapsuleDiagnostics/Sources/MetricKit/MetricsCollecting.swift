import Foundation

/// The lifecycle seam for OS metric/diagnostic collection.
///
/// Abstracted so the coordinator's consent-gated start/stop logic is testable
/// without registering a real `MXMetricManager` subscriber.
public protocol MetricsCollecting: AnyObject {
    /// Begin receiving metric & diagnostic payloads.
    func start()
    /// Stop receiving payloads.
    func stop()
}
