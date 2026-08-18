# Ruxius

[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/sobhanmohammadi-dev/Ruxius)

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

## ARCHITECTURE

[See architecture](ARCHITECTURE.md)

## Contributing

Issues and pull requests are welcome. Please run `cargo fmt` and
`cargo clippy` before submitting a PR.

## License

MIT — see [LICENSE](LICENSE).
