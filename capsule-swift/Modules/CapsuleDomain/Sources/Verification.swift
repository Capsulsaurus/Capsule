import Foundation

// MARK: - RejectReason

/// Why `verify_asset` terminally rejected a manifest
/// (*Cryptography — Keys: Write Authorization*).
///
/// Every one of these lands the asset in ``QuarantineSurface/verifyAssetReject``
/// — never applied, never silently dropped. The reason is structured rather
/// than a message so the UI can distinguish "your app is out of date" from
/// "someone tampered with this" without parsing text, and so a support report
/// carries a stable value.
public enum RejectReason: Sendable, Equatable, Hashable, CaseIterable {
    /// The album authority's admin chain does not verify — the state it speaks
    /// for is untrusted, so nothing under it can be accepted.
    case untrustedAuthority
    /// The manifest names a different album than this authority speaks for.
    case wrongAlbum
    /// `crypto_suite_id` is not in the current inventory — an unknown suite, or
    /// a downgrade attempt.
    case suiteDowngrade
    /// A structural rule was violated — a non-`create` with a null prior hash,
    /// a `retention_until` on something that is not a delete.
    case structural
    /// The recomputed ciphertext hash does not match the manifest's declared
    /// hash: the bytes are not the bytes that were signed.
    case ciphertextHashMismatch
    /// The signing device is not in the user's published directory.
    case unknownDevice
    /// The device's `added_at` postdates the manifest timestamp — a key older
    /// than itself.
    case deviceAddedAfter
    /// A timestamp field was not valid RFC 3339.
    case badTimestamp
    /// The device signature did not verify.
    case badDeviceSig
    /// `amk_version` exceeds the MLS-attested epoch ceiling — a fabricated
    /// future epoch.
    case wrongEpoch
    /// The write-tier signature did not verify for the claimed epoch: signed by
    /// a reader, a removed writer, or for the wrong epoch.
    case badWriteSig
    /// `prior_provenance_hash` does not match the local chain head — stale,
    /// forked, or a replay.
    case forgedChain
    /// A `create` for an asset that already exists locally.
    case replayed
}

// MARK: - PendingReason

/// Why a manifest is held rather than accepted or rejected.
///
/// Pending is **not** a soft rejection: the manifest may well be perfectly
/// valid and simply ahead of local MLS state. Treating it as a failure would
/// make every legitimate key-delivery lag look like tampering.
public enum PendingReason: Sendable, Equatable, Hashable, CaseIterable {
    /// The epoch is attested, but its AMK content key has not arrived locally
    /// yet. Retry as MLS state catches up.
    case amkNotYetLocal
}

// MARK: - VerifyOutcome

/// The outcome of the single `verify_asset` chokepoint.
///
/// Three outcomes, not two, and the third is the important one: without
/// ``pending(_:)`` a client racing MLS delivery would quarantine its own
/// legitimate assets.
public enum VerifyOutcome: Sendable, Equatable, Hashable {
    /// Acknowledge the asset.
    case accept
    /// Reject and quarantine, with a structured reason.
    case terminalReject(RejectReason)
    /// Hold and retry as state catches up.
    case pending(PendingReason)

    /// Whether verification accepted.
    public var isAccepted: Bool {
        self == .accept
    }

    /// Whether this outcome should produce a quarantine entry. Pending does
    /// not — it is a retry, not a human decision.
    public var isQuarantining: Bool {
        if case .terminalReject = self { return true }
        return false
    }

    /// Whether the caller should retry later rather than surface a failure.
    public var isRetryable: Bool {
        if case .pending = self { return true }
        return false
    }
}
