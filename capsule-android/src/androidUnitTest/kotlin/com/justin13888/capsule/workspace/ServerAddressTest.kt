package com.justin13888.capsule.workspace

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

/** What the server field accepts, and the SDK roots an address hands the client. */
class ServerAddressTest {
    @Test
    fun aBareDomainIsAnHttpsOrigin() {
        val address = ServerAddress.parse("  Capsule.Example.com/ ")
        assertEquals("https://capsule.example.com", address?.origin)
        assertEquals("https://capsule.example.com/v1/auth", address?.authBaseUrl)
        assertEquals("https://capsule.example.com/v1/upload", address?.uploadBaseUrl)
    }

    @Test
    fun aPortAndAnExplicitHttpsSchemeAreKept() {
        assertEquals("https://capsule.example.com:8443", ServerAddress.parse("https://capsule.example.com:8443")?.origin)
        assertEquals("https://10.0.2.2:8443", ServerAddress.parse("10.0.2.2:8443")?.origin)
    }

    @Test
    fun anythingButAnHttpsOriginIsRefused() {
        for (input in listOf(
            "",
            "   ",
            "http://capsule.example.com",
            "ftp://capsule.example.com",
            "https://capsule.example.com/v1",
            "https://capsule.example.com?x=1",
            "https://user:pw@capsule.example.com",
            "https://",
            "not a url",
        )) {
            assertNull(ServerAddress.parse(input), input)
        }
    }
}
