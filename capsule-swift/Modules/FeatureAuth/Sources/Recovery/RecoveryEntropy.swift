import Foundation

// MARK: - RecoveryEntropyPolicy

/// The strength floor a recovery secret must clear, and the wordlist it is
/// measured against.
///
/// *Backup & Recovery — Master-Key Escrow* is unambiguous: the escrow blob is
/// offline-attackable once exfiltrated and Argon2id raises brute-force cost only
/// linearly, so **the secret itself must carry the security** — ≥128 bits, no
/// low-entropy code without an attested enclave.
///
/// The wordlist size is a parameter rather than a constant because entropy is a
/// property of the *generator*, not of the screen: a meter that assumed BIP39
/// would over-report a secret drawn from a smaller list, which is the one
/// direction an entropy meter must never be wrong in.
public struct RecoveryEntropyPolicy: Sendable, Equatable, Hashable {
    /// How many words the generator draws from.
    public var wordlistSize: Int
    /// The floor, in bits.
    public var floorBits: Int

    public init(wordlistSize: Int, floorBits: Int = 128) {
        self.wordlistSize = wordlistSize
        self.floorBits = floorBits
    }

    /// A BIP39-style 2048-word list: 11 bits a word, so 12 words clear the
    /// floor exactly.
    public static let bip39 = RecoveryEntropyPolicy(wordlistSize: 2048)

    /// Bits per word, `log2(wordlistSize)`.
    public var bitsPerWord: Double {
        guard wordlistSize > 1 else { return 0 }
        return log2(Double(wordlistSize))
    }
}

// MARK: - RecoveryEntropyEstimate

/// What the meter shows.
///
/// It reports a floor-relative verdict rather than a five-bar "strength"
/// gauge, because the rule being enforced is a threshold, not a vibe: a secret
/// either carries ≥128 bits or it does not, and a bar that reads "strong" at
/// 66 bits would be lying about the one number that matters.
public struct RecoveryEntropyEstimate: Sendable, Equatable, Hashable {
    public var wordCount: Int
    public var bits: Int
    public var floorBits: Int

    public init(wordCount: Int, bits: Int, floorBits: Int) {
        self.wordCount = wordCount
        self.bits = bits
        self.floorBits = floorBits
    }

    /// Whether the secret clears the documented floor.
    public var meetsFloor: Bool { bits >= floorBits }

    /// The meter's fill, clamped to 1. Paired with a number and a label, never
    /// the only signal.
    public var fraction: Double {
        guard floorBits > 0 else { return 0 }
        return min(1, Double(bits) / Double(floorBits))
    }
}

// MARK: - RecoveryEntropy

/// Measures a generated secret against a policy.
public enum RecoveryEntropy {
    /// Estimate the entropy of a word-based secret.
    ///
    /// Deliberately **floors** the bit count: a meter that rounded up would
    /// report a secret as clearing 128 bits when it does not.
    public static func estimate(
        wordCount: Int,
        policy: RecoveryEntropyPolicy = .bip39
    ) -> RecoveryEntropyEstimate {
        let bits = Int((Double(max(0, wordCount)) * policy.bitsPerWord).rounded(.down))
        return RecoveryEntropyEstimate(wordCount: wordCount, bits: bits, floorBits: policy.floorBits)
    }
}
