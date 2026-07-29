package com.armeafamily.shepherd.media;

import android.app.NativeActivity;
import android.os.Bundle;
import android.util.Log;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.ViewGroup;
import android.widget.FrameLayout;

/**
 * Hosts the Rust cdylib exactly as {@link NativeActivity} would, plus a
 * {@link SurfaceView} for video underneath the activity's own window.
 *
 * <p>Why this exists: mpv can decode straight into an {@code android.view.Surface}
 * with {@code --vo=mediacodec_embed --hwdec=mediacodec --wid=<surface>}, which
 * keeps frames on the GPU and lets SurfaceFlinger put them on a hardware overlay
 * plane. The alternative — the libmpv GL render API, which is what this app used
 * before — can only reach {@code mediacodec-copy}: every decoded frame is copied
 * back into system RAM and re-uploaded as a texture. On a Fire TV that copy is
 * the dominant cost of playback (issue #115).
 *
 * <p>A plain {@code NativeActivity} has one surface, so there is nowhere to put
 * the video. This subclass adds a second one. Android composites a SurfaceView's
 * surface <em>behind</em> the activity window, so for the video to be visible the
 * window is declared translucent in {@code Theme.ShepherdMedia} and the GL side
 * leaves the video area unpainted — see {@code playback.rs}, which clears to
 * transparent and draws only the transport overlay.
 *
 * <p><b>Do not make this class implement {@link SurfaceHolder.Callback}.</b>
 * {@code NativeActivity} already implements {@code SurfaceHolder.Callback2} and
 * relies on those methods to hand the <em>window's</em> surface to native code.
 * Overriding them without chaining starves the native side of its surface: no GL
 * context is ever created and the app exits a few milliseconds after launch,
 * with nothing in the log to say why. The video SurfaceView gets its own
 * separate callback object below for exactly that reason.
 *
 * <p>Everything else about {@code NativeActivity} is inherited untouched: the
 * {@code android.app.lib_name} meta-data still names the Rust library, and
 * {@code ANativeActivity_onCreate} still comes from android-activity's
 * native-activity backend.
 */
public class ShepherdMediaActivity extends NativeActivity {

    private static final String TAG = "shepherd-media";

    /** The video surface, or null whenever one does not currently exist. */
    private volatile Surface videoSurface;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        SurfaceView videoView = new SurfaceView(this);
        // A separate callback object, never the activity itself — see above.
        videoView.getHolder().addCallback(new SurfaceHolder.Callback() {
            @Override
            public void surfaceCreated(SurfaceHolder holder) {
                videoSurface = holder.getSurface();
                Log.i(TAG, "video surface created: " + videoSurface);
            }

            @Override
            public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
                videoSurface = holder.getSurface();
            }

            @Override
            public void surfaceDestroyed(SurfaceHolder holder) {
                Log.i(TAG, "video surface destroyed");
                videoSurface = null;
            }
        });

        // Leave the default Z order: the surface belongs behind the window, not
        // above it. setZOrderMediaOverlay/setZOrderOnTop would put the video
        // over the transport overlay instead of under it.
        ViewGroup content = findViewById(android.R.id.content);
        content.addView(
                videoView,
                0,
                new FrameLayout.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.MATCH_PARENT));
    }

    /**
     * The current video surface, or null if none exists yet or it has been
     * destroyed. Called from Rust over JNI; see {@code surface.rs}.
     *
     * <p>Callers must not hold the returned reference past a surface
     * destruction — the Rust side re-reads this before each playback and
     * detaches mpv when playback ends.
     */
    public Surface getVideoSurface() {
        return videoSurface;
    }
}
