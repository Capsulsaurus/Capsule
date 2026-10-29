package com.justin13888.capsule

import android.app.Application
import com.justin13888.capsule.alerts.AlertsGraph
import com.justin13888.capsule.di.initKoin
import com.justin13888.capsule.screens.DetailViewModel
import com.justin13888.capsule.screens.ListViewModel
import com.justin13888.capsule.workspace.WorkspaceGraph
import org.koin.dsl.module

class CapsuleApp : Application() {
    /** The local-alert loop. Built on first use, so a launch that never needs it pays nothing. */
    val alerts: AlertsGraph by lazy { AlertsGraph(this) }

    /**
     * The library and the server session the screens run on. Built on first use and held
     * for the process, so a recreated activity finds the library already open.
     */
    val workspace: WorkspaceGraph by lazy { WorkspaceGraph(this) }

    override fun onCreate() {
        super.onCreate()
        initKoin(
            listOf(
                module {
                    factory { ListViewModel(get()) }
                    factory { DetailViewModel(get()) }
                },
            ),
        )
    }
}
