import CapsuleDomain
import FeatureAuth
import Foundation
import Testing

// MARK: - RecoveryRevealTests

/// The reveal half: mint once, measure honestly, keep exactly one copy.
@Suite("The recovery phrase is shown once and measured against the floor")
@MainActor
struct RecoveryRevealTests {
    @Test("revealing mints the phrase, splits it into words, and measures it")
    func revealMintsAndMeasures() async {
        let model = RecoveryPassphraseViewModel(recovery: StubRecoveryPort())

        await model.reveal()

        #expect(model.state == .ready)
        #expect(model.stage == .reveal)
        #expect(model.wordCount == 12)
        #expect(model.revealedWords.count == 12)
        #expect(model.revealedWords.first == "harbor")
        #expect(model.entropy?.bits == 132)
        #expect(model.entropy?.meetsFloor == true)
        #expect(!model.isBelowEntropyFloor)
    }

    /// A below-floor secret is a defect in the build, not a user error — the
    /// user did not pick anything.
    @Test("a generator that produced a weak phrase is reported as a problem")
    func belowFloorPhraseIsReported() async {
        let weak = StubRecoveryPort(mintedPhrase: "harbor-lantern-quartz-meadow")
        let model = RecoveryPassphraseViewModel(recovery: weak)

        await model.reveal()

        #expect(model.wordCount == 4)
        #expect(model.entropy?.bits == 44)
        #expect(model.isBelowEntropyFloor)
    }

    @Test("a failed mint reveals nothing and lands in the failed state")
    func failedMintRevealsNothing() async {
        let failing = StubRecoveryPort(
            setUpFailure: CapsuleError(code: .authRateLimited, detail: "stub")
        )
        let model = RecoveryPassphraseViewModel(recovery: failing)

        await model.reveal()

        #expect(model.state.failure?.code == .authRateLimited)
        #expect(model.state.failure?.isRetryable == true)
        #expect(model.revealedWords.isEmpty)
        #expect(model.secretForCopying() == nil)
        #expect(model.wordCount == 0)
    }

    @Test("the guided re-wrap mints a fresh secret through its own entry point")
    func rotateMintsAFreshSecret() async {
        let model = RecoveryPassphraseViewModel(recovery: StubRecoveryPort(), source: .rotate)

        await model.reveal()

        #expect(model.revealedWords.first == "beacon")
        #expect(model.revealedWords.count == 12)
    }

    @Test("copying is a boolean and the plaintext leaves through one named door")
    func copyingIsTrackedButNotStored() async {
        let model = RecoveryPassphraseViewModel(recovery: StubRecoveryPort())
        await model.reveal()

        #expect(!model.hasCopied)
        model.markCopied()

        #expect(model.hasCopied)
        #expect(model.secretForCopying() == StubRecoveryPort.phrase)
    }

    @Test("beginning the gate before anything is revealed does nothing")
    func gateNeedsASecretFirst() {
        let model = RecoveryPassphraseViewModel(recovery: StubRecoveryPort())

        model.beginTypeBack()

        #expect(model.stage == .reveal)
        #expect(model.challenges.isEmpty)
        #expect(!model.canComplete)
    }
}

// MARK: - RecoveryTypeBackTests

/// The gate half. *Device Enrollment* step 6 gates setup on this precisely so
/// the user records the phrase rather than dismissing the screen.
@Suite("The type-back gate is the only way past the phrase screen")
@MainActor
struct RecoveryTypeBackTests {
    private static func revealed() async -> RecoveryPassphraseViewModel {
        let model = RecoveryPassphraseViewModel(recovery: StubRecoveryPort())
        await model.reveal()
        model.beginTypeBack()
        return model
    }

    private static func answerAll(_ model: RecoveryPassphraseViewModel, correctly: Bool = true) {
        let words = StubRecoveryPort.phrase.split(separator: "-").map(String.init)
        for challenge in model.challenges {
            let answer = correctly ? words[challenge.wordIndex] : "wrongword"
            model.submit(answer, forWordAt: challenge.wordIndex)
        }
    }

    @Test("the gate asks for exactly three distinct positions inside the phrase")
    func gateAsksForThreeDistinctPositions() async {
        let model = await Self.revealed()

        #expect(RecoveryPassphraseViewModel.challengeCount == 3)
        #expect(model.stage == .typeBack)
        #expect(model.challenges.count == 3)
        #expect(Set(model.challenges.map(\.wordIndex)).count == 3)
        #expect(model.challenges.allSatisfy { (0 ..< 12).contains($0.wordIndex) })
        #expect(model.challenges.map(\.wordIndex) == model.challenges.map(\.wordIndex).sorted())
        #expect(model.challenges.allSatisfy { $0.displayPosition == $0.wordIndex + 1 })
    }

    @Test(
        "position selection never repeats and never runs past the end of the phrase",
        arguments: [0, 1, 2, 3, 12, 24]
    )
    func positionSelectionIsSafe(wordCount: Int) {
        var generator = SystemRandomNumberGenerator()

        let positions = RecoveryPassphraseViewModel.challengePositions(
            wordCount: wordCount,
            count: 3,
            using: &generator
        )

        #expect(positions.count == min(3, wordCount))
        #expect(Set(positions).count == positions.count)
        #expect(positions.allSatisfy { $0 >= 0 && $0 < wordCount })
        #expect(positions == positions.sorted())
    }

    @Test("a wrong answer is rejected, and correcting it is what verifies the word")
    func wrongAnswersAreRejected() async {
        let model = await Self.revealed()
        let words = StubRecoveryPort.phrase.split(separator: "-").map(String.init)
        guard let first = model.challenges.first else {
            Issue.record("the gate produced no challenges")
            return
        }

        model.submit("definitely-not-it", forWordAt: first.wordIndex)
        #expect(model.challenges.first?.isVerified == false)
        #expect(!model.canComplete)

        model.submit(words[first.wordIndex], forWordAt: first.wordIndex)
        #expect(model.challenges.first?.isVerified == true)
    }

    @Test("a word typed the way a human types it still matches")
    func answersAreComparedTheWayAHumanTypes() async {
        let model = await Self.revealed()
        let words = StubRecoveryPort.phrase.split(separator: "-").map(String.init)
        guard let first = model.challenges.first else {
            Issue.record("the gate produced no challenges")
            return
        }

        model.submit("  \(words[first.wordIndex].uppercased())  ", forWordAt: first.wordIndex)

        #expect(model.challenges.first?.isVerified == true)
    }

    @Test("submitting for a position the gate did not ask about changes nothing")
    func unaskedPositionsAreIgnored() async {
        let model = await Self.revealed()
        let asked = Set(model.challenges.map(\.wordIndex))
        guard let unasked = (0 ..< 12).first(where: { !asked.contains($0) }) else {
            Issue.record("every position was challenged")
            return
        }

        model.submit("harbor", forWordAt: unasked)

        #expect(model.challenges.count == 3)
        #expect(!model.canComplete)
    }

    @Test("two of three right is still not through the gate")
    func partialAnswersDoNotOpenTheGate() async {
        let model = await Self.revealed()
        let words = StubRecoveryPort.phrase.split(separator: "-").map(String.init)
        for challenge in model.challenges.prefix(2) {
            model.submit(words[challenge.wordIndex], forWordAt: challenge.wordIndex)
        }

        #expect(model.remainingChallengeCount == 1)
        #expect(!model.canComplete)
        #expect(model.complete() == false)
        #expect(model.stage == .typeBack)
    }

    @Test("answering every position correctly is what opens the gate")
    func fullyAnsweredGateOpens() async {
        let model = await Self.revealed()

        Self.answerAll(model)

        #expect(model.remainingChallengeCount == 0)
        #expect(model.canComplete)
        #expect(model.complete())
        #expect(model.stage == .completed)
    }

    @Test("completing drops the secret, so the screen cannot show it again")
    func completingDropsTheSecret() async {
        let model = await Self.revealed()
        Self.answerAll(model)

        #expect(model.complete())

        #expect(model.secretForCopying() == nil)
        #expect(model.revealedWords.isEmpty)
        #expect(model.challenges.allSatisfy { $0.typed.isEmpty })
    }

    @Test("going back to the phrase is allowed before the gate passes, not after")
    func returningToTheRevealIsGatedByStage() async {
        let model = await Self.revealed()

        model.returnToReveal()
        #expect(model.stage == .reveal)

        model.beginTypeBack()
        Self.answerAll(model)
        #expect(model.complete())

        model.returnToReveal()
        #expect(model.stage == .completed, "a passed gate does not reopen")
    }

    /// The security-relevant one: there is no `skip()`, no `dismiss()`, no
    /// `continueAnyway()`, and nothing on the public surface reaches
    /// ``RecoveryPassphraseViewModel/Stage/completed`` without three correct
    /// answers. Every mutating entry point is exercised here with the gate
    /// unsatisfied.
    @Test("no path on the public surface completes the flow without passing the gate")
    func noPathBypassesTheGate() async {
        let model = RecoveryPassphraseViewModel(recovery: StubRecoveryPort())

        await model.reveal()
        #expect(model.complete() == false)
        model.markCopied()
        #expect(model.complete() == false)
        model.beginTypeBack()
        #expect(model.complete() == false)
        model.returnToReveal()
        #expect(model.complete() == false)
        model.beginTypeBack()
        Self.answerAll(model, correctly: false)
        #expect(model.complete() == false)
        model.beginTypeBack()
        #expect(model.complete() == false)
        await model.reveal()
        #expect(model.complete() == false)

        #expect(model.stage != .completed)
        #expect(!model.canComplete)
    }

    @Test("an empty challenge set cannot be treated as a satisfied one")
    func emptyChallengeSetIsNotSatisfied() async {
        let model = RecoveryPassphraseViewModel(recovery: StubRecoveryPort())
        await model.reveal()

        #expect(model.challenges.isEmpty)
        #expect(!model.canComplete, "vacuous truth must not open a gate")
        #expect(model.complete() == false)
    }
}
