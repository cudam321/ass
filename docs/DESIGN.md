# Ass: design

How Ass looks, and why. Updated whenever a design decision changes. Read it before any visual work.

The working source is the prototype, `design/shell-prototype.html`: one page that opens by itself in any browser (`open design/shell-prototype.html`) and carries the theme, the dot art and the mark's drawing code. The shell page (`src/shell_ui/`) and the start page (`src/start_ui/`) are built from it.

## Principles

- Black and white is the default theme. Marks, icons and art must work in 1-bit and in any single color.
- The user can change all of it. Every color, and the dot art, comes from theme tokens. Nothing is hard-coded in a component.
- Imagery is built from dots. It must not look like another company's dot identity.
- Nothing generic, and nothing decorative without a reason. A design is rendered and looked at before anyone calls it done.
- Arc is the reference for behaviour and feel: what each feature does, and the detail that makes it feel right. Never Arc's names, look or assets. The brand is ours.
- One brand everywhere. Colors, type, spacing, motion and wording come from one source, and every screen is built from it. We are not there: the shell page's styles were ported from the prototype by hand, and the two share no token file.

## Decisions

All of this is in the prototype.

### Theme

- Two colors draw everything: ink and ground. The default pair is black and white and follows the system light or dark setting.
- Every other color (borders, hover, muted text, raised surfaces) is mixed from those two. No third color is stored anywhere.
- The user can pick any pair. The contrast ratio is shown live, with a warning under 4.5 to 1.
- Corner radius, dot pattern, dot shape and dot density are settings too.

### Dot art

- Our own canvas renderer. Patterns: honeycomb (default), matrix, spiral. Dot shape: round or square. The pattern buttons show a plain round patch of each grid, not a small mark.
- A dot's area follows how much light reaches its patch of the form, as if the outline were inflated like a cushion. The resting light is mostly from the front.
- A dot on the edge is drawn smaller in proportion to how much of its cell the form covers. This is what keeps the outline true.
- Motion carries state. On the start page the light follows the pointer. One fast-turning light means loading, and two lights turning opposite ways mean an agent is at work. With reduced motion, everything is still.
- Verified by rendering each grid against the solid shape: a diagonal (45 degree) grid cuts the round end to a point and the form reads as a heart. Honeycomb keeps it round. Tested lobes up, before the mark was turned over. The honeycomb grid is the same upside down, and the turned mark was rendered on it at every size.

### The mark

- The outline is a construction, so it can be redrawn exactly: 1.18 times as wide as tall; one bowl (superellipse, exponent 2.5) on top of two lobes (superellipses, exponent 2.4); all three meet at the widest point, 60% of the height from the top. The cleft is at the bottom center.
- It was first drawn lobes up and turned over on 2026-11-23: the same construction mirrored top to bottom. The code still builds it lobes up and turns it over. "The turn" below means that day's change. Proportions, cleft depths, the size rule and the light did not change with it.
- The cleft is 15% of the height when the mark is filled in at 32 pixels and up, and in dot art finer than 13 across. It is drawn that deep so that the two lobes read.
- In the dotted icon (11 across) the cleft is 19%, so that it is two rows of dots deep.
- In an icon of 96 pixels and up (the mark itself at least 80 pixels wide): drawn in dots (honeycomb, 11 across, every dot full size except at the edge).
- Smaller than that: filled in. Dots blur there. The cleft is never less than 3.5 pixels deep, so it gets deeper as the mark gets smaller (30% at most).
- The small marks in the address field and on a tab are always filled in. While a page loads or an agent works they fade in and out, and never turn into dots.
- On the agent screen the mark is 96 pixels wide, in dots. Two lights turn in it while the agent works.
- Measured after the turn, 2026-11-23: width over height 1.18; half-width 0.67 at 10% of the height, 0.80 at 17%, 0.94 at 33%, 1.00 at 50%, 0.93 at 84%; cleft 15% of the height, measured from the bottom. These are the earlier figures mirrored.
- Rendered after the turn: every size from 144 down to 16 on both grounds, the dotted ones with the outline of the filled-in shape drawn over them; and each small size alone, magnified, at 1x and 2x: 48, 32, 24, 18 and 16 on the plate, and the bare mark at 28, 20, 16, 14 and 12. The cleft shows in each.
- In dots, the cleft is never less than two rows deep: 19% at 11 across, 16% at 13 across, 15% from there up.
- The mark always uses round dots on the honeycomb grid, whatever the user chose for the art. It needs one color.

### The app icon (2026-11-24)

- Made by `design/app-icon/build.sh` from `design/app-icon/icon.html`. The page carries the prototype's drawing code unchanged and names the line ranges it copied. Headless Chrome draws the PNGs and `iconutil` packs `resources/mac/Ass.icns`. Two runs give the same file (Chrome 158).
- First version: a black plate with a white mark. The plate's colour is not settled. The same page draws a white plate with a black mark (`?variant=light`). That one is a preview and is not in the app. Both, side by side on a dark and a light ground: `design/app-icon/black-and-white.png`.
- The plate: Apple's grid for a macOS app icon, a 1024 canvas with a plate of 824 and continuous corners of radius 185.4. The corner's control points are the widely published reconstruction, not an Apple file. The plate's margin is rounded down to a whole pixel at each size, which is the plate macOS 27.2 draws itself (measured at six sizes).
- The mark fills 84% of the plate's width, as in the prototype's own app icon. Rendered at 88%, the side dots crowd the plate's edge.
- Dots or filled in: the 16, 32, 64 and 128 pixel images are filled in, and 256 and up are in dots. The 128 image would pass the size rule (its mark is 87 pixels wide), but macOS shrinks that image for every icon of 36 to 48 points on a 2x screen, the Dock among them, and the dots then fall under the rule and blur. Rendered both ways and looked at. So an icon of up to 64 points on a 2x screen shows the filled-in mark, the Dock among them. From 68 points up macOS takes the 256 image and the mark is in dots, at least 92 pixels wide (read back for the real app at 68, 72, 76, 80 and 96 points).
- What macOS 27.2 does with it, read back through `NSWorkspace` for the built app and not looked at on a screen: at 64 pixels and up, and at every size on a 2x screen, it keeps our plate, cuts it with its own mask, and adds a light rim, a soft shadow and a slight shading over the mark. At 16 and 32 pixels on a 1x screen it sets the icon inside a grey plate of its own, as it does for three other apps on the same Mac that ship the same kind of icon file. A full-bleed square is shrunk onto the system's plate, so the mark gets smaller. An icon file without the small images is shrunk into mush.
- The way out of that grey plate is Apple's newer icon format. It is compiled by a tool that comes with Xcode, which our build does not need (`docs/ARCHITECTURE.md`, D3). Not done.
- At 16 pixels the cleft is a notch two pixels deep. It reads, and it is the weakest size.

### Checked, and not checked

- Checked in our own browser, before the turn (see "The mark"): dark, light, two custom color pairs, phone width; 28 interaction checks pass; no sideways scrolling; no console errors.
- Checked after the turn, in headless Chrome 158 (no window): the four screens and the command bar in dark and light, the four screens at phone width in both; no sideways scrolling; no console errors. The 28 interaction checks were not rerun, because the turn changes one drawing function and no interaction.
- Not checked: other browsers.

## Rejected

- 2026-11-23: three brand directions (a donkey mark, a monospace `ass://` wordmark, a lilac rounded look). Do not revive any part of them.
- 2026-11-23: a dotted sphere as the mark. A circle could be anyone's.
- 2026-11-23: dots in the mark under 96 pixels. They blur and the shape is lost.
- 2026-11-23, after rendering: a diagonal dot grid for the mark (reads as a heart); a seam line down from the cleft; an outline made of blended circles (kinks in the sides); random dot glyphs as tab icons (looked like noise).
