# OmniComms

One desktop window holding every chat service you use — WhatsApp Web, Discord,
Telegram, Google Messages, Instagram, and anything else reachable by URL — as
tabs in the title bar.

Built on [Tauri 2](https://v2.tauri.app). Each tab is a native OS webview, not a
bundled browser engine, so the whole app is ~13 MB and uses a fraction of the
memory an Electron equivalent would.

---

## Requirements

| | |
|---|---|
| **Rust** | stable toolchain (`rustc` + `cargo`) |
| **Node** | any current LTS — only used to run the Tauri CLI |
| **macOS** | Xcode Command Line Tools (`xcode-select --install`) |
| **Linux** | `webkit2gtk`, `libayatana-appindicator`, `librsvg` dev packages |
| **Windows** | WebView2 runtime (ships with Windows 11) |

Install Rust via [rustup](https://rustup.rs) or `brew install rust`.

## Running

Install the CLI once:

```bash
npm install
```

Run in development, with hot reload for the tab-strip UI:

```bash
npm run dev
```

Build a distributable bundle:

```bash
npm run build
```

Artifacts land in `src-tauri/target/release/bundle/` — on macOS that's
`macos/OmniComms.app` and `dmg/OmniComms_<version>_aarch64.dmg`.

The first build compiles the full Rust dependency tree and takes several
minutes; later builds take seconds.

---

## Using it

**Switch tabs** — click one in the title bar, or cycle with the keyboard:

| Shortcut | Action |
|---|---|
| `Ctrl`+`Tab` | Next tab (wraps around) |
| `Ctrl`+`Shift`+`Tab` | Previous tab |
| `Cmd`+`R` | Reload the current tab |

These also live under the **Tabs** menu.

**Reorder** — drag a tab sideways; the new order is saved immediately.

**Reload** — the `⟳` button in the title bar reloads the current tab.

**Add a service** — click `+`, enter a name and URL. The scheme is optional
(`web.signal.org` works). Any site that runs in a browser works here.

**Edit** — click `✎` to enter edit mode, where you can:
- rename a tab by typing in its field (Enter saves, Escape reverts)
- change its icon by clicking the icon button and picking an image file
- delete a tab with `✕`

Click `✓` to leave edit mode.

**Move the window** — drag the empty space in the title bar, to the right of the
last tab.

### Where your data lives

| Path (macOS) | Contents |
|---|---|
| `~/Library/Application Support/com.omnicomms.app/tabs.json` | Tab list, order, and active tab |
| `~/Library/Application Support/com.omnicomms.app/icons/` | Downloaded favicons, one file per tab |

On Linux this is `~/.config/com.omnicomms.app/`, on Windows
`%APPDATA%\com.omnicomms.app\`.

Each tab gets its own **isolated session** (cookies, storage, logins), keyed by a
per-tab `data_store_id`. Two tabs pointed at the same service can hold two
different accounts, and signing out of one doesn't touch the other.

---

## How it works

```
src/                     Tab-strip UI (plain HTML/CSS/JS, no framework)
  index.html
  tabbar.css
  tabbar.js
src-tauri/
  src/main.rs            Window, webviews, tab state, icon fetching
  capabilities/          Which webview may call which command
  permissions/           Auto-generated per-command ACL entries
  tauri.conf.json        Bundle + window config
app-icon.png             Master icon; regenerate the set with
                         `npx tauri icon app-icon.png`
```

The window holds one webview per tab plus a small one for the tab strip itself,
positioned by the backend. Tab webviews are created lazily on first visit and
kept alive afterwards, so switching back is instant.

### Notes on a few non-obvious decisions

**The tab strip lives in the title bar.** macOS draws the native title bar *over*
window content, so a strip placed at `y=0` gets its top ~28px clipped. The window
uses `TitleBarStyle::Overlay` with a hidden title, and the strip reserves 78px on
the left for the traffic lights.

**Webview bounds are set in physical pixels.** Tauri's multi-webview API is still
unstable and doesn't reliably honour the logical/physical distinction on macOS,
so logical offsets silently halve on a Retina display.

**Tabs report a desktop Safari user agent.** WKWebView's default UA trips the
browser-version gates on WhatsApp Web and others ("please update Safari") even on
current WebKit.

**Favicons are fetched in Rust, not in the page.** Several services — WhatsApp,
Instagram, Discord — refuse image requests coming from the webview's `tauri://`
origin, even where the same URL serves fine over plain HTTP. The backend reads
each page's declared `<link rel="icon">` (Discord and Google Messages use
non-standard paths; Instagram uses a CDN `.webp`) and falls back to the
conventional paths. Requests go only to the service you configured — no
third-party favicon proxy, so your tab list isn't disclosed to anyone.

**Only the tab strip can call commands.** The capability in
`src-tauri/capabilities/default.json` is scoped to the `tabbar` webview, so the
pages loaded in tabs have no access to the IPC layer at all.

**Window dragging uses `data-tauri-drag-region`, not CSS.** `-webkit-app-region`
is a Chromium property and does nothing in the WKWebView this runs in — the
attribute in `index.html` is what makes empty strip space drag the window.

**Tab cycling is a menu accelerator, not a key listener.** Keystrokes go to
whichever webview has focus, so a `keydown` handler in the tab strip would be
dead exactly when you need it — while you're using a service. Menu key
equivalents are handled before the webview sees the event.

---

## Code signing (macOS)

By default the build is **ad-hoc signed**, which works fine locally but has one
visible consequence: macOS asks for your login keychain password on most
launches, with a prompt about the *"OmniComms WebCrypto Master Key"*.

That happens because WebKit stores a WebCrypto key in your keychain, and the
keychain matches apps by code identity. An ad-hoc build gets a **new identity on
every rebuild**, so the access list never matches and "Always Allow" can't stick.

To stop the prompt, sign with a stable identity. A self-signed certificate is
enough — it doesn't need to be from Apple.

### 1. Create the certificate

In **Keychain Access** → *Certificate Assistant* → *Create a Certificate…*

| Field | Value |
|---|---|
| Name | `OmniComms Local Signing` |
| Identity Type | **Self Signed Root** |
| Certificate Type | **Code Signing** |

Getting *Certificate Type* right matters — the default is S/MIME, which
`codesign` cannot use.

### 2. Verify it registered

```bash
security find-identity -v -p codesigning
```

This **must** list `OmniComms Local Signing`. If it prints `0 valid identities
found`, the build will fail with `no identity found` — the certificate either
wasn't created, isn't a Code Signing certificate, or isn't trusted yet. To fix
trust, find it in Keychain Access, open it, expand **Trust**, and set *Code
Signing* to **Always Trust**.

### 3. Build with it

```bash
APPLE_SIGNING_IDENTITY="OmniComms Local Signing" npm run build
```

Launch the result once, click **Always Allow** on the prompt, and it will stay
quiet across future rebuilds as long as the same certificate signs them.

`tauri dev` ignores code signing entirely, so development runs will keep
prompting regardless. Plain `npm run build` stays ad-hoc signed and also keeps
prompting — the environment variable is what enables signing.

> Hardened runtime is deliberately **off**. It's only needed for Apple
> notarisation, and enabling it without a JIT entitlement breaks the webviews.

---

## Known gaps

- **Unsigned and un-notarised.** Distributing the `.dmg` to another machine will
  hit Gatekeeper. Shipping properly needs a paid Apple Developer ID, hardened
  runtime re-enabled, and a JIT entitlement for the webviews.
- **Only built and tested on macOS (Apple Silicon).** The codebase is
  cross-platform and the Windows/Linux targets should work, but neither has been
  run — expect the title-bar layout in particular to need adjusting, since the
  overlay style is macOS-specific.
- **The bundle identifier ends in `.app`** (`com.omnicomms.app`), which Tauri
  warns conflicts with the macOS bundle extension. Harmless now, but worth
  changing before distribution. Note that changing it moves the data directory,
  so you'd be signed out of every tab.
- **No notifications, badges, or global shortcuts.** Background tabs stay loaded
  and keep running, but nothing surfaces unread counts to the OS.
