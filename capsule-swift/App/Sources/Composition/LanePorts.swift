import CapsuleDomain
import CapsuleMock
import CapsulePorts
import ManagedStore

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
    /// The managed store over its layout: its catalog and the workspace its
    /// writes go through. `nil` keeps the store in memory, written through a
    /// `LocalManagedWorkspace` (``AppEnvironment/makeManagedLane(root:openLibrary:)``).
    var openManagedLibrary: (@Sendable (ManagedLibraryLayout) -> ManagedLibrary)?
    /// The render ladder every surface draws through: `render_step` in the
    /// FFI lane, the mock lane's stand-in for it otherwise.
    var renderLadder: RenderLadder = MockRenderLadder.ladder
    /// Where a RAW original's embedded preview is read. `nil` without a Rust
    /// core, which draws every rung from the original.
    var embeddedPreviews: (any EmbeddedPreviewPort)?

    /// The mock world's own adapters.
    static func mock(_ mock: MockEnvironment) -> LanePorts {
        LanePorts(labels: mock.labels, transferEvents: mock.transferEvents)
    }
}
