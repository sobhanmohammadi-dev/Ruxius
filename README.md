# Ruxius

**Ruxius** packages a PHP web app into a standalone Windows desktop
executable — without ever recompiling anything.

You compile `ruxius.exe` **once**. From then on, `rux` itself is the tool you
use to stamp out apps: point it at a PHP install and a folder of PHP
files, and it produces a new `.exe` with everything bundled in. Building an
app is a packaging step, not a Rust build.

```powershell
rux build .\my-app php74 MyApp.exe
```

`MyApp.exe` is now a single, portable file. Running it:

- extracts its bundled PHP + app files to `%LOCALAPPDATA%` (once — later
  runs skip straight to launch),
- starts PHP's built-in server on a free localhost port,
- opens the app in a native **WebView2** window,
- and cleans up after itself on close — no leftover PHP processes.

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
                     %LOCALAPPDATA%/Ruxius/apps/<hash>/    │ php.exe -S ... ◀▶ WebView2 │
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
someone, whatever. It doesn't depend on `ruxius.exe`, PHP being installed, or
anything else on the target machine except the WebView2 runtime (present
on most modern Windows installs already).

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

rux php add <name> <path>        Register a PHP install under <name>
rux php remove <name>            Un-register it
rux php list                     List registered + auto-discovered PHP installs,
                                   each with its `php -v` version string
rux php clear-cache              Delete cached PHP archives (see below)

rux doctor                       Check this machine for what rux needs
                                   (WebView2 Runtime, registered PHP installs)

rux --help                       Full help
rux --version                    Print the version
```

```powershell
rux build .\my-app php74 .\dist\MyApp.exe --title "My App" --width 1200 --height 800
```

`rux php add/remove/list` manage the version registry used by `rux build`
(stored once in `%LOCALAPPDATA%\Ruxius\config.json`, shared across every
app you build).

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
    ├── config.rs                # persisted config: PHP registry + overrides
    ├── extract.rs                 # first-run/update extraction logic
    ├── php.rs                       # spawns & supervises the PHP server
    ├── webview.rs                     # native WebView2 window (wry/tao)
    ├── ui.rs                            # colored output + spinner (no external crate)
    ├── version.rs                        # rux's own version string
    ├── logger.rs                          # rotating file + stdout logging
    └── error.rs                             # centralized error types
```

## Requirements

- **Windows 10/11 x64** (WebView2 Runtime — preinstalled on most modern
  Windows systems; run `rux doctor` to check).
- **Rust** 1.85+ (edition 2024) with the MSVC toolchain (`rustup target
  add x86_64-pc-windows-msvc`) — only needed to build `ruxius.exe` itself, not
  to package apps with it.
- A PHP build for Windows — official builds at
  [windows.php.net/download](https://windows.php.net/download/) (grab the
  **Non Thread Safe (NTS)** x64 zip and point `rux php add` at its
  `php.exe`).

## Where things live at runtime

| What                          | Where                                                    |
|--------------------------------|-----------------------------------------------------------|
| Extracted app (per build)      | `%LOCALAPPDATA%\Ruxius\apps\<payload-checksum>\`        |
| Cached php/app archives (build-time)| `%LOCALAPPDATA%\Ruxius\cache\archives\`             |
| Config (registry, overrides)   | `%LOCALAPPDATA%\Ruxius\config.json`                     |
| Logs                           | `%LOCALAPPDATA%\Ruxius\logs\ruxius-YYYY-MM-DD.log`      |
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
