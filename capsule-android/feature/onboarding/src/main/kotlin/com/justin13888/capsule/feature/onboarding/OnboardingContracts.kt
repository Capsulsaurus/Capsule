package com.justin13888.capsule.feature.onboarding

import androidx.compose.runtime.Immutable
import kotlinx.collections.immutable.ImmutableList

/**
 * The two ways a Capsule library can exist, as a first-class choice.
 *
 * [LocalOnly] is not a deferred [ServerBacked] and not a trial: the Local Gallery contract
 * (FR2) says never having signed in is a valid, complete mode, so the type spells the two as
 * peers rather than modelling "signed in" as a boolean that defaults to false.
 */
enum class OnboardingMode {
    /** A whole library on this device, with no account and no network. */
    LocalOnly,

    /** A library backed by a Capsule server the user names and signs in to. */
    ServerBacked,
}

/**
 * A way a deployment lets a user prove who they are.
 *
 * A deployment may offer any subset — local auth and OIDC are both first-class, and a
 * self-hosted server that runs no identity provider offers only the former. The picker is
 * therefore built from what the server advertises rather than from a fixed list.
 */
enum class AuthMethod {
    /** A platform passkey, checked by this device's biometrics or screen lock. */
    Passkey,

    /** A password plus a time-based one-time code. */
    PasswordTotp,

    /** An external identity provider, over authorization-code + PKCE. */
    Oidc,
}

/**
 * What `.well-known/capsule/server-info` says about a server, before any credential is sent.
 *
 * Server-scoped facts only. The record deliberately carries no user list — a federated peer
 * being able to enumerate a server's users is the one thing the discovery design refuses —
 * so there is nothing here that names a person, and the card that renders it can be shown to
 * someone who has not signed in.
 */
@Immutable
data class ServerInfo(
    /** The address the API is actually served from, which may differ from what was typed. */
    val apiBase: String,
    /** The auth paths this deployment offers, in the order it advertises them. */
    val authMethods: ImmutableList<AuthMethod>,
    /** The oldest `protocol_version` the server still accepts. */
    val minProtocolVersion: Int,
    /** The newest `protocol_version` the server understands. */
    val maxProtocolVersion: Int,
)

/**
 * Reads a server's public information.
 *
 * A port rather than a repository because no `core:data` contract owns discovery yet. Keeping
 * it here — narrow, one method — means the screen is written against an interface that the
 * real transport can claim unchanged, and that a test can satisfy without a network.
 */
interface ServerProbe {
    /**
     * Fetches `.well-known/capsule/server-info` for [address].
     *
     * Throws a `CapsuleError` rather than returning a result type: every failure here is one
     * the screen renders as an error state with a remediation, and a nullable return would
     * lose the distinction between "unreachable" and "too old to talk to".
     */
    suspend fun probe(address: String): ServerInfo
}

/** Where a sign-in attempt got to. */
sealed interface AuthStep {
    /** There is a session; onboarding may continue. */
    data object Complete : AuthStep

    /** The first factor passed and a time-based code is now required. */
    data object TotpRequired : AuthStep
}

/**
 * Establishing a session against the chosen server.
 *
 * Every path mints the same session and binds identity to the master key identically; only
 * the check differs, which is why one port carries all four rather than three parallel ones.
 */
interface AuthGateway {
    /** Checks a password. May answer [AuthStep.TotpRequired] rather than completing. */
    suspend fun signIn(
        email: String,
        password: String,
    ): AuthStep

    /** Answers the time-based challenge raised by [signIn]. */
    suspend fun verifyTotp(code: String)

    /** Runs the platform passkey ceremony. */
    suspend fun authenticateWithPasskey()

    /** Completes the hand-off to an external identity provider. */
    suspend fun completeOidc()

    /** Creates an account. Establishes the account only — it confers no data access. */
    suspend fun register(
        email: String,
        password: String,
    )
}
