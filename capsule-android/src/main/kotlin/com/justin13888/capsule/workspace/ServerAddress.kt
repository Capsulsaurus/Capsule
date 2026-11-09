package com.justin13888.capsule.workspace

import java.net.URI
import java.net.URISyntaxException

/**
 * The origin of a Capsule server, as the person typed it on the sign-in screen: a bare
 * domain (`capsule.example.com`), a domain with a port, or a full `https://` origin.
 *
 * The auth and upload surfaces hang off the shared origin at `/v1/auth` and `/v1/upload`,
 * the same layout the CLI's remote configuration uses. Only `https` is accepted: the app
 * never sends a password over cleartext, and Android refuses cleartext traffic anyway.
 */
@JvmInline
value class ServerAddress private constructor(
    /** `https://host[:port]`, with no path and no trailing slash. */
    val origin: String,
) {
    /** The root the SDK's auth client takes. */
    val authBaseUrl: String get() = "$origin/v1/auth"

    /** The root the SDK's upload client takes. */
    val uploadBaseUrl: String get() = "$origin/v1/upload"

    companion object {
        /**
         * The server [input] names, or `null` when it names none: blank, not a URL, a
         * scheme other than `https`, or a path, query or credentials beyond the origin.
         */
        fun parse(input: String): ServerAddress? {
            val trimmed = input.trim()
            if (trimmed.isEmpty()) return null
            val withScheme = if ("://" in trimmed) trimmed else "https://$trimmed"
            val uri =
                try {
                    URI(withScheme)
                } catch (_: URISyntaxException) {
                    return null
                }
            val host = uri.host
            // A lone trailing slash is still the origin.
            val path = uri.rawPath.orEmpty()
            val beyondOrigin = (path.isNotEmpty() && path != "/") || uri.rawQuery != null || uri.rawFragment != null
            return when {
                !uri.scheme.equals("https", ignoreCase = true) -> null
                host.isNullOrEmpty() || uri.rawUserInfo != null || beyondOrigin -> null
                else -> {
                    val port = if (uri.port == -1) "" else ":${uri.port}"
                    ServerAddress("https://${host.lowercase()}$port")
                }
            }
        }
    }
}
