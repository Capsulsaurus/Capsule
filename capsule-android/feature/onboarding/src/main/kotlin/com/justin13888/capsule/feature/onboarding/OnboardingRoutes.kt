package com.justin13888.capsule.feature.onboarding

import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import org.koin.androidx.compose.koinViewModel

/**
 * The welcome tour and the mode choice, wired to its view model.
 *
 * The two callbacks are the only thing the app module has to supply, and they are peers on
 * purpose: choosing a local library is a destination, not a dismissal.
 *
 * @param onServerChosen the user wants to name a server.
 * @param onLocalOnlyChosen the user wants a library on this device and nothing else.
 */
@Composable
fun OnboardingWelcomeRoute(
    onServerChosen: () -> Unit,
    onLocalOnlyChosen: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: WelcomeViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    WelcomeScreen(
        state = state,
        onPaneChanged = viewModel::showPane,
        onAdvance = viewModel::advance,
        onSkipTour = viewModel::skipTour,
        onChooseLocalOnly = {
            viewModel.choose(OnboardingMode.LocalOnly)
            onLocalOnlyChosen()
        },
        onConnectServer = {
            viewModel.choose(OnboardingMode.ServerBacked)
            onServerChosen()
        },
        modifier = modifier,
    )
}

/**
 * Naming a server and choosing a way in.
 *
 * @param onServerAccepted the server answered and an auth path was picked.
 * @param onLocalOnlyChosen the user gave up on the server and took the local library instead.
 */
@Composable
fun OnboardingServerRoute(
    onServerAccepted: () -> Unit,
    onLocalOnlyChosen: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: ServerConnectViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    ServerConnectScreen(
        state = state,
        onAddressChanged = viewModel::addressChanged,
        onConnect = viewModel::connect,
        onSelectMethod = viewModel::selectMethod,
        onContinue = {
            viewModel.commit()
            onServerAccepted()
        },
        onUseLocalOnly = {
            viewModel.useLocalOnly()
            onLocalOnlyChosen()
        },
        modifier = modifier,
    )
}

/**
 * Establishing a session.
 *
 * @param onSignedIn a session now exists; the account ceremony comes next.
 */
@Composable
fun OnboardingSignInRoute(
    onSignedIn: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: SignInViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(state.isComplete) {
        if (state.isComplete) onSignedIn()
    }
    SignInScreen(
        state = state,
        actions =
            SignInActions(
                onEmailChanged = viewModel::emailChanged,
                onPasswordChanged = viewModel::passwordChanged,
                onTotpCodeChanged = viewModel::totpCodeChanged,
                onSubmitCredentials = viewModel::submitCredentials,
                onSubmitTotp = viewModel::submitTotp,
                onAuthenticateWithPasskey = viewModel::authenticateWithPasskey,
                onCompleteOidc = viewModel::completeOidc,
                onToggleRegistering = viewModel::toggleRegistering,
            ),
        modifier = modifier,
    )
}

/**
 * The media-permission primer and the hand-off into the library.
 *
 * The launcher's own result map is deliberately thrown away in favour of re-asking the
 * platform: on Android 14 a user who picks a handful of photos produces a map full of
 * denials while the app has, in fact, gained partial access.
 *
 * @param onFinish onboarding is over; show the library.
 */
@Composable
fun OnboardingCompleteRoute(
    onFinish: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: OnboardingCompleteViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val launcher =
        rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
            viewModel.promptClosed()
        }
    OnboardingCompleteScreen(
        state = state,
        onRequestAccess = { launcher.launch(viewModel.requestedPermissions().toTypedArray()) },
        onOpenAppSettings = {
            val intent =
                Intent(
                    Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                    Uri.fromParts("package", context.packageName, null),
                ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            context.startActivity(intent)
        },
        onFinish = onFinish,
        modifier = modifier,
    )
}
