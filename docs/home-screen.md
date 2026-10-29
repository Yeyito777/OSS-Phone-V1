# The home screen: editing it, and the App Library

Moon's home screen works as an iPhone's does: the apps can be moved around
the grid and the dock, taken off the home screen, and brought back from the
App Library; widgets come off and go back on from a widget gallery. The code is `src/a26-shell/src/home.rs` (layout, gestures,
motion) and `ui.rs` (drawing); the layout is a Moon setting
(`settings.md`).

## The home page

The page is a grid of four columns and six rows of cells under the status
bar, its first row about iOS's top inset (82 pt) from the top edge. An app
takes a cell, with its name under it; a widget (Calendar: the day and the
date; Battery: the charge) takes two by two cells on the top rows, from the
left, and the apps fill the other cells in reading order (beside a single
widget, then below). At the bottom is the dock: up to four apps without
names, centered as a group. By default the page has both widgets, the dock
Phone, Browser, Messages and System, and the grid Exocortex.

An app can put a count on its icon (`badge N`, `app-services.md`): iOS's
red badge at the icon's top-right corner, on the grid, in the dock and in
the App Library, kept while the app is closed.

A tap opens an app: its card grows from where its icon is now. Going home
zooms the app back into its icon (or, for an app not on the home screen,
shrinks it away in the middle of the screen).

## Edit mode ("jiggle" mode)

| Do this | And this happens |
| --- | --- |
| Hold an app icon (grid or dock), a widget or empty space for 0.5 s | Edit mode: every icon and widget jiggles, each a little out of phase, with a "−" badge at its top-left, and **+** and **Done** show in a row of their own under the status bar, + on the grid's left margin and Done on its right one (iOS's light circle and pill), centred between the status bar and the first row's badges (a swipe down from the status bar above them still opens the notifications or Control Centre). A tap haptic plays (when `haptics` is on) |
| Keep holding and move | The held icon lifts (a little larger, under the finger) and drags at once |
| In edit mode, touch an icon and move | It drags |
| Drag over the grid or the dock | After a short rest over a place (90 ms), the other icons spring out of the way to leave it room. Grid ↔ dock both ways; a full dock (four) takes no fifth app |
| Let go | The icon settles into its place (a haptic) and the layout is saved |
| Tap a "−" badge | The alert **Remove “App”?** — *Removing from Home Screen will keep the app in your App Library.* — with **Remove from Home Screen** and **Cancel** |
| Remove from Home Screen | The icon shrinks away, the others close the gap, the layout is saved |
| Tap a widget's "−" badge | The alert **Remove Widget?** — *Removing this widget will not delete any data.* — with **Remove** (red) and **Cancel**. Remove: the widget shrinks away, the apps move up into its place (without widgets, the first row sits under the status bar) |
| Tap **+** | The widget gallery rises from the bottom: each widget with a preview, what it shows, and **Add Widget** (*Added* when it is on the page, *No Room* when the page's apps leave no room for it). Add Widget puts it at the top of the page, the apps making room, and closes the gallery; its ✕, a tap above it or the home gesture close it too |
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
`set DOCK GRID`, `remove APP`, `add APP`, `widget add|remove WIDGET`,
`widgets LIST` (`layout widgets -` removes both widgets), `edit on|off`,
`library on|off`. `state` shows the layout as `settings.home` and the
screen as `home`:

```json
"home": {"editing": false, "page": "home", "dragging": null, "alert": null, "menu": null, "gallery": false}
```

`page` is `home` or `library`; `dragging` and `menu` name the app dragged
or whose App Library menu is open, `alert` the app or widget asked about;
`gallery` is the widget gallery. `last_action` reports the steps:
`home_edit`, `home_drag`, `home_drop`, `home_remove_alert`, `home_removed`,
`widget_removed`, `home_remove_cancel`, `home_edit_done`, `widget_gallery`,
`widget_added`, `app_library`, `home_page`, `library_menu`, `home_added`.

## Drawing

The plain home page stays one CPU-drawn layer (wallpaper, widgets, dock,
icons, names, status bar), redrawn only when something on it changes. While
the home screen is *live* (edit mode, the App Library, a page swipe, icons
moving, the alert, the menu or the gallery) the frame is composed from
parts instead, each a layer drawn once: the wallpaper, the dock's shelf,
the status bar, the App Library, the + and Done buttons, the badge and the
gallery. Every icon is
its own compositor node, as is its name, its "−" badge and its count badge
(a layer per app, redrawn only when its count changes): nothing is
re-rasterized per frame, on the GPU compositor and on the XRender path
alike.

The compositor has no rotation, so the jiggle uses sprites: for each app,
its icon and shadow pre-turned 11 ways (±0.03 rad) side by side in one
layer (drawn in idle time after startup); a frame shows the sprite frame
nearest the icon's angle, and its badges and name follow the turn (the
count badge scales with a lifted or shrinking icon too). The unturned frame
is drawn exactly as the home page draws the icon and its count badge, so
edit mode starts and ends without a visible step (unit tests check this).
Widgets jiggle the same way from 5 pre-turned frames of each (their sprite
is made only while the widget is on the page, in idle time, and redrawn
when what it shows changes: the date, the battery's charge). Node counts
stay fixed while live, so showing or hiding an icon never restructures a
frame.

Not done yet: widgets cannot be dragged (they stay at the top of the page,
in the order added) and come in one size; there is one home page (no page
dots, no dragging to a new page); the App Library has no search or
categories; a **Reset Home Screen Layout** entry in the System app (today:
`ctl layout reset`).
