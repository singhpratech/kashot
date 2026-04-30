# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Naming

Project, namespace, assembly, binary, and every user-visible string are all `Kashot`. The project lives in `Kashot/Kashot.csproj`. The outer parent folder on disk happens to still be named `LightCapture` for historical reasons (originally the project was called LightCapture, then PratShot, now Kashot) — that name doesn't appear in code or any user-facing surface and you can ignore it.

## Build / Run

There is no `.sln` — operate on the `.csproj` directly.

```sh
dotnet build Kashot/Kashot.csproj          # debug build
dotnet run   --project Kashot/Kashot.csproj
dotnet publish Kashot/Kashot.csproj -c Release
```

Target framework is `net8.0-windows` with `UseWindowsForms=true`, so this **only builds and runs on Windows**. No tests, no linter, no formatter configured.

## Architecture

Tray-resident screenshot tool with annotation editor. `Program.Main` runs a `TrayContext : ApplicationContext` — there's no main window.

### File map

| File | Role |
|---|---|
| `Program.cs` | Entry point; runs `TrayContext`. |
| `TrayContext.cs` | Tray icon, settings load, hotkey registration, owns the `OverlayForm` lifecycle. |
| `OverlayForm.cs` | The full-screen capture/edit surface. ~1000 lines, the bulk of the app. |
| `Annotations.cs` | `Tool` enum + polymorphic `Annotation` hierarchy. |
| `Settings.cs` | `AppSettings` POCO + JSON load/save to `%APPDATA%/Kashot/settings.json`. |
| `SettingsForm.cs` | Dialog for hotkey, save folder, start-with-Windows. Also defines `HotkeyTextBox`. |
| `PinForm.cs` | Borderless `TopMost` window that pins a captured image to the screen, draggable. |
| `StartupHelper.cs` | Toggles `HKCU\…\Run\Kashot` registry value. |
| `NativeMethods.cs` | `RegisterHotKey`/`UnregisterHotKey` P/Invoke + `MOD_*` / `WM_HOTKEY` / `VK_SNAPSHOT` constants. |

### Capture trigger flow (`TrayContext.cs`)

- `TrayContext` loads `AppSettings`, syncs the start-with-Windows registry value, builds the tray menu (Capture / Settings… / Exit), and creates a `HotkeyWindow`.
- `HotkeyWindow` is a `NativeWindow` whose `WndProc` listens for `WM_HOTKEY`; `Register(mods, vk)` and `Unregister()` thinly wrap `RegisterHotKey`/`UnregisterHotKey`. The settings dialog calls `Unregister()` while open so the user can rebind without their own keypress firing the existing hotkey.
- Both the hotkey and the tray menu route to `StartCapture()`, which closes the tray context menu, sends two `ESC` keys to dismiss the system tray flyout, waits **500ms**, and only then constructs the `OverlayForm`. The delay is load-bearing — without it the menu/flyout ends up in the screenshot. Don't shorten it without testing on a real tray.
- A real `.ico` is generated and cached at `%APPDATA%/Kashot/icon.ico` on first run via `Icon.Save(Stream)` (the standard ICO writer); subsequent launches load from disk. The original GDI+ drawing routine still lives in `CreateIcon()` as the source of truth — the cached file is regenerated if you delete it.

### Capture surface (`OverlayForm.cs`)

- On construction, the form takes an `AppSettings` and restores the user's last tool / color / thickness from it. It snapshots `SystemInformation.VirtualScreen` (the union of all monitors) into `_screenshot` and sizes itself to cover that virtual bounds. Form coordinates are virtual-screen coordinates, not single-screen.
- Form is borderless, `TopMost`, `KeyPreview`, double-buffered. `CreateParams` adds `WS_EX_COMPOSITED` (`0x02000000`) to kill flicker — keep it.
- All rendering goes through one `OnPaint`: draws screenshot → dim overlay → "punches a hole" by redrawing the screenshot inside the selection → border → annotations clipped to the selection → resize handles → dimension label → crosshair + magnifier (idle/selecting only). `OnPaintBackground` is intentionally empty.
- **State machine**: `State { Idle, Selecting, Selected, Drawing, TextInput, Resizing, Moving }`. `OnMouseDown/Move/Up` and `OnKeyDown` all branch on `_state`. Right-click semantics are state-dependent: cancels the active annotation while `Drawing`, cancels the textbox while `TextInput`, otherwise closes the overlay. Read the existing `switch (_state)` blocks before adding new input handling — new behavior almost always belongs as another case there, not as a new event handler.
- **Selection editing**: in `Selected` state, `HitTestEdge` returns one of the 8 `Edge` values when the cursor is within `EdgeThreshold` (8px) of the selection's edges/corners; that switches the cursor to a size cursor and a click enters `Resizing`. Holding `Alt` and dragging inside the selection enters `Moving`. Both states call `PositionToolbars` on each move so the floating panels track the selection.
- **Annotations** (`Annotations.cs`) are a polymorphic hierarchy. The `Tool` enum is **public** and lives here, not in `OverlayForm`. Adding a new tool means: (1) new `Annotation` subclass with `Draw(Graphics)`, (2) new entry in the `Tool` enum, (3) case in `StartDrawing` (and `UpdateDrawing` if it's a drag-shape), (4) entry in the `tools[]` array in `CreateToolPanel` with an icon-drawing delegate, (5) keyboard-shortcut entry in the `OnKeyDown` tool-switch `switch`. There are no image assets — every toolbar icon is procedurally drawn by an `IconXxx` static method using GDI+.
- **Click-to-place vs drag-to-shape**: `StepAnnotation` is finalized on `MouseDown` (one click → numbered circle) instead of going through the `Drawing` state. `StartDrawing` short-circuits for `Tool.Step`. `TextAnnotation` uses its own `TextInput` state with a child `TextBox`. All other tools follow the standard down→drag→up flow.
- **Pixelate** redacts via `PixelateAnnotation`, which holds a reference to `_screenshot` and on `Draw` downsamples the rect with bilinear and upsamples with `NearestNeighbor`. It always re-pixelates the **original** screenshot, ignoring annotations underneath — that's the desired behavior, but means draw-order matters (always pixelate first, annotate over it).
- **Undo/redo**: `_annotations` is the live list, `_redoStack` holds undone items. Adding any new annotation must clear `_redoStack` (use the `AddAnnotation` helper or `FinalizeDrawing`).
- Toolbars are torn down and rebuilt every time the selection changes (`HideToolbars` / `ShowToolbars`) and re-positioned relative to the selection rect (`PositionToolbars` flips them to the opposite side when they'd fall off-screen). `CycleThickness` rebuilds the entire toolbar to refresh the thickness icon — that's by design, not laziness.
- `GetFinalImage()` is what produces the saved / copied / pinned bitmap: it crops `_screenshot` to `_selection`, then translates the graphics origin by `-_selection.X/Y` so the existing annotations (whose coordinates are in form/virtual-screen space) draw correctly into the cropped bitmap. Any new "save" or "share" action should funnel through `GetFinalImage()` rather than re-implement the compositing. The pin button hands the bitmap off to `PinForm`, which takes ownership and disposes it on close.

### Settings flow

- `AppSettings.Load()` reads `%APPDATA%/Kashot/settings.json`, returning defaults silently on missing/malformed JSON. `AppSettings.Save()` is idempotent and swallows IO errors — the app should never crash because of settings persistence.
- `OverlayForm` saves the latest tool/color/thickness in `OnFormClosed` regardless of how the user exited (Esc, Save, Copy, Pin, Close). The save dir is updated in `SaveToFile` after a successful save, so subsequent saves remember the last-used folder.
- The `SettingsForm` writes back into the same `AppSettings` instance and calls `_settings.Save()` + `StartupHelper.SetEnabled` directly. After `ShowDialog` returns, `TrayContext` re-registers the hotkey and refreshes the tray tooltip.

## Keyboard shortcuts

In the `Selected` state with no modifiers, single-letter keys switch tools:

| Key | Tool |
|---|---|
| P | Pen |
| L | Line |
| A | Arrow |
| R | Rectangle |
| E | Ellipse |
| M | Marker |
| T | Text |
| N | Numbered step |
| B | Blur / pixelate |

Plus:

- `Esc` — cancel text input / cancel active draw / close overlay (state-dependent)
- `Ctrl+Z` — undo
- `Ctrl+Y` or `Ctrl+Shift+Z` — redo
- `Ctrl+C` — copy final image to clipboard (only in `Selected`)
- `Ctrl+S` — save final image via `SaveFileDialog` (only in `Selected`)
- `Alt`+drag inside selection — move the whole selection
- Drag selection edges/corners — resize
