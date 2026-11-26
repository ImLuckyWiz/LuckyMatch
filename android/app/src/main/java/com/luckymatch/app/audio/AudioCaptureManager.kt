package com.luckymatch.app.audio

import android.annotation.SuppressLint
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Build
import android.os.SystemClock
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.math.sqrt

enum class CaptureSource(val label: String) {
    MIC("Microphone"),
    INTERNAL_AUDIO("Internal Audio"),
}

data class CapturedAudio(
    val samples: List<Short>,
    val sampleRate: Int,
    val channels: Int,
    val captureStartElapsedMs: Long,
    val rms: Float,
)

enum class ProgressiveOutcome {
    MATCHED,
    SILENT,
    NO_MATCH,
    CANCELLED,
}

object AudioCaptureManager {
    private const val TARGET_SAMPLE_RATE = 16_000
    private val PROGRESSIVE_CHECKPOINTS = floatArrayOf(3.5f, 7.0f, 11.5f)
    private const val PROGRESSIVE_SLIDE_STEP_SECONDS = 4.5f

    @SuppressLint("MissingPermission")
    suspend fun captureClip(
        source: CaptureSource,
        seconds: Float = 10.5f,
        onAudioLevel: (Float) -> Unit = {},
    ): CapturedAudio? {
        val clampedSeconds = seconds.coerceIn(2.0f, 12.0f)
        val totalSamplesTarget = (TARGET_SAMPLE_RATE * clampedSeconds).toInt()
        val minBufBytes = AudioRecord.getMinBufferSize(
            TARGET_SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
        ).coerceAtLeast(4096)

        val record = buildAudioRecord(source, minBufBytes * 2) ?: return null
        if (record.state != AudioRecord.STATE_INITIALIZED) {
            record.release()
            throw IllegalStateException("AudioRecord failed to initialize for ${source.label}")
        }

        val collected = ArrayList<Short>(totalSamplesTarget + 2048)
        val chunk = ShortArray(1024)
        var captureStartMs: Long? = null
        var smoothedLevel = 0f
        val deadlineMs = SystemClock.elapsedRealtime() + ((clampedSeconds + 5.0f) * 1000L).toLong()

        try {
            record.startRecording()
            while (collected.size < totalSamplesTarget) {
                if (!currentCoroutineContext().isActive) {
                    return null
                }
                if (SystemClock.elapsedRealtime() > deadlineMs) {
                    if (collected.isEmpty()) return null
                    break
                }

                val readCount = record.read(chunk, 0, chunk.size)
                if (readCount > 0) {
                    if (captureStartMs == null) {
                        captureStartMs = SystemClock.elapsedRealtime()
                    }
                    val remaining = totalSamplesTarget - collected.size
                    val toTake = readCount.coerceAtMost(remaining)
                    for (i in 0 until toTake) {
                        collected.add(chunk[i])
                    }
                    if (toTake > 0) {
                        smoothedLevel = updateSmoothedLevel(chunk, toTake, smoothedLevel)
                        onAudioLevel(smoothedLevel)
                    }
                } else if (readCount < 0) {
                    throw IllegalStateException("AudioRecord read failed ($readCount)")
                }
            }
        } finally {
            onAudioLevel(0f)
            runCatching { record.stop() }
            record.release()
        }

        val anchor = captureStartMs ?: SystemClock.elapsedRealtime()
        val rms = computeRms(collected)
        return CapturedAudio(
            samples = collected,
            sampleRate = TARGET_SAMPLE_RATE,
            channels = 1,
            captureStartElapsedMs = anchor,
            rms = rms,
        )
    }

    /**
     * Continuously captures into a bounded rolling window ([maxWindowSeconds], max 12s) while
     * calling [onTryMatch] at growing checkpoints (3.5s, 7.0s, 11.5s) and then sliding the
     * rolling window forward every 4.5s until a match succeeds or [maxTotalSeconds] elapses.
     */
    @SuppressLint("MissingPermission")
    suspend fun captureProgressive(
        source: CaptureSource,
        silenceRmsThreshold: Float = 0.0005f,
        maxWindowSeconds: Float = 12.0f,
        maxTotalSeconds: Float = 45.0f,
        silenceTimeoutSeconds: Float? = null,
        onAudioLevel: (Float) -> Unit = {},
        onTryMatch: suspend (CapturedAudio) -> Boolean,
    ): ProgressiveOutcome {
        val clampedWindow = maxWindowSeconds.coerceIn(2.0f, 12.0f)
        val maxSamples = (TARGET_SAMPLE_RATE * clampedWindow).toInt()
        val minBufBytes = AudioRecord.getMinBufferSize(
            TARGET_SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
        ).coerceAtLeast(4096)

        val record = buildAudioRecord(source, minBufBytes * 2) ?: return ProgressiveOutcome.CANCELLED
        if (record.state != AudioRecord.STATE_INITIALIZED) {
            record.release()
            throw IllegalStateException("AudioRecord failed to initialize for ${source.label}")
        }

        val lock = Any()
        val rolling = ArrayDeque<Short>(maxSamples + 2048)
        var firstSampleElapsedMs: Long? = null
        var droppedSamples = 0L
        var totalReceivedSamples = 0L
        var readerError: Throwable? = null
        val stopRequested = AtomicBoolean(false)
        val hadAudioActivity = AtomicBoolean(false)
        val startedAtMs = SystemClock.elapsedRealtime()

        return coroutineScope {
            var readerJob: kotlinx.coroutines.Job? = null
            try {
                record.startRecording()

                readerJob = launch(Dispatchers.IO) {
                    val chunk = ShortArray(1024)
                    var smoothedLevel = 0f
                    while (isActive && !stopRequested.get()) {
                        val readCount = record.read(chunk, 0, chunk.size)
                        if (readCount > 0) {
                            val nowMs = SystemClock.elapsedRealtime()
                            synchronized(lock) {
                                if (firstSampleElapsedMs == null) {
                                    firstSampleElapsedMs = nowMs
                                }
                                for (i in 0 until readCount) {
                                    rolling.addLast(chunk[i])
                                }
                                totalReceivedSamples += readCount.toLong()
                                while (rolling.size > maxSamples) {
                                    rolling.removeFirst()
                                    droppedSamples += 1L
                                }
                            }
                            smoothedLevel = updateSmoothedLevel(chunk, readCount, smoothedLevel)
                            if (smoothedLevel >= 0.003f) {
                                hadAudioActivity.set(true)
                            }
                            onAudioLevel(smoothedLevel)
                        } else if (readCount < 0 && !stopRequested.get()) {
                            synchronized(lock) {
                                readerError = IllegalStateException("AudioRecord read failed ($readCount)")
                            }
                            break
                        }
                    }
                }

                val deadlineMs = startedAtMs + ((maxTotalSeconds + 3.0f) * 1000L).toLong()
                var checkpointIdx = 0
                var nextCheckSec = PROGRESSIVE_CHECKPOINTS[0]
                var hadNonSilent = false

                while (isActive) {
                    val err = synchronized(lock) { readerError }
                    if (err != null) {
                        throw err
                    }

                    val nowLoopMs = SystemClock.elapsedRealtime()
                    if (silenceTimeoutSeconds != null &&
                        !hadAudioActivity.get() &&
                        (nowLoopMs - startedAtMs) >= (silenceTimeoutSeconds * 1000f).toLong()
                    ) {
                        return@coroutineScope ProgressiveOutcome.SILENT
                    }

                    if (nextCheckSec > maxTotalSeconds || nowLoopMs > deadlineMs) {
                        return@coroutineScope if (hadNonSilent) {
                            ProgressiveOutcome.NO_MATCH
                        } else {
                            ProgressiveOutcome.SILENT
                        }
                    }

                    val receivedSecs = synchronized(lock) {
                        totalReceivedSamples.toFloat() / TARGET_SAMPLE_RATE.toFloat()
                    }

                    if (receivedSecs >= nextCheckSec) {
                        checkpointIdx += 1
                        nextCheckSec = if (checkpointIdx < PROGRESSIVE_CHECKPOINTS.size) {
                            PROGRESSIVE_CHECKPOINTS[checkpointIdx]
                        } else {
                            nextCheckSec + PROGRESSIVE_SLIDE_STEP_SECONDS
                        }

                        val snapshot = synchronized(lock) {
                            val firstAt = firstSampleElapsedMs
                            if (firstAt == null || rolling.isEmpty()) {
                                null
                            } else {
                                val list = rolling.toList()
                                val anchorMs = firstAt + ((droppedSamples * 1000L) / TARGET_SAMPLE_RATE.toLong())
                                CapturedAudio(
                                    samples = list,
                                    sampleRate = TARGET_SAMPLE_RATE,
                                    channels = 1,
                                    captureStartElapsedMs = anchorMs,
                                    rms = computeRms(list),
                                )
                            }
                        }

                        if (snapshot != null && snapshot.rms >= silenceRmsThreshold) {
                            hadAudioActivity.set(true)
                            hadNonSilent = true
                            if (onTryMatch(snapshot)) {
                                return@coroutineScope ProgressiveOutcome.MATCHED
                            }
                        }
                    }

                    delay(80L)
                }

                ProgressiveOutcome.CANCELLED
            } finally {
                stopRequested.set(true)
                runCatching { record.stop() }
                withContext(NonCancellable) {
                    readerJob?.cancel()
                    runCatching { readerJob?.join() }
                    record.release()
                }
                onAudioLevel(0f)
            }
        }
    }

    private fun updateSmoothedLevel(chunk: ShortArray, count: Int, current: Float): Float {
        if (count <= 0) return current
        var sumSq = 0.0
        for (i in 0 until count) {
            val norm = chunk[i].toDouble() / 32768.0
            sumSq += norm * norm
        }
        val chunkRms = sqrt(sumSq / count.toDouble()).toFloat()
        val target = (chunkRms * 6.5f).coerceIn(0f, 1f)
        val coeff = if (target > current) 0.65f else 0.22f
        return current + (target - current) * coeff
    }

    @SuppressLint("MissingPermission")
    private fun buildAudioRecord(source: CaptureSource, bufferSizeBytes: Int): AudioRecord? {
        val format = AudioFormat.Builder()
            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
            .setSampleRate(TARGET_SAMPLE_RATE)
            .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
            .build()

        return when (source) {
            CaptureSource.MIC -> {
                AudioRecord.Builder()
                    .setAudioSource(MediaRecorder.AudioSource.MIC)
                    .setAudioFormat(format)
                    .setBufferSizeInBytes(bufferSizeBytes)
                    .build()
            }
            CaptureSource.INTERNAL_AUDIO -> {
                if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
                    throw IllegalStateException("Internal audio capture requires Android 10 or newer")
                }
                val projection = MediaProjectionService.activeProjection
                    ?: throw IllegalStateException("Internal audio permission has not been granted yet")
                val config = AudioPlaybackCaptureConfiguration.Builder(projection)
                    .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
                    .addMatchingUsage(AudioAttributes.USAGE_GAME)
                    .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
                    .build()
                AudioRecord.Builder()
                    .setAudioFormat(format)
                    .setBufferSizeInBytes(bufferSizeBytes)
                    .setAudioPlaybackCaptureConfig(config)
                    .build()
            }
        }
    }

    private fun computeRms(samples: List<Short>): Float {
        if (samples.isEmpty()) return 0f
        var sumSq = 0.0
        for (s in samples) {
            val norm = s.toDouble() / 32768.0
            sumSq += norm * norm
        }
        return sqrt(sumSq / samples.size.toDouble()).toFloat()
    }
}
