"""Translate the #217 hand-off's Android adaptive-icon SVGs into VectorDrawables.

VectorDrawable has no <rect>, <circle> or <mask>, so rects become rounded-rect
paths and the monochrome masks become even-odd holes in the tin's path.

    python3 vector_drawables.py OUT_DIR

writes companion_fg.xml, companion_mono.xml, media_fg.xml and media_mono.xml,
which are the apps' res/drawable/ic_launcher_{foreground,monochrome}.xml.
"""
import math, sys

INK, ENAMEL, WELL, YELLOW, LIP = "#1C1B18", "#2FB5A5", "#F7F5EE", "#FFD166", "#241C1B18"


def f(v):
    s = f"{v:.2f}".rstrip("0").rstrip(".")
    return s


def rrect(x, y, w, h, r, ccw=False):
    """Rounded rect as path data, clockwise (screen coords) unless ccw."""
    if not ccw:
        return (f"M{f(x+r)},{f(y)} H{f(x+w-r)} A{f(r)},{f(r)} 0 0,1 {f(x+w)},{f(y+r)} "
                f"V{f(y+h-r)} A{f(r)},{f(r)} 0 0,1 {f(x+w-r)},{f(y+h)} "
                f"H{f(x+r)} A{f(r)},{f(r)} 0 0,1 {f(x)},{f(y+h-r)} "
                f"V{f(y+r)} A{f(r)},{f(r)} 0 0,1 {f(x+r)},{f(y)} Z")
    raise NotImplementedError


def circle(cx, cy, r):
    return f"M{f(cx-r)},{f(cy)} A{f(r)},{f(r)} 0 1,0 {f(cx+r)},{f(cy)} A{f(r)},{f(r)} 0 1,0 {f(cx-r)},{f(cy)} Z"


def lock_hole():
    """Outline of the companion mask's padlock cut-out: the body
    rrect(155,136,56,45,10) united with the shackle, which is
    `M167 136v-12a16 16 0 0 1 31 0v12` stroked 9 wide with round caps.
    Returns the outer outline plus the pocket inside the shackle as a second
    subpath, so that under evenOdd the pocket stays filled."""
    bx, by, bw, bh, br = 155, 136, 56, 45, 10
    half = 4.5
    # Arc centre: chord from (167,124) to (198,124), radius 16, bulging up.
    cx = (167 + 198) / 2
    cy = 124 + math.sqrt(16**2 - ((198 - 167) / 2) ** 2)
    ro, ri = 16 + half, 16 - half
    lo, ro_x = 167 - half, 198 + half   # outer leg edges
    li, ri_x = 167 + half, 198 - half   # inner leg edges
    # Where the outer legs meet the body's rounded top corners.
    ly = by + br - math.sqrt(br**2 - (bx + br - lo) ** 2)
    ry = by + br - math.sqrt(br**2 - (ro_x - (bx + bw - br)) ** 2)
    # Where each leg edge meets the concentric arcs.
    oy = cy - math.sqrt(ro**2 - (cx - lo) ** 2)
    iy = cy - math.sqrt(ri**2 - (cx - li) ** 2)
    outer = (
        f"M{f(bx)},{f(by+bh-br)} V{f(by+br)} A{f(br)},{f(br)} 0 0,1 {f(lo)},{f(ly)} "
        f"V{f(oy)} A{f(ro)},{f(ro)} 0 0,1 {f(ro_x)},{f(oy)} V{f(ry)} "
        f"A{f(br)},{f(br)} 0 0,1 {f(bx+bw)},{f(by+br)} V{f(by+bh-br)} "
        f"A{f(br)},{f(br)} 0 0,1 {f(bx+bw-br)},{f(by+bh)} H{f(bx+br)} "
        f"A{f(br)},{f(br)} 0 0,1 {f(bx)},{f(by+bh-br)} Z"
    )
    pocket = (f"M{f(li)},{f(by)} V{f(iy)} A{f(ri)},{f(ri)} 0 0,1 {f(ri_x)},{f(iy)} "
              f"V{f(by)} Z")
    return outer + " " + pocket


def path(d, fill=None, stroke=None, width=None, join=None, cap=None, even_odd=False, indent="        "):
    a = [f'android:pathData="{d}"']
    if fill:
        a.append(f'android:fillColor="{fill}"')
    if even_odd:
        a.append('android:fillType="evenOdd"')
    if stroke:
        a.append(f'android:strokeColor="{stroke}"')
        a.append(f'android:strokeWidth="{width}"')
    if join:
        a.append(f'android:strokeLineJoin="{join}"')
    if cap:
        a.append(f'android:strokeLineCap="{cap}"')
    inner = ("\n" + indent + "    ").join(a)
    return f"{indent}<path\n{indent}    {inner} />"


def doc(comment, paths):
    body = "\n".join(paths)
    return f'''<?xml version="1.0" encoding="utf-8"?>
<!-- {comment} -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="108dp"
    android:height="108dp"
    android:viewportWidth="108"
    android:viewportHeight="108">
    <!-- The mark is drawn on the hand-off's 256-unit grid and placed at 60dp
         inside the 66dp safe zone: translate(24 24) scale(60/256). -->
    <group
        android:translateX="24"
        android:translateY="24"
        android:scaleX="0.234375"
        android:scaleY="0.234375">
{body}
    </group>
</vector>
'''


def foreground(well_w, marks):
    lip = f"M68,62 h{well_w-36} a18,18 0 0,1 18,18 v10 h-{well_w} v-10 a18,18 0 0,1 18,-18 z"
    return [
        path(rrect(92, 18, 72, 28, 14), fill=INK),
        path(rrect(28, 40, 200, 190, 30), fill=ENAMEL, stroke=INK, width=12),
        path(rrect(50, 62, well_w, 146, 18), fill=WELL),
        path(lip, fill=LIP),
        path(rrect(50, 62, well_w, 146, 18), stroke=INK, width=9),
    ] + marks


companion_fg = foreground(98, [
    path("M78,112 l41,23 -41,23 z", fill=YELLOW, stroke=INK, width=9, join="round"),
    path("M168,137 v-11 a15,15 0 0,1 29,0 v11", stroke=INK, width=9, cap="round"),
    path(rrect(157, 137, 52, 42, 9), fill=YELLOW, stroke=INK, width=9),
    path(circle(183, 154, 6), fill=INK),
    path(rrect(180, 154, 6, 14, 3), fill=INK),
])
media_fg = foreground(156, [
    path("M96,97 l68,38 -68,38 z", fill=YELLOW, stroke=INK, width=9, join="round"),
])

MONO = "#FF000000"
companion_mono = [
    path(rrect(92, 18, 72, 28, 14), fill=MONO),
    path(rrect(22, 34, 212, 202, 36) + " " + rrect(46, 58, 102, 154, 18) + " " + lock_hole(),
         fill=MONO, even_odd=True),
    path("M82,110 l40,25 -40,25 z", fill=MONO),
    path(circle(183, 154, 6), fill=MONO),
    path(rrect(180, 154, 7, 15, 3), fill=MONO),
]
media_mono = [
    path(rrect(92, 18, 72, 28, 14), fill=MONO),
    path(rrect(22, 34, 212, 202, 36) + " " + rrect(46, 58, 164, 154, 18), fill=MONO, even_odd=True),
    path("M94,95 l72,40 -72,40 z", fill=MONO),
]

FG = ("The Lunchbox {name} mark (issue #217): {what}. Translated from\n"
      "     assets/branding/icon/apps/{file}-adaptive-foreground.svg; keep the two in step.")
MO = ("The {name} mark in one colour, for Android 13 themed icons: the wells are\n"
      "     holes, cut with evenOdd because VectorDrawable has no masks. Translated\n"
      "     from assets/branding/icon/apps/{file}-adaptive-monochrome.svg.")

out = {
    "companion_fg": doc(FG.format(name="Companion", file="companion",
                                  what="the tin with its packed column\n     swapped for a padlock"), companion_fg),
    "companion_mono": doc(MO.format(name="Companion", file="companion"), companion_mono),
    "media_fg": doc(FG.format(name="Media", file="media",
                              what="the tin with a single compartment and a\n     large play"), media_fg),
    "media_mono": doc(MO.format(name="Media", file="media"), media_mono),
}
import pathlib
d = pathlib.Path(sys.argv[1])
for k, v in out.items():
    (d / f"{k}.xml").write_text(v)
