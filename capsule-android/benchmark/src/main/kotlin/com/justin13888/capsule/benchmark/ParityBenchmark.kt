package com.justin13888.capsule.benchmark

import androidx.benchmark.macro.ExperimentalMetricApi
import androidx.benchmark.macro.FrameTimingMetric
import androidx.benchmark.macro.MacrobenchmarkScope
import androidx.benchmark.macro.StartupMode
import androidx.benchmark.macro.StartupTimingMetric
import androidx.benchmark.macro.TraceSectionMetric
import androidx.benchmark.macro.junit4.MacrobenchmarkRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.BySelector
import androidx.test.uiautomator.Direction
import androidx.test.uiautomator.Until
import androidx.tracing.trace
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The responsiveness parity benchmark: Capsule and the platform photo app, on the same device
 * and library, measured the same way.
 *
 * Each test is named `<client>_<measure>`, which is the contract `xtask bench-apps import`
 * maps onto the normalized metrics; renaming one without the importer fails the import
 * instead of dropping the number. The measures:
 *
 * - `coldStartToFirstThumbnail` - Capsule reports fully drawn once its first thumbnail has
 *   decoded (`timeToFullDisplayMs`); a platform app that never reports is held to its first
 *   frame (`timeToInitialDisplayMs`), which can only flatter it.
 * - `scroll` - flings the library grid; `FrameTimingMetric`'s overrun gives dropped frames.
 * - `fullResolutionOpen` - tap a thumbnail until the viewer's image exists, timed by this
 *   process's own trace section, so both clients are timed by the same clock.
 * - `inputToFrame` - Capsule only: the app's own `inputToFrame` section around a tap. The
 *   platform app emits no such section, so parity reports it as not compared.
 *
 * Instrumentation arguments (`-e name value`, set by `mise run bench-apps`):
 *
 * - `platformPackage` - the platform photo app; Google Photos by default.
 * - `platformThumbnailRes`, `platformViewerRes` - resource ids of the platform app's grid
 *   thumbnail and viewer image. They differ per app and per release, so they are supplied with
 *   the device rather than guessed; a platform test that needs one is skipped without it.
 *
 * The library itself is device state: the bench device holds the same 100,000 assets in the
 * platform app and in Capsule before the run (see the bench runbook in `mise-tasks/bench-apps`).
 */
@OptIn(ExperimentalMetricApi::class)
@RunWith(AndroidJUnit4::class)
@Suppress("FunctionNaming") // `<client>_<measure>` is the importer's contract.
class ParityBenchmark {
    @get:Rule
    val rule = MacrobenchmarkRule()

    private val arguments = InstrumentationRegistry.getArguments()
    private val platformPackage = arguments.getString("platformPackage") ?: GOOGLE_PHOTOS
    private val platformThumbnail = arguments.getString("platformThumbnailRes")?.let { By.res(it) }
    private val platformViewer = arguments.getString("platformViewerRes")?.let { By.res(it) }

    private val capsuleThumbnail = By.res(GRID_TILE)
    private val capsuleViewer = By.res(VIEWER_IMAGE)

    @Test
    fun capsule_coldStartToFirstThumbnail() = coldStart(CAPSULE, capsuleThumbnail)

    @Test
    fun platformApp_coldStartToFirstThumbnail() = coldStart(platformPackage, platformThumbnail)

    @Test
    fun capsule_scroll() = scroll(CAPSULE, capsuleThumbnail)

    @Test
    fun platformApp_scroll() = scroll(platformPackage, requirePlatform(platformThumbnail))

    @Test
    fun capsule_fullResolutionOpen() = fullResolutionOpen(CAPSULE, capsuleThumbnail, capsuleViewer)

    @Test
    fun platformApp_fullResolutionOpen() =
        fullResolutionOpen(platformPackage, requirePlatform(platformThumbnail), requirePlatform(platformViewer))

    @Test
    fun capsule_inputToFrame() =
        rule.measureRepeated(
            packageName = CAPSULE,
            metrics = listOf(TraceSectionMetric(INPUT_TO_FRAME_SECTION, TraceSectionMetric.Mode.First)),
            iterations = ITERATIONS,
            startupMode = StartupMode.WARM,
            setupBlock = { launchToGrid(capsuleThumbnail) },
        ) {
            device.findObject(capsuleThumbnail).click()
            device.wait(Until.hasObject(capsuleViewer), TIMEOUT_MS)
        }

    private fun requirePlatform(selector: BySelector?): BySelector {
        assumeTrue("platform app selectors are supplied with the bench device", selector != null)
        return checkNotNull(selector)
    }

    private fun coldStart(
        packageName: String,
        thumbnail: BySelector?,
    ) = rule.measureRepeated(
        packageName = packageName,
        metrics = listOf(StartupTimingMetric()),
        iterations = STARTUP_ITERATIONS,
        startupMode = StartupMode.COLD,
        setupBlock = { pressHome() },
    ) {
        startActivityAndWait()
        // Keep the trace open until the thumbnail exists, so a full-display report lands in it.
        thumbnail?.let { device.wait(Until.hasObject(it), TIMEOUT_MS) }
    }

    private fun scroll(
        packageName: String,
        thumbnail: BySelector,
    ) = rule.measureRepeated(
        packageName = packageName,
        metrics = listOf(FrameTimingMetric()),
        iterations = ITERATIONS,
        startupMode = StartupMode.WARM,
        setupBlock = { launchToGrid(thumbnail) },
    ) {
        val grid = device.findObject(By.scrollable(true))
        // Keep the fling off the system gesture areas, which would turn it into navigation.
        grid.setGestureMargin(device.displayWidth / GESTURE_MARGIN_DIVISOR)
        repeat(FLINGS) {
            grid.fling(Direction.DOWN)
        }
    }

    private fun fullResolutionOpen(
        packageName: String,
        thumbnail: BySelector,
        viewer: BySelector,
    ) = rule.measureRepeated(
        packageName = packageName,
        // This process's section, so the platform app is timed exactly as Capsule is.
        metrics = listOf(TraceSectionMetric(FULL_RESOLUTION_OPEN_SECTION, targetPackageOnly = false)),
        iterations = ITERATIONS,
        startupMode = StartupMode.WARM,
        setupBlock = { launchToGrid(thumbnail) },
    ) {
        trace(FULL_RESOLUTION_OPEN_SECTION) {
            device.findObject(thumbnail).click()
            check(device.wait(Until.hasObject(viewer), TIMEOUT_MS)) { "the viewer never showed its image" }
        }
    }

    private fun MacrobenchmarkScope.launchToGrid(thumbnail: BySelector) {
        pressHome()
        startActivityAndWait()
        check(device.wait(Until.hasObject(thumbnail), TIMEOUT_MS)) { "the library never drew a thumbnail" }
    }

    private companion object {
        const val CAPSULE = "com.justin13888.capsule"
        const val GOOGLE_PHOTOS = "com.google.android.apps.photos"

        // Mirrors `com.justin13888.capsule.perf.BenchmarkAnchors`, which this APK cannot import.
        const val GRID_TILE = "grid.tile"
        const val VIEWER_IMAGE = "viewer.image"
        const val INPUT_TO_FRAME_SECTION = "inputToFrame"

        const val FULL_RESOLUTION_OPEN_SECTION = "fullResolutionOpen"
        const val STARTUP_ITERATIONS = 10
        const val ITERATIONS = 5
        const val FLINGS = 3
        const val GESTURE_MARGIN_DIVISOR = 5
        const val TIMEOUT_MS = 10_000L
    }
}
