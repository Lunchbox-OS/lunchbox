package com.lunchbox_os.companion.ui.theme

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

private val LunchboxGreen = Color(0xFF1B5E20)
private val LunchboxGreenLight = Color(0xFF4C8C4A)
private val LunchboxAccent = Color(0xFF00796B)

private val LightColors = lightColorScheme(
    primary = LunchboxGreen,
    secondary = LunchboxAccent,
    tertiary = LunchboxGreenLight,
)

private val DarkColors = darkColorScheme(
    primary = LunchboxGreenLight,
    secondary = LunchboxAccent,
    tertiary = LunchboxGreen,
)

@Composable
fun LunchboxTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    // Material You dynamic color where available (Android 12+), falling
    // back to the lunchbox green palette.
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
