package com.justin13888.capsule.data

/**
 * One item shown by the scaffold list/detail screens.
 *
 * This is the starter template's record, kept only so the scaffold screens compile. It is
 * not a Capsule asset and is replaced when the screens move onto `capsule-core-kotlin`.
 */
data class MuseumObject(
    val objectID: Int,
    val title: String,
    val artistDisplayName: String,
    val medium: String,
    val dimensions: String,
    val objectDate: String,
    val primaryImageSmall: String,
    val repository: String,
    val department: String,
    val creditLine: String,
)
