import FeatureSettings
import Foundation
import Testing

// MARK: - TypedPhraseGateTests

/// The gate exists precisely because a formality is what a button already was,
/// so the comparison is exact — case included — and loosening it would make the
/// gate decorative.
@Suite("A typed-phrase confirmation matches exactly, trimmed only at the ends")
@MainActor
struct TypedPhraseGateTests {
    @Test("a fresh gate is empty and unsatisfied")
    func freshGateIsUnsatisfied() {
        let gate = TypedPhraseGate(requiredPhrase: "RESTORE")

        #expect(gate.typedPhrase.isEmpty)
        #expect(!gate.isSatisfied)
        #expect(!gate.isPartiallyTyped, "an empty field is not a mismatch to shout about")
    }

    @Test(
        "the comparison is exact, with surrounding whitespace forgiven and nothing else",
        arguments: [
            (typed: "RESTORE", satisfied: true),
            (typed: "  RESTORE", satisfied: true),
            (typed: "RESTORE  ", satisfied: true),
            (typed: "\tRESTORE\n", satisfied: true),
            (typed: "restore", satisfied: false),
            (typed: "Restore", satisfied: false),
            (typed: "RESTORe", satisfied: false),
            (typed: "RES TORE", satisfied: false),
            (typed: "RESTOREX", satisfied: false),
            (typed: "", satisfied: false),
            (typed: "   ", satisfied: false),
        ]
    )
    func comparisonIsDeliberate(sample: (typed: String, satisfied: Bool)) {
        let gate = TypedPhraseGate(requiredPhrase: "RESTORE")

        gate.typedPhrase = sample.typed

        #expect(gate.isSatisfied == sample.satisfied)
    }

    @Test("case is part of the phrase, not an accident of the keyboard")
    func caseIsLoadBearing() {
        let gate = TypedPhraseGate(requiredPhrase: "Delete Library")

        gate.typedPhrase = "delete library"
        #expect(!gate.isSatisfied)

        gate.typedPhrase = "Delete Library"
        #expect(gate.isSatisfied)
    }

    @Test("interior whitespace is part of the phrase, unlike whitespace at the ends")
    func interiorWhitespaceIsNotTrimmed() {
        let gate = TypedPhraseGate(requiredPhrase: "Delete Library")

        gate.typedPhrase = "  Delete Library  "
        #expect(gate.isSatisfied)

        gate.typedPhrase = "DeleteLibrary"
        #expect(!gate.isSatisfied)
    }

    /// A gate with nothing to type would otherwise be satisfied by an empty
    /// field, which is a gate that authorises everything.
    @Test("a gate with an empty required phrase can never be satisfied")
    func emptyRequiredPhraseNeverPasses() {
        let gate = TypedPhraseGate(requiredPhrase: "")

        #expect(!gate.isSatisfied)

        gate.typedPhrase = ""
        #expect(!gate.isSatisfied)

        gate.typedPhrase = "   "
        #expect(!gate.isSatisfied)
    }

    @Test("a half-typed phrase is the state an inline hint belongs in")
    func partialTypingIsItsOwnState() {
        let gate = TypedPhraseGate(requiredPhrase: "RESTORE")

        gate.typedPhrase = "REST"
        #expect(gate.isPartiallyTyped)
        #expect(!gate.isSatisfied)

        gate.typedPhrase = "RESTORE"
        #expect(!gate.isPartiallyTyped, "a matched phrase is not a mismatch")
        #expect(gate.isSatisfied)
    }

    @Test("resetting clears the field, so re-opening never starts pre-satisfied")
    func resetClearsTheField() {
        let gate = TypedPhraseGate(requiredPhrase: "RESTORE")
        gate.typedPhrase = "RESTORE"
        #expect(gate.isSatisfied)

        gate.reset()

        #expect(gate.typedPhrase.isEmpty)
        #expect(!gate.isSatisfied)
        #expect(!gate.isPartiallyTyped)
    }

    /// A user confirming in French types the French word, so the gate compares
    /// what the catalog resolved to at runtime rather than a hard-coded token.
    @Test("a gate built from a catalog key compares whatever that key resolved to")
    func catalogBackedGateComparesTheResolvedText() {
        let key = "ios.settings.confirm.phrase.restore"
        let resolved = SettingsPhrase.text(forKey: key)
        let gate = TypedPhraseGate(phraseKey: key)

        #expect(!resolved.isEmpty)
        #expect(gate.requiredPhrase == resolved)

        gate.typedPhrase = resolved
        #expect(gate.isSatisfied)

        gate.typedPhrase = resolved + "!"
        #expect(!gate.isSatisfied)
    }
}
