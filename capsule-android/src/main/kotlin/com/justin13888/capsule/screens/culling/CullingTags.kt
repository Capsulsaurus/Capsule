package com.justin13888.capsule.screens.culling

/** Test tags of the culling surface's elements, so a UI test finds them without reading their text. */
object CullingTags {
    const val ASSET = "culling.asset"
    const val SWEEP = "culling.sweep"
    const val MORE = "culling.more"

    fun verb(verb: CullVerb): String = "culling.verb.${verb.name.lowercase()}"
}
