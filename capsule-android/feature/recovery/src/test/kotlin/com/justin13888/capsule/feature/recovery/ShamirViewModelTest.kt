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
 * The guardian-share screen's state machine.
 *
 * The clamping is the point. An `m`-of-`n` where `m` exceeds `n` is a scheme nobody can
 * satisfy, and a screen that let one be *entered* and then refused it on submit would have
 * already let the user write down three envelopes' worth of shares that rebuild nothing.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ShamirViewModelTest {
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
    fun `it opens on the default two-of-three scheme with nothing split`() =
        runTest(dispatcher) {
            val viewModel = viewModel()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(ShamirUiState.DEFAULT_THRESHOLD, state.threshold)
            assertEquals(ShamirUiState.DEFAULT_TOTAL, state.total)
            assertTrue(state.isEmpty)
            assertTrue(state.isSchemeValid)
        }

    @Test
    fun `the threshold can never exceed the number of shares that will exist`() =
        runTest(dispatcher) {
            val viewModel = viewModel()
            advanceUntilIdle()

            viewModel.onThresholdChange(99)

            assertEquals(viewModel.state.value.total, viewModel.state.value.threshold)
            assertTrue(viewModel.state.value.isSchemeValid)
        }

    @Test
    fun `the share count can never fall below the threshold`() =
        runTest(dispatcher) {
            val viewModel = viewModel()
            advanceUntilIdle()

            viewModel.onTotalChange(1)

            assertEquals(viewModel.state.value.threshold, viewModel.state.value.total)
        }

    @Test
    fun `a threshold of one is refused, because one share would be the whole secret`() =
        runTest(dispatcher) {
            val viewModel = viewModel()
            advanceUntilIdle()

            viewModel.onThresholdChange(1)

            assertEquals(ShamirSplitter.MIN_THRESHOLD, viewModel.state.value.threshold)
        }

    @Test
    fun `generating splits a secret into the proposed scheme and remembers it`() =
        runTest(dispatcher) {
            val store = InMemoryShamirShareStore()
            val viewModel = viewModel(store)
            advanceUntilIdle()
            viewModel.onTotalChange(5)
            viewModel.onThresholdChange(3)

            viewModel.generate()
            advanceUntilIdle()

            val enrollment = viewModel.state.value.enrollment
            assertNotNull(enrollment)
            assertEquals(3, enrollment!!.threshold)
            assertEquals(5, enrollment.total)
            assertEquals(5, enrollment.shares.size)
            assertEquals(listOf(1, 2, 3, 4, 5), enrollment.shares.map(ShamirShare::index))
            assertNotNull(store.enrollment.value)
        }

    @Test
    fun `the shares this device minted really do rebuild one secret between them`() =
        runTest(dispatcher) {
            val viewModel = viewModel()
            advanceUntilIdle()

            viewModel.generate()
            advanceUntilIdle()

            val shares =
                viewModel.state.value.enrollment!!
                    .shares
            assertEquals(
                1,
                shares.map { it.payload.length }.toSet().size,
                "shares of one secret are all its length",
            )
            assertEquals(shares.size, shares.map(ShamirShare::payload).toSet().size, "no two shares are alike")
        }

    @Test
    fun `a freshly minted share is not counted as distributed`() =
        runTest(dispatcher) {
            val viewModel = viewModel()
            advanceUntilIdle()
            viewModel.generate()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(0, state.enrollment!!.deliveredCount)
            assertFalse(state.isDistributed, "shares in the drawer beside the phone are not a recovery path")
        }

    @Test
    fun `recording a holder needs a name, and marks only that share`() =
        runTest(dispatcher) {
            val viewModel = viewModel()
            advanceUntilIdle()
            viewModel.generate()
            advanceUntilIdle()

            viewModel.onEditHolder(1)
            viewModel.markDelivered(1)
            advanceUntilIdle()
            assertEquals(
                0,
                viewModel.state.value.enrollment!!
                    .deliveredCount,
                "an unnamed holder records nothing",
            )

            viewModel.onHolderDraftChange(HOLDER)
            viewModel.markDelivered(1)
            advanceUntilIdle()

            val enrollment = viewModel.state.value.enrollment!!
            assertEquals(1, enrollment.deliveredCount)
            assertEquals(HOLDER, enrollment.shares.first { it.index == 1 }.holder)
            assertNull(viewModel.state.value.editingShare, "the holder field closes once it is answered")
        }

    @Test
    fun `enough delivered shares make it a recovery path`() =
        runTest(dispatcher) {
            val viewModel = viewModel()
            advanceUntilIdle()
            viewModel.generate()
            advanceUntilIdle()

            listOf(1, 2).forEach { index ->
                viewModel.onEditHolder(index)
                viewModel.onHolderDraftChange(HOLDER)
                viewModel.markDelivered(index)
            }
            advanceUntilIdle()

            assertTrue(viewModel.state.value.isDistributed)
        }

    @Test
    fun `regenerating replaces the old set rather than adding to it`() =
        runTest(dispatcher) {
            val viewModel = viewModel()
            advanceUntilIdle()
            viewModel.generate()
            advanceUntilIdle()
            val first =
                viewModel.state.value.enrollment!!
                    .shares
                    .map(ShamirShare::payload)

            viewModel.generate()
            advanceUntilIdle()

            val second =
                viewModel.state.value.enrollment!!
                    .shares
                    .map(ShamirShare::payload)
            assertEquals(first.size, second.size)
            assertFalse(first == second, "a new set is a new secret, and the old shares are gone")
        }

    @Test
    fun `a re-wrap elsewhere clears the list, because those shares now rebuild nothing`() =
        runTest(dispatcher) {
            val store = InMemoryShamirShareStore()
            val viewModel = viewModel(store)
            advanceUntilIdle()
            viewModel.generate()
            advanceUntilIdle()

            store.invalidate()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.isEmpty)
        }

    private fun viewModel(store: ShamirShareStore = InMemoryShamirShareStore()): ShamirViewModel =
        ShamirViewModel(store, LocalRecoveryRewrapGateway(store, random()), random())

    private fun random(): SecureRandom = SecureRandom.getInstance(ALGORITHM)

    private companion object {
        const val ALGORITHM = "SHA1PRNG"
        const val HOLDER = "My sister"
    }
}
