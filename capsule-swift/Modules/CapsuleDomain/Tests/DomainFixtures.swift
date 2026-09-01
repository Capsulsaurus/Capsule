import Foundation

import CapsuleDomain
import CapsuleFoundation

/// Shared fixtures for the domain suites.
///
/// Every value is deliberately boring and explicit — a test that has to squint
/// at a fixture to know what it is asserting is a test that will be misread the
/// next time it fails.
enum Fixtures {
    static let deviceA = DeviceID("00000000-0000-4000-8000-00000000000a")
    static let deviceB = DeviceID("00000000-0000-4000-8000-00000000000b")

    /// 2026-01-01T00:00:00Z.
    static let epoch = CapsuleTimestamp(epochSeconds: 1767225600)

    static func time(offsetSeconds: Int64) -> CapsuleTimestamp {
        CapsuleTimestamp(epochSeconds: epoch.epochSeconds + offsetSeconds)
    }

    static func stamped(
        _ value: String,
        offsetSeconds: Int64,
        author: DeviceID = deviceA
    ) -> Stamped<String> {
        Stamped(value: value, timestamp: time(offsetSeconds: offsetSeconds), author: author)
    }

    static func term(
        _ field: QueryField,
        _ operatorKind: PredicateOperator,
        _ operand: Operand
    ) -> Term {
        Term(field: field, operatorKind: operatorKind, operand: operand)
    }

    /// A minimal valid term, for building bounds fixtures that are not about
    /// the term's own contents.
    static let ratingTerm = term(.rating, .greaterThanOrEqual, .number(3))

    static func nested(depth: Int) -> SmartAlbumPredicate {
        depth <= 1 ? .term(ratingTerm) : .all([nested(depth: depth - 1)])
    }

    static func libraryAsset(
        id: String,
        captureSeconds: Int64,
        captureUTCSeconds: Int64? = nil,
        isDeleted: Bool = false,
        isStackHidden: Bool = false,
        isUserHidden: Bool = false
    ) -> LibraryAsset {
        LibraryAsset(
            id: .managed(uuid: id),
            mediaType: .photo,
            contentType: .jpeg,
            captureTime: CaptureTime(
                captureTimestamp: CapsuleTimestamp(epochSeconds: captureSeconds),
                captureUTC: captureUTCSeconds.map { CapsuleTimestamp(epochSeconds: $0) }
            ),
            importTimestamp: epoch,
            isDeleted: isDeleted,
            isStackHidden: isStackHidden,
            isUserHidden: isUserHidden
        )
    }
}
