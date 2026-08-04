# Ruxius

**Package any PHP application into a single, portable Windows `.exe` — without recompiling PHP or rebuilding your app.**

Ruxius is a lightweight Rust-powered packager that turns a **PHP runtime + your application files** into a standalone Windows executable.

Build `ruxius.exe` once. After that, packaging an application is just a file operation:

```powershell
rux build .\my-app php74 .\dist\MyApp.exe
```

That's it.

The resulting `MyApp.exe` contains everything it needs to run your PHP application. On first launch, Ruxius extracts the bundled runtime, starts PHP's built-in web server, opens the application in a native **WebView2** window, and cleans everything up when the app closes.

> **Build Ruxius once. Package PHP apps forever.**

---

## ✨ Features

* 📦 **Single-file distribution** — your PHP app ships as one `.exe`.
* ⚡ **No compilation during packaging** — `rux build` only packages existing files.
* 🐘 **Bring your own PHP runtime** — package any compatible Windows PHP build.
* 🔌 **PHP extensions included** — the correct `ext/` directory is bundled with each PHP runtime.
* 🖥️ **Native desktop window** — runs inside WebView2 instead of a browser.
* 🚀 **Fast subsequent launches** — extracted files are cached and reused.
* 🧩 **Multiple PHP versions** — register and switch between PHP installations.
* 🛡️ **Process cleanup** — PHP is supervised and terminated when the app exits.
* 🔒 **Isolated applications** — each packaged payload gets its own extraction directory.
* 🪶 **No runtime dependency on Ruxius** — distributed applications don't need the builder.

---

## How It Works

Ruxius separates **building the packager** from **packaging an application**.

You compile Ruxius itself only once:

```powershell
cargo build --release
```

After that, `rux build` doesn't invoke Rust, Cargo, or any compiler.

It simply:

1. Copies the Ruxius executable.
2. Compresses your PHP runtime and application files.
3. Appends the archive to the executable.
4. Writes a small footer describing the embedded payload.
5. Produces a new standalone `.exe`.

Conceptually:

```text
┌───────────────────────┐
│      ruxius.exe       │
│                       │
│ Generic executable    │
│ + packaging logic     │
└───────────┬───────────┘
            │
            │ rux build
            │
            ▼
┌──────────────────────────────────┐
│            MyApp.exe             │
│                                  │
│  [ Ruxius runtime ]              │
│  [ PHP runtime ]                 │
│  [ PHP extensions ]              │
│  [ Application files ]           │
│  [ Payload footer ]              │
└────────────────┬─────────────────┘
                 │
                 │ Double-click
                 ▼
        ┌──────────────────┐
        │ First launch     │
        │                  │
        │ Extract payload  │
        │ Start PHP server │
        │ Open WebView2    │
        └────────┬─────────┘
                 │
                 ▼
          Your PHP Desktop App
```

### Builder vs. Packaged Application

Ruxius uses the same executable for both roles.

**No payload footer:**

```text
ruxius.exe
    │
    └── Builder / CLI
```

Running it displays the CLI help and allows commands such as:

```powershell
rux php list
rux php add php83 "C:\php\php.exe"
rux build .\my-app php83 .\MyApp.exe
```

**Payload footer present:**

```text
MyApp.exe
    │
    └── Packaged application
```

Double-clicking it automatically launches the bundled PHP application.

No CLI command is required.

---

# 🚀 Getting Started

## 1. Build Ruxius

You only need to do this when building or updating Ruxius itself.

```powershell
cargo build --release
```

The executable will be created at:

```text
target/release/ruxius.exe
```

You can keep this executable and use it as your application packager.

> **Important:** You do **not** need Rust or Cargo on machines where you use the finished `ruxius.exe` to package applications.

---

## 2. Register PHP

Register the PHP installations you want to package:

```powershell
rux php add php74 "C:\php\php7.4\php.exe"
rux php add php83 "C:\php\php8.3\php.exe"
```

List registered and automatically discovered PHP installations:

```powershell
rux php list
```

You can also skip registration entirely and provide a direct path to `php.exe` when building.

---

## 3. Package Your PHP Application

Assuming your application looks like this:

```text
my-app/
├── index.php
├── assets/
├── config/
└── ...
```

Run:

```powershell
rux build .\my-app php74 .\dist\MyApp.exe
```

Where:

| Argument           | Description                     |
| ------------------ | ------------------------------- |
| `.\my-app`         | PHP application's document root |
| `php74`            | Registered PHP runtime          |
| `.\dist\MyApp.exe` | Output executable               |

The result:

```text
dist/
└── MyApp.exe
```

Run it directly:

```powershell
.\dist\MyApp.exe
```

Or simply double-click it.

---

## 4. Try the Example

Ruxius includes a minimal PHP application so you can test the complete workflow immediately.

```powershell
rux build .\examples\sample-app php74 .\dist\Sample.exe
```

Then:

```powershell
.\dist\Sample.exe
```

You should see your PHP application open in its native WebView2 window.

---

# 📦 What's Inside the `.exe`?

A packaged Ruxius application is essentially:

```text
┌─────────────────────────────┐
│ Ruxius executable           │
├─────────────────────────────┤
│ Compressed PHP runtime      │
├─────────────────────────────┤
│ PHP extensions              │
├─────────────────────────────┤
│ Your PHP application        │
├─────────────────────────────┤
│ Payload metadata / footer   │
└─────────────────────────────┘
```

The payload is appended directly to a copy of the Ruxius executable.

This means the final application is **one portable `.exe` file**.

You don't need to distribute:

* PHP
* `php.exe`
* PHP extension DLLs
* your application directory
* a separate launcher
* Ruxius itself

The target machine only needs the **WebView2 Runtime**, which is already installed on most modern Windows systems.

---

# 🐘 PHP Extensions & `extension_dir`

PHP extensions are ABI-specific.

For example, an extension DLL built for PHP 8.3 cannot simply be reused with PHP 7.4.

Ruxius handles this automatically.

When running:

```powershell
rux build .\my-app php83 .\MyApp.exe
```

Ruxius looks next to the selected `php.exe` for:

```text
ext/
```

or:

```text
extensions/
```

and bundles the appropriate directory with the PHP runtime.

At runtime, the packaged application explicitly points PHP at its bundled extension directory:

```text
-d extension_dir=<bundled-extension-directory>
```

This ensures that the application loads extensions belonging to the **exact PHP runtime it was packaged with**, rather than accidentally picking up extensions from another installation.

---

# ⚡ Smart Extraction

Ruxius doesn't unpack the application every time it launches.

On first run, the payload is extracted to:

```text
%LOCALAPPDATA%\Ruxius\apps\<payload-checksum>\
```

On subsequent launches, Ruxius detects the existing extraction and starts the application directly.

This makes repeated launches significantly faster.

The payload checksum also provides isolation between applications and versions:

```text
%LOCALAPPDATA%\Ruxius\
└── apps/
    ├── 8f2a.../
    │   └── App A
    │
    ├── 31bd.../
    │   └── App B
    │
    └── c9e1.../
        └── App A (new version)
```

Different applications — and different builds of the same application — won't overwrite each other.

---

# 🖥️ Runtime Architecture

When a packaged application starts, Ruxius performs roughly this sequence:

```text
MyApp.exe
    │
    ├── Detect embedded payload
    │
    ├── Calculate payload identity
    │
    ├── Extract if necessary
    │
    ├── Find a free localhost port
    │
    ├── Start PHP
    │      └── php.exe -S 127.0.0.1:<port>
    │
    ├── Create native WebView2 window
    │
    ├── Navigate to local PHP application
    │
    └── On exit
           ├── Close WebView2
           └── Terminate PHP process
```

The result is a PHP application that behaves like a native desktop application while keeping the development model of a normal PHP web application.

---

# 🧰 CLI Reference

```text
rux
    No payload:
        Show CLI help

    Payload detected:
        Launch packaged application


rux build <app> <php> <output>
    Package a PHP application into a standalone .exe

    <app>:
        Application document root

    <php>:
        Registered PHP name or direct path to php.exe

    <output>:
        Destination executable


rux php add <name> <path>
    Register a PHP installation


rux php remove <name>
    Remove a registered PHP installation


rux php list
    List registered and automatically discovered PHP installations


rux --help
    Show complete CLI help


rux --version
    Print the Ruxius version
```

---

# ⚙️ PHP Registry

Registered PHP installations are stored globally at:

```text
%LOCALAPPDATA%\Ruxius\config.json
```

The registry is shared across all applications packaged with Ruxius.

For example:

```text
PHP 7.4  → php74
PHP 8.1  → php81
PHP 8.3  → php83
```

You can then choose the runtime during packaging:

```powershell
rux build .\legacy-app php74 .\Legacy.exe
rux build .\modern-app php83 .\Modern.exe
```

---

# 📁 Project Structure

```text
Ruxius/
├── Cargo.toml
├── examples/
│   └── sample-app/
│       └── index.php
│
└── src/
    ├── main.rs        # CLI entry point and orchestration
    ├── cli.rs         # clap CLI definition
    ├── payload.rs     # Self-appending package format
    ├── config.rs      # PHP registry and configuration
    ├── extract.rs     # Payload extraction and caching
    ├── php.rs         # PHP process management
    ├── webview.rs     # Native WebView2 window
    ├── version.rs     # Ruxius version information
    ├── logger.rs      # File and stdout logging
    └── error.rs       # Centralized error types
```

---

# 💻 Requirements

### Target machines

* **Windows 10 / 11**
* **x64**
* **WebView2 Runtime**

WebView2 is already present on most modern Windows installations.

### Building Ruxius

Only required when compiling Ruxius itself:

* **Rust 1.85+**
* **Edition 2024**
* **MSVC toolchain**

Install the target with:

```powershell
rustup target add x86_64-pc-windows-msvc
```

### PHP

A Windows PHP build is required for packaging.

For best compatibility, use the official **x64 Non Thread Safe (NTS)** builds.

Download PHP from:

[windows.php.net](https://windows.php.net/download/)

Then point Ruxius at the included `php.exe`:

```powershell
rux php add php83 "C:\php\php83\php.exe"
```

---

# 📍 Runtime Files

Ruxius keeps its runtime data under:

```text
%LOCALAPPDATA%\Ruxius\
```

| Resource                     | Location                     |
| ---------------------------- | ---------------------------- |
| Extracted applications       | `apps\<payload-checksum>\`   |
| PHP registry & configuration | `config.json`                |
| Logs                         | `logs\ruxius-YYYY-MM-DD.log` |
| Single-instance lock         | `ruxius.lock`                |

---

# 🛠️ Development

Clone the repository and build:

```powershell
cargo build --release
```

Before submitting a PR, please run:

```powershell
cargo fmt
cargo clippy
```

Issues and pull requests are welcome.

---

# 📄 License

Ruxius is released under the **MIT License**.

See [`LICENSE`](LICENSE) for the complete license text.

---

<div align="center">

**Ruxius**

*PHP → Windows Desktop*

Build once. Package anywhere.

</div>
