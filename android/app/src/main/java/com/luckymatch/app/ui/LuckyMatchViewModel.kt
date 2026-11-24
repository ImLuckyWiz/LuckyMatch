package com.luckymatch.app.ui

import android.app.Application
import android.os.SystemClock
import androidx.compose.ui.graphics.Color
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.luckymatch.app.audio.AudioCaptureManager
import com.luckymatch.app.audio.CaptureSource
import com.luckymatch.app.audio.MediaProjectionService
import com.luckymatch.app.audio.ProgressiveOutcome
import com.luckymatch.app.data.Credentials
import com.luckymatch.app.data.CredentialsStore
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import uniffi.luckymatch_ffi.FfiException
import uniffi.luckymatch_ffi.FfiLyricsResult
import uniffi.luckymatch_ffi.FfiRecognizedTrack
import uniffi.luckymatch_ffi.FfiSyncSnapshot
import uniffi.luckymatch_ffi.LuckyMatchClient
import kotlin.math.abs

sealed interface RecognitionStatus {
    data object Idle : RecognitionStatus
    data class Capturing(val source: CaptureSource, val startedAtMs: Long, val durationSec: Float?) : RecognitionStatus
    data object Identifying : RecognitionStatus
    data object SilentAudio : RecognitionStatus
    data object NoMatch : RecognitionStatus
    data class Error(val message: String) : RecognitionStatus
}

data class LuckyMatchUiState(
    val status: RecognitionStatus = RecognitionStatus.Idle,
    val source: CaptureSource = CaptureSource.MIC,
    val autoSync: Boolean = false,
    val audioLevel: Float = 0f,
    val track: FfiRecognizedTrack? = null,
    val lyrics: FfiLyricsResult? = null,
    val accentColor: Color = Color(0xFF38BDF8),
    val manualOffsetSec: Double = 0.0,
    val isPaused: Boolean = false,
    val currentPosSec: Double = 0.0,
    val syncSnapshot: FfiSyncSnapshot? = null,
    val credentials: Credentials = Credentials(),
)

private data class AnchorClock(
    var matchOffsetSec: Double,
    var timeskew: Double,
    var captureAnchorMs: Long,
    var pausedPosSec: Double? = null,
) {
    fun positionAt(nowMs: Long, manualOffsetSec: Double): Double {
        val base = pausedPosSec ?: run {
            val elapsedSec = ((nowMs - captureAnchorMs).coerceAtLeast(0L)) / 1000.0
            matchOffsetSec + elapsedSec * (1.0 + timeskew)
        }
        return (base + manualOffsetSec).coerceAtLeast(0.0)
    }

    fun pause(nowMs: Long) {
        if (pausedPosSec == null) {
            val elapsedSec = ((nowMs - captureAnchorMs).coerceAtLeast(0L)) / 1000.0
            pausedPosSec = matchOffsetSec + elapsedSec * (1.0 + timeskew)
        }
    }

    fun resume(nowMs: Long) {
        val paused = pausedPosSec ?: return
        matchOffsetSec = paused
        captureAnchorMs = nowMs
        pausedPosSec = null
    }
}

class LuckyMatchViewModel(application: Application) : AndroidViewModel(application) {
    private val store = CredentialsStore(application.applicationContext)
    private var client: LuckyMatchClient = LuckyMatchClient(store.buildFfiConfig())

    private val _uiState = MutableStateFlow(
        LuckyMatchUiState(credentials = store.load())
    )
    val uiState: StateFlow<LuckyMatchUiState> = _uiState.asStateFlow()

    private var clock: AnchorClock? = null
    private var identifyJob: Job? = null
    private var identifyIsManual: Boolean = false
    private var autoSyncJob: Job? = null
    private var pendingSeekCandidate: Pair<Double, Long>? = null

    fun saveCredentials(creds: Credentials) {
        store.save(creds)
        client.close()
        client = LuckyMatchClient(store.buildFfiConfig())
        _uiState.update { it.copy(credentials = creds) }
    }

    fun setSource(source: CaptureSource) {
        _uiState.update { it.copy(source = source) }
    }

    fun toggleAutoSync(enabled: Boolean) {
        _uiState.update { it.copy(autoSync = enabled) }
        scheduleNextAutoSync(delayMs = AUTO_REVERIFY_MS)
    }

    private fun scheduleNextAutoSync(delayMs: Long) {
        autoSyncJob?.cancel()
        if (!_uiState.value.autoSync || _uiState.value.track == null) return
        autoSyncJob = viewModelScope.launch {
            delay(delayMs)
            if (isActive && _uiState.value.autoSync && identifyJob?.isActive != true) {
                identifyNow(isManual = false)
            }
        }
    }

    fun nudgeOffset(deltaSec: Double) {
        _uiState.update { state ->
            val nextOffset = state.manualOffsetSec + deltaSec
            val pos = clock?.positionAt(SystemClock.elapsedRealtime(), nextOffset) ?: state.currentPosSec
            val snap = state.lyrics?.document?.let { client.evaluateSync(it, pos, HOLD_GAP_SEC) }
            state.copy(
                manualOffsetSec = nextOffset,
                currentPosSec = pos,
                syncSnapshot = snap,
            )
        }
    }

    fun resetOffset() {
        _uiState.update { state ->
            val pos = clock?.positionAt(SystemClock.elapsedRealtime(), 0.0) ?: state.currentPosSec
            val snap = state.lyrics?.document?.let { client.evaluateSync(it, pos, HOLD_GAP_SEC) }
            state.copy(
                manualOffsetSec = 0.0,
                currentPosSec = pos,
                syncSnapshot = snap,
            )
        }
    }

    fun togglePause() {
        val nowMs = SystemClock.elapsedRealtime()
        val c = clock ?: return
        val currentlyPaused = _uiState.value.isPaused
        if (currentlyPaused) {
            c.resume(nowMs)
        } else {
            c.pause(nowMs)
        }
        val pos = c.positionAt(nowMs, _uiState.value.manualOffsetSec)
        val snap = _uiState.value.lyrics?.document?.let { client.evaluateSync(it, pos, HOLD_GAP_SEC) }
        _uiState.update {
            it.copy(
                isPaused = !currentlyPaused,
                currentPosSec = pos,
                syncSnapshot = snap,
            )
        }
    }

    fun updateAccentColor(color: Color) {
        _uiState.update { it.copy(accentColor = color) }
    }

    fun tickFrame() {
        val state = _uiState.value
        val c = clock ?: return
        if (state.isPaused) return
        val pos = c.positionAt(SystemClock.elapsedRealtime(), state.manualOffsetSec)
        val snap = state.lyrics?.document?.let { client.evaluateSync(it, pos, HOLD_GAP_SEC) }
        _uiState.update {
            it.copy(
                currentPosSec = pos,
                syncSnapshot = snap,
            )
        }
    }

    fun cancelIdentify() {
        identifyJob?.cancel()
        identifyJob = null
        identifyIsManual = false
        _uiState.update {
            it.copy(
                audioLevel = 0f,
                status = RecognitionStatus.Idle,
            )
        }
        scheduleNextAutoSync(AUTO_REVERIFY_MS)
    }

    fun identifyNow(isManual: Boolean = true) {
        if (identifyJob?.isActive == true) {
            if (isManual && !identifyIsManual) {
                identifyJob?.cancel()
            } else {
                return
            }
        }

        val source = _uiState.value.source

        if (source == CaptureSource.INTERNAL_AUDIO && MediaProjectionService.activeProjection == null) {
            _uiState.update {
                it.copy(status = RecognitionStatus.Error("Grant internal audio capture permission first"))
            }
            return
        }

        identifyIsManual = isManual
        identifyJob = viewModelScope.launch(Dispatchers.IO) {
            if (isManual) {
                runProgressiveIdentify(source)
            } else {
                runAutoSyncIdentify(source)
            }
        }
    }

    private suspend fun runProgressiveIdentify(source: CaptureSource) {
        val startedMs = SystemClock.elapsedRealtime()
        _uiState.update {
            it.copy(
                status = RecognitionStatus.Capturing(source, startedMs, durationSec = null),
                audioLevel = 0f,
            )
        }

        var matchedPair: Pair<FfiRecognizedTrack, Long>? = null
        val outcome = try {
            AudioCaptureManager.captureProgressive(
                source = source,
                silenceRmsThreshold = SILENCE_RMS_THRESHOLD,
                onAudioLevel = { level ->
                    _uiState.update { it.copy(audioLevel = level) }
                },
                onTryMatch = { snapshot ->
                    _uiState.update { it.copy(status = RecognitionStatus.Identifying) }
                    val match = client.recognizePcmI16(
                        samples = snapshot.samples,
                        sampleRate = snapshot.sampleRate.toUInt(),
                        channels = snapshot.channels.toUShort(),
                    )
                    if (match != null) {
                        matchedPair = match to snapshot.captureStartElapsedMs
                        true
                    } else {
                        _uiState.update {
                            it.copy(status = RecognitionStatus.Capturing(source, startedMs, durationSec = null))
                        }
                        false
                    }
                },
            )
        } catch (e: CancellationException) {
            throw e
        } catch (e: FfiException) {
            _uiState.update {
                it.copy(
                    audioLevel = 0f,
                    status = RecognitionStatus.Error(e.message ?: "Recognition failed"),
                )
            }
            scheduleNextAutoSync(15_000L)
            return
        } catch (t: Throwable) {
            _uiState.update {
                it.copy(
                    audioLevel = 0f,
                    status = RecognitionStatus.Error(t.message ?: "Recognition failed"),
                )
            }
            scheduleNextAutoSync(15_000L)
            return
        }

        when (outcome) {
            ProgressiveOutcome.MATCHED -> {
                val (match, anchorMs) = matchedPair ?: return
                handleMatchedTrack(match, anchorMs, isManual = true)
            }
            ProgressiveOutcome.SILENT -> {
                _uiState.update { it.copy(audioLevel = 0f, status = RecognitionStatus.SilentAudio) }
                scheduleNextAutoSync(12_000L)
            }
            ProgressiveOutcome.NO_MATCH -> {
                _uiState.update { it.copy(audioLevel = 0f, status = RecognitionStatus.NoMatch) }
                scheduleNextAutoSync(12_000L)
            }
            ProgressiveOutcome.CANCELLED -> {
                _uiState.update { it.copy(audioLevel = 0f, status = RecognitionStatus.Idle) }
                scheduleNextAutoSync(AUTO_REVERIFY_MS)
            }
        }
    }

    private suspend fun runAutoSyncIdentify(source: CaptureSource) {
        val startedMs = SystemClock.elapsedRealtime()
        _uiState.update {
            it.copy(
                status = RecognitionStatus.Capturing(source, startedMs, AUTO_SYNC_CAPTURE_SECONDS),
                audioLevel = 0f,
            )
        }

        val captured = try {
            AudioCaptureManager.captureClip(source, AUTO_SYNC_CAPTURE_SECONDS) { level ->
                _uiState.update { it.copy(audioLevel = level) }
            }
        } catch (e: CancellationException) {
            throw e
        } catch (t: Throwable) {
            _uiState.update {
                it.copy(
                    audioLevel = 0f,
                    status = RecognitionStatus.Error(t.message ?: "Audio capture failed"),
                )
            }
            scheduleNextAutoSync(15_000L)
            return
        }

        if (captured == null || captured.rms < SILENCE_RMS_THRESHOLD) {
            _uiState.update {
                it.copy(audioLevel = 0f, status = RecognitionStatus.SilentAudio)
            }
            scheduleNextAutoSync(12_000L)
            return
        }

        _uiState.update {
            it.copy(audioLevel = 0f, status = RecognitionStatus.Identifying)
        }

        val match = try {
            client.recognizePcmI16(
                samples = captured.samples,
                sampleRate = captured.sampleRate.toUInt(),
                channels = captured.channels.toUShort(),
            )
        } catch (e: CancellationException) {
            throw e
        } catch (e: FfiException) {
            _uiState.update {
                it.copy(status = RecognitionStatus.Error(e.message ?: "Recognition failed"))
            }
            scheduleNextAutoSync(15_000L)
            return
        } catch (t: Throwable) {
            _uiState.update {
                it.copy(status = RecognitionStatus.Error(t.message ?: "Recognition failed"))
            }
            scheduleNextAutoSync(15_000L)
            return
        }

        if (match == null) {
            _uiState.update { it.copy(status = RecognitionStatus.NoMatch) }
            scheduleNextAutoSync(12_000L)
            return
        }

        handleMatchedTrack(match, captured.captureStartElapsedMs, isManual = false)
    }

    private suspend fun handleMatchedTrack(
        match: FfiRecognizedTrack,
        captureStartElapsedMs: Long,
        isManual: Boolean,
    ) {
        val prevTrack = _uiState.value.track
        val sameSong = prevTrack != null && isSameTrack(prevTrack, match)
        val nowMs = SystemClock.elapsedRealtime()

        val newAnchor = AnchorClock(
            matchOffsetSec = match.offsetSec,
            timeskew = match.timeskew,
            captureAnchorMs = captureStartElapsedMs,
        )

        val nextVerifyMs = computeNextVerifyDelayMs(
            durationMs = match.durationMs,
            posSec = newAnchor.positionAt(nowMs, _uiState.value.manualOffsetSec),
        )

        if (sameSong && clock != null) {
            val existing = clock!!
            val predicted = existing.positionAt(nowMs, _uiState.value.manualOffsetSec)
            val measured = newAnchor.positionAt(nowMs, _uiState.value.manualOffsetSec)
            val drift = abs(measured - predicted)
            if (isManual) {
                clock = newAnchor
                pendingSeekCandidate = null
            } else if (drift <= 0.6) {
                pendingSeekCandidate = null
            } else if (drift <= 8.0) {
                clock = newAnchor
                pendingSeekCandidate = null
            } else {
                val prev = pendingSeekCandidate
                val confirmed = prev != null &&
                    (nowMs - prev.second) < 60_000L &&
                    abs(prev.first - measured) <= 2.5
                if (confirmed) {
                    clock = newAnchor
                    pendingSeekCandidate = null
                } else {
                    pendingSeekCandidate = measured to nowMs
                }
            }
            _uiState.update {
                it.copy(
                    status = RecognitionStatus.Idle,
                    audioLevel = 0f,
                    track = match,
                    isPaused = false,
                )
            }
            scheduleNextAutoSync(nextVerifyMs)
            return
        }

        pendingSeekCandidate = null
        clock = newAnchor
        val pos = newAnchor.positionAt(nowMs, _uiState.value.manualOffsetSec)
        _uiState.update {
            it.copy(
                status = RecognitionStatus.Idle,
                audioLevel = 0f,
                track = match,
                lyrics = null,
                syncSnapshot = null,
                accentColor = Color(0xFF38BDF8),
                isPaused = false,
                currentPosSec = pos,
            )
        }
        scheduleNextAutoSync(nextVerifyMs)

        val lyricsResult = runCatching {
            client.fetchLyrics(
                title = match.title,
                artist = match.artist,
                spotifyTrackId = match.spotifyTrackId,
                isrc = match.isrc,
                durationMs = match.durationMs,
            )
        }.getOrNull()

        val latestPos = clock?.positionAt(SystemClock.elapsedRealtime(), _uiState.value.manualOffsetSec) ?: pos
        val snap = lyricsResult?.document?.let { client.evaluateSync(it, latestPos, HOLD_GAP_SEC) }
        _uiState.update { state ->
            if (state.track?.title == match.title && state.track.artist == match.artist) {
                state.copy(
                    lyrics = lyricsResult,
                    currentPosSec = latestPos,
                    syncSnapshot = snap,
                )
            } else {
                state
            }
        }
    }

    private fun computeNextVerifyDelayMs(durationMs: ULong?, posSec: Double): Long {
        val dur = durationMs?.toDouble() ?: return AUTO_REVERIFY_MS
        if (dur <= 0.0) return AUTO_REVERIFY_MS
        val remainingMs = (((dur / 1000.0) - posSec + 3.0).coerceAtLeast(12.0) * 1000.0).toLong()
        return remainingMs.coerceAtMost(AUTO_REVERIFY_MS)
    }

    private fun isSameTrack(a: FfiRecognizedTrack, b: FfiRecognizedTrack): Boolean {
        if (a.spotifyTrackId != null && a.spotifyTrackId == b.spotifyTrackId) return true
        if (a.isrc != null && a.isrc == b.isrc) return true
        return a.title.trim().equals(b.title.trim(), ignoreCase = true) &&
            a.artist.trim().equals(b.artist.trim(), ignoreCase = true)
    }

    override fun onCleared() {
        autoSyncJob?.cancel()
        identifyJob?.cancel()
        client.close()
        super.onCleared()
    }

    companion object {
        private const val AUTO_SYNC_CAPTURE_SECONDS = 10.5f
        private const val SILENCE_RMS_THRESHOLD = 0.0005f
        private const val HOLD_GAP_SEC = 2.5
        private const val AUTO_REVERIFY_MS = 45_000L
    }
}
