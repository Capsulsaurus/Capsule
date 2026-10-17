import CapsuleDomain
import CapsuleNavigation
import CapsulePorts
import Foundation
import Observation

// MARK: - ShareLinkComposerViewModel

/// Drives the create-a-share-link screen (*Share Links*).
///
/// What this model deliberately does **not** offer is as load-bearing as what
/// it does. There is no write toggle and no per-recipient analytics: v1 links
/// are view-only, and the link *is* the credential, so the server learns that a
/// link was used and never by whom. There is no privacy-strip opt-out either —
/// the strip is unconditional on the serve path, so a control here would be a
/// lie about what the server does.
///
/// The issued ``ShareLink`` carries live decryption material. It is held only
/// so the user can copy it once, is never logged, and is dropped by ``reset()``.
@MainActor
@Observable
public final class ShareLinkComposerViewModel {
    /// What the link will point at. Fixed at construction: the composer is
    /// reached *from* an asset or an album, never the other way round.
    public let scope: ShareScope

    public var expiryEnabled = false
    public var expiryDate: Date
    public var passphraseEnabled = false
    public var passphrase = ""

    public private(set) var phase: SharingPhase = .ready
    public private(set) var isSubmitting = false
    public private(set) var connection: ConnectionClass?

    /// The mandatory strip. A constant, because on this surface it is.
    public let privacyPolicy = PrivacyStripPolicy.mandatory

    /// The issued link, present only between a successful create and a reset.
    ///
    /// `private(set)` and never printed: `ShareLink` holds both the opaque id
    /// and the fragment secret, and anything that logs, screenshots, or
    /// analytics-reports it is exfiltrating access.
    public private(set) var issued: ShareLink?

    private let share: any SharePort
    private let connectivity: SharingConnectivity
    private let homeServer: String

    /// How long a revocation can take to be seen everywhere: the serving
    /// endpoint caches its fail-closed decision for 60 seconds by default. The
    /// UI promises "within about a minute" rather than "instantly".
    public static let revocationPropagationSeconds = 60

    public init(
        scope: ShareScope,
        share: any SharePort,
        homeServer: String,
        connectivity: SharingConnectivity = SharingConnectivity(),
        now: Date = Date()
    ) {
        self.scope = scope
        self.share = share
        self.homeServer = homeServer
        self.connectivity = connectivity
        expiryDate = now.addingTimeInterval(Self.defaultExpiryInterval)
    }

    /// Seven days — long enough to be useful, short enough that a forgotten
    /// link is not a permanent one.
    static let defaultExpiryInterval: TimeInterval = 7 * 86400

    // MARK: Derived state

    /// Whether the album-wide warning applies. An album scope hands over the
    /// AMK for every epoch the album's history policy covers, which is a
    /// categorically larger thing than one file key.
    public var isAlbumWide: Bool {
        scope.isAlbumWide
    }

    /// Whether ``createLink()`` would be accepted.
    public var canSubmit: Bool {
        guard !isSubmitting, issued == nil else { return false }
        return !passphraseEnabled || !passphrase.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// The link to hand to a recipient, or `nil` before one is issued.
    ///
    /// **The only place the fragment secret is rendered.** The fragment is the
    /// decryption key and no browser sends it to a server, so the client is the
    /// only place it can leak from — and it leaks by being printed.
    public var shareURL: URL? {
        guard let issued else { return nil }
        return DeepLink.shareURL(
            host: homeServer,
            opaqueID: issued.opaqueID,
            secret: LinkSecret(issued.secret)
        )
    }

    // MARK: Actions

    /// Refresh the connection class so a failure can be told apart from "no
    /// network".
    public func load() async {
        connection = await connectivity.probe()
    }

    /// Issue the link.
    public func createLink() async {
        guard canSubmit else { return }
        isSubmitting = true
        defer { isSubmitting = false }
        connection = await connectivity.probe()
        do {
            issued = try await share.createLink(
                scope: scope,
                expiresAt: expiryEnabled ? CapsuleTimestamp(epochSeconds: Int64(expiryDate.timeIntervalSince1970)) : nil,
                passphrase: passphraseEnabled ? passphrase : nil
            )
            phase = .ready
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// Drop the issued link and its secret, returning the form to its initial
    /// state. Called when the sheet is dismissed as well as by the user, so the
    /// secret does not outlive the screen that was allowed to show it.
    public func reset() {
        issued = nil
        passphrase = ""
        passphraseEnabled = false
        expiryEnabled = false
        phase = .ready
    }
}
