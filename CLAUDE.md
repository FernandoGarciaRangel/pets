# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Pets is a Windows-only Rust desktop app. It shows a Codex pet (`~/.codex/pets/<pet>/pet.json` + spritesheet) floating on top of every window, on any monitor. The pet's animation follows the state of Claude Code and Codex sessions, which reaches the app through hooks. The top priority is staying lightweight: Win32 through `windows-sys`, no webview, no GPU, no render loop. Code comments and user-facing text are in Brazilian Portuguese.

## Commands

```powershell
./dev.ps1                 # cargo run (debug, console); hook events are printed as "[hook] ..."
./dev.ps1 -Release        # optimized build
./dev.ps1 -Install        # cargo build --release + `pets.exe install`
./dev.ps1 -- install-hooks  # passes args through to pets.exe
cargo test                # all tests
cargo test agents::tests::single_session_lifecycle   # a single test
cargo fmt --check; cargo clippy --all-targets -- -D warnings   # CI requires both
```

Always start services in the current terminal with `dev.ps1`. Never open an external window, because the logs would be lost.

CLI modes (`src/main.rs`): with no arguments the app runs; `hook <claude|codex>`, `install`, `update`, `check-update`, `install-hooks` and `uninstall-hooks` are the other modes.

## Architecture

A single exe plays two roles:

1. **App** (`window.rs`):
   - **Window:** layered Win32 window (`UpdateLayeredWindow` with premultiplied BGRA). Transparent pixels let clicks through without extra code.
   - **Animation:** a `SetTimer` fires only when the frame changes, using that frame's own duration.
   - **Single instance:** a named mutex. A second launch posts `WM_APP_SHOW` to the instance already running.
   - **Tray** (`tray.rs`): the tray icon is generated from the pet's first frame.
2. **Hook client** (`hook.rs`): the agents invoke `pets.exe hook claude|codex`. It reads the payload from stdin and writes one JSON line (`agents::HookEvent`) to the named pipe `\\.\pipe\pets-agent-state`, then exits. If the app is not running it exits silently. It must never block the agent or print to stdout.

Data flow:
- A thread in `agents::spawn_pipe_server` receives each event and hands a `Box<HookEvent>` to the window via `PostMessageW(WM_APP_HOOK)`.
- `Sessions`, keyed by (agent, session_id), maps hook events to `AgentState`. Each state expires on its own: review lasts 60 s, failed 5 s, and sessions go stale after 10 or 30 min.
- `aggregate` picks the most urgent state across sessions: waiting > working > failed > done > idle.
- `animation::select` applies interaction on top of that state: dragging → running-left/right, hover → jumping.

Pet format (`pets.rs`):
- It is a port of the Orca rules (`codex-pet-sprite-defaults.ts`, `pet-bundle*.ts` in github.com/stablyai/orca).
- With no `frame` or `animations` in the manifest and a Codex grid sheet (1536 px wide, 208 px rows, 9 or more rows), the Codex layout and timing table applies: 192×208 frames, 9 named animations.
- Otherwise it uses the explicit layout, validated the same way Orca does, or falls back to a single static frame.
- `sprite.rs` decodes the sheet once, crops and scales only the frames actually used, then drops the sheet.

Installation (`install.rs`):
- `merge` is pure and tested. It adds this app's hook groups to `~/.claude/settings.json` and `~/.codex/hooks.json` without touching other apps' hooks (Orca, Codenotch). It recognizes its own groups by a command containing `pets.exe` + ` hook `.
- It writes a `.bak-pets` backup first. `serde_json` uses `preserve_order` so the user's key order survives.
- `install` copies the exe to `%LOCALAPPDATA%\Programs\Pets\pets.exe` (renaming the old exe aside first, which works even while it is running), reinstalls the hooks pointing at that copy, and starts the app detached.

Updates (`update.rs`):
- The app checks `api.github.com/repos/FernandoGarciaRangel/pets/releases/latest` on startup (after 10 s) and every 6 h, using WinHTTP from a separate thread.
- The result returns to the window as `UpdateMsg` via `WM_APP_UPDATE`, which drives the notification and the menu item.
- Updating downloads the `pets.exe` asset, verifies it against `SHA256SUMS.txt` (using BCrypt), saves it to `%TEMP%\pets-update`, and runs `<downloaded> install --from-update` detached. That reuses `install_app`: it quits the current instance, swaps the exe, reinstalls the hooks and starts the app again.
- `pets.exe update` / `check-update` run the same path from the CLI, which is the way to test without the GUI. To simulate an old version, build with a lower `version` in `Cargo.toml` and a separate `CARGO_TARGET_DIR`.
- The release asset names (`pets.exe`, `SHA256SUMS.txt`) are a contract between `release.yml` and `update.rs`.

Config lives in `%APPDATA%\pets\config.toml` (`config.rs`).

## Gotchas

- **Reentrancy:** the `App` sits in a `thread_local RefCell` and the wndproc uses `try_borrow_mut`. Win32 calls such as `SetWindowPos`, `ReleaseCapture`, `TrackPopupMenu` and `MessageBox` re-enter the wndproc. While the App is borrowed, the message goes to `DefWindowProcW`. Modal calls (menu, MessageBox) must happen outside the borrow; see `show_menu` and the `After` enum.
- **Release builds are GUI subsystem** (`windows_subsystem = "windows"`):
  - In PowerShell 5.1, `'...' | pets.exe hook codex` does not wait for the process and does not deliver stdin. To test a hook by hand, use `Start-Process -RedirectStandardInput file -Wait -NoNewWindow`.
  - `Start-Process -Wait` also waits for child processes, so it hangs on `install`; that is why `dev.ps1` uses `System.Diagnostics.Process`.
  - CLI commands call `system::attach_console()` so their output shows up.
- **Installed hooks point at `%LOCALAPPDATA%\Programs\Pets\pets.exe`, not at `target/`.** To test a change through the real agents, run `./dev.ps1 -Install`.
- Constants missing from the enabled `windows-sys` features (e.g. `WM_MOUSELEAVE`) are defined locally instead of enabling a whole feature.

## CI/CD

`.github/workflows/ci.yml` runs fmt, clippy (`-D warnings`) and tests on `windows-latest` for every push to `main` and every PR. `release.yml` triggers on a `v*` tag, which must match the `Cargo.toml` version. It publishes `pets.exe`, a zip and `SHA256SUMS.txt` to a GitHub Release. To publish: bump the version in `Cargo.toml`, then `git tag vX.Y.Z && git push origin vX.Y.Z`.
