package com.lunchboxos.media;

import android.app.NativeActivity;
import android.os.Bundle;
import android.util.Log;
import android.view.Gravity;
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
 * window is declared translucent in {@code Theme.LunchboxMedia} and the GL side
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
 *
 * <p><b>The video's shape is this view's size, not mpv's problem.</b> Under
 * {@code mediacodec_embed} the decoder scales its output to fill whatever
 * Surface it is given, and mpv's {@code --keepaspect} never gets a look in
 * because no pass under mpv's control draws the frame. A {@code MATCH_PARENT}
 * SurfaceView therefore stretches every video to the shape of the display — a
 * 16:9 video on a 2424x1080 phone came out 26% too wide. {@link #setVideoBounds}
 * sizes and places the view at the rectangle the video should occupy instead.
 *
 * <p>Rust works that rectangle out rather than this class measuring itself to an
 * aspect, because the letterbox bars around it have to be painted there anyway:
 * {@code NativeActivity} hands the window's surface to the native renderer with
 * {@code getWindow().takeSurface}, so the Java view hierarchy is never drawn and
 * a background set here would never appear. One side owning the geometry keeps
 * the bars and the video from disagreeing about where the edge is. See
 * {@code surface.rs::fit_video} and {@code playback.rs}.
 */
public class LunchboxMediaActivity extends NativeActivity {

    private static final String TAG = "lunchbox-media";

    /** The video surface, or null whenever one does not currently exist. */
    private volatile Surface videoSurface;

    /** The view that surface belongs to, so its bounds can be updated. */
    private SurfaceView videoView;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        videoView = new SurfaceView(this);
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
        //
        // Starts filling the window, and is resized to the video's shape by
        // setVideoBounds once a file is open.
        ViewGroup content = findViewById(android.R.id.content);
        content.addView(
                videoView,
                0,
                new FrameLayout.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.MATCH_PARENT));
    }

    /**
     * Place the video surface at this rectangle, in window pixels.
     *
     * <p>Called from Rust over JNI whenever the rectangle changes: a file
     * opening, its dimensions changing, or the window being resized. A
     * non-positive size means "not known", and the view goes back to filling the
     * window — which is what it should do before any file is open.
     */
    public void setVideoBounds(final int x, final int y, final int width, final int height) {
        final SurfaceView view = videoView;
        if (view == null) {
            return;
        }
        runOnUiThread(() -> {
            FrameLayout.LayoutParams params;
            if (width > 0 && height > 0) {
                params = new FrameLayout.LayoutParams(width, height, Gravity.TOP | Gravity.START);
                params.leftMargin = x;
                params.topMargin = y;
            } else {
                params = new FrameLayout.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        Gravity.TOP | Gravity.START);
            }
            view.setLayoutParams(params);
        });
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
