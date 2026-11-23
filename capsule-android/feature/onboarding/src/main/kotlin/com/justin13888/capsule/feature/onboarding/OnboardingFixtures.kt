package com.justin13888.capsule.feature.onboarding

import com.justin13888.capsule.core.data.AuthError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.ProtocolError
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import java.util.concurrent.atomic.AtomicInteger

/**
 * A [ServerProbe] whose answer is a pure function of the address typed at it.
 *
 * Discovery has no `core:mock` fake to drive because no repository contract owns it yet, and
 * a randomised stand-in would make every state except the happy one unreachable by hand. The
 * rules below are therefore part of the fixture's contract: a reviewer, a screenshot suite
 * and a bug report can all reach the same state by typing the same host.
 *
 * | Host contains | Result                                                        |
 * | ------------- | ------------------------------------------------------------- |
 * | `unreachable` | [LocalError.Offline] — the "could not reach that server" state |
 * | `legacy`      | [ProtocolError.VersionUnsupported] — the server is too old     |
 * | `sso`         | OIDC only                                                      |
 * | `passkey`     | passkeys only                                                  |
 * | `bare`        | no method this build implements                                |
 * | anything else | all three paths                                                |
 */
class FixtureServerProbe : ServerProbe {
    override suspend fun probe(address: String): ServerInfo {
        val host = address.lowercase()
        if (UNREACHABLE_MARKER in host) throw LocalError.Offline
        if (LEGACY_MARKER in host) throw ProtocolError.VersionUnsupported
        val methods =
            when {
                OIDC_MARKER in host -> persistentListOf(AuthMethod.Oidc)
                PASSKEY_MARKER in host -> persistentListOf(AuthMethod.Passkey)
                BARE_MARKER in host -> persistentListOf()
                else -> AuthMethod.entries.toImmutableList()
            }
        return ServerInfo(
            apiBase = address.trimEnd('/') + API_SUFFIX,
            authMethods = methods,
            minProtocolVersion = MIN_PROTOCOL,
            maxProtocolVersion = MAX_PROTOCOL,
        )
    }

    private companion object {
        const val UNREACHABLE_MARKER = "unreachable"
        const val LEGACY_MARKER = "legacy"
        const val OIDC_MARKER = "sso"
        const val PASSKEY_MARKER = "passkey"
        const val BARE_MARKER = "bare"
        const val API_SUFFIX = "/api/v1"
        const val MIN_PROTOCOL = 1
        const val MAX_PROTOCOL = 3
    }
}

/**
 * An [AuthGateway] with documented levers for every refusal the screens have to render.
 *
 * The rules are deliberately blunt and stated here rather than derived, for the same reason
 * [FixtureServerProbe]'s are: a failure state nobody can reach on purpose is a failure state
 * nobody reviews.
 *
 * - A password under [MINIMUM_PASSWORD] characters is refused as invalid credentials. The
 *   refusal is identical for a wrong password and an unknown account, because a login
 *   endpoint that distinguished them would be an account-existence oracle.
 * - A fourth consecutive refusal is rate-limited, which is the state the real server reaches
 *   through the same counter.
 * - Registering `taken@…` reports that the account already exists.
 * - Any six-digit code answers the time-based challenge except [REJECTED_TOTP], which is the
 *   lever for the wrong-code state.
 */
class FixtureAuthGateway : AuthGateway {
    private val consecutiveFailures = AtomicInteger(0)

    override suspend fun signIn(
        email: String,
        password: String,
    ): AuthStep {
        if (password.length < MINIMUM_PASSWORD) throw refusal(AuthError.InvalidCredentials)
        consecutiveFailures.set(0)
        return AuthStep.TotpRequired
    }

    override suspend fun verifyTotp(code: String) {
        val wellFormed = code.length == TOTP_LENGTH && code.all(Char::isDigit)
        if (!wellFormed || code == REJECTED_TOTP) throw refusal(AuthError.InvalidCredentials)
        consecutiveFailures.set(0)
    }

    override suspend fun authenticateWithPasskey() {
        consecutiveFailures.set(0)
    }

    override suspend fun completeOidc() {
        consecutiveFailures.set(0)
    }

    override suspend fun register(
        email: String,
        password: String,
    ) {
        if (password.length < MINIMUM_PASSWORD) throw refusal(AuthError.InvalidCredentials)
        if (email.substringBefore('@').equals(TAKEN_LOCAL_PART, ignoreCase = true)) {
            throw refusal(AuthError.UserAlreadyExists)
        }
        consecutiveFailures.set(0)
    }

    /**
     * Records a refusal and escalates once the budget is spent.
     *
     * The escalation is per-gateway rather than per-call so that the rate limit is reached by
     * doing the thing that reaches it — failing repeatedly — rather than by a test reaching
     * into a flag.
     */
    private fun refusal(error: AuthError): AuthError =
        if (consecutiveFailures.incrementAndGet() > ATTEMPT_BUDGET) AuthError.RateLimited else error

    private companion object {
        const val MINIMUM_PASSWORD = 8
        const val ATTEMPT_BUDGET = 3
        const val TOTP_LENGTH = 6
        const val REJECTED_TOTP = "000000"
        const val TAKEN_LOCAL_PART = "taken"
    }
}
