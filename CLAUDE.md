# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Pets is a Windows-only Rust desktop app. It shows a Codex pet (`~/.codex/pets/<pet>/pet.json` + spritesheet) floating on top of every window, on any monitor. The pet's animation follows the state of Claude Code and Codex sessions, which reaches the app through hooks. The top priority is staying lightweight: Win32 through `windows-sys`, no webview, no GPU, no render loop. Code comments and user-facing text are in Brazilian Portuguese.

## Commands

```powershell
./dev.ps1                 # cargo run (debug, console); prints "[hook] ..." and "[pets] ..." (folder reloads)
./dev.ps1 -Release        # optimized build
./dev.ps1 -Install        # cargo build --release + `pets.exe install`
./dev.ps1 -- install-hooks  # passes args through to pets.exe
cargo test                # all tests
cargo test agents::tests::single_session_lifecycle   # a single test
cargo fmt --check; cargo clippy --all-targets -- -D warnings   # CI requires both
```

Always start services in the current terminal with `dev.ps1`. Never open an external window, because the logs would be lost.

CLI modes (`src/main.rs`): with no arguments the app runs; `hook <claude|codex>`, `install`, `update`, `check-update`, `check-pets`, `update-pets`, `install-hooks` and `uninstall-hooks` are the other modes.

## Architecture

A single exe plays two roles:

1. **App** (`window.rs`):
   - **Window:** layered Win32 window (`UpdateLayeredWindow` with premultiplied BGRA). Transparent pixels let clicks through without extra code.
   - **Animation:** a `SetTimer` fires only when the frame changes, using that frame's own duration.
   - **Single instance:** a named mutex. A second launch posts `WM_APP_SHOW` to the instance already running.
   - **Tray** (`tray.rs`): the tray icon is always the app logo (`tray::app_icon`, resource 1); the pet is already visible on screen.
2. **Hook client** (`hook.rs`): the agents invoke `pets.exe hook claude|codex`. It reads the payload from stdin and writes one JSON line (`agents::HookEvent`) to the named pipe `\\.\pipe\pets-agent-state`, then exits. If the app is not running it exits silently. It must never block the agent or print to stdout. When the pipe is busy it waits up to ~2 s (`WaitNamedPipeW`, 10 × 200 ms).

Data flow:
- A thread in `agents::spawn_pipe_server` receives each event and hands a `Box<HookEvent>` to the window via `PostMessageW(WM_APP_HOOK)`.
- The pipe server creates the **next** instance before reading the current one, so the pipe name never disappears between events (a client that found no pipe would assume the app was closed and drop the event). Instances are overlapped: each client gets at most 250 ms (`READ_DEADLINE`) to send its line, so a client that connects and never closes cannot stall the others. Test with ~20 concurrent `target/debug/pets.exe hook claude` processes, with and without a client holding the pipe open.
- `Sessions`, keyed by (agent, session_id), maps hook events to `AgentState`. Each state expires on its own: review lasts 60 s, failed 5 s, and sessions go stale after 10 or 30 min.
- `aggregate` picks the most urgent state across sessions: waiting > working > failed > done > idle.
- `animation::select` applies interaction on top of that state: dragging → running-left/right, hover → jumping.
- Click animations (`window::play_click`): a click without dragging plays the pet's next `click` animation **once** (`click_anim`, which `refresh_anim` prefers over everything but a real drag), then `next_frame` drops it and the state/hover animation returns. A real drag interrupts it.

Pets folder watcher:
- `pets::spawn_watch` blocks on `ReadDirectoryChangesW` (no polling) and posts the changed top-level pet folders as `WM_APP_PETS_CHANGED` (`Box<Vec<String>>`; empty = buffer overflowed, treat everything as changed). After a failure it waits 5 s before reopening, so a folder without change notifications (network, WSL) does not spin the CPU.
- The window debounces with `TIMER_PETS` (500 ms of silence, re-armed on purpose) and `reload_changed_pets` rediscovers the list; the on-screen pet is rebuilt only if its own folder changed or vanished. A load error shows a tray notification. There is no manual "Recarregar pets" item, and catalog updates also rely on the watcher.
- `discover` orders pets by folder creation date (inclusion date); updating a pet swaps files, not the folder, so it keeps its place. Menu ids are positions in `menu_pets`, a snapshot taken in `build_menu`, because the list can change while the menu is open.

Pet format (`pets.rs`):
- It is a port of the Orca rules (`codex-pet-sprite-defaults.ts`, `pet-bundle*.ts` in github.com/stablyai/orca).
- With no `frame` or `animations` in the manifest and a Codex grid sheet (1536 px wide, 208 px rows, 9 or more rows), the Codex layout and timing table applies: 192×208 frames, 9 named animations.
- Otherwise it uses the explicit layout, validated the same way Orca does, or falls back to a single static frame.
- `animations` has two meanings, told apart by `sourceRow` (`pets::Animations`): Orca's explicit layout (`row` + `frames`, requires `frame`), or Codex **extras** on top of the Codex grid (`sourceRow` names a Codex row, `frameCount`, `timingMs`), referenced by `interactions.click.animations` (mode `cycle`). Extras reuse a state's row with more frames than the state shows (e.g. 8 instead of 6). An invalid extra (unknown row, 0 frames, more frames than columns, unparsable) is dropped alone and never breaks the pet; `Layout::click` keeps only names that exist.
- `sprite.rs` decodes the sheet once, crops and scales only the frames actually used, then drops the sheet.
- Size is set with a slider: the menu item "Tamanho…" opens `size_panel.rs`, a separate popup window with a comctl32 trackbar that closes when it loses focus or on Esc (handled in the message loop, since focus stays on the trackbar).
- While you drag, `window::preview_width` → `queue_resize` coalesces the trackbar events (`TIMER_RESIZE_PREVIEW`, at most one every 16 ms, without re-arming a pending timer). `resize_to` then scales **only the visible animation** (`Cells::scale_only`, fast filter) from `sprite::Cells`, the cropped frames at their original resolution, kept in memory while the panel is open.
- 600 ms after the last change, `settle_resize` rebuilds every animation with the final filter but keeps the `Cells`. When the panel closes (`commit_width` → `finish_resize`), the `Cells` are freed and the config is saved.
- Cost per step: ~5 ms versus ~50 ms when scaling everything. `[profile.dev] opt-level = 1` keeps the dev build at that speed.
- The panel is created outside the App borrow (`After::OpenSizePanel`).

Installation (`install.rs`):
- `merge` is pure and tested. It adds this app's hook groups to `settings.json` in the Claude config dir and `hooks.json` in the Codex one (`config::claude_home` / `codex_home`: `CLAUDE_CONFIG_DIR` / `CODEX_HOME`, else `~/.claude` / `~/.codex`; the pets dir follows `codex_home` too) without touching other apps' hooks (Orca, Codenotch).
- `is_our_hook` recognizes only the exact `hook_command` shape, `<any dir>/pets.exe hook <agent>` (quoted or not); `codex-pets.exe hook` is not ours. Removal takes only our hook out of a group, so a group shared with another app's hook survives.
- The `.bak-pets` backup is written once, before the first change, and never overwritten, so it keeps the pre-pets file. Uninstall does not rewrite a file that has no pets hooks. `serde_json` uses `preserve_order` so the user's key order survives.
- The hooks always point at the installed copy when it exists (`hook_exe`), even if the app is running from a dev build. The menu shows `install::status()` (Claude/Codex ✓/✗) and only offers "Reinstalar hooks" when something is missing or points at another exe.
- `install` copies the exe to `%LOCALAPPDATA%\Programs\Pets\pets.exe` (renaming the old exe aside first, which works even while it is running), reinstalls the hooks pointing at that copy, creates the shortcuts and starts the app detached.
- `install_app(from_update)`: on an update it only redoes the hooks of agents that already had pets hooks (`contains_ours`), so it never undoes an `uninstall-hooks`. Once the old instance is quit, every failure still leaves a pet running: a failed exe swap restarts the old exe, and a failed hook merge is reported only after the new app was started.
- Shortcuts (`system::create_shortcut`): `Pets.lnk` is created through WScript.Shell from a PowerShell without a window, because windows-sys has no IShellLink COM interfaces. The folders come from `[Environment]::GetFolderPath`, because the Desktop can be redirected to OneDrive. The Start Menu shortcut is created on every install or update; the desktop one only once (`config.desktop_shortcut_created`), so an update never recreates a shortcut the user deleted. A failure here does not abort the install.

Updates (`update.rs`):
- The app checks `api.github.com/repos/FernandoGarciaRangel/pets/releases/latest` on startup (after 10 s) and every 6 h, using WinHTTP from a separate thread.
- The result returns to the window as `UpdateMsg` via `WM_APP_UPDATE`, which drives the notification and the menu item.
- Updating downloads the `pets.exe` asset, verifies it against `SHA256SUMS.txt` (using BCrypt), saves it to `%TEMP%\pets-update`, and runs `<downloaded> install --from-update` detached. That reuses `install_app`: it quits the current instance, swaps the exe, reinstalls the hooks and starts the app again.
- `pets.exe update` / `check-update` run the same path from the CLI, which is the way to test without the GUI. To simulate an old version, build with a lower `version` in `Cargo.toml` and a separate `CARGO_TARGET_DIR`.
- The release asset names (`pets.exe`, `SHA256SUMS.txt`) are a contract between `release.yml` and `update.rs`.
- `net::request`: WinHTTP timeouts are per operation, so a whole download also has a 5 min deadline (`DOWNLOAD_DEADLINE`), and a body shorter or longer than `Content-Length` is rejected. Max 32 MB.
- The SHA256 check catches corruption, not a compromised release (the sums come from the same release). Signing the update was discussed and deliberately not done for now.

Pet updates (`catalog.rs`):
- The sources are codex-pets.net (`/api/pets/<id>/share-data`, one request per pet), petdex.dev (the public manifest `assets.petdex.dev/manifests/petdex-v1.json`) and awesome-codex-pet, the GitHub repo behind aimcp.info (fetched through the git tree API; folders are `<id>--<author>`).
- A local pet only gets updates once it is **identified exactly**, meaning its spritesheet matches a catalog entry: same size on codex-pets (`spritesheetBytes`), the same `Content-Length` via HEAD on petdex, or the same git blob SHA-1 on GitHub.
- What is known about each pet lives in `%APPDATA%\pets\pet-sources.json`: `tracked` with source, key and version, or `unmatched` with size and whether the name exists in some catalog.
- The version marker is the `v` in the URL on codex-pets, the spritesheet URL (which carries a hash) on petdex, and the blob SHA on GitHub.
- `apply` downloads the new version, validates it with `Cells::load` in `%APPDATA%\pets\staging` (removed again if validation fails), backs up the old files to `backups/<id>-<ts>` (only the newest `KEEP_BACKUPS` = 3 per pet are kept), then swaps the files while keeping the local `id`. An update replaces `pet.json`, so local edits to it (renamed `displayName`, added click animations) are lost.
- The check runs in the same thread as the app check (`spawn_check`) and returns as `UpdateMsg::PetsChecked`.
- From the CLI: `check-pets` / `update-pets`. The network test is `cargo test awesome_codex_pet_end_to_end -- --ignored`.

Config lives in `%APPDATA%\pets\config.toml` (`config.rs`).

## Gotchas

- **Reentrancy:** the `App` sits in a `thread_local RefCell` and the wndproc uses `try_borrow_mut`. Win32 calls such as `SetWindowPos`, `ReleaseCapture`, `TrackPopupMenu` and `MessageBox` re-enter the wndproc. While the App is borrowed, the message goes to `DefWindowProcW`, except `WM_APP_HOOK` and `WM_APP_PETS_CHANGED`, which are re-posted to the queue so no event is lost (and freed only if the post fails). Modal calls (menu, MessageBox) must happen outside the borrow; see `show_menu` and the `After` enum. `TrackPopupMenu`'s modal loop still dispatches timers, so App state can change while the menu is open.
- **Release builds are GUI subsystem** (`windows_subsystem = "windows"`):
  - In PowerShell 5.1, `'...' | pets.exe hook codex` does not wait for the process and does not deliver stdin. To test a hook by hand, use `Start-Process -RedirectStandardInput file -Wait -NoNewWindow`.
  - `Start-Process -Wait` also waits for child processes, so it hangs on `install`; that is why `dev.ps1` uses `System.Diagnostics.Process`.
  - CLI commands call `system::attach_console()` so their output shows up.
- **Installed hooks point at `%LOCALAPPDATA%\Programs\Pets\pets.exe`, not at `target/`.** To test a change through the real agents, run `./dev.ps1 -Install`.
- Constants missing from the enabled `windows-sys` features (e.g. `TBM_GETPOS`, `SS_CENTERIMAGE`) are defined locally instead of enabling a whole feature.
- `build.rs` embeds the Common Controls v6 manifest (for the modern slider and buttons) and compiles `pets.rc` (icon 1 = `assets/pets.ico`, the fox logo, used by `tray::app_icon`) via `embed-resource`, which needs the Windows SDK `rc.exe`.
- Built-in pets (`builtin.rs`): only pets we have the right to distribute are embedded, using the `builtin!("<id>")` macro (which `include_bytes!` from `assets/pets/<id>/`). Today those are the tatu-canastra (`DEFAULT_PET`) and draco, both originals by the author. To add a pet: put its folder in `assets/pets/<id>/` and add it to `BUILTIN`; the test `builtin_pets_are_valid_codex_pets` validates every entry. The "Pet" submenu lists the built-ins first under "Do Pets", in `BUILTIN` order, then the user's pets under "Seus pets". Each one is installed into `pets_dir` **once** (`config.installed_builtins`): an existing folder is never overwritten, and a pet the user deleted is not recreated. `DEFAULT_PET` is used when no pet is configured.

## CI/CD

`.github/workflows/ci.yml` runs fmt, clippy (`-D warnings`), tests and a `cargo build --release` on `windows-latest` for every push to `main` and every PR. `release.yml` triggers on a `v*` tag, which must match the `Cargo.toml` version; it runs fmt, clippy and `cargo test --release` again before publishing, because a tag can point at a commit CI never saw. It publishes `pets.exe`, a zip and `SHA256SUMS.txt` to a GitHub Release. To publish: bump the version in `Cargo.toml` (commit "Versão X.Y.Z"), then `git tag vX.Y.Z && git push origin vX.Y.Z`.

Third-party actions are pinned by commit SHA with the tag in a comment (`release.yml` has `contents: write`). A SHA-pinned `dtolnay/rust-toolchain` needs `toolchain: stable` as input, since it can no longer infer it from the ref name. To bump an action, resolve the new SHA with `git ls-remote https://github.com/<owner>/<repo> <ref>`.
