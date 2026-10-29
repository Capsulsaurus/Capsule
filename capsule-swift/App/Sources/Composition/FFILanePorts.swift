#if CAPSULE_FFI
    import CapsuleCatalogFFI
    import CapsuleDomain
    import CapsuleFoundation
    import CapsuleMock
    import CapsulePorts

    // The FFI lane's half of the composition root: the Rust-backed adapters
    // chosen over an opened workspace and, once signed in, its session. Fenced by
    // `CAPSULE_FFI`, which `Project.swift` sets only when `TUIST_FFI=1` adds the
    // `CapsuleCatalogFFI` dependency; the mock lane compiles none of it.

    extension LanePorts {
        /// The Rust-backed adapters over `workspace` and `session`.
        ///
        /// Labels and regions are local edits of the workspace, so they work
        /// signed out. Transfer events come from a session; with none there is
        /// nothing in flight, and the stream finishes at once rather than
        /// showing the mock world's transfers.
        static func ffi(workspace: FfiWorkspace, session: FfiSession?) -> LanePorts {
            let transferEvents: any TransferEventPort = if let session {
                FFITransferEventFeed(session: session)
            } else {
                IdleTransferEventFeed()
            }
            CapsuleLog.app.info("ffi lane ports composed, session=\(session != nil, privacy: .public)")
            return LanePorts(labels: FFILabelPort(workspace: workspace), transferEvents: transferEvents)
        }
    }

    extension AppEnvironment {
        /// The FFI lane's world: the mock world for every port no Rust-backed
        /// adapter exists for yet, and the workspace's and session's adapters
        /// for the rest.
        init(mock: MockEnvironment, workspace: FfiWorkspace, session: FfiSession?) {
            self.init(mock: mock, lane: .ffi(workspace: workspace, session: session))
        }
    }

    /// The transfer event stream of a launch with no signed-in session: nothing
    /// moves, so every subscription finishes at once.
    struct IdleTransferEventFeed: TransferEventPort {
        func events() -> AsyncStream<TransferFeedItem> {
            AsyncStream { $0.finish() }
        }
    }
#endif
