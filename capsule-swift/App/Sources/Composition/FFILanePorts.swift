#if CAPSULE_FFI
    import CapsuleCatalog
    import CapsuleCatalogFFI
    import CapsuleDomain
    import CapsuleFoundation
    import CapsuleMock
    import CapsulePorts
    import ManagedStore

    // The FFI lane's half of the composition root: the Rust-backed adapters
    // chosen over an opened workspace and, once signed in, its session. Fenced by
    // `CAPSULE_FFI`, which `Project.swift` sets only when `TUIST_FFI=1` adds the
    // `CapsuleCatalogFFI` dependency; the mock lane compiles none of it.

    extension LanePorts {
        /// The Rust-backed adapters over `workspace` and `session`.
        ///
        /// Labels and regions are local edits of the workspace, so they work
        /// signed out. So does the render ladder and the embedded-preview read:
        /// both are pure exports over files this device already holds. So does
        /// the cull pass: flags and the sweep are the workspace's own signed
        /// verbs. Transfer events come from a session; with none there is
        /// nothing in flight, and the stream finishes at once rather than
        /// showing the mock world's transfers.
        ///
        /// The managed store's writes are the workspace's verbs, and its
        /// catalog is the workspace's own index under the same root, so
        /// `workspace` must have been opened at the managed library's root.
        ///
        /// A purge reaches the server only with a session and the API root it
        /// signed in against (`apiBaseURL`, `http[s]://host:port`): the
        /// session delivers it to the album's ops surface there. Without
        /// either, a purge stays local and its records wait on the
        /// workspace's kept chain until a signed-in launch delivers them.
        static func ffi(workspace: FfiWorkspace, session: FfiSession?, apiBaseURL: String? = nil) -> LanePorts {
            let transferEvents: any TransferEventPort = if let session {
                FFITransferEventFeed(session: session)
            } else {
                IdleTransferEventFeed()
            }
            let purgeDelivery: (any PurgeDelivering)? = if let session, let apiBaseURL {
                FFIPurgeDelivery(session: session, workspace: workspace, apiBaseURL: apiBaseURL)
            } else {
                nil
            }
            CapsuleLog.app.info(
                "ffi lane ports composed, session=\(session != nil, privacy: .public) purgeDelivery=\(purgeDelivery != nil, privacy: .public)"
            )
            let managedWorkspace = FFIManagedWorkspace(workspace: workspace)
            return LanePorts(
                labels: FFILabelPort(workspace: workspace),
                transferEvents: transferEvents,
                openManagedLibrary: { layout in
                    ManagedLibrary(
                        layout: layout,
                        catalogOpener: FFICatalogOpener(),
                        workspace: managedWorkspace,
                        purgeDelivery: purgeDelivery
                    )
                },
                renderLadder: .ffi,
                embeddedPreviews: FFIEmbeddedPreviewReader(),
                cull: FFICullPort(workspace: workspace)
            )
        }
    }

    extension AppEnvironment {
        /// The FFI lane's world: the mock world for every port no Rust-backed
        /// adapter exists for yet, and the workspace's and session's adapters
        /// for the rest. `apiBaseURL` is the root `session` signed in against.
        init(mock: MockEnvironment, workspace: FfiWorkspace, session: FfiSession?, apiBaseURL: String? = nil) {
            self.init(mock: mock, lane: .ffi(workspace: workspace, session: session, apiBaseURL: apiBaseURL))
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
