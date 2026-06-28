package com.armeafamily.shepherd.companion.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

private val ShepherdGreen = Color(0xFF1B5E20)
private val ShepherdGreenLight = Color(0xFF4C8C4A)
private val ShepherdAccent = Color(0xFF00796B)

private val LightColors = lightColorScheme(
    primary = ShepherdGreen,
    secondary = ShepherdAccent,
    tertiary = ShepherdGreenLight,
)

private val DarkColors = darkColorScheme(
    primary = ShepherdGreenLight,
    secondary = ShepherdAccent,
    tertiary = ShepherdGreen,
)

@Composable
fun ShepherdTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    // Material You dynamic color where available (Android 12+), falling
    // back to the shepherd green palette.
    val colorScheme = when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> {
            val context = LocalContext.current
            if (darkTheme) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        }
        darkTheme -> DarkColors
        else -> LightColors
    }
    MaterialTheme(colorScheme = colorScheme, content = content)
}
