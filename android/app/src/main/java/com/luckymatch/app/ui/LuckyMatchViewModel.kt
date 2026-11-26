package com.luckymatch.app.ui

import android.app.Application
import android.os.SystemClock
import androidx.compose.ui.graphics.Color
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.luckymatch.app.BuildConfig
import com.luckymatch.app.audio.AudioCaptureManager
import com.luckymatch.app.audio.CaptureSource
import com.luckymatch.app.audio.MediaProjectionService
import com.luckymatch.app.audio.ProgressiveOutcome
import com.luckymatch.app.data.Credentials
import com.luckymatch.app.data.CredentialsStore
import com.luckymatch.app.update.AppUpdater
import com.luckymatch.app.update.ReleaseInfo
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
import uniffi.luckymatch_ffi.FfiLyricsKind
import uniffi.luckymatch_ffi.FfiLyricsResult
import uniffi.luckymatch_ffi.FfiRecognizedTrack
import uniffi.luckymatch_ffi.FfiSyncSnapshot
import uniffi.luckymatch_ffi.LuckyMatchClient
import java.io.File
import kotlin.math.abs

sealed interface RecognitionStatus {
    data object Idle : RecognitionStatus
    data class Capturing(val source: CaptureSource, val startedAtMs: Long, val durationSec: Float?) : RecognitionStatus
    data object Identifying : RecognitionStatus
    data object SilentAudio : RecognitionStatus
    data object NoMatch : RecognitionStatus
    data class Error(val message: String) : RecognitionStatus
}

sealed interface UpdateState {
    data object Idle : UpdateState
    data object Checking : UpdateState
    data object UpToDate : UpdateState
    data class Available(val release: ReleaseInfo, val dismissed: Boolean = false) : UpdateState
    data class Downloading(val release: ReleaseInfo, val progress: Float?) : UpdateState
    data class ReadyToInstall(val release: ReleaseInfo, val apkFile: File) : UpdateState
    data class Error(val message: String) : UpdateState
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
    val updateState: UpdateState = UpdateState.Idle,
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
    private var updateJob: Job? = null
    private var pendingSeekCandidate: Pair<Double, Long>? = null
    private var awaitingNextSong: Boolean = false
    private var nextSongPrevPosSec: Double = 0.0
    private var nextSongSilenceRetry: Boolean = false
    private var songEndTriggered: Boolean = false

    init {
        checkForUpdates(manual = false)
    }

    fun checkForUpdates(manual: Boolean = true) {
        val current = _uiState.value.updateState
        if (current is UpdateState.Checking || current is UpdateState.Downloading) return
        if (manual && current is UpdateState.Available && current.dismissed) {
            _uiState.update { it.copy(updateState = current.copy(dismissed = false)) }
            return
        }

        updateJob?.cancel()
        if (manual) {
            _uiState.update { it.copy(updateState = UpdateState.Checking) }
        }

        updateJob = viewModelScope.launch(Dispatchers.IO) {
            try {
                val release = AppUpdater.checkLatestRelease(BuildConfig.VERSION_NAME)
                if (release != null) {
                    _uiState.update { it.copy(updateState = UpdateState.Available(release)) }
                } else if (manual) {
                    _uiState.update { it.copy(updateState = UpdateState.UpToDate) }
                }
            } catch (e: CancellationException) {
                throw e
            } catch (t: Throwable) {
                if (manual) {
                    _uiState.update {
                        it.copy(updateState = UpdateState.Error(t.message ?: "Update check failed"))
                    }
                }
            }
        }
    }

    fun downloadUpdate() {
        val release = when (val st = _uiState.value.updateState) {
            is UpdateState.Available -> st.release
            is UpdateState.ReadyToInstall -> st.release
            else -> return
        }
        updateJob?.cancel()
        _uiState.update { it.copy(updateState = UpdateState.Downloading(release, 0f)) }

        updateJob = viewModelScope.launch(Dispatchers.IO) {
            try {
                val apkFile = AppUpdater.downloadApk(
                    context = getApplication<Application>().applicationContext,
                    release = release,
                    onProgress = { progress ->
                        _uiState.update {
                            it.copy(updateState = UpdateState.Downloading(release, progress))
                        }
                    },
                )
                _uiState.update {
                    it.copy(updateState = UpdateState.ReadyToInstall(release, apkFile))
                }
            } catch (e: CancellationException) {
                throw e
            } catch (t: Throwable) {
                _uiState.update {
                    it.copy(updateState = UpdateState.Error(t.message ?: "Download failed"))
                }
            }
        }
    }

    fun dismissUpdateBanner() {
        _uiState.update { state ->
            when (val st = state.updateState) {
                is UpdateState.Available -> state.copy(updateState = st.copy(dismissed = true))
                is UpdateState.Error, UpdateState.UpToDate -> state.copy(updateState = UpdateState.Idle)
                else -> state
            }
        }
    }

    fun clearUpdateError() {
        if (_uiState.value.updateState is UpdateState.Error) {
            _uiState.update { it.copy(updateState = UpdateState.Idle) }
        }
    }

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
        if (!enabled) {
            autoSyncJob?.cancel()
            autoSyncJob = null
            awaitingNextSong = false
            nextSongSilenceRetry = false
            songEndTriggered = false
            if (identifyJob?.isActive == true && !identifyIsManual) {
                identifyJob?.cancel()
                identifyJob = null
                _uiState.update {
                    it.copy(audioLevel = 0f, status = RecognitionStatus.Idle)
                }
            }
        } else {
            scheduleAutoSyncForCurrentState()
        }
    }

    private fun scheduleNextAutoSync(delayMs: Long) {
        autoSyncJob?.cancel()
        val state = _uiState.value
        if (!state.autoSync || state.track == null || state.isPaused) return
        autoSyncJob = viewModelScope.launch {
            delay(delayMs)
            val latest = _uiState.value
            if (isActive && latest.autoSync && !latest.isPaused && identifyJob?.isActive != true) {
                if (awaitingNextSong) {
                    startNextSongIdentify(nextSongPrevPosSec, isSilenceRetry = nextSongSilenceRetry)
                } else {
                    identifyNow(isManual = false)
                }
            }
        }
    }

    private fun scheduleAutoSyncForCurrentState() {
        autoSyncJob?.cancel()
        val state = _uiState.value
        if (!state.autoSync || state.track == null || state.isPaused) return

        val nowMs = SystemClock.elapsedRealtime()
        val pos = clock?.positionAt(nowMs, state.manualOffsetSec) ?: state.currentPosSec
        val trigger = computeSongEndTriggerSec(state.track, state.lyrics)

        if (trigger != null) {
            val remSec = trigger - pos
            if (remSec <= 0.0) {
                if (!songEndTriggered && identifyJob?.isActive != true) {
                    songEndTriggered = true
                    startNextSongIdentify(pos, isSilenceRetry = false)
                }
                return
            }
            if (remSec <= AUTO_REVERIFY_SEC + MID_SONG_REVERIFY_END_MARGIN_SEC) {
                autoSyncJob = viewModelScope.launch {
                    delay((remSec * 1000.0).toLong().coerceAtLeast(50L))
                    val latest = _uiState.value
                    if (isActive && latest.autoSync && !latest.isPaused && identifyJob?.isActive != true) {
                        songEndTriggered = true
                        val latestPos = clock?.positionAt(
                            SystemClock.elapsedRealtime(),
                            latest.manualOffsetSec,
                        ) ?: trigger
                        startNextSongIdentify(latestPos, isSilenceRetry = false)
                    }
                }
                return
            }
        }

        scheduleNextAutoSync(delayMs = AUTO_REVERIFY_MS)
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
        resetSongEndIfBeforeTrigger()
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
        resetSongEndIfBeforeTrigger()
    }

    private fun resetSongEndIfBeforeTrigger() {
        val state = _uiState.value
        val trigger = computeSongEndTriggerSec(state.track, state.lyrics)
        if (trigger != null) {
            val pos = clock?.positionAt(SystemClock.elapsedRealtime(), state.manualOffsetSec)
                ?: state.currentPosSec
            if (pos < trigger - 2.0) {
                songEndTriggered = false
                awaitingNextSong = false
                nextSongSilenceRetry = false
            }
        }
        scheduleAutoSyncForCurrentState()
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
        val nextPaused = !currentlyPaused
        val pos = c.positionAt(nowMs, _uiState.value.manualOffsetSec)
        val snap = _uiState.value.lyrics?.document?.let { client.evaluateSync(it, pos, HOLD_GAP_SEC) }
        _uiState.update {
            it.copy(
                isPaused = nextPaused,
                currentPosSec = pos,
                syncSnapshot = snap,
            )
        }
        if (nextPaused) {
            autoSyncJob?.cancel()
            autoSyncJob = null
            awaitingNextSong = false
            nextSongSilenceRetry = false
            if (identifyJob?.isActive == true && !identifyIsManual) {
                identifyJob?.cancel()
                identifyJob = null
                _uiState.update {
                    it.copy(audioLevel = 0f, status = RecognitionStatus.Idle)
                }
            }
        } else {
            scheduleAutoSyncForCurrentState()
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
        if (state.autoSync && !songEndTriggered && identifyJob?.isActive != true) {
            val trigger = computeSongEndTriggerSec(state.track, state.lyrics)
            if (trigger != null && pos >= trigger) {
                songEndTriggered = true
                autoSyncJob?.cancel()
                startNextSongIdentify(pos, isSilenceRetry = false)
            }
        }
    }

    fun cancelIdentify() {
        identifyJob?.cancel()
        identifyJob = null
        identifyIsManual = false
        awaitingNextSong = false
        nextSongSilenceRetry = false
        _uiState.update {
            it.copy(
                audioLevel = 0f,
                status = RecognitionStatus.Idle,
            )
        }
        scheduleAutoSyncForCurrentState()
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

        if (isManual) {
            awaitingNextSong = false
            nextSongSilenceRetry = false
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

    private fun startNextSongIdentify(prevPosSec: Double, isSilenceRetry: Boolean) {
        if (identifyJob?.isActive == true) {
            if (!identifyIsManual) {
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

        awaitingNextSong = true
        nextSongPrevPosSec = prevPosSec
        nextSongSilenceRetry = isSilenceRetry
        identifyIsManual = false
        val silenceTimeout = if (isSilenceRetry) {
            NEXT_SONG_RETRY_SILENCE_TIMEOUT_SEC
        } else {
            NEXT_SONG_INITIAL_SILENCE_TIMEOUT_SEC
        }
        identifyJob = viewModelScope.launch(Dispatchers.IO) {
            runNextSongIdentify(source, prevPosSec, silenceTimeout)
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

    private suspend fun runNextSongIdentify(
        source: CaptureSource,
        prevPosSec: Double,
        silenceTimeoutSec: Float,
    ) {
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
                silenceTimeoutSeconds = silenceTimeoutSec,
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
                        val prevTrack = _uiState.value.track
                        val sameSong = prevTrack != null && isSameTrack(prevTrack, match)
                        val nowMs = SystemClock.elapsedRealtime()
                        val measuredPos = match.offsetSec +
                            (((nowMs - snapshot.captureStartElapsedMs).coerceAtLeast(0L)) / 1000.0) *
                            (1.0 + match.timeskew) +
                            _uiState.value.manualOffsetSec
                        val durationSec = prevTrack?.durationMs?.toDouble()?.takeIf { it > 0.0 }?.div(1000.0)
                        val accept = !sameSong ||
                            (prevPosSec > 35.0 && measuredPos < 25.0) ||
                            (durationSec != null && durationSec - measuredPos > 12.0)
                        if (accept) {
                            matchedPair = match to snapshot.captureStartElapsedMs
                            true
                        } else {
                            _uiState.update {
                                it.copy(status = RecognitionStatus.Capturing(source, startedMs, durationSec = null))
                            }
                            false
                        }
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
                handleMatchedTrack(match, anchorMs, isManual = false, forceResync = true)
            }
            ProgressiveOutcome.SILENT -> {
                nextSongSilenceRetry = true
                _uiState.update { it.copy(audioLevel = 0f, status = RecognitionStatus.SilentAudio) }
                scheduleNextAutoSync(NEXT_SONG_SILENT_RETRY_MS)
            }
            ProgressiveOutcome.NO_MATCH -> {
                nextSongSilenceRetry = false
                _uiState.update { it.copy(audioLevel = 0f, status = RecognitionStatus.NoMatch) }
                scheduleNextAutoSync(NEXT_SONG_NO_MATCH_RETRY_MS)
            }
            ProgressiveOutcome.CANCELLED -> {
                awaitingNextSong = false
                nextSongSilenceRetry = false
                _uiState.update { it.copy(audioLevel = 0f, status = RecognitionStatus.Idle) }
                scheduleAutoSyncForCurrentState()
            }
        }
    }

    private suspend fun handleMatchedTrack(
        match: FfiRecognizedTrack,
        captureStartElapsedMs: Long,
        isManual: Boolean,
        forceResync: Boolean = false,
    ) {
        awaitingNextSong = false
        nextSongSilenceRetry = false
        val prevTrack = _uiState.value.track
        val sameSong = prevTrack != null && isSameTrack(prevTrack, match)
        val nowMs = SystemClock.elapsedRealtime()

        val newAnchor = AnchorClock(
            matchOffsetSec = match.offsetSec,
            timeskew = match.timeskew,
            captureAnchorMs = captureStartElapsedMs,
        )

        if (sameSong && clock != null) {
            val existing = clock!!
            val predicted = existing.positionAt(nowMs, _uiState.value.manualOffsetSec)
            val measured = newAnchor.positionAt(nowMs, _uiState.value.manualOffsetSec)
            val drift = abs(measured - predicted)
            if (isManual || forceResync) {
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
            resetSongEndIfBeforeTrigger()
            return
        }

        pendingSeekCandidate = null
        songEndTriggered = false
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
        scheduleAutoSyncForCurrentState()

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
        resetSongEndIfBeforeTrigger()
    }

    private fun maxSyncedLyricsEndSec(lyrics: FfiLyricsResult?): Double? {
        val doc = lyrics?.document ?: return null
        if (doc.kind == FfiLyricsKind.STATIC || doc.lines.isEmpty()) return null
        var maxEnd: Double? = null
        for (line in doc.lines) {
            val end = line.endTime
            if (end.isFinite() && end > 0.0) {
                maxEnd = if (maxEnd == null) end else maxOf(maxEnd, end)
            }
        }
        return maxEnd
    }

    private fun computeSongEndTriggerSec(
        track: FfiRecognizedTrack?,
        lyrics: FfiLyricsResult?,
    ): Double? {
        if (track == null) return null
        val trackDur = track.durationMs?.toDouble()?.takeIf { it > 0.0 }?.div(1000.0)
        val lyricsEnd = maxSyncedLyricsEndSec(lyrics)
        return when {
            trackDur != null && lyricsEnd != null -> maxOf(trackDur, lyricsEnd) + SONG_END_DURATION_BUFFER_SEC
            trackDur != null -> trackDur + SONG_END_DURATION_BUFFER_SEC
            lyricsEnd != null -> lyricsEnd + SONG_END_LYRICS_BUFFER_SEC
            else -> null
        }
    }

    private fun isSameTrack(a: FfiRecognizedTrack, b: FfiRecognizedTrack): Boolean {
        if (a.spotifyTrackId != null && a.spotifyTrackId == b.spotifyTrackId) return true
        if (a.isrc != null && a.isrc == b.isrc) return true
        return a.title.trim().equals(b.title.trim(), ignoreCase = true) &&
            a.artist.trim().equals(b.artist.trim(), ignoreCase = true)
    }

    override fun onCleared() {
        updateJob?.cancel()
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
        private const val AUTO_REVERIFY_SEC = 45.0
        private const val SONG_END_DURATION_BUFFER_SEC = 1.5
        private const val SONG_END_LYRICS_BUFFER_SEC = 4.0
        private const val MID_SONG_REVERIFY_END_MARGIN_SEC = 18.0
        private const val NEXT_SONG_INITIAL_SILENCE_TIMEOUT_SEC = 25.0f
        private const val NEXT_SONG_RETRY_SILENCE_TIMEOUT_SEC = 4.5f
        private const val NEXT_SONG_NO_MATCH_RETRY_MS = 6_000L
        private const val NEXT_SONG_SILENT_RETRY_MS = 10_000L
    }
}
