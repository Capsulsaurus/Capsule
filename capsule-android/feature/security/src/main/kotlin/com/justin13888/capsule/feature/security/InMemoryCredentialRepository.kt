package com.justin13888.capsule.feature.security

import android.os.Build
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.time.Clock
import kotlin.random.Random

/**
 * The second-factor surface, held in memory until a real one exists.
 *
 * A deliberate stand-in, in the same spirit as `core:mock`: the screens are built against
 * [CredentialRepository], so the day the platform authenticator and the server endpoints land
 * this class is replaced and nothing above it moves. It lives in the feature rather than in
 * `core:mock` because the contract it satisfies lives here too, and splitting a port from its
 * only adapter across modules would make the pair harder to delete together.
 *
 * The code check is a shape check, not a cryptographic one. It accepts any six digits, which
 * is exactly as much as a mock can honestly claim: computing a real TOTP would need a secret
 * this stand-in has no business holding as authoritative. What it does enforce is that a code
 * is *asked for* and can be *refused*, which is the part the flow above it has to handle.
 */
class InMemoryCredentialRepository(
    private val clock: Clock = Clock.systemUTC(),
    private val random: Random = Random.Default,
    deviceLabel: String = Build.MODEL.orEmpty().ifBlank { FALLBACK_LABEL },
) : CredentialRepository {
    private val label = deviceLabel
    private val passkeyState = MutableStateFlow<List<Passkey>>(emptyList())
    private val totpState = MutableStateFlow<TotpEnrollment?>(null)

    override fun passkeys(): Flow<List<Passkey>> = passkeyState.asStateFlow()

    override suspend fun addPasskey(): Passkey {
        val passkey =
            Passkey(
                id = PasskeyId(randomToken(CREDENTIAL_ID_BYTES)),
                label = label,
                addedAt = clock.instant(),
            )
        passkeyState.update { listOf(passkey) + it }
        return passkey
    }

    override suspend fun deletePasskey(id: PasskeyId) {
        passkeyState.update { current -> current.filterNot { it.id == id } }
    }

    override fun totp(): Flow<TotpEnrollment?> = totpState.asStateFlow()

    override suspend fun beginTotpEnrollment(): TotpEnrollment {
        val enrollment =
            TotpEnrollment(
                secret = randomBase32(SECRET_CHARS),
                startedAt = clock.instant(),
                isConfirmed = false,
            )
        totpState.value = enrollment
        return enrollment
    }

    override suspend fun confirmTotp(code: String) {
        if (!code.isWellFormedCode()) throw TotpCodeRejected()
        totpState.update { it?.copy(isConfirmed = true) }
    }

    override suspend fun cancelTotpEnrollment() {
        // Only an unconfirmed enrollment is abandonable. Turning a live factor off is
        // `disableTotp`, and it costs a code — a cancel that quietly did both would be a way
        // to drop a second factor without proving anything.
        totpState.update { current -> current?.takeIf(TotpEnrollment::isConfirmed) }
    }

    override suspend fun disableTotp(code: String) {
        if (!code.isWellFormedCode()) throw TotpCodeRejected()
        totpState.value = null
    }

    private fun String.isWellFormedCode(): Boolean = length == CODE_LENGTH && all(Char::isDigit)

    private fun randomToken(bytes: Int): String = (0 until bytes).joinToString(separator = "") { randomHexByte() }

    private fun randomHexByte(): String = "%02x".format(random.nextInt(BYTE_RANGE))

    private fun randomBase32(chars: Int): String =
        (0 until chars).map { BASE32[random.nextInt(BASE32.length)] }.joinToString(separator = "")

    private companion object {
        /** What a device with no readable model name is called. */
        const val FALLBACK_LABEL = "Passkey"

        const val CODE_LENGTH = 6
        const val CREDENTIAL_ID_BYTES = 16
        const val SECRET_CHARS = 32
        const val BYTE_RANGE = 256
        const val BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
    }
}

/**
 * Where the proof for a global revoke comes from.
 *
 * A port with one method, because the important thing about the global revoke is *who*
 * authenticates it: a signature by the account's identity key over a server-issued challenge,
 * never the caller's session token. Modelling the proof as something obtained from a separate
 * source keeps that visible in the code — a view model that could produce it from its own
 * session would be the bug the asymmetry exists to prevent.
 */
fun interface MasterKeyProofSource {
    /**
     * Signs the server's challenge with the account's identity key.
     *
     * Returning an empty array is a legitimate answer: a device that cannot reach the master
     * key has nothing to prove with, and the repository refuses on an empty proof rather than
     * the client pretending it succeeded.
     */
    suspend fun proveMasterKey(): ByteArray
}

/**
 * The stand-in proof source for the mocked build.
 *
 * The value it returns is the one the seeded session repository accepts. That is a deliberate
 * coupling between two stand-ins and it is stated here rather than hidden: no real key store
 * exists on this build, and a source that returned nothing would leave the global-revoke flow
 * permanently in its refused state with no way to exercise the success path.
 */
class StubMasterKeyProofSource : MasterKeyProofSource {
    override suspend fun proveMasterKey(): ByteArray = PROOF.toByteArray()

    private companion object {
        const val PROOF = "capsule-master-key-proof"
    }
}
