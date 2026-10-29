package com.justin13888.capsule.workspace

import uniffi.capsule_sdk.FfiCapsuleClientInterface
import uniffi.capsule_sdk.FfiLoginOutcome
import uniffi.capsule_sdk.FfiSession
import uniffi.capsule_sdk.NoHandle
import java.io.File

/** Seals by XOR with a fixed byte and a marker, so a foreign or tampered value fails to open. */
class FakeSealer(
    private val mask: Byte = 0x5A,
) : SecretSealer {
    var sealed = 0

    override fun seal(plaintext: ByteArray): ByteArray {
        sealed++
        return byteArrayOf(MARKER) + plaintext.map { (it.toInt() xor mask.toInt()).toByte() }
    }

    override fun open(sealed: ByteArray): ByteArray {
        check(sealed.firstOrNull() == MARKER) { "not sealed by this key" }
        return sealed.drop(1).map { (it.toInt() xor mask.toInt()).toByte() }.toByteArray()
    }

    private companion object {
        const val MARKER: Byte = 0x7F
    }
}

/** Counts each draw, and answers with a value that differs per draw. */
class CountingRandom {
    var draws = 0

    fun bytes(count: Int): ByteArray {
        draws++
        return ByteArray(count) { (it + draws).toByte() }
    }
}

fun unlockStore(
    dir: File,
    sealer: SecretSealer = FakeSealer(),
    random: CountingRandom = CountingRandom(),
): UnlockSecretStore = UnlockSecretStore(File(dir, "unlock.sealed"), sealer, random::bytes)

/** A session handle that reaches no core: the session object is only ever held here. */
class FakeSession : FfiSession(NoHandle) {
    var closed = false

    override fun close() {
        closed = true
    }
}

/** A server that answers each login and code check with whatever the test queued. */
class FakeClient : FfiCapsuleClientInterface {
    val logins = mutableListOf<Pair<String, String>>()
    val codes = mutableListOf<Pair<String, String>>()
    var login: () -> FfiLoginOutcome = { FfiLoginOutcome.Session(FakeSession()) }
    var verify: () -> FfiSession = { FakeSession() }

    override suspend fun login(
        email: String,
        password: String,
    ): FfiLoginOutcome {
        logins += email to password
        return login()
    }

    override suspend fun verifySecondFactor(
        mfaToken: String,
        totpCode: String,
    ): FfiSession {
        codes += mfaToken to totpCode
        return verify()
    }

    override suspend fun register(
        email: String,
        password: String,
    ): FfiSession = error("register is not part of sign-in")
}

class MemoryAddressStore(
    var stored: ServerAddress? = null,
) : ServerAddressStore {
    override fun load(): ServerAddress? = stored

    override fun save(address: ServerAddress) {
        stored = address
    }
}
