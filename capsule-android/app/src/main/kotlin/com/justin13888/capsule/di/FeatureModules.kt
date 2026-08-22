package com.justin13888.capsule.di

import org.koin.core.module.Module
import org.koin.dsl.module

/** The library listing, the trash beneath it, and the hidden view beside it. */
val timelineModule: Module =
    module {
        // Registered by `:capsule-android:feature:timeline` when that slice lands.
    }

/** Albums, smart albums and stacks. */
val albumsModule: Module =
    module {
        // Registered by `:capsule-android:feature:albums` when that slice lands.
    }

/** The single-asset viewer and everything it can say about one file. */
val viewerModule: Module =
    module {
        // Registered by `:capsule-android:feature:viewer` when that slice lands.
    }

/** Search and the saved searches it produces. */
val searchModule: Module =
    module {
        // Registered by `:capsule-android:feature:search` when that slice lands.
    }

/** People and pets. */
val peopleModule: Module =
    module {
        // Registered by `:capsule-android:feature:people` when that slice lands.
    }

/** Reading files in from a source the user chose. */
val importerModule: Module =
    module {
        // Registered by `:capsule-android:feature:importer` when that slice lands.
    }

/** Share links out and upload links in. */
val sharingModule: Module =
    module {
        // Registered by `:capsule-android:feature:sharing` when that slice lands.
    }

/** The inbox of files that arrived through an upload link. */
val dropsModule: Module =
    module {
        // Registered by `:capsule-android:feature:drops` when that slice lands.
    }

/** Reconciliation with the server, and the upload queue behind it. */
val syncModule: Module =
    module {
        // Registered by `:capsule-android:feature:sync` when that slice lands.
    }

/** What the library occupies, locally and remotely. */
val storageModule: Module =
    module {
        // Registered by `:capsule-android:feature:storage` when that slice lands.
    }

/** Sessions, devices, passkeys and the local-auth gate. */
val securityModule: Module =
    module {
        // Registered by `:capsule-android:feature:security` when that slice lands.
    }

/** Account key recovery, escrow and backups. */
val recoveryModule: Module =
    module {
        // Registered by `:capsule-android:feature:recovery` when that slice lands.
    }

/** Reports, blocks and appeals. */
val moderationModule: Module =
    module {
        // Registered by `:capsule-android:feature:moderation` when that slice lands.
    }

/** The account's security-relevant event log. */
val auditModule: Module =
    module {
        // Registered by `:capsule-android:feature:audit` when that slice lands.
    }

/** App and account preferences. */
val settingsModule: Module =
    module {
        // Registered by `:capsule-android:feature:settings` when that slice lands.
    }

/** First run, from nothing to a library. */
val onboardingModule: Module =
    module {
        // Registered by `:capsule-android:feature:onboarding` when that slice lands.
    }

/** Adding a device to an account that already exists. */
val enrollmentModule: Module =
    module {
        // Registered by `:capsule-android:feature:enrollment` when that slice lands.
    }

/**
 * Every module the application starts with, shell first.
 *
 * One module per product area, declared up front and empty. Empty on purpose, and worth the
 * declaration anyway: seventeen feature slices are coming and each needs somewhere to register
 * its repositories and view models. Without a module already named and already listed here,
 * the first slice to need one either invents a naming scheme the next sixteen have to guess
 * at, or dumps its bindings into a shared module that then belongs to nobody. The declaration
 * is the contract — a feature slice adds `singleOf(::Thing)` inside its own module and touches
 * nothing else.
 *
 * The modules live in the app module rather than in each feature because Koin modules are
 * graph assembly, and assembly is what the app module is for; a feature that started its own
 * graph would be a feature that could be started twice.
 *
 * Order is not significant to Koin — it resolves lazily — but keeping the shell at the head
 * documents the dependency direction: features may reach for shell singletons, never the
 * reverse.
 */
val capsuleModules: List<Module> =
    listOf(
        shellModule,
        dataModule,
        timelineModule,
        albumsModule,
        viewerModule,
        searchModule,
        peopleModule,
        importerModule,
        sharingModule,
        dropsModule,
        syncModule,
        storageModule,
        securityModule,
        recoveryModule,
        moderationModule,
        auditModule,
        settingsModule,
        onboardingModule,
        enrollmentModule,
    )
