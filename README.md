# Claude Accounts

Run multiple **isolated copies of the real Claude desktop app** side by side —
each with its own login, kept completely separate from your main Claude. A small
native launcher built with [Tauri](https://v2.tauri.app/) (Rust).

> Independent utility. Not affiliated with, or endorsed by, Anthropic.
> Your main Claude login (`%APPDATA%\Claude`) is never used or touched.

---

## Why this is non-trivial

Claude Desktop on Windows ships as a **Microsoft Store (MSIX) package**. That
means it's single-instance, its `Claude.exe` can't be launched directly with
arguments, and Store activation won't forward a custom data directory. So there's
no built-in way to run two isolated instances.

This launcher works around that:

1. **Mirror** the installed app's files to a private writable folder
   (`%LOCALAPPDATA%\dev.deckspace.claude-accounts\claude-app`).
2. **Auto-re-mirror** whenever the installed Claude version changes — so it stays
   current through updates automatically (a quick incremental copy).
3. **Launch** that mirrored `claude.exe` per account with:
   - the **`CLAUDE_USER_DATA_DIR`** environment variable set to a per-account
     folder — this is Claude's *own* mechanism for a custom profile, and it's
     what makes multiple instances actually coexist (see below),
   - `--user-data-dir=<same folder>` → isolated Chromium profile / single-instance
     lock, and
   - `--no-sandbox` → **required**, because a relocated/unpackaged Electron app's
     child processes crash without it.

### Why `CLAUDE_USER_DATA_DIR` (not just `--user-data-dir`)

Claude's main process contains this logic:

```js
if (process.env.CLAUDE_USER_DATA_DIR) app.setPath("userData", <that dir>)
else /* on Windows */ app.setPath("userData", LOCALAPPDATA\Claude-3p)   // a single shared folder
```

Without the env var, the app **overrides** whatever `--user-data-dir` you pass and
forces `userData` onto one shared folder (`Claude-3p`). Every instance then shares
the same single-instance lock, so opening a second account just focuses/replaces
the first — you can't run more than one. Setting `CLAUDE_USER_DATA_DIR` per account
skips that override, giving each account a genuinely separate instance that runs
alongside the others and your main app.

### The `--no-sandbox` tradeoff (read this)

Running the mirrored copies with `--no-sandbox` disables Chromium's process
sandbox **for those isolated Claude windows only** (not your real Claude, not the
rest of your system). It's necessary to make the relocated app run at all. Real
risk is low for Anthropic's own app, but it *is* a genuine security reduction —
that's the cost of running the native app in isolated instances on a Store-only
install.

---

## Requirements

- **Claude Desktop for Windows** installed (from the Microsoft Store).
- **Rust** (stable) — https://rustup.rs — only needed to build.
- **WebView2 runtime** (preinstalled on Windows 10/11) for the launcher's own UI.

## Run / build

```powershell
cd src-tauri
cargo run                 # dev

cargo build --release     # produces src-tauri/target/release/claude-accounts.exe
```

A ready-to-run `Claude Accounts.exe` is also placed on your Desktop.

---

## Using it

1. Launch **Claude Accounts**. It detects your installed Claude and, on first run,
   does a one-time ~540 MB copy (shown with a progress overlay).
2. Click **Add account**, name it (e.g. *Work*, *Personal*).
3. Click the card → the real Claude desktop app opens in its own isolated profile.
   Sign in once; that account stays logged in there.
4. Open several at once — each is a separate, isolated Claude. The green dot marks
   which are running. The **⋯** menu renames, closes, or deletes an account
   (delete also erases that profile's data).

Data layout (all under `%LOCALAPPDATA%\dev.deckspace.claude-accounts`):

```
accounts.json      your accounts (names, colors)
accounts.bak.json  automatic backup of the above
mirror.json        which Claude version is currently mirrored
claude-app\        the mirrored Claude desktop app (auto-updated)
profiles\<id>\     isolated login/data per account (+ .ca-meta.json)
```

**Your accounts can't be silently lost.** `accounts.json` is written durably
(temp file → fsync → atomic rename) and backed up on every change, and a bad read
never overwrites it with a blank list. Each profile folder also stores its own
`.ca-meta.json`, so if the index is ever lost the launcher **rebuilds the account
list from the surviving profiles** (which hold your logins) on next start.

---

## Known limitation: Google / SSO sign-in

Windows registers the **`claude://`** URL scheme system-wide to a single handler —
your main Store Claude:

```
claude://  ->  "C:\Program Files\WindowsApps\Claude_…\Claude.exe" "%1"
```

Claude's desktop Google/SSO flow opens a browser and redirects back to
`claude://…`. Windows routes that callback to whoever owns the scheme — **the
main app** — regardless of which isolated instance started the sign-in. So
"Continue with Google" from an isolated window lands in your main Claude, not the
isolated one. This is inherent to a single system-wide protocol handler; there's
no per-instance routing.

**Reliable workaround: sign into isolated accounts with "Continue with email".**
The email-code flow is handled in-app (no `claude://` callback), so it works
correctly per instance, every time.

*(A protocol-router that hijacks `claude://` while the launcher runs and forwards
the callback to the active isolated instance is possible, but it can disrupt the
main app's own deep links and can't be fully tested without a live Google login —
so it's intentionally not enabled by default.)*

## Other limitations

- **All isolated windows are titled just "Claude"** — the native app doesn't take
  a title argument, so tell them apart by what you opened.
- **`--no-sandbox`** is required (see above).
- Isolation is at the data-directory level (login/cookies/storage). A few global
  side effects aren't sandboxed (e.g. the app registers a shared Chrome
  native-messaging host).
- If Anthropic significantly changes how the Windows app is packaged, the mirror
  step may need updating.

## Project layout

```
src/                 launcher UI (HTML/CSS/JS — premium dark)
src-tauri/src/main.rs  detection, mirroring, isolated-launch logic
src-tauri/tauri.conf.json / capabilities / icons
tools/preview-ui.js  optional: preview the launcher UI in a browser
```
