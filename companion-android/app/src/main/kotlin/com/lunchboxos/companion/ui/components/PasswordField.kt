package com.lunchboxos.companion.ui.components

import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Visibility
import androidx.compose.material.icons.filled.VisibilityOff
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation

/**
 * A text field for a secret: the Wi-Fi password, the web UI's password.
 *
 * Masking the text is not enough on its own. [PasswordVisualTransformation]
 * only changes what is drawn; the keyboard still sees an ordinary text field,
 * so it shows the typed word in its suggestion strip, autocorrects it into
 * something the network will refuse, and may learn it. [KeyboardType.Password]
 * is what tells the keyboard this is a secret, and autocorrection is turned
 * off as well in case a keyboard ignores that.
 *
 * The eye shows the text, because a Wi-Fi password is typically copied off a
 * sticker on the router, and one mistyped character is otherwise invisible
 * until the join fails. Showing it changes only the drawing: the keyboard
 * type stays [KeyboardType.Password], so revealing does not bring the
 * suggestions back.
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
    var revealed by rememberSaveable { mutableStateOf(false) }
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        singleLine = true,
        isError = isError,
        supportingText = supportingText,
        visualTransformation = if (revealed) {
            VisualTransformation.None
        } else {
            PasswordVisualTransformation()
        },
        trailingIcon = {
            IconButton(onClick = { revealed = !revealed }) {
                Icon(
                    if (revealed) Icons.Filled.VisibilityOff else Icons.Filled.Visibility,
                    contentDescription = if (revealed) "Hide password" else "Show password",
                )
            }
        },
        keyboardOptions = KeyboardOptions(
            keyboardType = KeyboardType.Password,
            autoCorrectEnabled = false,
        ),
        modifier = modifier,
    )
}
