import CapsuleDomain
import Foundation

// MARK: - LinkCapsIssue

/// Why a set of caps would not be accepted.
///
/// Validated on the client **as well as** the server. The server is the
/// authority — caps are enforced at the no-key layer on every drop session, so
/// a leaked link still cannot push storage past the owner's hard limit — but a
/// cap that contradicts itself is worth catching before a link exists, because
/// a link that has been handed out cannot be edited, only revoked.
public enum LinkCapsIssue: String, Sendable, Equatable, Hashable, Identifiable, CaseIterable {
    /// The expiry is already in the past, so the link would be dead on arrival.
    case expiryInPast
    /// A cap was enabled and left at zero.
    case zeroCap
    /// The per-file cap exceeds the cumulative cap, so it can never bind.
    case fileSizeExceedsTotal
    /// Single-use with a file count above one: the link dies after its first
    /// successful drop, so the count is unreachable.
    case singleUseWithMultipleFiles

    public var id: String { rawValue }
}

// MARK: - LinkCapsDraft

/// The editable form behind a ``LinkCaps`` (*Web Upload — Security Contract*).
///
/// Every cap is optional and off by default, mirroring the wire type: an absent
/// cap means "no cap", which is a different statement from "zero". Keeping the
/// draft separate from ``LinkCaps`` is what lets the form hold a half-typed
/// value — an enabled toggle with an empty number — without that ever becoming
/// a representable cap.
public struct LinkCapsDraft: Sendable, Equatable, Hashable {
    public var expiryEnabled = false
    public var expiryDate: Date

    public var totalBytesEnabled = false
    /// Cumulative cap across every drop on the link, in gibibytes.
    public var totalGibibytes: Double = 2

    public var fileCountEnabled = false
    public var fileCount: Int = 25

    public var fileSizeEnabled = false
    /// Largest single file, in mebibytes.
    public var fileMebibytes: Double = 512

    public var singleUse = false

    public init(now: Date = Date()) {
        expiryDate = now.addingTimeInterval(Self.defaultExpiryInterval)
    }

    /// Fourteen days: long enough for a wedding photographer's guests to get
    /// round to it, short enough that a forgotten link closes itself.
    public static let defaultExpiryInterval: TimeInterval = 14 * 86400

    static let bytesPerGibibyte: Double = 1073741824
    static let bytesPerMebibyte: Double = 1048576

    /// The caps this draft describes.
    public var caps: LinkCaps {
        LinkCaps(
            expiresAt: expiryEnabled
                ? CapsuleTimestamp(epochSeconds: Int64(expiryDate.timeIntervalSince1970))
                : nil,
            maxTotalBytes: totalBytesEnabled ? UInt64(max(0, totalGibibytes * Self.bytesPerGibibyte)) : nil,
            maxFileCount: fileCountEnabled ? UInt32(max(0, fileCount)) : nil,
            maxFileSize: fileSizeEnabled ? UInt64(max(0, fileMebibytes * Self.bytesPerMebibyte)) : nil,
            singleUse: singleUse
        )
    }

    /// Everything wrong with the draft, in a stable order so the form does not
    /// reshuffle its messages as the user types.
    public func issues(now: Date = Date()) -> [LinkCapsIssue] {
        var found: [LinkCapsIssue] = []
        if expiryEnabled, expiryDate <= now { found.append(.expiryInPast) }
        if hasZeroCap { found.append(.zeroCap) }
        if exceedsTotal { found.append(.fileSizeExceedsTotal) }
        if singleUse, fileCountEnabled, fileCount > 1 { found.append(.singleUseWithMultipleFiles) }
        return found
    }

    /// Whether a link may be provisioned from this draft.
    public func isValid(now: Date = Date()) -> Bool {
        issues(now: now).isEmpty
    }

    private var hasZeroCap: Bool {
        (totalBytesEnabled && totalGibibytes <= 0)
            || (fileCountEnabled && fileCount <= 0)
            || (fileSizeEnabled && fileMebibytes <= 0)
    }

    private var exceedsTotal: Bool {
        guard totalBytesEnabled, fileSizeEnabled else { return false }
        return fileMebibytes * Self.bytesPerMebibyte > totalGibibytes * Self.bytesPerGibibyte
    }
}
