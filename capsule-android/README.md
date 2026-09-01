# capsule-android

The Android application: a Jetpack Compose app that is intended to run over the shared
`capsule-core-kotlin` library, which links `capsule-core` through its uniffi bindings.

Gradle wires it as `:android` (`settings.gradle.kts`), and it declares
`implementation(projects.core)` so the shared library is already on its compile path.

## Current state

**The app module does not compile.** What is checked in is the Kotlin Multiplatform
starter template with its data, dependency-injection, and view-model layers removed, so
the six surviving files reference types that do not exist anywhere in the repository:

| Missing symbol | Referenced from |
| --- | --- |
| `com.justin13888.capsule.data.MuseumObject` | `screens/ListScreen.kt`, `screens/DetailScreen.kt` |
| `com.justin13888.capsule.di.initKoin` | `CapsuleApp.kt` |
| `ListViewModel` / `DetailViewModel` | `CapsuleApp.kt` (imported), `screens/ListScreen.kt`, `screens/DetailScreen.kt` (used unqualified) |

The navigation graph in `App.kt` is likewise the template's list/detail flow keyed by an
`objectId: Int`, not by a Capsule asset. No screen reads a Capsule library, and nothing in
this module calls `capsule-core-kotlin`.

Treat every file under `src/androidMain/` as scaffolding to be replaced rather than as a
partial implementation to be extended.

`capsule-core-kotlin` — the shared library this app is meant to sit on — does not build
either: its two smoke tests call `FfiWorkspace.create` and
`FfiWorkspace.createWithHardwareSigner` without the `client: FfiClientBuild` argument both
constructors have required since `capsule-core/src/ffi.rs` gained client build identity.
So the Kotlin lane is broken end to end, not just at the app layer.

The client contract this app must satisfy is [Clients](../capsule-docs/src/content/docs/design/clients.md);
the shared-core boundary is [Module Map](../capsule-docs/src/content/docs/design/module-map.md).
Work items are tracked in the repo-root `SLICES.md` (lane F, platform/FFI).

## Build

**Neither build command below currently succeeds** — `:android:compileDebugKotlinAndroid`
and `:core:compileDebugUnitTestKotlin` both fail on the unresolved references above. They
are recorded here as the commands to use once the module is rebuilt, not as instructions
that work today. `.github/workflows/build-android.yml` runs them on every change under
`capsule-android/**` and is red for this reason.

Both need the Android SDK (`ANDROID_HOME` or `local.properties`) and a JDK in the supported
range for the pinned Gradle version — see the toolchain caveats in
[`capsule-core-kotlin/README.md`](../capsule-core-kotlin/README.md), which apply to the
whole Gradle build.

```sh
mise run build-kotlin     # repo-wide Kotlin build
mise run check-kotlin     # ktlint + detekt
```
