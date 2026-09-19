package com.lunchbox_os.companion

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import com.lunchbox_os.companion.ui.App
import com.lunchbox_os.companion.ui.theme.LunchboxTheme

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            LunchboxTheme {
                App()
            }
        }
    }
}
