package com.armeafamily.shepherd.dpc;

import android.app.admin.DeviceAdminReceiver;

/**
 * The device-admin component. Set as device owner via
 * {@code adb shell dpm set-device-owner com.armeafamily.shepherd.dpc/.AdminReceiver}.
 * No behavior of its own — its existence is what lets the app act as a DPC.
 */
public class AdminReceiver extends DeviceAdminReceiver {
}
