package com.luckymatch.app.update

import android.content.Context
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL

private const val GITHUB_REPO = "ImLuckyWiz/LuckyMatch"
private const val ANDROID_APK_ASSET = "luckymatch-android.apk"
private const val MAX_REDIRECTS = 5

data class ReleaseInfo(
    val tagName: String,
    val version: String,
    val name: String,
    val notes: String,
    val htmlUrl: String,
    val apkUrl: String,
    val apkSize: Long,
)

object AppUpdater {
    fun parseSemver(version: String): Triple<Int, Int, Int>? {
        val stripped = version.trim().removePrefix("v").removePrefix("V")
        val core = stripped.substringBefore('-').substringBefore('+').trim()
        if (core.isEmpty()) return null
        val parts = core.split('.')
        if (parts.size !in 2..3) return null
        val major = parts[0].toIntOrNull() ?: return null
        val minor = parts[1].toIntOrNull() ?: return null
        val patch = if (parts.size == 3) parts[2].toIntOrNull() ?: return null else 0
        return Triple(major, minor, patch)
    }

    fun isNewerVersion(latest: String, current: String): Boolean {
        val l = parseSemver(latest) ?: return false
        val c = parseSemver(current) ?: return false
        if (l.first != c.first) return l.first > c.first
        if (l.second != c.second) return l.second > c.second
        return l.third > c.third
    }

    fun checkLatestRelease(currentVersion: String): ReleaseInfo? {
        val url = URL("https://api.github.com/repos/$GITHUB_REPO/releases/latest")
        val conn = (url.openConnection() as HttpURLConnection).apply {
            requestMethod = "GET"
            connectTimeout = 15_000
            readTimeout = 15_000
            setRequestProperty("Accept", "application/vnd.github+json")
            setRequestProperty("User-Agent", "LuckyMatch-Android/$currentVersion")
        }

        return try {
            val code = conn.responseCode
            if (code !in 200..299) {
                throw IOException("GitHub release check returned HTTP $code")
            }

            val body = conn.inputStream.bufferedReader().use { it.readText() }
            val json = JSONObject(body)
            if (json.optBoolean("draft", false) || json.optBoolean("prerelease", false)) {
                return null
            }

            val tagName = json.optString("tag_name", "").trim()
            if (tagName.isEmpty()) return null
            val version = tagName.removePrefix("v").removePrefix("V")
            if (!isNewerVersion(version, currentVersion)) {
                return null
            }

            val assets = json.optJSONArray("assets") ?: return null
            var chosenUrl: String? = null
            var chosenSize = 0L

            for (i in 0 until assets.length()) {
                val asset = assets.optJSONObject(i) ?: continue
                val assetName = asset.optString("name", "")
                val downloadUrl = asset.optString("browser_download_url", "")
                if (downloadUrl.isBlank()) continue
                if (assetName.equals(ANDROID_APK_ASSET, ignoreCase = true)) {
                    chosenUrl = downloadUrl
                    chosenSize = asset.optLong("size", 0L)
                    break
                }
                if (chosenUrl == null && assetName.endsWith(".apk", ignoreCase = true)) {
                    chosenUrl = downloadUrl
                    chosenSize = asset.optLong("size", 0L)
                }
            }

            val apkUrl = chosenUrl ?: return null
            val releaseName = json.optString("name", "").ifBlank { tagName }
            ReleaseInfo(
                tagName = tagName,
                version = version,
                name = releaseName,
                notes = json.optString("body", ""),
                htmlUrl = json.optString("html_url", ""),
                apkUrl = apkUrl,
                apkSize = chosenSize,
            )
        } finally {
            conn.disconnect()
        }
    }

    suspend fun downloadApk(
        context: Context,
        release: ReleaseInfo,
        onProgress: (Float?) -> Unit,
    ): File {
        val updatesDir = File(context.cacheDir, "updates").apply { mkdirs() }
        updatesDir.listFiles()?.forEach { old ->
            if (!old.isDirectory) old.delete()
        }

        val destFile = File(updatesDir, "luckymatch-${release.version}.apk")
        val tmpFile = File(updatesDir, "luckymatch-${release.version}.apk.tmp")

        val conn = openWithRedirects(release.apkUrl)
        try {
            val code = conn.responseCode
            if (code !in 200..299) {
                throw IOException("APK download returned HTTP $code")
            }

            val headerLen = conn.contentLengthLong
            val total = when {
                headerLen > 0L -> headerLen
                release.apkSize > 0L -> release.apkSize
                else -> -1L
            }

            onProgress(if (total > 0L) 0f else null)

            var downloaded = 0L
            var lastReportedPct = -1

            conn.inputStream.use { input ->
                FileOutputStream(tmpFile).use { output ->
                    val buffer = ByteArray(16 * 1024)
                    while (true) {
                        currentCoroutineContext().ensureActive()
                        val read = input.read(buffer)
                        if (read == -1) break
                        output.write(buffer, 0, read)
                        downloaded += read
                        if (total > 0L) {
                            val frac = (downloaded.toDouble() / total.toDouble()).toFloat().coerceIn(0f, 1f)
                            val pct = (frac * 100f).toInt()
                            if (pct != lastReportedPct) {
                                lastReportedPct = pct
                                onProgress(frac)
                            }
                        }
                    }
                    output.fd.sync()
                }
            }

            if (downloaded <= 0L) {
                throw IOException("Downloaded APK is empty")
            }
            if (release.apkSize > 0L && downloaded != release.apkSize) {
                throw IOException("APK size mismatch: got $downloaded bytes, expected ${release.apkSize}")
            }

            if (destFile.exists()) {
                destFile.delete()
            }
            if (!tmpFile.renameTo(destFile)) {
                throw IOException("Failed to finalize downloaded APK")
            }
            return destFile
        } catch (t: Throwable) {
            tmpFile.delete()
            throw t
        } finally {
            conn.disconnect()
        }
    }

    private fun openWithRedirects(initialUrl: String): HttpURLConnection {
        var currentUrl = initialUrl
        repeat(MAX_REDIRECTS + 1) {
            val conn = (URL(currentUrl).openConnection() as HttpURLConnection).apply {
                instanceFollowRedirects = false
                requestMethod = "GET"
                connectTimeout = 15_000
                readTimeout = 60_000
                setRequestProperty("User-Agent", "LuckyMatch-Android-Updater")
            }
            val code = conn.responseCode
            if (code == HttpURLConnection.HTTP_MOVED_PERM ||
                code == HttpURLConnection.HTTP_MOVED_TEMP ||
                code == HttpURLConnection.HTTP_SEE_OTHER ||
                code == 307 ||
                code == 308
            ) {
                val location = conn.getHeaderField("Location")
                conn.disconnect()
                if (location.isNullOrBlank()) {
                    throw IOException("Redirect without Location header")
                }
                currentUrl = URL(URL(currentUrl), location).toString()
            } else {
                return conn
            }
        }
        throw IOException("Too many redirects while downloading APK")
    }
}
