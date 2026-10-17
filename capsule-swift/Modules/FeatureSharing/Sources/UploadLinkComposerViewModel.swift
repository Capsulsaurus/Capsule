import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation
import CapsulePorts
import Foundation
import Observation

// MARK: - UploadLinkComposerViewModel

/// Provisions a guest web-upload link (*Web Upload*).
///
/// Two honesty constraints shape the model. The link is **write-only**: it
/// grants no view access, so nothing here offers the guest a way to see what is
/// already in the album. And the optional passphrase is an **abuse gate, not a
/// confidentiality layer** — it limits who may spend the owner's quota and adds
/// no secrecy, so the UI must not describe it as encryption.
@MainActor
@Observable
public final class UploadLinkComposerViewModel {
    public var draft: LinkCapsDraft
    public var passphraseEnabled = false
    public var passphrase = ""
    public var destination: AlbumID?

    public private(set) var albums: [ContainerAlbum] = []
    public private(set) var phase: SharingPhase = .loading
    public private(set) var isSubmitting = false
    public private(set) var connection: ConnectionClass?
    public private(set) var issued: ShareLink?

    private let drops: any DropPort
    private let albumPort: any AlbumPort
    private let connectivity: SharingConnectivity
    private let homeServer: String
    private let now: @Sendable () -> Date

    public init(
        drops: any DropPort,
        albums albumPort: any AlbumPort,
        homeServer: String,
        connectivity: SharingConnectivity = SharingConnectivity(),
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.drops = drops
        self.albumPort = albumPort
        self.homeServer = homeServer
        self.connectivity = connectivity
        self.now = now
        draft = LinkCapsDraft(now: now())
    }

    // MARK: Derived state

    /// What is wrong with the caps right now, for inline reporting.
    public var issues: [LinkCapsIssue] {
        draft.issues(now: now())
    }

    public var canSubmit: Bool {
        guard !isSubmitting, issued == nil, destination != nil, issues.isEmpty else { return false }
        return !passphraseEnabled || !passphrase.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// The guest-facing URL. As with a share link, the fragment carries key
    /// material and is rendered only here, only for copying.
    public var uploadURL: URL? {
        guard let issued else { return nil }
        return DeepLink.uploadURL(
            host: homeServer,
            opaqueID: issued.opaqueID,
            key: LinkSecret(issued.secret)
        )
    }

    // MARK: Actions

    /// Load the destinations a drop can be adopted into.
    ///
    /// Container albums only. A view album is never a destination — it holds no
    /// keys and owns no assets — and the port's types make passing one a
    /// compile error rather than a runtime surprise.
    public func load() async {
        connection = await connectivity.probe()
        do {
            albums = try await albumPort.containerAlbums()
            destination = destination ?? albums.first(where: \.isDefault)?.id ?? albums.first?.id
            phase = albums.isEmpty ? .empty : .ready
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// Provision the link.
    public func createLink() async {
        guard canSubmit, let destination else { return }
        isSubmitting = true
        defer { isSubmitting = false }
        connection = await connectivity.probe()
        do {
            issued = try await drops.createUploadLink(
                destination: destination,
                caps: draft.caps,
                passphrase: passphraseEnabled ? passphrase : nil
            )
            phase = .ready
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// Drop the issued link and its key material.
    public func reset() {
        issued = nil
        passphrase = ""
        passphraseEnabled = false
        draft = LinkCapsDraft(now: now())
    }
}
