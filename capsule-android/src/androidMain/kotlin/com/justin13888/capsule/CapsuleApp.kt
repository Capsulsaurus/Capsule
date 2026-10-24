package com.justin13888.capsule

import android.app.Application
import com.justin13888.capsule.alerts.AlertsGraph
import com.justin13888.capsule.di.initKoin
import com.justin13888.capsule.screens.DetailViewModel
import com.justin13888.capsule.screens.ListViewModel
import org.koin.dsl.module

class CapsuleApp : Application() {
    /** The local-alert loop. Built on first use, so a launch that never needs it pays nothing. */
    val alerts: AlertsGraph by lazy { AlertsGraph(this) }

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
