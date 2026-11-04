#if CAPSULE_FFI
    import AssetKit
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
        /// A purge, delete or restore reaches the server only with a session
        /// and the API root it signed in against (`apiBaseURL`,
        /// `http[s]://host:port`): the session delivers it to the album's ops
        /// surface there. Without either, it stays local and its records wait
        /// on the workspace's chains until a signed-in launch delivers them.
        /// The same session and root sync the managed store
        /// (``managedSync(workspace:session:apiBaseURL:)``); without them
        /// nothing syncs.
        static func ffi(workspace: FfiWorkspace, session: FfiSession?, apiBaseURL: String? = nil) -> LanePorts {
            let transferEvents: any TransferEventPort = if let session {
                FFITransferEventFeed(session: session)
            } else {
                IdleTransferEventFeed()
            }
            // One delivery serves purges and live assets' held records alike,
            // so both read the server's heads off one feed drain.
            let delivery: FFIPurgeDelivery? = if let session, let apiBaseURL {
                FFIPurgeDelivery(session: session, workspace: workspace, apiBaseURL: apiBaseURL)
            } else {
                nil
            }
            let purgeDelivery: (any PurgeDelivering)? = delivery
            let lifecycleDelivery: (any LifecycleDelivering)? = delivery
            CapsuleLog.app.info(
                "ffi lane ports composed, session=\(session != nil, privacy: .public) delivery=\(delivery != nil, privacy: .public)"
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
                        purgeDelivery: purgeDelivery,
                        lifecycleDelivery: lifecycleDelivery
                    )
                },
                renderLadder: .ffi,
                embeddedPreviews: FFIEmbeddedPreviewReader(),
                openManagedSync: managedSync(workspace: workspace, session: session, apiBaseURL: apiBaseURL),
                cull: FFICullPort(workspace: workspace)
            )
        }

        /// The managed store's sync over `session`, against the server at
        /// `apiBaseURL`: an ``FFISyncDriver`` keeping its cursor and staging
        /// its fetches under the store's `sync/` directory, and writing each
        /// applied entry into the store's catalog and its verified original
        /// into the store's media partitions. Each drain's newly indexed
        /// assets go to the provider's redelivery walk, and a drain that
        /// applied an entry re-publishes the timeline, which then lists the
        /// rows it wrote. A drain that fails is logged; the next one resumes
        /// from the last page it kept.
        static func managedSync(
            workspace: FfiWorkspace,
            session: FfiSession?,
            apiBaseURL: String?
        ) -> OpenManagedLaneSync? {
            guard let session, let apiBaseURL else { return nil }
            return { layout, provider in
                let driver = FFISyncDriver(
                    session: session,
                    workspace: workspace,
                    apiBaseURL: apiBaseURL,
                    directory: layout.root.appending(path: "sync", directoryHint: .isDirectory),
                    catalog: FFISyncCatalogWriter(
                        catalogFile: layout.catalogFile,
                        mediaDirectory: { layout.mediaDirectory(forCaptureDate: $0) }
                    )
                ) { drain in
                    await provider.syncDrained(newlyIndexed: drain.newlyIndexed)
                    if drain.applied > 0 {
                        await provider.refresh()
                    }
                }
                return {
                    do {
                        try await driver.sync()
                    } catch {
                        CapsuleLog.app.error(
                            "managed lane sync failed: \(String(describing: error), privacy: .public)"
                        )
                    }
                }
            }
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
