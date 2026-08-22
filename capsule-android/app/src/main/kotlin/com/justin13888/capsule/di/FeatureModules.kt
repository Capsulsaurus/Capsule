package com.justin13888.capsule.di

import com.justin13888.capsule.feature.albums.albumsFeatureModule
import com.justin13888.capsule.feature.audit.auditFeatureModule
import com.justin13888.capsule.feature.drops.dropsFeatureModule
import com.justin13888.capsule.feature.enrollment.enrollmentFeatureModule
import com.justin13888.capsule.feature.importer.importerFeatureModule
import com.justin13888.capsule.feature.moderation.moderationFeatureModule
import com.justin13888.capsule.feature.onboarding.onboardingFeatureModule
import com.justin13888.capsule.feature.people.peopleFeatureModule
import com.justin13888.capsule.feature.recovery.recoveryFeatureModule
import com.justin13888.capsule.feature.search.searchFeatureModule
import com.justin13888.capsule.feature.security.securityFeatureModule
import com.justin13888.capsule.feature.settings.settingsFeatureModule
import com.justin13888.capsule.feature.sharing.sharingFeatureModule
import com.justin13888.capsule.feature.storage.storageFeatureModule
import com.justin13888.capsule.feature.sync.syncFeatureModule
import com.justin13888.capsule.feature.timeline.timelineFeatureModule
import com.justin13888.capsule.feature.viewer.di.viewerFeatureModule
import org.koin.core.module.Module

// ── One alias per product area ───────────────────────────────────────────────
// Named for the area rather than re-exported directly, so this file reads as the
// app's inventory of what it is made of and a feature's own module name stays an
// implementation detail of that feature.

/** The library listing, the culling pass, and the trash beneath them. */
val timelineModule: Module = timelineFeatureModule

/** Albums, smart albums, stacks, places and the gated views. */
val albumsModule: Module = albumsFeatureModule

/** The single-asset viewer and everything it can say about one file. */
val viewerModule: Module = viewerFeatureModule

/** Search and the facets it offers. */
val searchModule: Module = searchFeatureModule

/** People, face groupings and the AI-derived albums. */
val peopleModule: Module = peopleFeatureModule

/** Bringing assets in, from a folder, an archive or a camera. */
val importerModule: Module = importerFeatureModule

/** Share links out. */
val sharingModule: Module = sharingFeatureModule

/** Upload links in, and the inbox they deposit into. */
val dropsModule: Module = dropsFeatureModule

/** Sync state, transfer queues and the quarantine surface. */
val syncModule: Module = syncFeatureModule

/** Quota, local footprint and what is kept offline. */
val storageModule: Module = storageFeatureModule

/** Sessions, devices and credentials. */
val securityModule: Module = securityFeatureModule

/** Recovery secrets, Shamir shares, backup and restore. */
val recoveryModule: Module = recoveryFeatureModule

/** Reports, blocks, suspension and appeals. */
val moderationModule: Module = moderationFeatureModule

/** Preferences, across the library and this device. */
val settingsModule: Module = settingsFeatureModule

/** The library's audit log. */
val auditModule: Module = auditFeatureModule

/** First run: choosing a mode, connecting a server, signing in. */
val onboardingModule: Module = onboardingFeatureModule

/** Bringing this device, or another one, into the account. */
val enrollmentModule: Module = enrollmentFeatureModule

/**
 * Every module the application graph is assembled from.
 *
 * A feature owns its own bindings and publishes them as one `Module`; the app module does
 * nothing but collect them. That keeps the seam one-directional — a feature never reaches
 * into the app, and adding a feature is an import and a list entry rather than an edit to
 * anything a feature can see.
 *
 * The graph is started eagerly in `CapsuleApplication`, because Koin resolves lazily by
 * default and a missing binding would otherwise surface at whichever screen first needed
 * it — which, for most of this app, would be long after the mistake was made.
 */
val capsuleModules: List<Module> =
    listOf(
        shellModule,
        dataModule,
        onboardingModule,
        enrollmentModule,
        timelineModule,
        importerModule,
        albumsModule,
        viewerModule,
        searchModule,
        peopleModule,
        sharingModule,
        dropsModule,
        syncModule,
        storageModule,
        securityModule,
        recoveryModule,
        moderationModule,
        settingsModule,
        auditModule,
    )
