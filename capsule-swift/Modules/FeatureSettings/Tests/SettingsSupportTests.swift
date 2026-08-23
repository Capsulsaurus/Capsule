import CapsuleDomain
import CapsuleFoundation
import FeatureSettings
import Foundation
import Testing

/// One row of the truncation table: the identifier, the budget, and whether the
/// budget bites.
struct TruncationSample: Sendable {
    let value: String
    let length: Int
    let truncated: Bool
}

// MARK: - AtRestPostureTests

/// The screen shows **one** row of the SR2 table — the running platform's —
/// because a Mac user reading the iOS row comes away believing a guarantee
/// their machine does not make.
@Suite("The at-rest posture is the running platform's row, not a reassurance")
struct AtRestPostureTests {
    @Test("a sandboxed container and a user directory are different postures")
    func thePostureFollowsTheStore() {
        let sandboxed = AtRestPosture.forPlatform(isSandboxPrivate: true)
        let userDirectory = AtRestPosture.forPlatform(isSandboxPrivate: false)

        #expect(sandboxed == .sandboxPrivate)
        #expect(userDirectory == .userDirectory)
        #expect(sandboxed != userDirectory, "the two rows must not collapse into one")
    }

    /// "OS user permissions only — any process of the same user can read it" is
    /// not a positive story, and the tone must not dress it up as one.
    @Test("only the sandboxed posture is drawn as reassuring")
    func tonesMatchWhatTheRowsSay() {
        #expect(AtRestPosture.sandboxPrivate.tone == .positive)
        #expect(AtRestPosture.userDirectory.tone == .caution)
    }

    @Test("every posture string is a catalog key, and no two rows share one")
    func postureStringsAreCatalogKeys() {
        let postures = [AtRestPosture.sandboxPrivate, .userDirectory]
        let keys = postures.flatMap { [$0.storeKey, $0.protectionKey, $0.summaryKey] }

        #expect(Set(keys).count == keys.count)
        for key in keys {
            #expect(key.hasPrefix("ios.settings.security.atrest."))
            #expect(!key.contains(" "))
        }
    }

    @Test("the running platform resolves to one of the two rows")
    func currentPostureIsOneOfTheRows() {
        #expect(AtRestPosture.current == .sandboxPrivate || AtRestPosture.current == .userDirectory)
    }
}

// MARK: - ConnectionClassPresentationTests

/// One table, so four screens describing the same fact do not read as three
/// different facts.
@Suite("Every connection class has one name and one tone")
struct ConnectionClassPresentationTests {
    @Test("every known class has its own catalog key", arguments: ConnectionClass.knownCases)
    func knownClassesHaveTheirOwnKey(connection: ConnectionClass) {
        let key = ConnectionClassPresentation.titleKey(connection)

        #expect(key == "ios.settings.connection.\(connection.rawValue)")
        #expect(!key.contains(" "))
    }

    @Test("keys are distinct, and a class from a newer writer still gets one")
    func keysAreDistinctAndTotal() {
        let known = ConnectionClass.knownCases.map(ConnectionClassPresentation.titleKey)
        let unknown = ConnectionClassPresentation.titleKey(ConnectionClass(rawValue: "starlink"))

        #expect(Set(known).count == known.count)
        #expect(unknown == "ios.settings.connection.unknown")
    }

    @Test("tone escalates with how little the connection can do")
    func tonesFollowCapability() {
        #expect(ConnectionClass.unmetered.tone == .positive)
        #expect(ConnectionClass.metered.tone == .caution)
        #expect(ConnectionClass.constrained.tone == .caution)
        #expect(ConnectionClass.adverse.tone == .caution)
        #expect(ConnectionClass.offline.tone == .critical)
        #expect(ConnectionClass(rawValue: "starlink").tone == .neutral, "an unknown class must not be dressed up")
    }

    @Test("a tone is reinforcement, so each one carries its own symbol")
    func tonesCarryTheirOwnSymbol() {
        let symbols = SettingsTone.allCases.map(\.symbol)

        #expect(Set(symbols).count == SettingsTone.allCases.count)
        #expect(symbols.allSatisfy { !$0.isEmpty })
    }
}

// MARK: - SettingsFormatTests

/// Values, not copy. What is asserted here is the behaviour a locale cannot
/// change: which facts are distinguished, and where the boundaries fall.
@Suite("Formatting distinguishes unknown from none, and truncates predictably")
struct SettingsFormatTests {
    @Test("an absent byte count reads as unknown, and zero bytes does not")
    func absentAndZeroAreDifferentFacts() {
        let absent = SettingsFormat.bytes(nil)
        let zero = SettingsFormat.bytes(UInt64(0))

        #expect(absent == SettingsFormat.unknown)
        #expect(zero != SettingsFormat.unknown, "'none' and 'unknown' are different facts")
        #expect(!zero.isEmpty)
    }

    @Test("byte counts scale, and the largest possible one does not trap")
    func byteCountsScaleAndClamp() {
        let small = SettingsFormat.bytes(UInt64(999))
        let large = SettingsFormat.bytes(UInt64(24 * 1073741824))
        let maximum = SettingsFormat.bytes(UInt64.max)

        #expect(small != large)
        #expect(!maximum.isEmpty, "an out-of-range figure clamps rather than crashing")
    }

    @Test("an event that has not happened reads as never, at both granularities")
    func absentInstantsReadAsNever() {
        #expect(SettingsFormat.timestamp(nil) == SettingsFormat.never)
        #expect(SettingsFormat.day(nil) == SettingsFormat.never)

        let stamped = SettingsFormat.timestamp(SettingsInstant.reference)
        let dayOnly = SettingsFormat.day(SettingsInstant.reference)
        #expect(stamped != SettingsFormat.never)
        #expect(stamped != dayOnly, "a settings row that wants the hour and one that does not differ")
    }

    @Test(
        "an identifier is truncated only when it is longer than the budget",
        arguments: [
            TruncationSample(value: "", length: 8, truncated: false),
            TruncationSample(value: "12345678", length: 8, truncated: false),
            TruncationSample(value: "123456789", length: 8, truncated: true),
            TruncationSample(value: "1234", length: 4, truncated: false),
            TruncationSample(value: "12345", length: 4, truncated: true),
        ]
    )
    func identifierTruncationBoundary(sample: TruncationSample) {
        let short = SettingsFormat.shortIdentifier(sample.value, length: sample.length)

        #expect(short.hasSuffix("…") == sample.truncated)
        if sample.truncated {
            #expect(short == String(sample.value.prefix(sample.length)) + "…")
            #expect(short.count == sample.length + 1)
        } else {
            #expect(short == sample.value)
        }
    }

    @Test("a model slot is spelled the way the AI doc spells it")
    func modelSlotIsSpelledConsistently() {
        let slot = ModelSlot(modelID: "clip-vit-b32", modelVersion: "2")

        #expect(SettingsFormat.modelSlot(slot) == "clip-vit-b32 2")
    }

    @Test("durations, minutes, and days are distinct renderings of a span")
    func spansRenderAtTheirOwnGranularity() {
        let countdown = SettingsFormat.duration(seconds: 125)
        let window = SettingsFormat.minutes(seconds: 300)
        let retention = SettingsFormat.days(30)

        #expect(!countdown.isEmpty)
        #expect(!window.isEmpty)
        #expect(!retention.isEmpty)
        #expect(countdown != SettingsFormat.duration(seconds: 0))
        #expect(window != SettingsFormat.minutes(seconds: 600))
        #expect(retention != SettingsFormat.days(7))
    }

    @Test("counts and percentages move with their input")
    func countsAndPercentagesMove() {
        #expect(SettingsFormat.count(0) != SettingsFormat.count(1000))
        #expect(SettingsFormat.percent(0) != SettingsFormat.percent(1))
        #expect(SettingsFormat.percent(0.5) != SettingsFormat.percent(0.9))
    }
}

// MARK: - SettingsClockTests

@Suite("The settings clock is injected, so a countdown is assertable")
struct SettingsClockTests {
    @Test("a fixed clock does not move")
    func fixedClockDoesNotMove() {
        let clock = SettingsClock.fixed(SettingsInstant.reference)

        #expect(clock.now() == SettingsInstant.reference)
        #expect(clock.now() == clock.now())
    }

    @Test("a clock can be pinned to an epoch second directly")
    func fixedFromEpochSeconds() {
        let clock = SettingsClock.fixed(epochSeconds: 1787400000)

        #expect(clock.now().epochSeconds == 1787400000)
        #expect(clock.now() == SettingsInstant.reference)
    }
}
