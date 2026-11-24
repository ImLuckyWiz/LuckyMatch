package com.luckymatch.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.luckymatch.app.data.Credentials

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsSheet(
    initial: Credentials,
    onSave: (Credentials) -> Unit,
    onDismiss: () -> Unit,
) {
    var spotifyId by remember(initial) { mutableStateOf(initial.spotifyClientId) }
    var spotifySecret by remember(initial) { mutableStateOf(initial.spotifyClientSecret) }
    var spicyKey by remember(initial) { mutableStateOf(initial.spicyLyricsApiKey) }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        containerColor = Color(0xFF141824),
        contentColor = Color.White,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 20.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                text = "Lyrics & Metadata Keys",
                fontSize = 18.sp,
                fontWeight = FontWeight.Bold,
                color = Color.White,
            )
            Text(
                text = "Optional: add Spotify Client Credentials + Spicy Lyrics API key for ISRC→Spotify ID resolution and TTML syllable lyrics. Shazam, NetEase, and LRCLIB work without keys.",
                fontSize = 12.sp,
                color = Color.White.copy(alpha = 0.65f),
            )

            val fieldColors = OutlinedTextFieldDefaults.colors(
                focusedTextColor = Color.White,
                unfocusedTextColor = Color.White,
                cursorColor = Color(0xFF38BDF8),
                focusedBorderColor = Color(0xFF38BDF8),
                unfocusedBorderColor = Color.White.copy(alpha = 0.30f),
                focusedLabelColor = Color(0xFF38BDF8),
                unfocusedLabelColor = Color.White.copy(alpha = 0.65f),
            )

            OutlinedTextField(
                value = spotifyId,
                onValueChange = { spotifyId = it },
                label = { Text("Spotify Client ID") },
                singleLine = true,
                colors = fieldColors,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = spotifySecret,
                onValueChange = { spotifySecret = it },
                label = { Text("Spotify Client Secret") },
                singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                colors = fieldColors,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = spicyKey,
                onValueChange = { spicyKey = it },
                label = { Text("Spicy Lyrics API Key") },
                singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                colors = fieldColors,
                modifier = Modifier.fillMaxWidth(),
            )

            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.End,
            ) {
                TextButton(onClick = onDismiss) {
                    Text("Cancel")
                }
                Button(
                    onClick = {
                        onSave(
                            Credentials(
                                spotifyClientId = spotifyId,
                                spotifyClientSecret = spotifySecret,
                                spicyLyricsApiKey = spicyKey,
                            )
                        )
                        onDismiss()
                    }
                ) {
                    Text("Save")
                }
            }
            Spacer(modifier = Modifier.height(16.dp))
        }
    }
}
