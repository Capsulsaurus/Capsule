package com.justin13888.capsule.feature.enrollment

import androidx.compose.runtime.Immutable
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import kotlin.random.Random

/**
 * The words the account master key is escrowed under.
 *
 * Held as a list of words rather than as one string because every screen that touches it
 * needs the positions: the reveal numbers them so a user can transcribe them in order, and
 * the type-back gate asks for three of them by position. Joining and re-splitting would make
 * "word four" a parsing question.
 *
 * Nothing here is ever written down by the app. The type is a plain in-memory value with no
 * persistence hook, and the ceremony that produced it hands it out once.
 */
@Immutable
data class RecoveryPassphrase(
    /** The words, in order. */
    val words: ImmutableList<String>,
) {
    /** The whole passphrase, for the clipboard and for printing. */
    val text: String get() = words.joinToString(" ")

    companion object {
        /** How many words a passphrase has. */
        const val LENGTH: Int = 12

        /**
         * Draws a passphrase from [random].
         *
         * The word pool is a small, deliberately unmemorable slice of the BIP39 English list:
         * this is a fixture, and a fixture that looked like a real 2048-word list would invite
         * someone to treat its output as a real secret. The shape — numbered, short, lowercase,
         * unambiguous words — is what the screens are built against, and that is faithful.
         */
        fun generate(random: Random): RecoveryPassphrase =
            RecoveryPassphrase(List(LENGTH) { WORD_POOL[random.nextInt(WORD_POOL.size)] }.toImmutableList())

        private val WORD_POOL =
            persistentListOf(
                "abandon",
                "absorb",
                "acoustic",
                "adapt",
                "amber",
                "anchor",
                "apple",
                "arctic",
                "armor",
                "aspect",
                "atlas",
                "author",
                "avocado",
                "banner",
                "basket",
                "beacon",
                "bicycle",
                "bishop",
                "bracket",
                "bridge",
                "bronze",
                "cactus",
                "camera",
                "canyon",
                "capsule",
                "carbon",
                "cargo",
                "castle",
                "cinder",
                "cobalt",
                "collar",
                "comet",
                "copper",
                "coral",
                "cotton",
                "crater",
                "crystal",
                "cypress",
                "damper",
                "dolphin",
                "ember",
                "falcon",
                "fabric",
                "fossil",
                "garden",
                "gravel",
                "harbor",
                "helmet",
                "island",
                "jacket",
                "kernel",
                "lantern",
                "marble",
                "meadow",
                "nectar",
                "orbit",
                "pebble",
                "quartz",
                "ribbon",
                "saffron",
                "timber",
                "velvet",
                "walnut",
                "zephyr",
            )
    }
}

/**
 * The words the user has to type back before setup completes.
 *
 * Positions rather than words, because the challenge is asked by position and answered by
 * word — and because holding the expected answers here would put a second copy of part of the
 * passphrase in a longer-lived object than the one that showed it.
 */
@Immutable
data class TypeBackChallenge(
    /** The zero-based positions being asked for, ascending. */
    val positions: ImmutableList<Int>,
) {
    companion object {
        /** How many words the gate asks for. */
        const val ASKED: Int = 3

        /** Picks [ASKED] distinct positions out of a passphrase of [length] words. */
        fun draw(
            length: Int,
            random: Random,
        ): TypeBackChallenge =
            TypeBackChallenge(
                generateSequence { random.nextInt(length) }
                    .distinct()
                    .take(ASKED)
                    .sorted()
                    .toList()
                    .toImmutableList(),
            )
    }
}

/**
 * Whether [answers] are what [passphrase] actually said at [challenge]'s positions.
 *
 * Case- and space-insensitive, because a user transcribing from paper is being asked to prove
 * they wrote the words down, not that they can reproduce whitespace. Anything looser would
 * turn the gate back into the advisory screen it exists to replace.
 */
internal fun TypeBackChallenge.accepts(
    passphrase: RecoveryPassphrase,
    answers: Map<Int, String>,
): Boolean =
    positions.all { position ->
        answers[position]?.trim()?.equals(passphrase.words[position], ignoreCase = true) == true
    }
