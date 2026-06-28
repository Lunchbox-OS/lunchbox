package com.shepherd.dpc;

import android.app.Activity;
import android.app.ActivityOptions;
import android.app.admin.DevicePolicyManager;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.os.Bundle;
import android.util.Log;

/**
 * No-UI entry point shepherd invokes to launch a kiosk app pinned in Lock Task
 * Mode:
 *
 *   am start -n com.shepherd.dpc/.LaunchActivity --es pkg com.android.calculator2
 *
 * As device owner it allowlists the target package for Lock Task, then launches
 * it with {@link ActivityOptions#setLockTaskEnabled(boolean)} so the target is
 * pinned even though it never calls {@code startLockTask()} itself. While
 * pinned, Home/Recents and launching other apps are blocked at the framework
 * level — the gold-standard containment beyond the statusbar `lock_down`.
 */
public class LaunchActivity extends Activity {
    private static final String TAG = "ShepherdDPC";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        String pkg = getIntent().getStringExtra("pkg");
        DevicePolicyManager dpm =
                (DevicePolicyManager) getSystemService(Context.DEVICE_POLICY_SERVICE);
        ComponentName admin = new ComponentName(this, AdminReceiver.class);

        if (pkg == null || pkg.isEmpty()) {
            Log.w(TAG, "LaunchActivity: missing 'pkg' extra");
            finish();
            return;
        }
        if (dpm == null || !dpm.isDeviceOwnerApp(getPackageName())) {
            Log.w(TAG, "LaunchActivity: not device owner; run dpm set-device-owner first");
            finish();
            return;
        }

        // Allowlist the target (and ourselves) so it may enter Lock Task.
        dpm.setLockTaskPackages(admin, new String[] { pkg, getPackageName() });

        Intent launch = getPackageManager().getLaunchIntentForPackage(pkg);
        if (launch == null) {
            Log.w(TAG, "LaunchActivity: no launch intent for " + pkg);
            finish();
            return;
        }
        launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK);

        ActivityOptions opts = ActivityOptions.makeBasic();
        opts.setLockTaskEnabled(true);
        startActivity(launch, opts.toBundle());
        Log.i(TAG, "LaunchActivity: launched " + pkg + " in Lock Task Mode");

        finish();
    }
}
