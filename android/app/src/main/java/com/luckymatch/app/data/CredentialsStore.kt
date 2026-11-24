package com.luckymatch.app.data

import android.content.Context
import uniffi.luckymatch_ffi.FfiConfig
import java.io.File

data class Credentials(
    val spotifyClientId: String = "",
    val spotifyClientSecret: String = "",
    val spicyLyricsApiKey: String = "",
)

class CredentialsStore(private val context: Context) {
    private val prefs = context.getSharedPreferences("luckymatch_creds", Context.MODE_PRIVATE)

    fun load(): Credentials {
        return Credentials(
            spotifyClientId = prefs.getString(KEY_SPOTIFY_ID, "").orEmpty(),
            spotifyClientSecret = prefs.getString(KEY_SPOTIFY_SECRET, "").orEmpty(),
            spicyLyricsApiKey = prefs.getString(KEY_SPICY_KEY, "").orEmpty(),
        )
    }

    fun save(creds: Credentials) {
        prefs.edit()
            .putString(KEY_SPOTIFY_ID, creds.spotifyClientId.trim())
            .putString(KEY_SPOTIFY_SECRET, creds.spotifyClientSecret.trim())
            .putString(KEY_SPICY_KEY, creds.spicyLyricsApiKey.trim())
            .apply()
    }

    fun buildFfiConfig(): FfiConfig {
        val creds = load()
        val cacheDir = File(context.cacheDir, "lyrics").apply { mkdirs() }
        val customDir = File(context.filesDir, "lyrics_custom").apply { mkdirs() }

        return FfiConfig(
            spotifyClientId = creds.spotifyClientId.trim().ifEmpty { null },
            spotifyClientSecret = creds.spotifyClientSecret.trim().ifEmpty { null },
            spicyLyricsApiKey = creds.spicyLyricsApiKey.trim().ifEmpty { null },
            cacheDir = cacheDir.absolutePath,
            customLyricsDir = customDir.absolutePath,
        )
    }

    companion object {
        private const val KEY_SPOTIFY_ID = "spotify_client_id"
        private const val KEY_SPOTIFY_SECRET = "spotify_client_secret"
        private const val KEY_SPICY_KEY = "spicy_lyrics_api_key"
    }
}
