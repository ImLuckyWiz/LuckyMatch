package com.luckymatch.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import uniffi.luckymatch_ffi.FfiLyricsDocument
import uniffi.luckymatch_ffi.FfiSyllableProgress
import uniffi.luckymatch_ffi.FfiSyncSnapshot
import kotlin.math.abs
import kotlin.math.sin

fun expandForWrapping(syllables: List<FfiSyllableProgress>): List<List<FfiSyllableProgress>> {
    val flat = ArrayList<FfiSyllableProgress>(syllables.size)
    for (syl in syllables) {
        val words = syl.text.trim().split(Regex("\\s+")).filter { it.isNotEmpty() }
        if (words.size <= 1) {
            flat.add(syl)
            continue
        }
        val totalChars = words.sumOf { it.length.coerceAtLeast(1) }.coerceAtLeast(1)
        val overall = syl.progress.coerceIn(0.0, 1.0)
        var offsetChars = 0
        words.forEachIndexed { idx, word ->
            val charLen = word.length.coerceAtLeast(1)
            val startFrac = offsetChars.toDouble() / totalChars.toDouble()
            val endFrac = (offsetChars + charLen).toDouble() / totalChars.toDouble()
            val span = (endFrac - startFrac).coerceAtLeast(1e-6)
            val wordProgress = ((overall - startFrac) / span).coerceIn(0.0, 1.0)
            offsetChars += charLen
            val isLast = idx == words.lastIndex
            flat.add(
                FfiSyllableProgress(
                    text = word,
                    progress = wordProgress,
                    isPartOfWord = if (isLast) syl.isPartOfWord else false,
                )
            )
        }
    }

    val groups = ArrayList<List<FfiSyllableProgress>>()
    var current = ArrayList<FfiSyllableProgress>()
    for (syl in flat) {
        current.add(syl)
        if (!syl.isPartOfWord) {
            groups.add(current)
            current = ArrayList()
        }
    }
    if (current.isNotEmpty()) {
        groups.add(current)
    }
    return groups
}

@Composable
fun LyricsStage(
    document: FfiLyricsDocument,
    snapshot: FfiSyncSnapshot?,
    posSec: Double,
    accentColor: Color,
    modifier: Modifier = Modifier,
) {
    val focus = snapshot?.focusIndex?.toInt() ?: 0
    val active = snapshot?.activeIndex?.toInt()
    val inGap = snapshot?.inInstrumentalGap ?: false

    val start = (focus - 2).coerceAtLeast(0)
    val end = (focus + 4).coerceAtMost(document.lines.size)

    Column(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 24.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.Center,
    ) {
        if (inGap) {
            InstrumentalDots(posSec = posSec, accentColor = accentColor)
            Spacer(modifier = Modifier.height(10.dp))
        }

        for (idx in start until end) {
            val line = document.lines[idx]
            val isActive = active == idx && !inGap
            val dist = abs(idx - focus)

            if (isActive && snapshot != null) {
                Column(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalAlignment = if (line.oppositeAligned) Alignment.End else Alignment.Start,
                ) {
                    SyllableLine(
                        syllables = snapshot.leadSyllables,
                        fontSize = 27.sp,
                        fontWeight = FontWeight.Bold,
                        litColor = Color.White,
                        unlitColor = Color.White.copy(alpha = 0.30f),
                        rightAlign = line.oppositeAligned,
                        liftActive = true,
                    )

                    for (bg in snapshot.backgroundLines) {
                        Spacer(modifier = Modifier.height(4.dp))
                        SyllableLine(
                            syllables = bg,
                            fontSize = 17.sp,
                            fontWeight = FontWeight.SemiBold,
                            litColor = Color(0xFF9BE7FF),
                            unlitColor = Color(0xFF9BE7FF).copy(alpha = 0.28f),
                            rightAlign = line.oppositeAligned,
                            liftActive = false,
                        )
                    }
                }
            } else {
                val (fontSize, baseAlpha) = when (dist) {
                    0 -> 23.sp to 0.72f
                    1 -> 19.sp to 0.44f
                    2 -> 16.5.sp to 0.24f
                    else -> 15.sp to 0.14f
                }
                val isPast = active != null && idx < active
                val finalAlpha = if (isPast) baseAlpha * 0.72f else baseAlpha

                Text(
                    text = line.text,
                    fontSize = fontSize,
                    fontWeight = FontWeight.SemiBold,
                    color = Color.White.copy(alpha = finalAlpha),
                    lineHeight = fontSize * 1.25f,
                    textAlign = if (line.oppositeAligned) TextAlign.End else TextAlign.Start,
                    modifier = Modifier.fillMaxWidth(),
                )
            }

            if (idx + 1 < end) {
                Spacer(modifier = Modifier.height(16.dp))
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun SyllableLine(
    syllables: List<FfiSyllableProgress>,
    fontSize: TextUnit,
    fontWeight: FontWeight,
    litColor: Color,
    unlitColor: Color,
    rightAlign: Boolean,
    liftActive: Boolean,
) {
    val wordGroups = remember(syllables) { expandForWrapping(syllables) }
    val horizontalSpacing = (fontSize.value * 0.26f).dp

    FlowRow(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = if (rightAlign) {
            Arrangement.spacedBy(horizontalSpacing, Alignment.End)
        } else {
            Arrangement.spacedBy(horizontalSpacing, Alignment.Start)
        },
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        for (group in wordGroups) {
            Row(verticalAlignment = Alignment.Bottom) {
                for (syl in group) {
                    val pct = syl.progress.coerceIn(0.0, 1.0).toFloat()
                    val sweeping = liftActive && pct > 0f && pct < 1f
                    Box(
                        modifier = if (sweeping) Modifier.offset(y = (-1.5).dp) else Modifier
                    ) {
                        Text(
                            text = syl.text,
                            fontSize = fontSize,
                            fontWeight = fontWeight,
                            color = unlitColor,
                            softWrap = false,
                        )
                        Text(
                            text = syl.text,
                            fontSize = fontSize,
                            fontWeight = fontWeight,
                            color = litColor,
                            softWrap = false,
                            modifier = Modifier.drawWithContent {
                                if (pct > 0f) {
                                    clipRect(right = size.width * pct) {
                                        this@drawWithContent.drawContent()
                                    }
                                }
                            },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun InstrumentalDots(posSec: Double, accentColor: Color) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        for (i in 0..2) {
            val phase = (sin(posSec * 3.5 - i * 0.9).toFloat() * 0.5f + 0.5f)
            val alpha = 0.25f + phase * 0.75f
            val dotSize = (7f + phase * 3f).dp
            Box(
                modifier = Modifier
                    .size(dotSize)
                    .clip(CircleShape)
                    .background(accentColor.copy(alpha = alpha))
            )
        }
    }
}
