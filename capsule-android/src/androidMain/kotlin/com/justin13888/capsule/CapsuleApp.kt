package com.justin13888.capsule

import android.app.Application
import com.justin13888.capsule.di.initKoin
import com.justin13888.capsule.screens.DetailViewModel
import com.justin13888.capsule.screens.ListViewModel
import org.koin.dsl.module

class CapsuleApp : Application() {
    override fun onCreate() {
        super.onCreate()
        initKoin(
            listOf(
                module {
                    factory { ListViewModel(get()) }
                    factory { DetailViewModel(get()) }
                }
            )
        )
    }
}
