package com.luckymatch.app.ui

import android.graphics.Bitmap
import android.graphics.drawable.BitmapDrawable
import android.os.Build
import android.os.SystemClock
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.MusicNote
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.palette.graphics.Palette
import coil.compose.AsyncImage
import coil.request.ImageRequest
import com.luckymatch.app.R
import com.luckymatch.app.audio.CaptureSource
import kotlinx.coroutines.isActive
import uniffi.luckymatch_ffi.FfiLyricsSource
import java.util.Locale
import kotlin.math.floor

@Composable
fun KaraokeScreen(
    viewModel: LuckyMatchViewModel,
    onRequestIdentify: (CaptureSource) -> Unit,
    onRequestSwitchSource: (CaptureSource) -> Unit,
) {
    val state by viewModel.uiState.collectAsState()
    var showSettings by remember { mutableStateOf(false) }
    var captureElapsedTick by remember { mutableLongStateOf(0L) }

    val isPlaying = state.track != null && !state.isPaused
    val isCapturing = state.status is RecognitionStatus.Capturing

    LaunchedEffect(isPlaying, isCapturing) {
        if (!isPlaying && !isCapturing) return@LaunchedEffect
        while (isActive) {
            withFrameNanos {
                if (isPlaying) {
                    viewModel.tickFrame()
                }
                if (isCapturing) {
                    captureElapsedTick = SystemClock.elapsedRealtime()
                }
            }
        }
    }

    val topBg = remember(state.accentColor) {
        val base = Color(0xFF0D0F15)
        val mix = 0.24f
        Color(
            red = base.red * (1f - mix) + state.accentColor.red * mix,
            green = base.green * (1f - mix) + state.accentColor.green * mix,
            blue = base.blue * (1f - mix) + state.accentColor.blue * mix,
            alpha = 1f,
        )
    }

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Brush.verticalGradient(listOf(topBg, Color(0xFF080A0F))))
            .statusBarsPadding()
            .navigationBarsPadding()
    ) {
        Column(modifier = Modifier.fillMaxSize()) {
            TopBar(
                status = state.status,
                isPaused = state.isPaused,
                hasTrack = state.track != null,
                nowMs = captureElapsedTick,
                onOpenSettings = { showSettings = true },
            )

            if (state.track != null) {
                TrackHeader(
                    title = state.track!!.title,
                    artist = state.track!!.artist,
                    album = state.track!!.album,
                    albumArtUrl = state.track!!.albumArt,
                    lyrics = state.lyrics,
                    posSec = state.currentPosSec,
                    accentColor = state.accentColor,
                    onAccentExtracted = viewModel::updateAccentColor,
                )
            }

            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth(),
                contentAlignment = Alignment.Center,
            ) {
                val track = state.track
                val lyrics = state.lyrics
                when {
                    track == null -> WelcomeCard(
                        source = state.source,
                        autoSync = state.autoSync,
                        accentColor = state.accentColor,
                        status = state.status,
                        audioLevel = state.audioLevel,
                        onListenClick = { onRequestIdentify(state.source) },
                        onCancelClick = viewModel::cancelIdentify,
                    )
                    lyrics == null -> Column(
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                        modifier = Modifier.padding(horizontal = 24.dp),
                    ) {
                        Text(
                            text = "Fetching synchronized lyrics...",
                            fontSize = 17.sp,
                            fontWeight = FontWeight.SemiBold,
                            color = Color.White.copy(alpha = 0.85f),
                        )
                        Text(
                            text = "Checking Cache → Spicy Lyrics → AMLL → NetEase → LRCLIB",
                            fontSize = 12.sp,
                            color = Color.White.copy(alpha = 0.50f),
                            textAlign = TextAlign.Center,
                        )
                    }
                    else -> LyricsStage(
                        document = lyrics.document,
                        snapshot = state.syncSnapshot,
                        posSec = state.currentPosSec,
                        accentColor = state.accentColor,
                        modifier = Modifier.fillMaxSize(),
                    )
                }
            }

            BottomControls(
                state = state,
                onIdentify = { onRequestIdentify(state.source) },
                onCancelIdentify = viewModel::cancelIdentify,
                onSwitchSource = {
                    val next = if (state.source == CaptureSource.MIC && Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                        CaptureSource.INTERNAL_AUDIO
                    } else {
                        CaptureSource.MIC
                    }
                    onRequestSwitchSource(next)
                },
                onToggleAutoSync = { viewModel.toggleAutoSync(!state.autoSync) },
                onNudge = viewModel::nudgeOffset,
                onResetOffset = viewModel::resetOffset,
                onTogglePause = viewModel::togglePause,
            )
        }
    }

    if (showSettings) {
        SettingsSheet(
            initial = state.credentials,
            onSave = viewModel::saveCredentials,
            onDismiss = { showSettings = false },
        )
    }
}

@Composable
private fun TopBar(
    status: RecognitionStatus,
    isPaused: Boolean,
    hasTrack: Boolean,
    nowMs: Long,
    onOpenSettings: () -> Unit,
) {
    val (label, color) = when (status) {
        is RecognitionStatus.Capturing -> {
            val raw = ((nowMs - status.startedAtMs).coerceAtLeast(0L) / 1000f)
            val elapsed = status.durationSec?.let { raw.coerceAtMost(it) } ?: raw
            String.format(Locale.US, "Listening (%s) %.1fs", status.source.label, elapsed) to Color(0xFF38BDF8)
        }
        RecognitionStatus.Identifying -> "Identifying..." to Color(0xFFFACC15)
        RecognitionStatus.SilentAudio -> "Silence detected" to Color.White.copy(alpha = 0.6f)
        RecognitionStatus.NoMatch -> "No match yet" to Color(0xFFFBBF24)
        is RecognitionStatus.Error -> status.message.take(36) to Color(0xFFF87171)
        RecognitionStatus.Idle -> when {
            hasTrack && isPaused -> "PAUSED" to Color(0xFFFBBF24)
            hasTrack -> "LIVE" to Color(0xFF4ADE80)
            else -> "Ready" to Color.White.copy(alpha = 0.6f)
        }
    }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 10.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
            modifier = Modifier.weight(1f),
        ) {
            Box(
                modifier = Modifier
                    .size(22.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .background(Color(0xFF111A33)),
                contentAlignment = Alignment.Center,
            ) {
                Image(
                    painter = painterResource(id = R.drawable.ic_launcher_foreground),
                    contentDescription = "LuckyMatch",
                    modifier = Modifier
                        .fillMaxSize()
                        .scale(1.45f),
                )
            }
            Text(
                text = "LuckyMatch",
                fontSize = 16.sp,
                fontWeight = FontWeight.Bold,
                color = Color.White,
            )
            Surface(
                color = Color.White.copy(alpha = 0.08f),
                shape = RoundedCornerShape(999.dp),
            ) {
                Text(
                    text = label,
                    fontSize = 11.sp,
                    color = color,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
                )
            }
        }

        IconButton(onClick = onOpenSettings) {
            Icon(
                imageVector = Icons.Default.Settings,
                contentDescription = "Settings",
                tint = Color.White.copy(alpha = 0.78f),
            )
        }
    }
}

@Composable
private fun TrackHeader(
    title: String,
    artist: String,
    album: String?,
    albumArtUrl: String?,
    lyrics: uniffi.luckymatch_ffi.FfiLyricsResult?,
    posSec: Double,
    accentColor: Color,
    onAccentExtracted: (Color) -> Unit,
) {
    val context = LocalContext.current
    val sourceText = lyrics?.let { "${sourceLabel(it.source)} • ${it.document.kind.name.lowercase().replaceFirstChar(Char::titlecase)}" }
        ?: "Searching lyrics..."

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 18.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(
            modifier = Modifier
                .size(58.dp)
                .clip(RoundedCornerShape(10.dp))
                .background(accentColor.copy(alpha = 0.25f))
                .border(1.dp, Color.White.copy(alpha = 0.12f), RoundedCornerShape(10.dp)),
            contentAlignment = Alignment.Center,
        ) {
            if (albumArtUrl != null) {
                AsyncImage(
                    model = ImageRequest.Builder(context)
                        .data(albumArtUrl)
                        .allowHardware(false)
                        .build(),
                    contentDescription = "Album art",
                    contentScale = ContentScale.Crop,
                    modifier = Modifier.fillMaxSize(),
                    onSuccess = { result ->
                        val bmp = (result.result.drawable as? BitmapDrawable)?.bitmap
                        if (bmp != null) {
                            extractAccentColor(bmp)?.let(onAccentExtracted)
                        }
                    },
                )
            } else {
                Icon(
                    imageVector = Icons.Default.MusicNote,
                    contentDescription = null,
                    tint = Color.White.copy(alpha = 0.7f),
                )
            }
        }

        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = title,
                fontSize = 17.sp,
                fontWeight = FontWeight.Bold,
                color = Color.White,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = if (album != null) "$artist • $album" else artist,
                fontSize = 13.sp,
                color = Color.White.copy(alpha = 0.7f),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Spacer(modifier = Modifier.height(4.dp))
            Surface(
                color = accentColor.copy(alpha = 0.22f),
                shape = RoundedCornerShape(999.dp),
            ) {
                Text(
                    text = sourceText,
                    fontSize = 11.sp,
                    color = Color(0xFFE2E8F0),
                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
                )
            }
        }

        Text(
            text = formatClock(posSec),
            fontSize = 15.sp,
            fontWeight = FontWeight.SemiBold,
            color = Color.White.copy(alpha = 0.85f),
        )
    }
}

@Composable
private fun WelcomeCard(
    source: CaptureSource,
    autoSync: Boolean,
    accentColor: Color,
    status: RecognitionStatus,
    audioLevel: Float,
    onListenClick: () -> Unit,
    onCancelClick: () -> Unit,
) {
    val isCapturing = status is RecognitionStatus.Capturing
    val isIdentifying = status is RecognitionStatus.Identifying
    val isBusy = isCapturing || isIdentifying

    val smoothLevel by animateFloatAsState(
        targetValue = if (isCapturing) audioLevel else 0f,
        animationSpec = spring(
            dampingRatio = Spring.DampingRatioNoBouncy,
            stiffness = Spring.StiffnessMediumLow,
        ),
        label = "beatLevel",
    )

    val infiniteTransition = rememberInfiniteTransition(label = "identifyPulse")
    val identifyPulse by infiniteTransition.animateFloat(
        initialValue = 0.15f,
        targetValue = 0.75f,
        animationSpec = infiniteRepeatable(
            animation = tween(durationMillis = 700),
            repeatMode = RepeatMode.Reverse,
        ),
        label = "identifyPulseValue",
    )

    val pulse = when {
        isCapturing -> smoothLevel
        isIdentifying -> identifyPulse
        else -> 0f
    }

    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(14.dp),
        modifier = Modifier.padding(horizontal = 28.dp),
    ) {
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier.size(164.dp),
        ) {
            if (isBusy) {
                Box(
                    modifier = Modifier
                        .size(110.dp)
                        .scale(1f + pulse * 0.45f)
                        .clip(CircleShape)
                        .background(accentColor.copy(alpha = 0.08f + pulse * 0.16f))
                        .border(1.dp, accentColor.copy(alpha = 0.18f + pulse * 0.35f), CircleShape)
                )
                Box(
                    modifier = Modifier
                        .size(92.dp)
                        .scale(1f + pulse * 0.28f)
                        .clip(CircleShape)
                        .background(accentColor.copy(alpha = 0.14f + pulse * 0.22f))
                )
            }

            Box(
                modifier = Modifier
                    .size(84.dp)
                    .scale(1f + pulse * 0.18f)
                    .clip(CircleShape)
                    .background(accentColor.copy(alpha = if (isBusy) 0.34f + pulse * 0.40f else 0.22f))
                    .border(
                        width = 1.5.dp,
                        color = accentColor.copy(alpha = if (isBusy) 0.75f + pulse * 0.25f else 0.65f),
                        shape = CircleShape,
                    )
                    .clickable {
                        if (isBusy) onCancelClick() else onListenClick()
                    },
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.Default.MusicNote,
                    contentDescription = if (isBusy) "Cancel listening" else "Listen and match",
                    tint = Color.White,
                    modifier = Modifier.size(36.dp),
                )
            }
        }

        Text(
            text = when {
                isCapturing -> "Listening to the beat..."
                isIdentifying -> "Matching song..."
                else -> "Tap ♪ to identify & sync lyrics"
            },
            fontSize = 20.sp,
            fontWeight = FontWeight.Bold,
            color = Color.White,
            textAlign = TextAlign.Center,
        )
        Text(
            text = if (isBusy) {
                "Tap ♪ to cancel"
            } else {
                "Source: ${source.label} • Auto-Sync ${if (autoSync) "ON" else "OFF"}"
            },
            fontSize = 13.sp,
            color = Color.White.copy(alpha = 0.6f),
            textAlign = TextAlign.Center,
        )
    }
}

@Composable
private fun BottomControls(
    state: LuckyMatchUiState,
    onIdentify: () -> Unit,
    onCancelIdentify: () -> Unit,
    onSwitchSource: () -> Unit,
    onToggleAutoSync: () -> Unit,
    onNudge: (Double) -> Unit,
    onResetOffset: () -> Unit,
    onTogglePause: () -> Unit,
) {
    val isBusy = state.status is RecognitionStatus.Capturing || state.status is RecognitionStatus.Identifying

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(Color(0xFF05070B).copy(alpha = 0.75f))
            .padding(horizontal = 14.dp, vertical = 10.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Button(
                onClick = { if (isBusy) onCancelIdentify() else onIdentify() },
                colors = ButtonDefaults.buttonColors(
                    containerColor = state.accentColor,
                    contentColor = Color(0xFF070A10),
                ),
                shape = RoundedCornerShape(10.dp),
            ) {
                Icon(
                    imageVector = Icons.Default.Refresh,
                    contentDescription = null,
                    modifier = Modifier.size(16.dp),
                )
                Spacer(modifier = Modifier.width(6.dp))
                Text(
                    text = if (isBusy) "Cancel" else "Re-sync",
                    fontSize = 12.sp,
                    fontWeight = FontWeight.Bold,
                )
            }

            FilterChip(
                selected = state.source == CaptureSource.INTERNAL_AUDIO,
                onClick = onSwitchSource,
                label = { Text(state.source.label, fontSize = 12.sp) },
                colors = FilterChipDefaults.filterChipColors(
                    labelColor = Color.White,
                    selectedLabelColor = Color.White,
                    selectedContainerColor = state.accentColor.copy(alpha = 0.28f),
                ),
            )

            FilterChip(
                selected = state.autoSync,
                onClick = onToggleAutoSync,
                label = { Text(if (state.autoSync) "Auto ON" else "Auto OFF", fontSize = 12.sp) },
                colors = FilterChipDefaults.filterChipColors(
                    labelColor = Color.White,
                    selectedLabelColor = Color.White,
                    selectedContainerColor = state.accentColor.copy(alpha = 0.28f),
                ),
            )
        }

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                SmallControlChip(label = "-0.2s", onClick = { onNudge(-0.2) })
                Text(
                    text = String.format(Locale.US, "%+.1fs", state.manualOffsetSec),
                    fontSize = 12.sp,
                    color = Color.White.copy(alpha = 0.85f),
                    modifier = Modifier
                        .clip(RoundedCornerShape(6.dp))
                        .clickable(onClick = onResetOffset)
                        .padding(horizontal = 8.dp, vertical = 4.dp),
                )
                SmallControlChip(label = "+0.2s", onClick = { onNudge(0.2) })
            }

            if (state.track != null) {
                Surface(
                    color = Color.White.copy(alpha = 0.10f),
                    shape = RoundedCornerShape(8.dp),
                    modifier = Modifier.clickable(onClick = onTogglePause),
                ) {
                    Row(
                        modifier = Modifier.padding(horizontal = 12.dp, vertical = 6.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        Icon(
                            imageVector = if (state.isPaused) Icons.Default.PlayArrow else Icons.Default.Pause,
                            contentDescription = null,
                            tint = Color.White,
                            modifier = Modifier.size(16.dp),
                        )
                        Text(
                            text = if (state.isPaused) "Resume" else "Pause",
                            fontSize = 12.sp,
                            color = Color.White,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun SmallControlChip(label: String, onClick: () -> Unit) {
    Surface(
        color = Color.White.copy(alpha = 0.08f),
        shape = RoundedCornerShape(8.dp),
        modifier = Modifier.clickable(onClick = onClick),
    ) {
        Text(
            text = label,
            fontSize = 12.sp,
            color = Color.White,
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
        )
    }
}

private fun formatClock(sec: Double): String {
    val clamped = sec.coerceAtLeast(0.0)
    val mins = floor(clamped / 60.0).toInt()
    val rem = floor(clamped % 60.0).toInt()
    return String.format(Locale.US, "%02d:%02d", mins, rem)
}

private fun sourceLabel(src: FfiLyricsSource): String = when (src) {
    FfiLyricsSource.CUSTOM -> "Custom"
    FfiLyricsSource.CACHE -> "Cache"
    FfiLyricsSource.SPICY_LYRICS -> "Spicy Lyrics"
    FfiLyricsSource.AMLL -> "AMLL TTML"
    FfiLyricsSource.NET_EASE -> "NetEase"
    FfiLyricsSource.LRC_LIB -> "LRCLIB"
}

private fun extractAccentColor(bitmap: Bitmap): Color? {
    val palette = Palette.from(bitmap).generate()
    val rgb = palette.getVibrantColor(
        palette.getLightVibrantColor(
            palette.getDominantColor(0)
        )
    )
    if (rgb == 0) return null
    return Color(rgb)
}
