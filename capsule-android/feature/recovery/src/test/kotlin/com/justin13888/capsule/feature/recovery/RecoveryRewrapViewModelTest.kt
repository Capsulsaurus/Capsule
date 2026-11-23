package com.justin13888.capsule.feature.recovery

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test
import java.security.SecureRandom

/**
 * The guided re-wrap.
 *
 * Two properties are asserted that no user will ever see and every user depends on: the
 * minted secret clears the entropy floor, and the type-back gate cannot be walked past. The
 * third is the one they will see — that guardian shares minted under the old secret are
 * invalidated and *said* to be, rather than left listed as a recovery path that no longer
 * exists.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class RecoveryRewrapViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @BeforeEach
    fun installMainDispatcher() {
        Dispatchers.setMain(dispatcher)
    }

    @AfterEach
    fun removeMainDispatcher() {
        Dispatchers.resetMain()
    }

    @Test
    fun `it opens on the explanation rather than on a minted secret`() =
        runTest(dispatcher) {
            val viewModel = RecoveryRewrapViewModel(gateway())

            assertEquals(RewrapStage.Explaining, viewModel.state.value.stage)
            assertNull(viewModel.state.value.secret)
        }

    @Test
    fun `the minted secret clears the entropy floor the escrow depends on`() =
        runTest(dispatcher) {
            val viewModel = RecoveryRewrapViewModel(gateway())

            viewModel.begin()

            val secret = viewModel.state.value.secret
            assertNotNull(secret)
            assertTrue(
                secret!!.entropyBits >= RecoveryRewrapGateway.MINIMUM_ENTROPY_BITS,
                "an offline-attackable escrow gets its security from the secret, not from the KDF",
            )
            assertEquals(secret.words.size, secret.phrase.split(" ").size)
        }

    @Test
    fun `the type-back gate asks for words the user has just been shown`() =
        runTest(dispatcher) {
            val viewModel = RecoveryRewrapViewModel(gateway())
            viewModel.begin()
            val secret = viewModel.state.value.secret!!

            viewModel.onSecretWrittenDown()

            val state = viewModel.state.value
            assertEquals(RewrapStage.TypingBack, state.stage)
            assertTrue(state.challenges.isNotEmpty())
            assertTrue(state.challenges.all { it in 1..secret.words.size })
            assertEquals(state.challenges.sorted(), state.challenges, "asked in reading order")
        }

    @Test
    fun `a wrong word is refused and the same word is asked again`() =
        runTest(dispatcher) {
            val viewModel = begun()
            val asked = viewModel.state.value.currentChallenge

            viewModel.onTypedChange("definitely-not-the-word")
            viewModel.confirm()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state.typedWrong)
            assertEquals(asked, state.currentChallenge, "the gate does not move on")
            assertEquals(RewrapStage.TypingBack, state.stage)
        }

    @Test
    fun `answering every word runs the re-wrap`() =
        runTest(dispatcher) {
            val viewModel = begun()

            answerEveryChallenge(viewModel)
            advanceUntilIdle()

            assertEquals(RewrapStage.Finished, viewModel.state.value.stage)
        }

    @Test
    fun `a partial answer never reaches the re-wrap`() =
        runTest(dispatcher) {
            val store = InMemoryShamirShareStore()
            val viewModel = begun(store)
            val secret = viewModel.state.value.secret!!
            val first = viewModel.state.value.currentChallenge!!

            viewModel.onTypedChange(secret.words[first - 1])
            viewModel.confirm()
            advanceUntilIdle()

            assertEquals(RewrapStage.TypingBack, viewModel.state.value.stage, "one of three is not the gate passed")
        }

    @Test
    fun `guardian shares minted under the old secret are invalidated and said to be`() =
        runTest(dispatcher) {
            val store = InMemoryShamirShareStore()
            store.replace(
                ShamirEnrollment(threshold = 2, total = 3, shares = listOf(ShamirShare(1, "aa"), ShamirShare(2, "bb"))),
            )
            val viewModel = begun(store)

            answerEveryChallenge(viewModel)
            advanceUntilIdle()

            assertTrue(viewModel.state.value.sharesInvalidated, "the screen has to offer a re-issue")
            assertNull(store.enrollment.value, "shares that rebuild a deleted escrow are not a recovery path")
        }

    @Test
    fun `an account with no shares is not told to re-issue any`() =
        runTest(dispatcher) {
            val viewModel = begun(InMemoryShamirShareStore())

            answerEveryChallenge(viewModel)
            advanceUntilIdle()

            assertFalse(viewModel.state.value.sharesInvalidated)
        }

    @Test
    fun `resetting throws away a secret that was never committed`() =
        runTest(dispatcher) {
            val viewModel = begun()

            viewModel.reset()

            assertEquals(RewrapStage.Explaining, viewModel.state.value.stage)
            assertNull(viewModel.state.value.secret)
        }

    /** Answers each challenge in turn with the word it asked for. */
    private fun answerEveryChallenge(viewModel: RecoveryRewrapViewModel) {
        val secret = viewModel.state.value.secret!!
        while (viewModel.state.value.currentChallenge != null &&
            viewModel.state.value.stage == RewrapStage.TypingBack
        ) {
            val position = viewModel.state.value.currentChallenge!!
            viewModel.onTypedChange(secret.words[position - 1])
            viewModel.confirm()
        }
    }

    private fun gateway(store: ShamirShareStore = InMemoryShamirShareStore()): RecoveryRewrapGateway =
        LocalRecoveryRewrapGateway(shares = store, random = seededRandom())

    private fun begun(store: ShamirShareStore = InMemoryShamirShareStore()): RecoveryRewrapViewModel =
        RecoveryRewrapViewModel(gateway(store), seededRandom()).also {
            it.begin()
            it.onSecretWrittenDown()
        }

    private fun seededRandom(): SecureRandom = SecureRandom.getInstance(ALGORITHM).apply { setSeed(SEED) }

    private companion object {
        const val ALGORITHM = "SHA1PRNG"
        val SEED = byteArrayOf(1, 2, 3, 4)
    }
}
