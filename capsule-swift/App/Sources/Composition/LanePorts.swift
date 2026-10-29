import CapsuleMock
import CapsulePorts

/// The ports a lane supplies in place of the mock world's.
///
/// ``AppEnvironment`` forwards every other port to its ``MockEnvironment``;
/// these are the ones a Rust-backed adapter already exists for, so they are
/// chosen here, once, rather than at each screen. The mock lane takes them from
/// the mock world (``mock(_:)``); the FFI lane composes them over an opened
/// workspace and session (`ffi(workspace:session:)`, built only with
/// `TUIST_FFI=1`).
struct LanePorts {
    /// Labels on regions, stacks and albums, and the regions themselves.
    let labels: any LabelPort
    /// The step-by-step transfer event stream the real-time transfer view
    /// draws.
    let transferEvents: any TransferEventPort

    /// The mock world's own adapters.
    static func mock(_ mock: MockEnvironment) -> LanePorts {
        LanePorts(labels: mock.labels, transferEvents: mock.transferEvents)
    }
}
