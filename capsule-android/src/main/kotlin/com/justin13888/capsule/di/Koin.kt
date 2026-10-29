package com.justin13888.capsule.di

import com.justin13888.capsule.data.EmptyMuseumRepository
import com.justin13888.capsule.data.MuseumRepository
import org.koin.core.context.startKoin
import org.koin.core.module.Module
import org.koin.dsl.module

/** Bindings for the data layer the screens depend on. */
val dataModule =
    module {
        single<MuseumRepository> { EmptyMuseumRepository() }
    }

/** Starts Koin with [dataModule] followed by [extraModules]. */
fun initKoin(extraModules: List<Module> = emptyList()) {
    startKoin {
        modules(listOf(dataModule) + extraModules)
    }
}
