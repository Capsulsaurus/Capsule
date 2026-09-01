import Foundation

// MARK: - ProvenanceAction

/// The closed lifecycle-action set — every authorized write an asset can
/// receive (*Authorization — The Closed Action Set*).
///
/// Named `ProvenanceAction` rather than `Action` because `Action` is far too
/// generic a name to occupy in a module every feature imports.
///
/// A value outside the set is a **structural error**, never a "future value to
/// ignore": adding one bumps `protocol_version`, and an album pinned to an
/// older version never sees it. The unknown case therefore exists to *render*
/// a newer record, never to author one.
public enum ProvenanceAction: ClosedWireEnum {
    /// First write of an asset; `prior_provenance_hash` is null.
    case create
    /// Replace the original bytes — e.g. re-encryption under a new AMK epoch.
    case replace
    /// Soft-delete; the asset enters trash with a signed retention window.
    case delete
    /// An edit to the encrypted metadata blob or sidecar fields.
    case metadataUpdate
    /// Add a thumbnail, preview, or embedding.
    case derivativeAdd
    /// Replace an existing derivative — the only authorized path; a silent
    /// overwrite is rejected.
    case derivativeReplace
    /// Recover a soft-deleted asset within its retention window.
    case trashRestore
    case unknown(String)

    public static let knownCases: [ProvenanceAction] = [
        .create, .replace, .delete, .metadataUpdate,
        .derivativeAdd, .derivativeReplace, .trashRestore,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    /// The wire strings are **kebab-case**, matching the Rust
    /// `#[serde(rename_all = "kebab-case")]` on `capsule_core::crypto::provenance::Action`.
    /// The casing is per-type, dictated by the Rust mirror, never guessed.
    public var rawValue: String {
        switch self {
        case .create: "create"
        case .replace: "replace"
        case .delete: "delete"
        case .metadataUpdate: "metadata-update"
        case .derivativeAdd: "derivative-add"
        case .derivativeReplace: "derivative-replace"
        case .trashRestore: "trash-restore"
        case let .unknown(raw): raw
        }
    }

    /// Whether this action roots a chain. `prior_provenance_hash` must be null
    /// **iff** this is true.
    public var isChainRoot: Bool {
        self == .create
    }

    /// Whether this action mints a new encrypted metadata blob, and so its
    /// manifest commits to `metadata_blob_hash`.
    ///
    /// Exactly `create | replace | metadata-update`; the other four omit the
    /// key entirely (absent, never null).
    public var bindsMetadataBlob: Bool {
        switch self {
        case .create, .replace, .metadataUpdate: true
        default: false
        }
    }

    /// Whether this action is admitted even in the grace-expired quota state.
    ///
    /// A user must be able to delete their way back under quota, so the
    /// provenance and metadata writes a `delete`, a `trash-restore`, or a
    /// trash-empty produces are **always** admitted (*Quota — Threshold Model*).
    public var isAdmittedWhenGraceExpired: Bool {
        self == .delete || self == .trashRestore
    }
}

// MARK: - DerivativeRole

/// What a derivative blob is (*Cryptography — Derivative Provenance*).
public enum DerivativeRole: ClosedWireEnum {
    /// A small grid thumbnail.
    case thumbnail
    /// A screen-resolution preview.
    case preview
    /// An ML embedding vector.
    case embedding
    case unknown(String)

    public static let knownCases: [DerivativeRole] = [.thumbnail, .preview, .embedding]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .thumbnail: "thumbnail"
        case .preview: "preview"
        case .embedding: "embedding"
        case let .unknown(raw): raw
        }
    }
}

// MARK: - KeyMode

/// How a reader obtains the asset's file key (*Cryptography — Provenance*).
///
/// ``derived`` is the default and is **wire-absent**: emitting it explicitly
/// would change the signed bytes and break verification of every manifest
/// signed before the field existed.
public enum KeyMode: ClosedWireEnum {
    /// The file key is recomputed from the AMK; nothing is stored.
    case derived
    /// The file key was chosen externally — an adopted web-upload drop — and is
    /// carried in the manifest, sealed under the AMK.
    case wrapped
    case unknown(String)

    public static let knownCases: [KeyMode] = [.derived, .wrapped]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .derived: "derived"
        case .wrapped: "wrapped"
        case let .unknown(raw): raw
        }
    }

    /// Whether this value is omitted on the wire.
    public var isWireAbsent: Bool {
        self == .derived
    }
}

// MARK: - BlobRole

/// What a blob is, from the server's keyless point of view
/// (*Upload Protocol — What Gets Uploaded*).
///
/// Declared at session creation so the server can reason about bundle
/// completeness without reading plaintext — the whole point being that it
/// never can.
public enum BlobRole: ClosedWireEnum {
    case original
    case derivative
    case metadata
    case provenance
    case backup
    case unknown(String)

    public static let knownCases: [BlobRole] = [.original, .derivative, .metadata, .provenance, .backup]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .original: "original"
        case .derivative: "derivative"
        case .metadata: "metadata"
        case .provenance: "provenance"
        case .backup: "backup"
        case let .unknown(raw): raw
        }
    }
}

// MARK: - ManifestCore

/// The signed core of an asset manifest — every field the two signatures cover
/// (*Cryptography — Provenance: Asset Manifest*).
///
/// A manifest carries **two** hybrid signatures over these same canonical
/// bytes: `device_sig` (provenance — which device produced it) and `write_sig`
/// (authorization — the album's per-epoch write-tier key). The core excludes
/// both, so the signing bytes are unambiguous and downgrade-resistant: both
/// signatures cover `crypto_suite_id`, `protocol_version`, and
/// `prior_provenance_hash`.
///
/// Optional fields here are **wire-absent, never null**, and their presence is
/// determined by ``action`` — see ``isStructurallyValid``.
public struct ManifestCore: Sendable, Equatable, Hashable {
    /// Schema version string (`asset-manifest/v1`).
    public var version: String
    /// The primitive bundle this manifest was produced under.
    public var cryptoSuiteID: UInt16
    /// Date-based wire protocol version (`YYYY-MM-DD`); matches the album pin.
    public var protocolVersion: String
    /// The asset's file id — the same UUIDv7 as the sidecar's `uuid`.
    public var fileID: String
    /// The container album the asset belongs to.
    public var albumID: String
    /// The AMK epoch (and write-tier key) this manifest is authorized under.
    public var amkVersion: UInt32
    /// Content-address digest over the **ciphertext**.
    public var ciphertextHash: BlobHash
    /// Total plaintext byte length.
    public var plaintextSize: UInt64
    /// Plaintext bytes per STREAM chunk.
    public var chunkSize: UInt32
    /// How the file key is obtained. Wire-absent when ``KeyMode/derived``.
    public var keyMode: KeyMode
    /// Content address of the asset's encrypted metadata blob. Present iff
    /// ``ProvenanceAction/bindsMetadataBlob``.
    public var metadataBlobHash: BlobHash?
    /// The user who produced the asset.
    public var createdByUser: String
    /// The device that produced it, resolved in the device directory.
    public var createdByDevice: DeviceID
    /// The producing client's version string.
    public var clientVersion: String
    /// Self-asserted write time. **Audit-only, never load-bearing** — a peer
    /// controls this value, so no ordering decision may rest on it.
    public var timestamp: CapsuleTimestamp
    /// The lifecycle action.
    public var action: ProvenanceAction
    /// Hash of the previous manifest in this asset's chain; null **iff**
    /// `action = create`.
    public var priorProvenanceHash: String?
    /// The server-visible retention deadline, set only for `action = delete`.
    ///
    /// It lives in the envelope precisely so the keyless purge worker can
    /// enforce it with no decryption key — the cryptographic floor that stops a
    /// hostile server accelerating a purge.
    public var retentionUntil: CapsuleTimestamp?

    public init(
        version: String,
        cryptoSuiteID: UInt16,
        protocolVersion: String,
        fileID: String,
        albumID: String,
        amkVersion: UInt32,
        ciphertextHash: BlobHash,
        plaintextSize: UInt64,
        chunkSize: UInt32,
        keyMode: KeyMode = .derived,
        metadataBlobHash: BlobHash? = nil,
        createdByUser: String,
        createdByDevice: DeviceID,
        clientVersion: String,
        timestamp: CapsuleTimestamp,
        action: ProvenanceAction,
        priorProvenanceHash: String? = nil,
        retentionUntil: CapsuleTimestamp? = nil
    ) {
        self.version = version
        self.cryptoSuiteID = cryptoSuiteID
        self.protocolVersion = protocolVersion
        self.fileID = fileID
        self.albumID = albumID
        self.amkVersion = amkVersion
        self.ciphertextHash = ciphertextHash
        self.plaintextSize = plaintextSize
        self.chunkSize = chunkSize
        self.keyMode = keyMode
        self.metadataBlobHash = metadataBlobHash
        self.createdByUser = createdByUser
        self.createdByDevice = createdByDevice
        self.clientVersion = clientVersion
        self.timestamp = timestamp
        self.action = action
        self.priorProvenanceHash = priorProvenanceHash
        self.retentionUntil = retentionUntil
    }

    /// Structural well-formedness, independent of any key.
    ///
    /// The presence-by-action rules, all four of them, in one place — a
    /// violation is ``RejectReason/structural`` at `verify_asset`, and the UI
    /// never renders such a manifest as if it were merely unverified.
    public var isStructurallyValid: Bool {
        // `prior_provenance_hash` is null **iff** the action is `create`.
        guard (priorProvenanceHash == nil) == action.isChainRoot else { return false }
        // `retention_until` is set **only** for `delete` (a delete may still
        // carry none — the rule is one-directional, matching `structural_ok`).
        guard retentionUntil == nil || action == .delete else { return false }
        // `metadata_blob_hash` is present **iff** the action binds a blob.
        guard (metadataBlobHash != nil) == action.bindsMetadataBlob else { return false }
        return true
    }
}

// MARK: - AssetManifest

/// A signed asset manifest: a ``ManifestCore`` plus its two hybrid signatures.
///
/// Signature bytes are opaque here. This layer never verifies — verification is
/// the single `verify_asset` chokepoint in `capsule-core`, and duplicating any
/// part of it in Swift would create a second, weaker gate.
public struct AssetManifest: Sendable, Equatable, Hashable {
    /// The signed core.
    public var core: ManifestCore
    /// Hybrid signature by the uploading device's key — *provenance*.
    public var deviceSignature: Data
    /// Hybrid signature under the epoch write-tier key — *authorization*.
    public var writeSignature: Data

    public init(core: ManifestCore, deviceSignature: Data, writeSignature: Data) {
        self.core = core
        self.deviceSignature = deviceSignature
        self.writeSignature = writeSignature
    }
}

// MARK: - ProvenanceRecord

/// One link in an asset's append-only, hash-chained provenance
/// (*Cryptography — Provenance of Library Modifications*).
///
/// **No path exists to overwrite or delete an existing record** — not via the
/// API, not via the local filesystem, not via federation. Even a hard purge
/// keeps the chain as a tombstone-with-history: the ciphertext goes, the audit
/// trail does not. The UI can therefore always answer "what happened to this
/// photo", including for a photo that no longer exists.
public struct ProvenanceRecord: Sendable, Equatable, Hashable, Identifiable {
    /// The asset this chain belongs to.
    public var assetID: String
    /// The signed manifest for this transition.
    public var manifest: AssetManifest
    /// Hash of the previous record; null only for `action = create`. Mirrors
    /// the manifest's own `prior_provenance_hash`, so signing the manifest
    /// signs this link.
    public var priorProvenanceHash: String?
    /// This record's own content hash — the next record's
    /// ``priorProvenanceHash``.
    public var recordHash: String

    public var id: String { recordHash }

    public init(
        assetID: String,
        manifest: AssetManifest,
        priorProvenanceHash: String?,
        recordHash: String
    ) {
        self.assetID = assetID
        self.manifest = manifest
        self.priorProvenanceHash = priorProvenanceHash
        self.recordHash = recordHash
    }

    /// Whether the manifest's `prior_provenance_hash` mirrors the record's, as
    /// required. A divergence is a forged or corrupted chain.
    public var mirrorsManifest: Bool {
        manifest.core.priorProvenanceHash == priorProvenanceHash
    }

    /// The action this record represents — the thing an activity view lists.
    public var action: ProvenanceAction {
        manifest.core.action
    }
}
