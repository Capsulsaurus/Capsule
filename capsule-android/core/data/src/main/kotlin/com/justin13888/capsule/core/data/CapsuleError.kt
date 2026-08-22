package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Every failure a repository in this module can report.
 *
 * A variant carries a **stable `error.*` code**, never an HTTP status. The status is a
 * transport detail that says nothing the user can act on and that two deployments may spell
 * differently for the same condition; the code is the contract, and the client localizes it.
 * The [detail] message beside it stays English on purpose so it remains greppable in a
 * support bundle after the code has been translated away.
 *
 * A sealed hierarchy rather than a status-plus-string pair because the interesting decisions
 * are per-variant: [QuotaError.Exceeded] means free space, [BlobError.PendingUpload] means
 * wait and re-fetch, [ProtocolError.VersionUnsupported] means update the app. A caller
 * matching on a string would get no help from the compiler when a new one appears.
 *
 * It extends `Exception` so a `suspend` contract can simply throw it — a `Result` wrapper on
 * every one-shot would push the same `when` into every call site without adding information.
 */
sealed class CapsuleError(
    /**
     * The stable catalogue key a client localizes.
     *
     * `null` only for [LocalError], which never crosses the wire and so has no server-issued
     * code to carry; nulling it is honest about that rather than inventing a key the
     * catalogue does not hold.
     */
    val code: ErrorCode?,
    /** The English detail message. Deliberately not localized. */
    val detail: String,
    cause: Throwable? = null,
) : Exception(detail, cause)

/**
 * A failure that happened entirely on this device and therefore has no catalogue code.
 *
 * Kept in the same hierarchy so a caller has one thing to catch. Splitting local and remote
 * failures into two unrelated types would mean every call site handled both, or — far more
 * likely — forgot one.
 */
sealed class LocalError(
    detail: String,
    cause: Throwable? = null,
) : CapsuleError(null, detail, cause) {
    /** The requested row is not in the local catalog. */
    data class NotFound(
        val what: String,
    ) : LocalError("not found locally: $what")

    /**
     * The operation needs the network and there is none.
     *
     * Distinct from a transport failure: gallery read paths must never attempt network I/O at
     * all, so a read that surfaces this is a bug in the caller rather than a condition to
     * retry.
     */
    data object Offline : LocalError("no usable network path")

    /** The caller's scope was cancelled. */
    data object Cancelled : LocalError("the operation was cancelled")

    /** Local storage refused the read or write. */
    data class Storage(
        val reason: String,
        val underlying: Throwable? = null,
    ) : LocalError("local storage failure: $reason", underlying)

    /**
     * Something no contract here anticipated.
     *
     * Present so an adapter never has to swallow an exception to satisfy a signature —
     * a swallowed failure is worse than an unclassified one.
     */
    data class Unexpected(
        val underlying: Throwable,
    ) : LocalError("unexpected failure: ${underlying.message}", underlying)
}
