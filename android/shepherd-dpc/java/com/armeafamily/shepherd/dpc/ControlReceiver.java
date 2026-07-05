package com.armeafamily.shepherd.dpc;

import android.app.admin.DevicePolicyManager;
import android.content.BroadcastReceiver;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.util.Log;

/**
 * Clears the Lock Task allowlist so a pinned session can be ended:
 *
 *   am broadcast -n com.armeafamily.shepherd.dpc/.ControlReceiver --es action unlock
 *
 * Removing the foreground app's package from the allowlist makes the framework
 * exit Lock Task for it, after which shepherd can close the window / force-stop
 * normally.
 */
public class ControlReceiver extends BroadcastReceiver {
    private static final String TAG = "ShepherdDPC";

    @Override
    public void onReceive(Context ctx, Intent intent) {
        DevicePolicyManager dpm =
                (DevicePolicyManager) ctx.getSystemService(Context.DEVICE_POLICY_SERVICE);
        ComponentName admin = new ComponentName(ctx, AdminReceiver.class);
        if (dpm == null || !dpm.isDeviceOwnerApp(ctx.getPackageName())) {
            return;
        }
        if ("unlock".equals(intent.getStringExtra("action"))) {
            dpm.setLockTaskPackages(admin, new String[] {});
            Log.i(TAG, "cleared Lock Task allowlist");
        }
    }
}
