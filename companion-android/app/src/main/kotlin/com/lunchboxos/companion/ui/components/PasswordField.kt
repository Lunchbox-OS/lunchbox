package com.lunchboxos.companion.ui.components

import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation

/**
 * A text field for a secret: the Wi-Fi password, the web UI's password.
 *
 * Masking the text is not enough on its own. [PasswordVisualTransformation]
 * only changes what is drawn; the keyboard still sees an ordinary text field,
 * so it shows the typed word in its suggestion strip, autocorrects it into
 * something the network will refuse, and may learn it. [KeyboardType.Password]
 * is what tells the keyboard this is a secret, and autocorrection is turned
 * off as well in case a keyboard ignores that.
 */
@Composable
fun PasswordField(
    value: String,
    onValueChange: (String) -> Unit,
    label: String,
    modifier: Modifier = Modifier,
    isError: Boolean = false,
    supportingText: (@Composable () -> Unit)? = null,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        singleLine = true,
        isError = isError,
        supportingText = supportingText,
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(
            keyboardType = KeyboardType.Password,
            autoCorrectEnabled = false,
        ),
        modifier = modifier,
    )
}
