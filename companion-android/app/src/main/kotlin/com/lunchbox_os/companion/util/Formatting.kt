package com.lunchbox_os.companion.util

import com.lunchbox_os.companion.domain.DurationSecs
import com.lunchbox_os.companion.domain.IsoTimestamp
import java.time.Duration
import java.time.OffsetDateTime
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/** Parsing/formatting helpers for wire timestamps and durations. */
object Formatting {

    private val timeFormatter: DateTimeFormatter =
        DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT)
    private val dateTimeFormatter: DateTimeFormatter =
        DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT)

    fun parse(ts: IsoTimestamp?): OffsetDateTime? =
        ts?.let { runCatching { OffsetDateTime.parse(it) }.getOrNull() }

    /** "5:07 PM" — just the clock time of a timestamp. */
    fun clock(ts: IsoTimestamp?): String =
        parse(ts)?.toLocalTime()?.format(timeFormatter) ?: "—"

    /** "Jun 21, 5:07 PM" — date + time. */
    fun dateTime(ts: IsoTimestamp?): String =
        parse(ts)?.toLocalDateTime()?.format(dateTimeFormatter) ?: "—"

    /** Seconds → "MM:SS" or "H:MM:SS". */
    fun hms(totalSeconds: Long): String {
        val s = totalSeconds.coerceAtLeast(0)
        val hours = s / 3600
        val minutes = (s % 3600) / 60
        val seconds = s % 60
        return if (hours > 0) {
            "%d:%02d:%02d".format(hours, minutes, seconds)
        } else {
            "%d:%02d".format(minutes, seconds)
        }
    }

    /** Seconds → coarse human duration like "1h 30m" / "45m" / "30s". */
    fun coarse(totalSeconds: Long): String {
        val s = totalSeconds.coerceAtLeast(0)
        val hours = s / 3600
        val minutes = (s % 3600) / 60
        return when {
            hours > 0 && minutes > 0 -> "${hours}h ${minutes}m"
            hours > 0 -> "${hours}h"
            minutes > 0 -> "${minutes}m"
            else -> "${s}s"
        }
    }

    fun coarse(duration: DurationSecs?): String =
        duration?.let { coarse(it.secs) } ?: "—"

    /** Whole seconds between now and a future deadline, clamped at 0. */
    fun secondsUntil(deadline: IsoTimestamp?): Long? {
        val target = parse(deadline) ?: return null
        return Duration.between(OffsetDateTime.now(), target).seconds.coerceAtLeast(0)
    }
}
