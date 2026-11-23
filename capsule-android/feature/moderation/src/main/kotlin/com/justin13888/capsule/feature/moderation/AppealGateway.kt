package com.justin13888.capsule.feature.moderation

import com.justin13888.capsule.core.model.ModerationEventId
import java.security.MessageDigest
import java.security.SecureRandom

/**
 * A nonce the home server issued for one appeal.
 *
 * Held as bytes rather than text because the thing that gets signed is the challenge itself,
 * and a value that had been through a text encoding on the way would sign something subtly
 * different from what the server checks.
 */
@JvmInline
value class AppealChallenge(
    /** The bytes to sign. Never rendered. */
    val bytes: ByteArray,
)

/** What the user is asking the administrators to reconsider, and their proof of standing. */
data class AppealRequest(
    /** The record entry being contested. */
    val caseId: ModerationEventId,
    /** What the user wants the administrators to know. */
    val statement: String,
    /** A signature over [AppealChallenge] made with the account master key. */
    val proof: ByteArray,
)

/** The home server's acknowledgement that an appeal reached the admin queue. */
data class AppealReceipt(
    /** A short handle the user can quote when following up. */
    val reference: String,
)

/**
 * Signing an appeal's challenge with the account master key.
 *
 * A separate port from [AppealGateway] because the two live in different places: the
 * challenge comes off the network, the signature comes out of the key hierarchy, and a single
 * interface holding both would invite an implementation that fetched a challenge and signed
 * it in one breath — which is precisely the shape that cannot be audited.
 *
 * The proof is master-key, not session: an appeal is frequently filed *because* the session
 * is the thing in dispute, so authenticating it with that session would authenticate nothing.
 */
fun interface MasterKeyProver {
    /** Signs [challenge], or throws when the key is unavailable. */
    suspend fun sign(challenge: AppealChallenge): ByteArray
}

/**
 * Filing an appeal with the home server that acted.
 *
 * Declared here rather than in `core:data` because the appeal transport has no repository
 * contract yet — the moderation design fixes the *authentication* (master-key proof) and the
 * destination (the home-server admin queue) and leaves the carrier open. Keeping it a port the
 * feature owns means the screens are already written against the shape the eventual
 * repository will take, and swapping it is a Koin binding rather than a rewrite.
 */
interface AppealGateway {
    /** Asks the home server for the nonce this appeal must be signed over. */
    suspend fun challenge(caseId: ModerationEventId): AppealChallenge

    /** Submits a signed appeal. Throws when the proof does not verify. */
    suspend fun submit(request: AppealRequest): AppealReceipt
}

/**
 * The stand-in behind both ports until the real ones exist.
 *
 * It exists so the appeal screen can be driven — by a preview, by a test, and by the mocked
 * app — without pretending to be cryptography. The "signature" is a domain-separated digest
 * of the challenge: enough for the gateway to reject a proof that does not correspond to the
 * challenge it issued, which is the one property the screen's failure path depends on, and
 * obviously not enough to be mistaken for the real thing.
 */
class LocalAppealGateway(
    private val random: SecureRandom = SecureRandom(),
) : AppealGateway,
    MasterKeyProver {
    private val issued = mutableMapOf<String, ByteArray>()

    override suspend fun challenge(caseId: ModerationEventId): AppealChallenge {
        val bytes = ByteArray(CHALLENGE_BYTES).also(random::nextBytes)
        issued[caseId.value] = bytes
        return AppealChallenge(bytes)
    }

    override suspend fun sign(challenge: AppealChallenge): ByteArray = tag(challenge.bytes)

    override suspend fun submit(request: AppealRequest): AppealReceipt {
        val challenge = issued[request.caseId.value] ?: throw AppealProofRejected()
        if (!request.proof.contentEquals(tag(challenge))) throw AppealProofRejected()
        issued -= request.caseId.value
        return AppealReceipt(
            reference =
                request.caseId.value
                    .takeLast(REFERENCE_LENGTH)
                    .uppercase(),
        )
    }

    private fun tag(challenge: ByteArray): ByteArray =
        MessageDigest
            .getInstance(DIGEST)
            .apply { update(DOMAIN.toByteArray()) }
            .digest(challenge)

    private companion object {
        /** Domain separation, so this tag can never collide with one derived for another use. */
        const val DOMAIN = "capsule-appeal-proof/v1"
        const val DIGEST = "SHA-256"
        const val CHALLENGE_BYTES = 32
        const val REFERENCE_LENGTH = 8
    }
}

/**
 * The master key did not sign the challenge the server issued.
 *
 * A distinct type rather than a generic failure because the screen has to say something
 * specific and reassuring about it: nothing was sent, so there is nothing to undo.
 */
class AppealProofRejected : Exception("the master-key proof did not verify")
