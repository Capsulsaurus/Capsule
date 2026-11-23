package com.justin13888.capsule.feature.security

import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.dsl.module

/**
 * This slice's bindings, declared beside the code they construct.
 *
 * Exported rather than registered: the app module owns graph assembly, so it decides when this
 * list joins the graph. Declaring it here keeps the constructor arguments and the binding that
 * satisfies them in one file, which is the only place a mismatch between them can be noticed
 * by reading.
 *
 * The two stand-ins are bound to their interfaces, not to their classes. That is the whole
 * point of declaring the ports in this module: when a real credential surface and a real key
 * store land, these two lines change and nothing above them does.
 */
val securityFeatureModule: Module =
    module {
        single<CredentialRepository> { InMemoryCredentialRepository() }
        single<MasterKeyProofSource> { StubMasterKeyProofSource() }
        viewModel { SessionsViewModel(sessions = get(), proofSource = get()) }
        viewModel { CredentialsViewModel(credentials = get()) }
    }
