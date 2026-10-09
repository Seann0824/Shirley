# Shirley desktop artwork

Revised on 2026-10-09 using the built-in `image_gen` tool. No fallback API was used. These are AI-generated illustrations of Shirley Fenette from Code Geass, based on the [official first-season character sheet](https://geass.jp/first/chara_06.html) ([image](https://geass.jp/first/img/chara_06.jpg)), especially the face in its lower-left animation still.

The first set was rejected: the logo covered one eye with redesigned hair, the app icon had an unwanted green border, and its generated transparency mask left visible defects in the light background. Those images have been replaced. Header, favicon and avatar now share the corrected front-facing portrait. The welcome illustration was also regenerated against that face reference.

The user subsequently requested removal of the desktop wallpaper. The app now uses its original solid canvas background, with the centered conversation, header, input, welcome illustration and spacing retained. The generated wallpaper and its approved reference (`exec-2ed7de40-0e92-4d33-a873-af0270dfe554.png`) are preserved only as source artwork in this directory; they are no longer exported or referenced by the app.

## Source and exports

| Source | Export | Use |
| --- | --- | --- |
| `shirley-app-icon.png` | `../web/public/brand/shirley-logo.png` (256 × 256) | Header and favicon |
| `shirley-app-icon.png` | `../web/public/brand/shirley-avatar.png` (256 × 256) | Chat avatar |
| `shirley-app-icon.png` | Root `icons/` | Native desktop icons |
| `shirley-character.png` | `../web/public/brand/shirley-character.png` (768 × 1152) | Original welcome illustration |
| `shirley-desktop-background.png` | Not exported | Retained artwork (1672 × 941), unused by the app |

The icon source has an opaque light background with no border. Exports use RGBA PNGs with alpha fixed at 255 to satisfy Tauri's format requirement without a generated transparency mask. The character illustration retains transparency. Native icons include 32px, 128px, 256px and 1024px PNGs, macOS ICNS, and Windows ICO entries at 16/24/32/48/64/128/256px.

Re-export on macOS from the repository root: `node scripts/generate-desktop-brand.mjs`. It uses Swift, sips and iconutil, with no image-service calls. The existing `bundle.active: false` setting still applies; exporting does not create an installable app bundle.

## Manual acceptance — pending user verification

1. Run `npm --prefix src/interface/desktop/web run build`, then `cargo desktop`. Expect the corrected portrait in the header, with both eyes visible, no green border, and no background holes.
2. Create a conversation. Expect the original solid light background and welcome illustration. Chat and input remain centered, capped at the original 760px width. At the 480 × 360 minimum window size, input and scrolled content should remain accessible. There should be no wallpaper or new panel, blur, border or shadow.
3. Send a message. Expect the corrected avatar. While streaming, a small neutral status dot should pulse at its bottom right, with no green spinning ring; the dot should disappear on completion.
4. Build and restart the relevant native app bundle; inspect Dock/Finder or Explorer/taskbar. Expect a borderless portrait with no transparent blocks in its background. UI appearance and native platform effects require user verification.
5. Send a long message and scroll through a long reply containing a code block. The background should remain solid. Open model/session selectors and the `@` file popover: menus should remain visible and clickable. UI appearance has not been manually accepted by the user.

## Final built-in generation prompts

### Corrected icon, avatar and logo

```text
Create a square desktop application icon showing Shirley Fenette (シャーリー・フェネット) from Code Geass, precisely matching the attached OFFICIAL reference. Use the face in the lower-left animation still as the face model. Preserve her original Code Geass angular slender face, tall green oval eyes, distinctive separated orange forehead fringe and long straight copper-orange hair. Both eyes fully visible, no hair covering either eye. A small gentle smile, pale-yellow Ashford uniform, dark green necktie. Front-facing head and shoulders, original 2006 TV anime cel drawing style, clean thin outlines and flat colors, not chibi or generic modern anime. Hair has 10% space above it. Completely plain solid opaque warm ivory square background. NO green border, NO circle, NO frame, NO transparency, NO glitter, NO noise, NO mosaic, NO text or watermark. A single polished high-resolution icon.
```

### Corrected character illustration

```text
Use case: identity-preserve
Asset type: transparent character illustration for Shirley desktop welcome page.
Inputs: image 1 is the OFFICIAL Shirley Fenette character sheet from Code Geass; image 2 is the corrected portrait we just created. Match Shirley's character identity and the frontal face of image 2 precisely.
Primary request: one accurate Shirley Fenette, original Code Geass TV anime appearance, standing gently smiling, both green eyes visible, distinctive separated orange forehead fringe, long straight copper-orange hair flowing behind her shoulders to below the waist. Narrow angular face and tall oval green eyes exactly as official reference. NO single-eye hairstyle, NO braided hair, NO giant curled S hair.
Clothing: faithful pale butter-yellow Ashford Academy fitted uniform jacket with BLACK lapel piping and BLACK cuffs, white shirt, dark green necktie with small gold ornament near knot, short black pleated skirt. Keep official uniform details.
Pose/composition: portrait 2:3 canvas, single character from complete head down to mid-thigh, straight relaxed stance at a slight three-quarter angle, arms naturally down, gentle cheerful expression; ample clear margin above hair and at sides; no other objects.
Style: clean ORIGINAL 2006 Code Geass anime cel drawing style, fine dark lines, flat two-tone shading, no modern doll gloss, no gradient glows.
Background: clean true transparent RGBA alpha cutout only outside character. Solid opaque character interiors. No backdrop, no glow, no shadow, no checkerboard, no texture, no speckles, no noise, no mosaic, no holes inside the hair/face/clothes.
Constraints: no text, no badge, no border, no watermark, one character only.
```

### Archived wallpaper prompt (unused by the app)

```text
Use case: identity-preserve
Asset type: full-window landscape wallpaper for the Shirley desktop chat app.
Input image: the attached approved portrait is the edit target and the authoritative character reference. The user explicitly selected THIS drawing. Preserve this exact face, expression, green eyes, side-swept orange hair, pale-yellow uniform with dark piping and green tie, slim proportions, hands and gentle pose. Do not replace her with a different interpretation, do not change hairstyle, do not redraw a different face.
Primary request: expand the approved portrait into a finished wide 16:9 desktop wallpaper, high-resolution landscape 2560x1440 composition. Place the character in the RIGHT third of the wallpaper, complete head and hair comfortably inside the frame with margin above, portrait visible from head to upper thigh, hands visible. Keep the character as crisp and polished as the reference, clean anime cel art.
Scene/backdrop: extend the portrait's warm amber/copper glow into a subtle atmospheric charcoal-brown backdrop across the entire wide canvas; warm soft halo behind the character, dark softly lit left side. Minimal unobtrusive abstract studio backdrop, very restrained smooth gradients, no new story objects, no room furniture, no scenery.
Composition: LEFT 60 percent of canvas is clean low-detail negative space reserved for an app's conversation UI. The character occupies roughly the rightmost 35 percent. Her face must stay in the right third and upper middle, comfortably inset from the right and top edges. This is a single seamless wallpaper, no seams or panels.
Constraints: fully OPAQUE image, no transparency, no checkerboard, no texture noise, no speckles, no mosaic, no frame, no border, no logo, no letters, no watermark, no user-interface elements, no extra characters. Keep the reference character's identity and original drawing style.
```
