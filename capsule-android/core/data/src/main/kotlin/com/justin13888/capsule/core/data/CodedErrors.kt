package com.justin13888.capsule.core.data

/**
 * Every [CapsuleError] variant that carries a catalogue code.
 *
 * Maintained beside the families themselves so a test can assert the two directions that
 * matter: no variant invents a code the catalogue does not hold, and no catalogue code is
 * left without a variant. Without the list there would be no way to check the second — a
 * code added to `locales/` and never wired up here would simply never be recognised, and the
 * first client to receive it would render the raw key at the user.
 *
 * [LocalError] is deliberately absent: it never crosses the wire and so has no code to hold
 * to anything.
 */
val ALL_CODED_ERRORS: List<CapsuleError> =
    AlbumError.ALL +
        AuthError.ALL +
        BlobError.ALL +
        DirectoryError.ALL +
        DropError.ALL +
        EnrollmentError.ALL +
        EscrowError.ALL +
        FederationError.ALL +
        ModerationError.ALL +
        ProtocolError.ALL +
        QuotaError.ALL +
        ShareError.ALL +
        StorageError.ALL +
        SyncError.ALL +
        UploadError.ALL
