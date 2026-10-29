# The home screen: editing it, and the App Library

Moon's home screen works as an iPhone's does: the apps can be moved around
the grid and the dock, taken off the home screen, and brought back from the
App Library. The code is `src/a26-shell/src/home.rs` (layout, gestures,
motion) and `ui.rs` (drawing); the layout is a Moon setting
(`settings.md`).

## The home page

Two widgets (the date and the battery) span the top. Under them is a grid
of four columns and up to four rows of apps with their names, and at the
bottom the dock: up to four apps without names, centered as a group. By
default the dock holds Phone, Browser, Messages and System, and the grid
Exocortex.

A tap opens an app: its card grows from where its icon is now. Going home
zooms the app back into its icon (or, for an app not on the home screen,
shrinks it away in the middle of the screen).

## Edit mode ("jiggle" mode)

| Do this | And this happens |
| --- | --- |
| Hold an app icon (grid or dock) or empty space for 0.5 s | Edit mode: every icon jiggles, each a little out of phase, with a "−" badge at its top-left, and **Done** shows at the top right: iOS's light pill in a row of its own under the status bar, its right edge on the grid's right margin (a swipe down from the status bar above it still opens Control Centre). A tap haptic plays (when `haptics` is on) |
| Keep holding and move | The held icon lifts (a little larger, under the finger) and drags at once |
| In edit mode, touch an icon and move | It drags |
| Drag over the grid or the dock | After a short rest over a place (90 ms), the other icons spring out of the way to leave it room. Grid ↔ dock both ways; a full dock (four) takes no fifth app |
| Let go | The icon settles into its place (a haptic) and the layout is saved |
| Tap a "−" badge | The alert **Remove “App”?** — *Removing from Home Screen will keep the app in your App Library.* — with **Remove from Home Screen** and **Cancel** |
| Remove from Home Screen | The icon shrinks away, the others close the gap, the layout is saved |
| **Done**, a tap on empty space, the swipe up from the bottom edge, `ctl home` | Edit mode ends: the jiggle and the badges fade out |
| The power key | The phone locks, as always; edit mode ends with it (a drag in progress keeps the place it was headed for) |

Apps here are built in and cannot be uninstalled: removing takes an app off
the home screen only.

## The App Library

Swipe left on the home page. The App Library is a search field (for looks;
there is no search yet) and every app by name, each with its icon. The
dock slides away and the wallpaper darkens; swipe right, or up from the
bottom edge, to go back.

| Do this | And this happens |
| --- | --- |
| Tap an app | It opens, its card growing from its row's icon (and closes back into it) |
| Hold an app that is not on the home screen | A menu: **Add to Home Screen** (it goes to the grid's first free place) |
| Hold any app and move | The home page comes back in edit mode with the app under the finger: drop it anywhere on the grid or the dock |

## Control socket

`layout ...` (see `settings.md`): `reset` (Reset Home Screen Layout),
`set DOCK GRID`, `remove APP`, `add APP`, `edit on|off`, `library on|off`.
`state` shows the layout as `settings.home` and the screen as `home`:

```json
"home": {"editing": false, "page": "home", "dragging": null, "alert": null, "menu": null}
```

`page` is `home` or `library`; `dragging`, `alert` and `menu` name the app
dragged, asked about, or whose App Library menu is open. `last_action`
reports the steps: `home_edit`, `home_drag`, `home_drop`,
`home_remove_alert`, `home_removed`, `home_remove_cancel`, `home_edit_done`,
`app_library`, `home_page`, `library_menu`, `home_added`.

## Drawing

The plain home page stays one CPU-drawn layer (wallpaper, widgets, dock,
icons, names, status bar), redrawn only when something on it changes. While
the home screen is *live* (edit mode, the App Library, a page swipe, icons
moving, the alert or the menu) the frame is composed from parts instead,
each a layer drawn once: the wallpaper, the widgets, the dock's shelf, the
status bar, the App Library, the Done button and the badge. Every icon is
its own compositor node, as is its name and its badge: nothing is
re-rasterized per frame, on the GPU compositor and on the XRender path
alike.

The compositor has no rotation, so the jiggle uses sprites: for each app,
its icon and shadow pre-turned 11 ways (±0.03 rad) side by side in one
layer (drawn in idle time after startup); a frame shows the sprite frame
nearest the icon's angle, and its badge and name follow the turn. The
unturned frame is drawn exactly as the home page draws the icon, so edit
mode starts and ends without a visible step (a unit test checks this).
Node counts stay fixed while live, so showing or hiding an icon never
restructures a frame.

Not done yet: widgets do not jiggle and cannot be removed; there is one
home page (no page dots, no dragging to a new page); the App Library has no
search or categories; a **Reset Home Screen Layout** entry in the System
app (today: `ctl layout reset`).
