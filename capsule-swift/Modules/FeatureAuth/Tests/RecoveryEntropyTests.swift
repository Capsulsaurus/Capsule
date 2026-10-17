import FeatureAuth
import Foundation
import Testing

/// One row of the floor table: a word count, the bits it carries, and whether
/// that clears 128.
struct EntropySample: Sendable {
    let words: Int
    let bits: Int
    let meets: Bool
}

// MARK: - RecoveryEntropyTests

/// The ≥128-bit floor is a threshold, not a vibe, so it is asserted from both
/// sides: a secret one word short must fail, and the shortest secret that
/// clears it must pass.
@Suite("A recovery secret clears 128 bits or it does not")
struct RecoveryEntropyTests {
    @Test("a BIP39 wordlist is 11 bits a word, so 12 words is the exact boundary")
    func bip39BoundaryHoldsFromBothSides() {
        let below = RecoveryEntropy.estimate(wordCount: 11)
        let boundary = RecoveryEntropy.estimate(wordCount: 12)

        #expect(below.bits == 121)
        #expect(!below.meetsFloor, "121 bits must not pass a 128-bit floor")
        #expect(boundary.bits == 132)
        #expect(boundary.meetsFloor)
        #expect(boundary.floorBits == 128)
    }

    @Test(
        "the verdict follows the bit count across the boundary",
        arguments: [
            EntropySample(words: 0, bits: 0, meets: false),
            EntropySample(words: 1, bits: 11, meets: false),
            EntropySample(words: 11, bits: 121, meets: false),
            EntropySample(words: 12, bits: 132, meets: true),
            EntropySample(words: 24, bits: 264, meets: true),
        ]
    )
    func floorVerdictTable(sample: EntropySample) {
        let estimate = RecoveryEntropy.estimate(wordCount: sample.words)

        #expect(estimate.bits == sample.bits)
        #expect(estimate.meetsFloor == sample.meets)
        #expect(estimate.wordCount == sample.words)
    }

    /// The one direction an entropy meter must never be wrong in.
    @Test("a smaller wordlist is measured as smaller, not assumed to be BIP39")
    func smallerWordlistIsNotOverReported() {
        let policy = RecoveryEntropyPolicy(wordlistSize: 1024)

        let twelve = RecoveryEntropy.estimate(wordCount: 12, policy: policy)
        let thirteen = RecoveryEntropy.estimate(wordCount: 13, policy: policy)

        #expect(policy.bitsPerWord == 10)
        #expect(twelve.bits == 120)
        #expect(!twelve.meetsFloor, "twelve words off a 1024-word list is 120 bits, not 132")
        #expect(thirteen.bits == 130)
        #expect(thirteen.meetsFloor)
    }

    @Test("a fractional bits-per-word is floored, never rounded up onto the floor")
    func fractionalBitsAreFloored() {
        let policy = RecoveryEntropyPolicy(wordlistSize: 6000)

        // log2(6000) ≈ 12.5507, so ten words is 125.507 bits.
        let estimate = RecoveryEntropy.estimate(wordCount: 10, policy: policy)

        #expect(estimate.bits == 125)
        #expect(!estimate.meetsFloor, "125.5 bits must not be rounded up into passing")
    }

    @Test("a degenerate wordlist carries no entropy rather than an accidental win")
    func degenerateWordlistCarriesNoEntropy() {
        for size in [0, 1] {
            let policy = RecoveryEntropyPolicy(wordlistSize: size)
            let estimate = RecoveryEntropy.estimate(wordCount: 24, policy: policy)

            #expect(policy.bitsPerWord == 0)
            #expect(estimate.bits == 0)
            #expect(!estimate.meetsFloor)
        }
    }

    @Test("the meter's fill is clamped, and a zero floor cannot divide by zero")
    func fractionIsBounded() {
        let boundary = RecoveryEntropy.estimate(wordCount: 12)
        let overshoot = RecoveryEntropy.estimate(wordCount: 48)
        let unfloored = RecoveryEntropyEstimate(wordCount: 12, bits: 132, floorBits: 0)

        #expect(boundary.fraction == 1, "a secret at or above the floor fills the meter")
        #expect(overshoot.fraction == 1)
        #expect(RecoveryEntropy.estimate(wordCount: 6).fraction < 1)
        #expect(unfloored.fraction == 0)
    }

    @Test("a custom floor is the floor that is enforced")
    func customFloorIsHonoured() {
        let strict = RecoveryEntropyPolicy(wordlistSize: 2048, floorBits: 160)

        let twelve = RecoveryEntropy.estimate(wordCount: 12, policy: strict)
        let fifteen = RecoveryEntropy.estimate(wordCount: 15, policy: strict)

        #expect(twelve.floorBits == 160)
        #expect(!twelve.meetsFloor)
        #expect(fifteen.bits == 165)
        #expect(fifteen.meetsFloor)
    }
}
