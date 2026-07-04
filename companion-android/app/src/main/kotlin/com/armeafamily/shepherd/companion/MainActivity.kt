package com.armeafamily.shepherd.companion

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import com.armeafamily.shepherd.companion.ui.App
import com.armeafamily.shepherd.companion.ui.theme.ShepherdTheme

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            ShepherdTheme {
                App()
            }
        }
    }
}
