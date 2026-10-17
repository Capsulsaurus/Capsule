import Foundation

// MARK: - ClosedWireEnum

/// A closed enum that crosses the FFI boundary as a **string**, and that must
/// therefore be able to *represent* a value this build does not know.
///
/// The asymmetry is the whole point, and it comes straight from the threat
/// model's schema rules (see *Threat Model — Schema Rules*, and the closed-enum
/// value sets in *Metadata*):
///
/// - **Reading** an unrecognised value is legal. The value is preserved
///   verbatim in the conformer's `unknown` case so nothing is stripped, and the
///   UI renders a "created with a newer version of Capsule" indicator instead of
///   guessing. Dropping it would violate the never-strip rule; coercing it to a
///   default would fabricate state.
/// - **Writing** an unrecognised value is a **structural rejection**
///   (``requireWritable()``). A client that cannot name a value must not author
///   one — that is how an old client is stopped from strip-and-resigning a
///   newer document.
///
/// ## Why it is hand-rolled
///
/// Swift forbids a raw-value enum from carrying a case with an associated
/// value, so `enum CullFlag: String { … case unknown(String) }` cannot compile.
/// Conformers therefore declare **no** raw type and satisfy `RawRepresentable`
/// by hand: a non-failable `init(rawValue:)` (which legally witnesses the
/// failable requirement) plus an explicit `rawValue` switch. In exchange the
/// stdlib's `RawRepresentable` conditional conformances give `Codable` for
/// free, and ``isKnown``/``requireWritable()`` come from this protocol's
/// extension, so each conformer adds only its cases, ``knownCases``, and the
/// two members.
///
/// The raw strings are **whatever the Rust `serde` attribute says for that
/// specific type** — `snake_case` for ``StackType``, `kebab-case` for
/// ``ProvenanceAction`` — never a casing this layer invents, because one wrong
/// character is a failed signature on the far side.
public protocol ClosedWireEnum: RawRepresentable, Sendable, Hashable, Codable where RawValue == String {
    /// Every value this build knows how to *write*, in canonical declaration
    /// order. Excludes the `unknown` case by construction.
    static var knownCases: [Self] { get }

    /// Whether this build recognises the value — `false` only for a value that
    /// arrived from a newer writer.
    var isKnown: Bool { get }
}

public extension ClosedWireEnum {
    /// Hash on the wire string, so hashing agrees with equality.
    ///
    /// **Load-bearing.** The stdlib supplies `==` for every `RawRepresentable`
    /// whose `RawValue` is `Equatable`, and that operator compares *raw values* —
    /// so `.unknown("pick")` and `.pick` are equal. Left to synthesise,
    /// `hash(into:)` would hash the enum *structurally* and give those two equal
    /// values different hashes, breaking the `Hashable` contract and with it
    /// every `Set` and dictionary keyed on a wire enum — including the
    /// ``QueryGrammar`` tables. Hashing the raw value restores the invariant.
    func hash(into hasher: inout Hasher) {
        hasher.combine(rawValue)
    }

    /// Default: a value is known when its wire string is one this build can
    /// write.
    ///
    /// Because equality is by raw value, an adversarial `.unknown("pick")`
    /// compares equal to `.pick` and is therefore reported as known — which is
    /// correct rather than a hole: writing it emits exactly `"pick"`, a value
    /// this build can name. What the gate must refuse is an *unrecognised
    /// string*, and it does.
    var isKnown: Bool {
        Self.knownCases.contains(self)
    }

    /// Whether the value may be written back. Identical to ``isKnown``; named
    /// separately because the *read* and *write* rules are deliberately
    /// different, and call sites should say which one they mean.
    var isWritable: Bool {
        isKnown
    }

    /// Gate a write on the value being one this build can name.
    ///
    /// - Throws: ``ClosedEnumWriteRejection`` when the value is unknown. This is
    ///   a structural rejection, never a warning to ignore.
    func requireWritable() throws {
        guard isKnown else {
            throw ClosedEnumWriteRejection(typeName: String(describing: Self.self), rawValue: rawValue)
        }
    }

    /// Decode from an optional wire string, returning `nil` for a wire-absent
    /// value.
    ///
    /// Fields whose default is wire-absent (`gps.datum`, `cull`, `hidden`,
    /// `key_mode`) use this rather than inventing a value — the difference
    /// between "never written" and "written to the default" is load-bearing for
    /// every CRDT register.
    static func decodingWireAbsent(_ raw: String?) -> Self? {
        guard let raw else { return nil }
        return Self(rawValue: raw)
    }
}

// MARK: - ClosedEnumWriteRejection

/// A write named a closed-enum value this build does not know.
///
/// Structural, not advisory: the caller must abandon the write, not coerce the
/// value. Carries enough context to log and to drive a "created with a newer
/// version of Capsule" surface, and no user-facing copy — display text lives in
/// the i18n catalog.
public struct ClosedEnumWriteRejection: Error, Sendable, Equatable, Hashable {
    /// The Swift type that refused the value, for diagnostics.
    public var typeName: String
    /// The unrecognised wire string, preserved verbatim.
    public var rawValue: String

    public init(typeName: String, rawValue: String) {
        self.typeName = typeName
        self.rawValue = rawValue
    }
}
