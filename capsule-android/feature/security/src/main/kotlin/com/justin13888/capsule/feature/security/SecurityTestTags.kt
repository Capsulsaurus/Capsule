package com.justin13888.capsule.feature.security

/** Test handle for the security hub. */
const val SECURITY_HUB_TAG: String = "security.hub"

/** Test handle for the sessions-and-devices screen. */
const val SECURITY_SESSIONS_TAG: String = "security.sessions"

/** Test handle for the scrolling list inside the sessions screen. */
const val SECURITY_LIST_TAG: String = "security.sessions.list"

/** Test handle for the passkeys screen. */
const val SECURITY_PASSKEYS_TAG: String = "security.passkeys"

/** Test handle for the authenticator section on the hub. */
const val SECURITY_TOTP_TAG: String = "security.totp"

/** Test handle for the six-digit code field. */
const val SECURITY_CODE_FIELD_TAG: String = "security.code"

/** Test handle for the global-revoke confirmation. */
const val SECURITY_REVOKE_ALL_TAG: String = "security.revoke-all"

/** Test handle for the support report sheet. */
const val SECURITY_SUPPORT_TAG: String = "security.support"

/** Test handle for the standing note that grouping is advisory only. */
const val SECURITY_ADVISORY_TAG: String = "security.advisory"

/** Test handle for the loading state. */
const val SECURITY_LOADING_TAG: String = "security.loading"

/** Test handle for the error state. */
const val SECURITY_ERROR_TAG: String = "security.error"

/** Test handle for the empty state of a list. */
const val SECURITY_EMPTY_TAG: String = "security.empty"

/** Test handle for one cohort group: `security.cohort:<hash or "none">`. */
fun securityCohortTag(cohortHash: String?): String = "security.cohort:" + (cohortHash ?: "none")

/** Test handle for one session row: `security.session:<id>`. */
fun securitySessionTag(sessionId: String): String = "security.session:$sessionId"

/** Test handle for one device row: `security.device:<id>`. */
fun securityDeviceTag(deviceId: String): String = "security.device:$deviceId"

/** Test handle for one passkey row: `security.passkey:<id>`. */
fun securityPasskeyTag(passkeyId: String): String = "security.passkey:$passkeyId"
