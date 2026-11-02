Solder preview for manual testing. This is a development build, with unfinished and unverified features tracked in [the roadmap](https://github.com/cire362/solder/blob/{{SOURCE}}/editor/ROADMAP.md).

Source: `{{SOURCE}}` · Tag: `{{TAG}}`.

Choose the archive for your computer:

- **macOS, Apple Silicon:** `macos-arm64.zip`. Requires macOS 15 or later. Unzip and move `Solder.app` to Applications.
- **macOS, Intel:** `macos-x86_64.zip`. Requires macOS 15 or later. Unzip and move `Solder.app` to Applications.
- **Windows x64:** `windows-x86_64.zip`. Extract the archive and run `solder.exe`. This is a portable app, without an installer. Extension web pages require Microsoft's WebView2 Runtime.
- **Linux x64:** `linux-x86_64.tar.gz`. Built on Ubuntu 24.04. Extract it and run `./solder` in a graphical session with a working Vulkan driver. It needs the system GTK 3, WebKitGTK 4.1, font, audio, Wayland and X11 libraries listed in the archive's runtime report. This archive does not bundle those system libraries.

These binaries are not signed with an Apple Developer or Windows publisher certificate. macOS bundles carry only a local ad hoc signature and are not notarized; Windows may show SmartScreen. On macOS, if the first launch is blocked, review the app under **System Settings → Privacy & Security → Open Anyway**. Do not disable system-wide security checks.

Each archive contains launch instructions, the source revision and runtime dependency information. `SHA256SUMS.txt` contains the SHA-256 checksums of all four archives.

This release checks native compilation and archive contents. It does not claim that every feature has been manually tested on every platform. Jupyter files can be read and saved; running cells requires a compatible extension and controller. Language servers, Git and other external tools are not bundled.
