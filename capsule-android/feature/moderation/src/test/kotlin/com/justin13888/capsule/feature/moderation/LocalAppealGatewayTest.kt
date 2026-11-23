package com.justin13888.capsule.feature.moderation

import com.justin13888.capsule.core.model.ModerationEventId
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows

/**
 * The stand-in behind the two appeal ports.
 *
 * It is not cryptography and does not pretend to be, but it does have to hold one property
 * the screen's failure path depends on: a proof that does not correspond to the challenge
 * that was issued is refused. Without that, the "nothing was sent" state would be
 * unreachable, and an untested state is a state that regresses.
 */
class LocalAppealGatewayTest {
    private val gateway = LocalAppealGateway()

    @Test
    fun `each case gets its own challenge`() =
        runTest {
            val first = gateway.challenge(CASE)
            val second = gateway.challenge(ModerationEventId("other"))

            assertFalse(
                first.bytes.contentEquals(second.bytes),
                "a reused nonce would let one appeal's proof file another",
            )
        }

    @Test
    fun `a proof over the issued challenge is accepted and yields a reference`() =
        runTest {
            val challenge = gateway.challenge(CASE)
            val proof = gateway.sign(challenge)

            val receipt = gateway.submit(AppealRequest(CASE, STATEMENT, proof))

            assertTrue(receipt.reference.isNotBlank())
        }

    @Test
    fun `a proof that answers nothing is refused`() =
        runTest {
            gateway.challenge(CASE)

            assertThrows<AppealProofRejected> {
                gateway.submit(AppealRequest(CASE, STATEMENT, ByteArray(1)))
            }
        }

    @Test
    fun `an appeal with no challenge behind it is refused`() =
        runTest {
            assertThrows<AppealProofRejected> {
                gateway.submit(AppealRequest(CASE, STATEMENT, ByteArray(1)))
            }
        }

    @Test
    fun `a challenge is spent once, so a replayed proof is refused`() =
        runTest {
            val challenge = gateway.challenge(CASE)
            val proof = gateway.sign(challenge)
            val first = gateway.submit(AppealRequest(CASE, STATEMENT, proof))

            assertThrows<AppealProofRejected> {
                gateway.submit(AppealRequest(CASE, STATEMENT, proof))
            }
            assertNotEquals("", first.reference)
        }

    private companion object {
        val CASE = ModerationEventId("0197a0d0-0000-7000-8000-0000000000f1")
        const val STATEMENT = "Please reconsider."
    }
}
