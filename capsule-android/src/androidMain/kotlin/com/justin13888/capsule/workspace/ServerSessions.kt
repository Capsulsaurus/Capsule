package com.justin13888.capsule.workspace

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.capsule_sdk.FfiCapsuleClient
import uniffi.capsule_sdk.FfiCapsuleClientInterface
import uniffi.capsule_sdk.FfiException
import uniffi.capsule_sdk.FfiLoginOutcome
import uniffi.capsule_sdk.FfiSessionInterface
import java.util.logging.Logger

/** A signed-in server: its address, the handle that signed in, and the session the SDK holds. */
class SignedIn(
    val server: ServerAddress,
    val handle: String,
    val session: FfiSessionInterface,
)

/** Where a sign-in stands after the password. */
sealed interface SignInStep {
    /** Signed in: [ServerSessions.current] now holds the session. */
    data object Done : SignInStep

    /** The password verified and the account wants its second-factor code. */
    class SecondFactorRequired internal constructor(
        internal val client: FfiCapsuleClientInterface,
        internal val server: ServerAddress,
        internal val handle: String,
        internal val mfaToken: String,
    ) : SignInStep
}

/** Why a sign-in stopped, as the stable `error.*` [code] the server or SDK gave, if any. */
class SignInFailure(
    val kind: Kind,
    val code: String?,
) : Exception("sign-in failed: $kind${code?.let { " ($it)" }.orEmpty()}") {
    enum class Kind {
        /** The address is not one the SDK can reach. */
        InvalidAddress,

        /** The server refused or could not finish the sign-in; [code] says why when it can. */
        Refused,
    }
}

/**
 * The app's one server session: signs in with a handle and password, finishes a sign-in
 * that wants a second-factor code, and holds the signed-in session for the screens.
 *
 * No token is ever seen here: the SDK keeps them inside the session handle, so a session
 * lasts as long as the process does and the next launch signs in again. The address
 * signed in to last is remembered in [addresses] so the form can offer it.
 */
class ServerSessions(
    private val addresses: ServerAddressStore,
    private val connect: (ServerAddress) -> FfiCapsuleClientInterface = ::ffiClient,
) {
    private val signedIn = MutableStateFlow<SignedIn?>(null)

    /** The signed-in session, or `null` while there is none. */
    val current: StateFlow<SignedIn?> = signedIn.asStateFlow()

    /** The address signed in to last, if any. */
    val lastAddress: ServerAddress? get() = addresses.load()

    /** Sign in to [server] as [handle] with [password]. Throws [SignInFailure]. */
    suspend fun signIn(
        server: ServerAddress,
        handle: String,
        password: String,
    ): SignInStep {
        log.info("sign-in: starting against ${server.origin}")
        val client =
            try {
                connect(server)
            } catch (error: FfiException) {
                throw error.asFailure()
            }
        val outcome =
            try {
                client.login(handle, password)
            } catch (error: FfiException) {
                throw error.asFailure()
            }
        return when (outcome) {
            is FfiLoginOutcome.Session -> {
                adopt(SignedIn(server, handle, outcome.session))
                SignInStep.Done
            }
            is FfiLoginOutcome.SecondFactorRequired -> {
                log.info("sign-in: the account wants a second-factor code")
                SignInStep.SecondFactorRequired(client, server, handle, outcome.mfaToken)
            }
        }
    }

    /** Finish [step] with the authenticator's [code]. Throws [SignInFailure]. */
    suspend fun verifySecondFactor(
        step: SignInStep.SecondFactorRequired,
        code: String,
    ) {
        val session =
            try {
                step.client.verifySecondFactor(step.mfaToken, code.trim())
            } catch (error: FfiException) {
                throw error.asFailure()
            }
        adopt(SignedIn(step.server, step.handle, session))
    }

    private fun adopt(next: SignedIn) {
        addresses.save(next.server)
        val previous = signedIn.value
        signedIn.value = next
        // The session signed in over is released, which also ends its transfer feed.
        (previous?.session as? AutoCloseable)?.close()
        log.info("sign-in: signed in to ${next.server.origin}")
    }

    private fun FfiException.asFailure(): SignInFailure {
        val failure =
            when (this) {
                is FfiException.InvalidArgument -> SignInFailure(SignInFailure.Kind.InvalidAddress, null)
                is FfiException.Auth -> SignInFailure(SignInFailure.Kind.Refused, code)
                else -> SignInFailure(SignInFailure.Kind.Refused, null)
            }
        log.warning("sign-in: ${failure.message}")
        return failure
    }

    companion object {
        /**
         * The protocol date this app speaks, which every upload request names. It moves with
         * the core's `PROTOCOL_VERSION`, as the web client's pin does.
         */
        const val PROTOCOL_VERSION = "2026-05-31"

        private val log: Logger = Logger.getLogger("capsule.workspace")

        private fun ffiClient(server: ServerAddress): FfiCapsuleClientInterface =
            FfiCapsuleClient(server.authBaseUrl, server.uploadBaseUrl, PROTOCOL_VERSION, null)
    }
}

/** Where the address signed in to last is kept. */
interface ServerAddressStore {
    fun load(): ServerAddress?

    fun save(address: ServerAddress)
}
