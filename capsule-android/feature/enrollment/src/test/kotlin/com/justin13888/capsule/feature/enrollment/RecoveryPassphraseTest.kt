package com.justin13888.capsule.feature.enrollment

import kotlinx.collections.immutable.persistentListOf
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.random.Random

/** The passphrase and the gate in front of it, as pure functions. */
class RecoveryPassphraseTest {
    @Test
    fun `a passphrase is twelve words and the same seed produces the same twelve`() {
        val first = RecoveryPassphrase.generate(Random(7))
        val second = RecoveryPassphrase.generate(Random(7))

        assertEquals(RecoveryPassphrase.LENGTH, first.words.size)
        assertEquals(first.words, second.words)
    }

    @Test
    fun `the gate asks for three distinct positions, in reading order`() {
        val challenge = TypeBackChallenge.draw(RecoveryPassphrase.LENGTH, Random(3))

        assertEquals(TypeBackChallenge.ASKED, challenge.positions.size)
        assertEquals(challenge.positions.distinct().size, challenge.positions.size)
        assertEquals(challenge.positions.sorted(), challenge.positions.toList())
    }

    @Test
    fun `the right words pass, whatever the case and the stray spaces`() {
        val passphrase = RecoveryPassphrase.generate(Random(11))
        val challenge = TypeBackChallenge(persistentListOf(0, 5, 9))
        val answers = challenge.positions.associateWith { " ${passphrase.words[it].uppercase()} " }

        assertTrue(challenge.accepts(passphrase, answers))
    }

    @Test
    fun `a single wrong word fails the whole gate`() {
        val passphrase = RecoveryPassphrase.generate(Random(11))
        val challenge = TypeBackChallenge(persistentListOf(0, 5, 9))
        val answers =
            challenge.positions.associateWith { passphrase.words[it] } + (5 to "definitely-not-the-word")

        assertFalse(challenge.accepts(passphrase, answers))
    }

    @Test
    fun `an unanswered position fails rather than being treated as blank-and-fine`() {
        val passphrase = RecoveryPassphrase.generate(Random(11))
        val challenge = TypeBackChallenge(persistentListOf(0, 5, 9))

        assertFalse(challenge.accepts(passphrase, mapOf(0 to passphrase.words[0])))
    }
}
