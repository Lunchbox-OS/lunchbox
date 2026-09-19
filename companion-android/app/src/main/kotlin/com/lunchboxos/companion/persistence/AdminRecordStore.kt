package com.lunchboxos.companion.persistence

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import com.lunchboxos.companion.domain.DeviceRecord
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json

/**
 * Encrypted-at-rest storage for the list of bonded [DeviceRecord]s.
 *
 * Backed by [EncryptedSharedPreferences] (AES-256 GCM values, keyed by an
 * `AndroidKeystore`-managed master key), so the per-device `http_token`
 * never sits in plaintext on disk. The whole list is serialised to one
 * JSON blob under a single key — the record set is tiny.
 *
 * Backup/transfer extraction is disabled at the manifest level
 * (`data_extraction_rules.xml`) so the keystore-wrapped blob can't be
 * exfiltrated to another device where the key wouldn't exist anyway.
 */
class AdminRecordStore(context: Context) {

    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }
    private val listSerializer = ListSerializer(DeviceRecord.serializer())

    private val prefs: SharedPreferences by lazy {
        val masterKey = MasterKey.Builder(context.applicationContext)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            context.applicationContext,
            PREFS_NAME,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    suspend fun load(): List<DeviceRecord> = withContext(Dispatchers.IO) {
        val raw = prefs.getString(KEY_RECORDS, null) ?: return@withContext emptyList()
        runCatching { json.decodeFromString(listSerializer, raw) }.getOrDefault(emptyList())
    }

    suspend fun save(records: List<DeviceRecord>) = withContext(Dispatchers.IO) {
        prefs.edit()
            .putString(KEY_RECORDS, json.encodeToString(listSerializer, records))
            .apply()
    }

    private companion object {
        const val PREFS_NAME = "lunchbox_admin_records"
        const val KEY_RECORDS = "records"
    }
}
