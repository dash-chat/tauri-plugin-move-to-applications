# Tauri Plugin: Move to Applications

Moves a macOS Tauri app into `/Applications` when it runs from somewhere it
cannot update itself, in the style of
[LetsMove](https://github.com/potionfactory/LetsMove).

`tauri-plugin-updater` replaces the running bundle in place, which fails when
the app runs from:

- a mounted disk image, which is read-only, or
- a freshly downloaded copy macOS has
  [translocated](https://developer.apple.com/forums/thread/724969) to a
  read-only, randomized path. Apps opened straight from a quarantined disk
  image are translocated too.

When the first window opens from such a place, the plugin asks over it, as a
sheet, whether to move the app. If the user agrees it:

1. copies the bundle into `/Applications` (replacing an older copy there, but
   never a same-or-newer one), asking for an administrator password only if
   the user cannot write there,
2. removes the quarantine flag from the copy,
3. quits, ejects the disk image it ran from, if any, and relaunches the app
   from `/Applications`.

The original is left where it is: removing it from `~/Downloads` would make
macOS ask the user to let the app into their Downloads folder.

## Platform support

| Platform      | Support |
|---------------|---------|
| macOS         | Yes     |
| Linux/Windows | No-op: `needsMove()` is `false`, `moveToApplicationsFolder()` rejects |

## Installation

```toml
[dependencies]
tauri-plugin-move-to-applications = { git = "https://github.com/dash-chat/tauri-plugin-move-to-applications" }
```

```sh
pnpm add github:dash-chat/tauri-plugin-move-to-applications
```

The launch prompt uses `tauri-plugin-dialog`, which the app must register too.

## Usage

```rust
tauri::Builder::default()
    .plugin(tauri_plugin_dialog::init())
    .plugin(tauri_plugin_move_to_applications::init(
        tauri_plugin_move_to_applications::Prompt {
            title: "Move to Applications?".into(),
            message: "My App can only update itself from your Applications folder.".into(),
            move_button: "Move to Applications".into(),
            cancel_button: "Not Now".into(),
        },
    ))
```

If you use `tauri-plugin-single-instance`, register it first, so that opening
a second copy from the disk image focuses the running app instead.

Allow the commands in your capability:

```json
"permissions": ["move-to-applications:default"]
```

Users who choose "Not Now" can still move the app later, for example when an
update is available:

```ts
import { needsMove, moveToApplicationsFolder } from 'tauri-plugin-move-to-applications-api';

if (await needsMove()) {
  await moveToApplicationsFolder(); // quits and relaunches on success
}
```

## Testing

`cargo test` covers the copy. To try the whole flow, build a signed DMG, mark
it quarantined (`xattr -w com.apple.quarantine "00c1;$(printf %x $(date +%s));Safari;" App.dmg`),
open it and launch the app from the mounted image.
