import CapsuleFoundation
import Foundation
import LocalAuthentication

/// The Apple implementation of the catalog's ``LocalAuthGate`` seam: an
/// `LAContext` fresh-local-auth challenge for a gated view.
///
/// Scope, stated as the design states it (Local Gallery — SR1): opening Recently Deleted
/// or Hidden requires fresh local authentication, which is **view-time UX protection
/// against a borrowed-unlocked-phone snoop — not a cryptographic boundary.** Nothing here
/// encrypts anything; the same bytes remain readable to anyone who defeats the platform
/// sandbox (SR2 covers what actually protects bytes at rest). What it buys is that handing
/// someone an unlocked phone does not hand them the trash.
///
/// `.deviceOwnerAuthentication` is the biometric-with-credential-fallback policy, so Face
/// ID / Touch ID is tried first and the device passcode backs it up — the fallback the
/// Rust seam documents as the adapter's concern. Only the outcome crosses the boundary;
/// the core never sees the biometric or the credential.
///
/// - Important: ``authenticate(view:)`` is synchronous because the UniFFI foreign-trait
///   seam is, so it **blocks its calling thread** for as long as the prompt is on screen.
///   Call it only through an ``AssetCatalog``, whose actor isolation keeps it off the main
///   thread; blocking the main thread here would deadlock the prompt it is waiting for.
public final class LocalAuthenticationGate: LocalAuthGate, @unchecked Sendable {
    /// Builds the `LAContext` for one challenge. Injectable so a test can supply a
    /// pre-configured context; production makes a fresh one per challenge, which is what
    /// keeps the authentication *fresh* (an `LAContext` caches its own successful
    /// evaluation for reuse, and reusing one would silently defeat the grace window).
    private let makeContext: @Sendable () -> LAContext

    public init(makeContext: @escaping @Sendable () -> LAContext = { LAContext() }) {
        self.makeContext = makeContext
    }

    public func authenticate(view: GatedView) throws {
        let context = makeContext()
        var probe: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &probe) else {
            // No biometric enrolled *and* no passcode set: there is no device-owner
            // credential to check, so the challenge is vacuously satisfied and the view
            // opens. Refusing instead would make Recently Deleted permanently unreachable
            // on such a device while protecting nothing — possession already is the only
            // credential. This matches what `HiddenView` has always done, and what the
            // system Photos app does with an unprotected device.
            CapsuleLog.catalog.info("local auth unavailable; opening gated view unchallenged")
            return
        }

        let evaluation = Mailbox()
        let waiter = DispatchSemaphore(value: 0)
        context.evaluatePolicy(
            .deviceOwnerAuthentication,
            localizedReason: Self.reason(for: view)
        ) { succeeded, error in
            evaluation.deliver(
                Evaluation(succeeded: succeeded, errorCode: (error as? LAError)?.code)
            )
            waiter.signal()
        }
        waiter.wait()

        guard let result = evaluation.collect() else {
            // The callback signalled, so it delivered; an empty mailbox would mean the
            // platform violated that contract. Fail closed rather than mint a grant.
            throw LocalAuthError.failed
        }
        if result.succeeded { return }
        throw Self.failure(for: result.errorCode)
    }

    /// The prompt line the system shows under "Face ID" / above the passcode field. It is
    /// user-facing, so it is a catalog key like every other string, resolved against the
    /// app bundle that carries `Localizable.xcstrings`.
    private static func reason(for view: GatedView) -> String {
        switch view {
        case .recentlyDeleted: String(localized: "app.recently_deleted.auth.reason")
        case .hidden: String(localized: "app.hidden.auth.reason")
        }
    }

    /// Map an `LAError` onto the seam's three outcomes. Anything unrecognised is a
    /// refusal: the grant is minted only on an explicit success.
    private static func failure(for code: LAError.Code?) -> LocalAuthError {
        switch code {
        case .userCancel, .appCancel, .systemCancel, .userFallback:
            .cancelled
        case .biometryNotAvailable, .biometryNotEnrolled, .passcodeNotSet, .invalidContext,
             .notInteractive:
            .unavailable
        default:
            .failed
        }
    }

    /// The `Sendable` snapshot carried out of the `LAContext` callback. `LAError` itself is
    /// not `Sendable`, so only its code crosses.
    private struct Evaluation: Sendable {
        let succeeded: Bool
        let errorCode: LAError.Code?
    }

    /// A one-shot handoff from the `LAContext` completion queue to the blocked caller.
    /// A lock rather than an actor because the caller is a synchronous FFI callback with
    /// no `await` to spend.
    private final class Mailbox: @unchecked Sendable {
        private let lock = NSLock()
        private var value: Evaluation?

        func deliver(_ evaluation: Evaluation) {
            lock.lock()
            defer { lock.unlock() }
            value = evaluation
        }

        func collect() -> Evaluation? {
            lock.lock()
            defer { lock.unlock() }
            return value
        }
    }
}
