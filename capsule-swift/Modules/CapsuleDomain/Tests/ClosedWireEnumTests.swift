import Foundation
import Testing

import CapsuleDomain

/// Parity rule 1: closed enums cross the FFI as strings, an unrecognised value
/// round-trips **verbatim**, and writing one is a structural rejection.
///
/// This is not defensive padding. The docs require that reading an unknown value
/// renders a "created with a newer version" indicator while *writing* one is a
/// structural rejection, so both halves are asserted here — a build that
/// silently coerced an unknown value to a default would pass a read-only test
/// and still corrupt a library.
@Suite("Closed wire enums preserve unknown values and refuse to write them")
struct ClosedWireEnumTests {
    @Test("an unrecognised value round-trips byte-for-byte")
    func unknownRoundTrips() {
        let raw = "image/futureformat"
        let decoded = ContentType(rawValue: raw)

        #expect(decoded == .unknown(raw))
        #expect(decoded.rawValue == raw)
        #expect(!decoded.isKnown)
    }

    @Test("a known value decodes to its case and re-encodes to the same string")
    func knownRoundTrips() {
        for known in ContentType.knownCases {
            #expect(ContentType(rawValue: known.rawValue) == known)
            #expect(known.isKnown)
        }
    }

    @Test("writing an unknown value is a structural rejection")
    func writingUnknownThrows() throws {
        let unknown = StackType(rawValue: "future-stack-type")
        #expect(!unknown.isWritable)
        #expect(throws: ClosedEnumWriteRejection.self) {
            try unknown.requireWritable()
        }
        // ...and a known value is writable, so the gate is not simply always-throw.
        try StackType.burst.requireWritable()
    }

    @Test("the rejection carries the offending value verbatim for logging")
    func rejectionCarriesRawValue() {
        let unknown = ProvenanceAction(rawValue: "future-action")
        do {
            try unknown.requireWritable()
            Issue.record("an unknown action must not be writable")
        } catch let rejection as ClosedEnumWriteRejection {
            #expect(rejection.rawValue == "future-action")
            #expect(rejection.typeName == "ProvenanceAction")
        } catch {
            Issue.record("unexpected error: \(error)")
        }
    }

    @Test("equality and hashing are both by wire string, so they agree")
    func equalityAndHashingAgree() {
        // The stdlib supplies `==` for every `RawRepresentable`, comparing raw
        // values — so `.unknown("pick")` *is* `.pick`. That is the right
        // semantics for a wire enum (same string, same value) and it is safe:
        // writing it emits exactly "pick".
        //
        // What must not drift is hashing. Structural hashing plus raw-value
        // equality breaks the `Hashable` contract and with it every `Set` and
        // dictionary keyed on a wire enum — including the grammar tables.
        let spoofed = CullFlag.unknown("pick")
        #expect(spoofed == CullFlag.pick)
        #expect(spoofed.hashValue == CullFlag.pick.hashValue)
        #expect(Set([spoofed, CullFlag.pick]).count == 1)

        // A genuinely unrecognised string is still refused at the write gate.
        let genuine = CullFlag(rawValue: "maybe")
        #expect(!genuine.isKnown)
        #expect(Set([genuine, CullFlag.pick]).count == 2)
    }

    @Test("a wire enum works as a dictionary key across known and unknown values")
    func usableAsDictionaryKey() {
        // The grammar tables are dictionaries keyed on `QueryField`; an
        // inconsistent hash would make lookups silently miss.
        var table: [QueryField: Int] = [:]
        for (index, field) in QueryField.knownCases.enumerated() {
            table[field] = index
        }
        #expect(table.count == QueryField.knownCases.count)
        #expect(table[QueryField(rawValue: "rating")] == table[.rating])
        #expect(table[QueryField(rawValue: "future_field")] == nil)
    }

    @Test("wire strings match the Rust serde casing per type, not one global casing")
    func wireCasingMirrorsRust() {
        // kebab-case, from `#[serde(rename_all = "kebab-case")]`.
        #expect(ProvenanceAction.metadataUpdate.rawValue == "metadata-update")
        #expect(ProvenanceAction.derivativeReplace.rawValue == "derivative-replace")
        #expect(ProvenanceAction.trashRestore.rawValue == "trash-restore")
        // snake_case, from `#[serde(rename_all = "snake_case")]`.
        #expect(StackType.rawJpeg.rawValue == "raw_jpeg")
        #expect(StackType.livePhoto.rawValue == "live_photo")
        #expect(StackType.hdrBracket.rawValue == "hdr_bracket")
        // MIME strings, exactly one canonical value per format.
        #expect(ContentType.jpeg.rawValue == "image/jpeg")
        #expect(ContentType.dng.rawValue == "image/x-adobe-dng")
    }

    @Test("a wire-absent field decodes to nil rather than a fabricated default")
    func wireAbsentDecodesToNil() {
        #expect(GpsDatum.decodingWireAbsent(nil) == nil)
        #expect(GpsDatum.decodingWireAbsent("gcj02") == .gcj02)
        // wgs84 is the wire-absent default, so a present key is still legal.
        #expect(GpsDatum.wgs84.isWireAbsent)
        #expect(!GpsDatum.gcj02.isWireAbsent)
    }

    @Test("Codable rides on the raw value, unknown cases included")
    func codableUsesRawValue() throws {
        let values: [ContentType] = [.heic, .unknown("image/futureformat")]
        let data = try JSONEncoder().encode(values)
        let decoded = try JSONDecoder().decode([ContentType].self, from: data)
        #expect(decoded == values)
        // The unknown value survives the round trip as its exact wire string —
        // asserted on the decoded value rather than the JSON text, because
        // `JSONEncoder` escapes the solidus.
        #expect(decoded.last?.rawValue == "image/futureformat")
        #expect(decoded.last?.isKnown == false)
    }

    @Test("the closed stack-type set has exactly the thirteen documented variants")
    func stackTypeSetIsClosed() {
        #expect(StackType.knownCases.count == 13)
        #expect(Set(StackType.knownCases.map(\.rawValue)).count == 13)
    }
}
