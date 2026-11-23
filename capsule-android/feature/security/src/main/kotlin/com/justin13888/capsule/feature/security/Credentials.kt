package com.justin13888.capsule.feature.security

import androidx.compose.runtime.Immutable
import kotlinx.coroutines.flow.Flow
import java.time.Instant

/** A registered passkey's identity. Opaque; minted by whoever registered the credential. */
@JvmInline
value class PasskeyId(
    /** The credential id, as the authenticator issued it. */
    val value: String,
)

/**
 * One passkey registered against the account.
 *
 * [lastUsedAt] is nullable rather than defaulted to the creation time, because "registered
 * and never used since" is exactly the state a user is looking for when they are deciding
 * which of four passkeys to delete — and a creation time in that column would hide it.
 */
@Immutable
data class Passkey(
    /** The credential id. */
    val id: PasskeyId,
    /** What the authenticator called itself. Device-supplied, never translated. */
    val label: String,
    /** When it was registered. */
    val addedAt: Instant,
    /** The last sign-in it authenticated, or `null` if it has never been used. */
    val lastUsedAt: Instant? = null,
)

/**
 * A time-based one-time-password enrollment, confirmed or still in progress.
 *
 * The unconfirmed half is modelled rather than kept in the screen, because an enrollment that
 * was started and abandoned is a real state: the secret exists, the user may have already put
 * it in their authenticator, and a client that forgot about it would leave them with a code
 * generator for an account that does not accept it.
 */
@Immutable
data class TotpEnrollment(
    /** The shared secret, base32, as the user types it into their authenticator. */
    val secret: String,
    /** When enrollment began. */
    val startedAt: Instant,
    /** Whether a generated code has been matched back, which is what activates it. */
    val isConfirmed: Boolean,
)

/**
 * Passkeys and TOTP, as this feature needs them.
 *
 * Declared here rather than in `core:data` because there is no contract there yet: the
 * repository set covers sessions, devices and the local-auth gate, and the second-factor
 * surface has not been cut. Owning the interface in the feature keeps the screens written
 * against a contract rather than against an implementation, so the day a `core:data`
 * repository lands this becomes a delete plus an import change.
 *
 * Enrollment is two calls, not one. [beginTotpEnrollment] hands over a secret and
 * [confirmTotp] proves the user's authenticator actually produces matching codes — a
 * one-shot "enable TOTP" would leave a user locked behind a factor they mistyped.
 */
interface CredentialRepository {
    /** Every passkey on the account, newest first. */
    fun passkeys(): Flow<List<Passkey>>

    /** Registers a passkey with the platform authenticator. */
    suspend fun addPasskey(): Passkey

    /** Removes one passkey. */
    suspend fun deletePasskey(id: PasskeyId)

    /** The current enrollment, confirmed or not, or `null` when TOTP is not set up. */
    fun totp(): Flow<TotpEnrollment?>

    /** Starts an enrollment and returns the secret to show the user. */
    suspend fun beginTotpEnrollment(): TotpEnrollment

    /** Proves the user's authenticator is generating matching codes, activating the factor. */
    suspend fun confirmTotp(code: String)

    /** Abandons an unconfirmed enrollment, discarding its secret. */
    suspend fun cancelTotpEnrollment()

    /** Turns the factor off, which needs a current code just as enabling it did. */
    suspend fun disableTotp(code: String)
}

/**
 * The authenticator refused the six-digit code.
 *
 * A feature-local exception rather than a [com.justin13888.capsule.core.data.CapsuleError],
 * because that hierarchy is sealed in `core:data` and this failure has no server-issued
 * `error.*` code behind it yet. Carrying it as its own type keeps the screen from having to
 * guess which of the auth codes to borrow, which would put "Invalid email or password" in
 * front of someone who typed a TOTP code.
 */
class TotpCodeRejected : Exception("the authenticator code did not match")
