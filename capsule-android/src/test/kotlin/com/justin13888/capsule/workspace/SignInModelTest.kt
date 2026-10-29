package com.justin13888.capsule.workspace

import kotlinx.coroutines.runBlocking
import uniffi.capsule_sdk.FfiException
import uniffi.capsule_sdk.FfiLoginOutcome
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertSame
import kotlin.test.assertTrue

/** Signing in: the sessions it leaves behind, and what the form shows on the way. */
class SignInModelTest {
    private val client = FakeClient()
    private val addresses = MemoryAddressStore()
    private val connected = mutableListOf<ServerAddress>()
    private val sessions =
        ServerSessions(addresses) {
            connected += it
            client
        }

    private fun model() = SignInModel(sessions)

    private fun SignInModel.fill(
        server: String = "capsule.example.com",
        handle: String = " ada@capsule.example.com ",
        password: String = "correct horse",
    ) {
        updateServer(server)
        updateHandle(handle)
        updatePassword(password)
    }

    @Test
    fun aPasswordSignInHoldsTheSessionAndRemembersTheServer() =
        runBlocking {
            val session = FakeSession()
            client.login = { FfiLoginOutcome.Session(session) }
            val model = model().apply { fill() }
            model.submit()

            val state = model.state.value
            assertTrue(state.isDone)
            assertEquals("", state.password)
            assertEquals(listOf("ada@capsule.example.com" to "correct horse"), client.logins)
            assertEquals("https://capsule.example.com/v1/auth", connected.single().authBaseUrl)
            assertSame(session, sessions.current.value?.session)
            assertEquals("https://capsule.example.com", addresses.stored?.origin)
            assertEquals("https://capsule.example.com", model().state.value.server)
        }

    @Test
    fun aSecondFactorIsAskedForAndFinishesTheSignIn() =
        runBlocking {
            client.login = { FfiLoginOutcome.SecondFactorRequired("challenge-1", 0uL) }
            val session = FakeSession()
            client.verify = { session }
            val model = model().apply { fill() }
            model.submit()
            assertTrue(model.state.value.askingForCode)
            assertNull(sessions.current.value)

            model.updateCode(" 123456 ")
            model.submit()
            assertEquals(listOf("challenge-1" to "123456"), client.codes)
            assertTrue(model.state.value.isDone)
            assertSame(session, sessions.current.value?.session)
            assertEquals(1, client.logins.size)
        }

    @Test
    fun anExpiredChallengeAsksForThePasswordAgain() =
        runBlocking {
            client.login = { FfiLoginOutcome.SecondFactorRequired("challenge-1", 0uL) }
            client.verify = { throw FfiException.Auth("error.auth.totp_challenge_invalid", "expired") }
            val model = model().apply { fill() }
            model.submit()
            model.updateCode("123456")
            model.submit()

            val state = model.state.value
            assertFalse(state.askingForCode)
            assertEquals(SignInMessage.ChallengeExpired, state.message)
            assertNull(sessions.current.value)
        }

    @Test
    fun aWrongCodeStaysOnTheCodeStep() =
        runBlocking {
            client.login = { FfiLoginOutcome.SecondFactorRequired("challenge-1", 0uL) }
            client.verify = { throw FfiException.Auth("error.auth.totp_invalid_code", "wrong") }
            val model = model().apply { fill() }
            model.submit()
            model.updateCode("000000")
            model.submit()
            assertTrue(model.state.value.askingForCode)
            assertEquals(SignInMessage.CodeInvalid, model.state.value.message)
        }

    @Test
    fun aRefusalShowsItsCodesMessageAndClearsThePassword() =
        runBlocking {
            client.login = { throw FfiException.Auth("error.auth.invalid_credentials", "no") }
            val model = model().apply { fill() }
            model.submit()
            val state = model.state.value
            assertEquals(SignInMessage.CredentialsRejected, state.message)
            assertEquals("", state.password)
            assertFalse(state.isSubmitting)
            assertNull(sessions.current.value)
            assertNull(addresses.stored)
        }

    @Test
    fun anAddressThatIsNotAnHttpsOriginReachesNoServer() =
        runBlocking {
            val model = model().apply { fill(server = "http://capsule.example.com") }
            model.submit()
            assertEquals(SignInMessage.InvalidAddress, model.state.value.message)
            assertEquals("correct horse", model.state.value.password)
            assertTrue(connected.isEmpty())
        }

    @Test
    fun eachCodeTheFormNamesMapsToItsMessage() {
        fun refused(code: String?) = SignInMessage.of(SignInFailure(SignInFailure.Kind.Refused, code))
        assertEquals(SignInMessage.RateLimited, refused("error.auth.rate_limited"))
        assertEquals(SignInMessage.AccountLocked, refused("error.auth.account_locked"))
        assertEquals(SignInMessage.ProtocolUnsupported, refused("error.protocol.version_unsupported"))
        assertEquals(SignInMessage.Unavailable, refused("error.auth.unavailable"))
        assertEquals(SignInMessage.Unavailable, refused(null))
        assertEquals(SignInMessage.InvalidAddress, SignInMessage.of(SignInFailure(SignInFailure.Kind.InvalidAddress, null)))
    }

    @Test
    fun signingInAgainReleasesTheSessionItReplaces() =
        runBlocking {
            val first = FakeSession()
            client.login = { FfiLoginOutcome.Session(first) }
            sessions.signIn(ServerAddress.parse("capsule.example.com")!!, "ada", "pw")
            val second = FakeSession()
            client.login = { FfiLoginOutcome.Session(second) }
            sessions.signIn(ServerAddress.parse("capsule.example.com")!!, "ada", "pw")
            assertTrue(first.closed)
            assertFalse(second.closed)
            assertSame(second, sessions.current.value?.session)
        }

    @Test
    fun anAddressTheSdkRefusesIsAnInvalidAddress() =
        runBlocking {
            val refusing = ServerSessions(addresses) { throw FfiException.InvalidArgument("bad url") }
            val model = SignInModel(refusing).apply { fill() }
            model.submit()
            assertEquals(SignInMessage.InvalidAddress, model.state.value.message)
        }
}
