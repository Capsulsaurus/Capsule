plugins {
    id("capsule.android.library")
}

// Nothing but generated string resources. It is a library module rather than part of
// the app because a feature module cannot see the application's R class, and every
// feature needs these keys.
android {
    namespace = "com.justin13888.capsule.core.i18n"
}
