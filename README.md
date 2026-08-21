# Ruxius

**Ruxius** packages a PHP web app into a standalone desktop executable —
without ever recompiling anything. Built for Windows first (and that's
the only platform this has actually been run on); macOS and Linux support
is implemented and platform-gated throughout the code, but genuinely
unverified — see [Platform support](#platform-support) for exactly what
that means before relying on it there.

You compile `ruxius` **once**. From then on, `rux` itself is the tool you
use to stamp out apps: point it at a PHP install and a folder of PHP
files, and it produces a new executable with everything bundled in.
Building an app is a packaging step, not a Rust build.

```powershell
rux build .\my-app php74 MyApp.exe
```

`MyApp.exe` is now a single, portable file. Running it:

- opens its window immediately with a small splash screen, while —
- extracting its bundled PHP + app files to the platform's local app-data
  directory (once — later runs skip straight to launch) and
- starting PHP's built-in server on a free localhost port, then
- navigating that same window to the real app the moment it's ready
  (WebView2 on Windows, WKWebView on macOS, WebKitGTK on Linux),
- and cleaning up after itself on close — no leftover PHP processes.

## How it works

`ruxius.exe` is a generic stub. `rux build` copies that stub and appends a
compressed archive of your PHP install and app files to the end of the
copy, followed by a small footer recording where the appended data starts.
Any Ruxius executable checks for that footer on startup:

- **No footer found** → this is the bare builder; running it with no
  arguments just prints the normal `--help` output instead of opening an
  empty window.
- **Footer found** → this is a built app; running it (e.g. by double
  clicking it) extracts and launches its bundled PHP + app — no commands
  needed.

```
┌─────────────────┐   rux build app php74 out.exe   ┌────────────────────────┐
│ ruxius.exe (stub,│ ────────────────────────────────▶│ out.exe                │
│ nothing bundled) │  copies self + appends payload    │ [stub][php+app][footer]│
└──────────────────┘                                  └───────────┬────────────┘
                                                                    │ double-click
                     first launch: extract once           ┌────────▼─────────────┐
                     %LOCALAPPDATA%/Ruxius/apps/<hash>/    │ php.exe -S ...  ◀▶ WebView   │
                                                            └───────────────────────────┘
```

No compiler, no `cargo build`, involved in producing `out.exe` — just file
copying and archiving.

## Getting started

### 1. Build `ruxius.exe` (once)

```powershell
cargo build --release
```

Output: `target/release/ruxius.exe`. This is your packager — keep it around,
you won't need to rebuild it again unless you're changing Ruxius itself.

### 2. Register a PHP install

```powershell
rux php add php74 "C:\php\php7.4\php.exe"
rux php add php83 "C:\php\php8.3\php.exe"
```

Not sure what's on your machine? `rux php list` shows registered versions
plus anything it can find automatically in common install locations and on
`PATH`.

By default this registry lives in `%LOCALAPPDATA%\Ruxius\config.json`. If
you'd rather carry `ruxius.exe` around (a USB stick, a shared network
drive, between machines) with its registry travelling with it instead of
being tied to one PC, drop an empty `config.json` next to `ruxius.exe`:

```powershell
'{}' | Out-File -Encoding utf8 .\config.json
```

From then on, `rux php add/remove` and `rux build` read and write that
file instead of `%LOCALAPPDATA%`, for as long as it's sitting next to the
executable.

### 3. Package your app

```powershell
rux build .\my-app php74 .\dist\MyApp.exe
```

- `.\my-app` — your PHP application's document root (an `index.php`
  and whatever else it needs).
- `php74` — the registered name from step 2 (a direct path to a `php.exe`
  also works, if you'd rather skip registering one).
- `.\dist\MyApp.exe` — where to write the finished, standalone executable.

Try it against the bundled example first if you want to see it work end to
end:

```powershell
rux build .\examples\sample-app php74 .\dist\Sample.exe
.\dist\Sample.exe
```

### 4. Ship it

`MyApp.exe` is the whole deliverable — copy it anywhere, send it to
someone, whatever. It doesn't depend on `ruxius`, PHP being installed, or
anything else on the target machine except a WebView backend (WebView2 on
Windows, present on most modern installs already).

## CLI reference

```
rux                               No app bundled: show this help (the builder)
                                   App bundled: launch it (what double-click does)

rux build <app> <php> <output>   Package <app>'s files + <php> into <output>.exe
                                  <php> is a registered name or a path to php.exe
  --title <TITLE>                 Window title (default: the app folder's name)
  --width <PIXELS>                 Window width (default: 1400)
  --height <PIXELS>                 Window height (default: 900)
  --force                           Rebuild even if output is already up to date
  --watch                           Keep watching <app> and rebuild on change (Ctrl+C to stop)
  --icon <path.ico>                 Set the built .exe's icon (Windows only, see below)
  --php-ini key=value                Extra php.ini directive, repeatable (see below)

rux php add <name> <path>        Register a PHP install under <name>
rux php remove <name>            Un-register it
rux php list                     List registered + auto-discovered PHP installs,
                                   each with its `php -v` version string and
                                   whether it's been archived (see below)
rux php clear-cache              Delete cached build archives (see below)
rux php archive                  Snapshot every registered PHP install into a
                                   <name>.pack file, for faster builds (see below)

rux php ext list <php>            List extensions configured (and available but
                                    not configured) in a PHP install's php.ini
rux php ext enable <php> <ext>    Enable an extension (uncomments or adds a line)
rux php ext disable <php> <ext>   Disable an extension (comments the line out)

rux logs                         Live-tail the most recently active app's PHP
                                   output (see below)
  --lines <N>                     How many existing lines to show first (default: 20)

rux config export <path>         Write the PHP registry to a JSON file
rux config import <path>         Load a PHP registry from a JSON file

rux init                         Guided first-time setup (see below)

rux doctor                       Check this machine for what rux needs
                                   (a WebView backend, registered PHP installs)

rux tui                          Open the interactive terminal dashboard (see below)

rux --help                       Full help
rux --version                    Print the version
```

```powershell
rux build .\my-app php74 .\dist\MyApp.exe --title "My App" --width 1200 --height 800
```

`rux php add/remove/list` manage the version registry used by `rux build`
(stored once in `%LOCALAPPDATA%\Ruxius\config.json`, shared across every
app you build).

### Pre-archiving PHP installs (`.pack` files)

The automatic build cache (below) still walks and fingerprints a PHP
directory on every build to check whether anything changed. `rux php
archive` skips that check entirely: it snapshots every registered PHP
install into a `<name>.pack` file (under `%LOCALAPPDATA%\Ruxius\packs\`,
or next to a portable `config.json` if you're using one), and from then on
`rux build` reads that file straight off disk instead of touching the PHP
directory at all — no walk, no `stat` calls, no re-read, no
re-compression.

```powershell
rux php archive
rux build .\my-app php74 .\dist\MyApp.exe    # uses php74.pack automatically
```

This is a deliberate trust-the-user tradeoff, unlike the automatic cache:
a `.pack` is treated as current from the moment it's made, not
re-validated against the live PHP directory on every build. If you update
a PHP install (add an extension, patch a DLL, whatever), re-run `rux php
archive` — otherwise builds will keep using the snapshot from whenever you
last archived it. `rux php list` shows which registered versions currently
have a `.pack`.

### Managing PHP extensions

```powershell
rux php ext list php74
rux php ext enable php74 curl
rux php ext disable php74 gd
```

`ext list` shows every `extension=`/`zend_extension=` line already in that
install's `php.ini` (enabled or commented-out), plus any `php_*.dll` sitting
in its `ext/` folder that isn't mentioned in `php.ini` at all yet. `enable`
uncomments an existing line, or adds a fresh `extension=<name>` line if the
DLL exists but was never configured; `disable` comments the matching line
out. Extension names work with or without the `php_`/`.dll` decoration
(`curl`, `php_curl`, and `php_curl.dll` all match the same thing). Writes
to `php.ini` are atomic (written to a temp file, then renamed into place),
so an interrupted write can't leave a half-written `php.ini` behind.

### Rebuilding on change (`--watch`)

```powershell
rux build .\my-app php74 .\dist\MyApp.exe --watch
```

Builds once immediately, then keeps watching `<app>`'s files and rebuilds
automatically whenever anything changes, until you hit Ctrl+C. There's no
extra file-watching dependency behind this — it reuses the same
fingerprinting the build cache already does (walk the directory, hash
names/sizes/mtimes) on a ~750ms poll, so a change is picked up almost
immediately without needing OS-level filesystem-event support. A failed
rebuild (a syntax error you haven't saved past yet, say) is printed and
watching continues — same as an editor with a linter wouldn't stop
watching your file just because it's mid-edit.

### Setting a custom icon (`--icon`)

```powershell
rux build .\my-app php74 .\dist\MyApp.exe --icon .\my-app\icon.ico
```

Windows only. Since `rux build` never recompiles, this can't work the way
`winres`/`embed-resource` do (baking an icon in at `cargo build` time) —
instead it patches the icon directly into the *already-built* executable's
PE resources, the same mechanism the Electron ecosystem's `rcedit` tool
uses. Concretely: it parses your `.ico` file, then uses
[`winres-edit`](https://docs.rs/winres-edit) (a thin wrapper around the
real `BeginUpdateResource`/`UpdateResource`/`EndUpdateResource` Win32
calls) to write each image size as its own icon resource, on a temporary
copy of the stub — before the app payload gets appended to it, so the
self-appending format's checksum/footer logic is always the last thing to
touch the final file, never interleaved with resource editing.

One thing to know: since the icon only affects the *stub* and the build
cache keys off your app/PHP files, changing just `--icon` on an
otherwise-unchanged app won't retrigger a rebuild on its own — pass
`--force` if you're iterating on the icon specifically.

### Custom php.ini directives (`--php-ini`)

```powershell
rux build .\my-app php74 .\dist\MyApp.exe --php-ini memory_limit=512M --php-ini upload_max_filesize=64M
```

Repeatable `key=value` pairs, passed straight to PHP's built-in server as
`-d key=value` flags at launch — the packaged `php.ini` itself is never
touched. Command-line `-d` already takes precedence over `php.ini`, so
this is both simpler and safer than rewriting an ini file inside the
payload would be.

### Guided setup (`rux init`)

```powershell
rux init
```

For getting started without reading the rest of this file first: checks
for a WebView backend, walks you through registering a PHP install
(auto-discovering candidates the same way `rux php list` does, or you can
paste a path directly), and offers to drop a minimal sample app in the
current directory so there's something to `rux build` immediately. Ends
by printing the exact command to try next.

### Watching PHP errors live

Every launch of a built app writes PHP's own output — its per-request
access log, plus any warnings, notices, and fatal errors — to
`%LOCALAPPDATA%\Ruxius\logs\php-<checksum>.log` (one file per distinct
built app, appended across runs rather than overwritten, so relaunching to
reproduce something doesn't lose what you were looking at). Two ways to
watch it:

```powershell
rux logs                 # live-tail from a terminal, Ctrl+C to stop
rux logs --lines 100      # show more history before tailing
```

or the **Logs** tab in `rux tui`, which does the same thing without
leaving the dashboard. Both just watch whichever `php-*.log` was written
to most recently — "the app you last ran" — rather than needing you to
specify which built app you mean.

### Sharing a PHP registry between machines

```powershell
rux config export ruxius-config.json
rux config import ruxius-config.json
```

Exports the PHP version registry (the names and paths `rux php add` set
up) as a single JSON file. Importing merges into whatever's already
registered on the target machine — entries with the same name are
updated, everything else registered there is left alone. Worth running
`rux php list` afterward, since a path valid on one machine obviously
might not resolve on another.

### Faster rebuilds

A PHP install rarely changes, but it's usually the bulk of a payload's
size — repacking and recompressing it on every single `rux build` would
make iterating on your app files slow. So `rux build` fingerprints both
the PHP directory and the app directory it's given (file names, sizes,
modified times) and caches each one's compressed archive separately under
`%LOCALAPPDATA%\Ruxius\cache\archives\`. Whichever side hasn't actually
changed since the last build — usually PHP, but if you haven't touched
your app files either, that gets skipped too — is reused as-is instead of
being re-walked and re-compressed. If the resulting output would end up
byte-for-byte identical to what's already there, `rux build` skips
rewriting it entirely and tells you so (pass `--force` to rebuild anyway).
If a cached archive ever seems stale or you just want to reclaim the disk
space, `rux php clear-cache` wipes the whole cache — the next build just
repacks everything from scratch.

On top of the caching, packing itself is parallel and avoids redundant
work at the algorithm level, not just the thread level:

- **One directory walk, not two.** Fingerprinting a PHP install and
  archiving it both need the same file list, so `rux build` walks the
  directory tree once and shares that list between them, instead of
  `walkdir`-ing it twice.
- **Parallel `stat`s for fingerprinting.** The fingerprint only needs each
  file's path, size, and modified time — no file contents — but that's
  still one syscall per file. Those run concurrently across a chunked
  thread pool (`payload::parallel_map`), then get folded into a single
  `Sha256` sequentially, since hashing itself is cheap and a hasher can't
  be parallelized across threads anyway.
- **Parallel file reads for archiving.** On a cache miss, every file's
  contents are read concurrently (I/O-bound work with real parallelism
  headroom) into memory first; only the actual tar-writing — pure memory
  copies at that point — happens on a single thread afterwards, since
  `tar::Builder` is inherently a sequential stream.
- **The PHP archive and the app archive are built on separate threads**
  via `std::thread::scope`, so neither has to wait for the other.
- **Compression itself is multithreaded** — zstd splits the work across
  all available CPU cores — which matters most exactly when it's needed
  most: a cache miss, where the PHP archive has to be compressed from
  scratch.
- **Extraction mirrors this**: on first run (or an update), the `php/` and
  `app/` archives are decompressed and unpacked concurrently too.

`rux php list` and PHP auto-discovery use the same `parallel_map` helper
to fetch every `php -v` version string and check every `PATH` directory
concurrently, instead of waiting on each process/stat in turn.

### About `extension_dir`

Different PHP builds compile their extension DLLs against that specific
build's ABI — a `php_*.dll` from PHP 8.3 will not load in PHP 7.4, and vice
versa. `rux build` looks for an `ext/` (or `extensions/`) folder next to
whichever `php.exe` you point it at and packages it alongside the binary;
the built app passes that folder explicitly via `-d extension_dir=...`
when starting its server, so the right extensions always load for the PHP
version actually bundled — never a stale directory left over from a
different install.

## Laravel & Symfony

PHP's built-in server (what every built app runs) has no rewrite rules of
its own — a request for `/login` just 404s unless something routes it to
the front controller, and Laravel/Symfony both expect to be served from a
`public/` folder, not the project root. `rux build` handles both
automatically:

- **Detects the layout.** If `public/index.php` exists, that becomes the
  docroot instead of the project root. `artisan` in the project root means
  Laravel; `bin/console` means Symfony; a bare `public/index.php` with
  neither is treated the same way as a generic "framework-shaped" app.
- **Injects a router script** — the same "serve real files as-is,
  otherwise hand off to the front controller" pattern Laravel's own `php
  artisan serve` uses under the hood. It's generated in memory and packed
  straight into the app archive; nothing gets written into your actual
  project folder.
- **Warns about missing setup it can't do for you.** Ruxius has no network
  access and won't run Composer on your behalf, so `rux build` checks for
  `vendor/autoload.php` and `.env` and prints a warning (not a hard
  failure — you might be configuring things another way) if either is
  missing.

```powershell
rux build .\my-laravel-app php83 .\dist\MyApp.exe
```

```
Packaging app
  app:       .\my-laravel-app
  framework: Laravel (serving from .\my-laravel-app\public)
  php:       php83 (C:\php\php8.3\php.exe)
  ...
```

Plain, non-framework apps (a flat `index.php` at the root, no `public/`
folder) are packaged exactly as before — this only activates when a
`public/index.php` is actually detected.

## Security

- **Payload integrity.** Every built app's appended payload carries a
  SHA-256 checksum in its footer; the payload is re-hashed and checked
  against it before extraction, and again before use, so silent
  corruption (bad copy, truncated download, disk error) is caught rather
  than run.
- **Safe extraction.** Extraction goes through the `tar` crate's own
  unpacking, which rejects `..` path components and validates every
  entry stays inside the destination directory — a malformed or
  malicious archive can't write outside where it's supposed to.
- **No network access, anywhere.** Ruxius doesn't download, phone home,
  or fetch anything — every operation is local files in, local files (or
  an appended `.exe`) out. There's no update check, no telemetry, nothing
  to intercept.
- **Won't clobber an unrelated file.** If `rux build`'s output path
  already exists and doesn't look like a previous Ruxius build (i.e. it's
  some other program entirely), the build refuses and tells you, instead
  of silently overwriting it. Overwriting a *previous* Ruxius build (the
  normal rebuild case) is unaffected; `--force` skips the check entirely
  if you're certain.
- **Single-instance locking** uses an OS-level exclusive file lock
  (released automatically on exit, including a crash), so two copies of
  the same built app can't race over the same port or extraction
  directory.
- **`.pack` files are integrity-checked, not blindly trusted forever.**
  Every `.pack` carries a SHA-256 checksum of its contents, verified
  before use — a corrupted or truncated `.pack` is a hard error, not a
  silent fallback that could mask real data loss. What integrity-checking
  *can't* do: detect that a `.pack` no longer matches its source PHP
  install, since that's the entire point of archiving one (skip
  re-checking). Treat a `.pack` with the same trust you'd give the PHP
  install it came from — don't copy one in from somewhere you don't
  trust, and re-run `rux php archive` after changing that install.
- **Extension names are validated before ever touching php.ini.**
  Enabling an extension that isn't already configured appends a new
  `extension=<name>` line built from user/TUI input; that name is
  restricted to alphanumerics, `_`, `-`, and `.` before it's used, so a
  crafted name can't inject arbitrary lines (a new `[section]`, another
  directive, etc.) into php.ini. All php.ini writes — from either the CLI
  or the TUI — go through the same validation, and are atomic (temp file
  + rename) so an interrupted write can't leave a half-written php.ini
  that breaks every extension on next launch.
- **php.ini is backed up before Ruxius ever touches it.** The first time
  an extension is toggled for a given install, the untouched `php.ini` is
  copied to `php.ini.orig` (never overwritten after that first copy), so
  there's always an unmodified baseline to restore from by hand — no
  matter how many extensions get flipped afterward.
- **Window titles are sanitized.** `--title` is stripped of control
  characters (including newlines) and capped at a sane length before it's
  handed to the OS's window-title API — a crafted title can't do anything
  unexpected there. Width/height are clamped to a sane range for the same
  reason.

## UX

- **Colored, readable output** — status lines are tagged and colored
  (`ok` in green, `warn` in yellow, `error`/`[MISSING]` in red) throughout
  `rux doctor`, `rux php list`, and `rux build`. Color respects
  [`NO_COLOR`](https://no-color.org), turns itself off automatically when
  output isn't a real terminal (e.g. piped to a file), and enables modern
  Windows terminals' ANSI support itself rather than assuming it's on.
  There's no dependency involved — it's a small internal module
  (`src/ui.rs`) using plain ANSI codes, so there's no external crate
  behavior to account for.
- **Progress feedback that means something.** `rux build` shows a live
  spinner while packing (falling back to a plain "...” line when color is
  off) and reports how long each step actually took, so a fast cache-hit
  rebuild and a slow first build are visibly different instead of both
  just going quiet for a while.
- **Clear errors.** Every failure path — a bad PHP path, a missing app
  folder, an about-to-be-clobbered file — explains what went wrong and
  what to do about it, not just that something failed.
- **A real terminal UI for the parts that benefit from one.** Registering
  PHP installs, archiving, and flipping extensions on and off is all
  keyboard-driven via `rux tui` if you'd rather not type individual
  commands — see [TUI](#tui) below.
- **`rux doctor` runs its checks concurrently** (WebView backend scan, PHP
  validation, cache/pack directory reads) instead of one after another,
  for the same reason build packing does — they're independent I/O, not
  CPU work, so there's no reason to serialize them.

## TUI

```powershell
rux tui
```

An interactive terminal dashboard covering the same ground the old GUI
did (plus more) — no browser engine, no window toolkit, just a
full-screen keyboard UI (built on `ratatui`) that runs anywhere a
terminal does.

- **PHP Versions** — `Tab`/arrows to navigate, `a` to add (prompts for a
  name then a path), `d` to remove the selected one, `x` to archive every
  registered version at once (runs in the background — the list stays
  interactive while it works), `r` to refresh.
- **Extensions** — `←`/`→` switches which PHP install you're looking at,
  `↑`/`↓` selects an extension, `Enter` toggles it on or off.
- **Build** — `b` starts a build: app folder, then PHP, then output path
  (pre-filled with a sensible default), each a quick prompt. Runs in the
  background with a live elapsed timer, same as archiving — the dashboard
  stays interactive while a slow first-time PHP pack happens.
- **Logs** — live-tails whichever built app's PHP log was most recently
  written to (see [`rux logs`](#watching-php-errors-live) below), colored
  by severity. Just switch to the tab; it stays current on its own.
- **Doctor** — the same checks as `rux doctor`, `r` to re-run them.
- `q` or `Esc` quits from anywhere.

Every action calls the exact same functions the CLI commands do — the TUI
isn't a separate implementation, just a different way to drive them.

## Project structure

```
Ruxius/
├── Cargo.toml
├── examples/
│   └── sample-app/       # minimal PHP app to try `rux build` against
│       └── index.php
└── src/
    ├── main.rs             # CLI entry point + orchestration
    ├── cli.rs                # clap CLI definition
    ├── payload.rs              # self-appending package format (build/detect/read)
    ├── pack.rs                   # .pack file format (rux php archive)
    ├── ext.rs                      # php.ini extension manager
    ├── framework.rs                  # Laravel/Symfony detection + router script
    ├── icon.rs                         # .ico parsing + PE resource embedding
    ├── config.rs                         # persisted config: PHP registry + overrides
    ├── extract.rs                        # first-run/update extraction logic
    ├── php.rs                              # spawns & supervises the PHP server
    ├── webview.rs                            # native WebView window (wry/tao)
    ├── tui.rs                                  # rux tui: terminal dashboard
    ├── ui.rs                                     # colored CLI output + spinner
    ├── version.rs                                  # rux's own version string
    ├── logger.rs                                     # rotating file + stdout logging
    └── error.rs                                        # centralized error types
```

## Platform support

Ruxius has actually been built and run on **Windows** — everything in
this README is written from that experience, and it's the only platform
any of this has been verified on.

macOS and Linux support is implemented, not aspirational: PHP binary
discovery checks Homebrew/`/usr/local`/`/opt` paths as well as `C:\`,
the WebView check knows to look for WKWebView vs. `webkit2gtk` instead of
WebView2, console-hiding and ANSI-color code only compiles in on Windows,
and the self-appending executable format doesn't depend on anything
Windows/PE-specific (macOS/Linux binaries tolerate trailing appended data
past their real end the same way Windows .exe files do — this is the same
trick self-extracting `.run`/`makeself` archives use). But none of that
has been exercised on an actual Mac or Linux box. If you try it there and
something's off, that's genuinely useful to know about.

FreeBSD is a further reach — `wry`/`tao` use the same GTK/WebKitGTK
backend there as Linux in principle, but this hasn't been tested and
there's less real-world usage to lean on than for the three
platforms above.

## Requirements

**Windows**
- Windows 10/11 x64, with the WebView2 Runtime (preinstalled on most
  modern systems; run `rux doctor` to check).
- **Rust** 1.85+ (edition 2024) with the MSVC toolchain (`rustup target
  add x86_64-pc-windows-msvc`) — only needed to build `ruxius` itself, not
  to package apps with it.
- A PHP build for Windows — official builds at
  [windows.php.net/download](https://windows.php.net/download/) (grab the
  **Non Thread Safe (NTS)** x64 zip and point `rux php add` at its
  `php.exe`).

**macOS** *(unverified — see [Platform support](#platform-support))*
- WKWebView is built into the OS; nothing extra to install for it.
- Rust 1.85+ via `rustup`.
- A PHP build — Homebrew's (`brew install php`) is the standard route;
  `rux php add` should find it under `/opt/homebrew` or `/usr/local`.

**Linux** *(unverified — see [Platform support](#platform-support))*
- `webkit2gtk` (and its `-dev`/`-devel` package to *build* `ruxius`, since
  `wry` links against it) — e.g. `libwebkit2gtk-4.1-dev` on Debian/Ubuntu,
  `webkit2gtk4.1-devel` on Fedora. `rux doctor` checks for it via
  `pkg-config` at runtime.
- Rust 1.85+ via `rustup`.
- Your distro's PHP package (`apt install php-cli`, `dnf install php-cli`,
  etc.), or a manually built one — `rux php add` checks `/usr/bin`,
  `/usr/local/bin`, `/opt`, and `PATH`.

## Where things live at runtime

All of it lives under one platform-appropriate app-data directory,
resolved by the [`directories`](https://docs.rs/directories) crate —
`%LOCALAPPDATA%\Ruxius\` on Windows (shown below), `~/Library/Application
Support/com.Ruxius.Ruxius/` on macOS, `~/.local/share/Ruxius/` on Linux.

| What                          | Where (Windows)                                          |
|--------------------------------|-----------------------------------------------------------|
| Extracted app (per build)      | `%LOCALAPPDATA%\Ruxius\apps\<payload-checksum>\`        |
| Cached php/app archives (build-time)| `%LOCALAPPDATA%\Ruxius\cache\archives\`             |
| `.pack` files (`rux php archive`)| `%LOCALAPPDATA%\Ruxius\packs\`                        |
| Config (registry, overrides)   | `%LOCALAPPDATA%\Ruxius\config.json`                     |
| Ruxius's own diagnostic log     | `%LOCALAPPDATA%\Ruxius\logs\ruxius-YYYY-MM-DD.log`     |
| Per-app PHP output (`rux logs`) | `%LOCALAPPDATA%\Ruxius\logs\php-<checksum>.log`        |
| Single-instance lock           | `%LOCALAPPDATA%\Ruxius\ruxius.lock`                     |

Each built app extracts to a folder named after its own payload checksum,
so multiple Ruxius apps (or multiple versions of the same app) never
collide, and re-running `rux build` with unchanged inputs reuses the
existing extraction instead of re-copying files.

## Contributing

Issues and pull requests are welcome. Please run `cargo fmt` and
`cargo clippy` before submitting a PR.

## License

MIT — see [LICENSE](LICENSE).
