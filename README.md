# Cosmictify

**Spotify panel applet for COSMIC Desktop / Pop!_OS** — now playing, animated marquee, progress bar, and MPRIS media controls.

[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.97%2B-orange.svg)](https://www.rust-lang.org/)
[![COSMIC](https://img.shields.io/badge/desktop-COSMIC-blue.svg)](https://system76.com/cosmic)
[![Pop!_OS](https://img.shields.io/badge/Pop!_OS-24.04-6C3CE9.svg)](https://pop.system76.com/)

> COSMIC™ Spotify tray · now playing · libcosmic · MPRIS · Linux panel applet

**App ID:** `com.brunocasarotti.Cosmictify`  
**License:** MIT

## Install (no Rust, no compile)

### One-liner

```bash
curl -fsSL https://raw.githubusercontent.com/brunocasarotti/cosmictify/main/install.sh | bash
```

Installs into `~/.local` (binary + desktop entry + icon).

### `.deb` (Pop!_OS / Ubuntu)

From the [latest release](https://github.com/brunocasarotti/cosmictify/releases/latest):

```bash
# download cosmictify_*_amd64.deb then:
sudo apt install ./cosmictify_*_amd64.deb
```

### After install

1. **Settings → Desktop → Panel → Configure panel applets → Cosmictify**
2. Ensure Spotify desktop is running

Uninstall user install:

```bash
rm -f ~/.local/bin/cosmictify \
  ~/.local/share/applications/com.brunocasarotti.Cosmictify.desktop \
  ~/.local/share/icons/hicolor/scalable/apps/com.brunocasarotti.Cosmictify.svg
```

## Features

- **Panel tray:** album cover + scrolling **Title — Artist** marquee + thin progress bar  
- **Popup:** large artwork, seek, play/pause, next/previous, volume, open in Spotify  
- **Shortcuts:** scroll = next/prev · middle-click = play/pause  
- **Spotify-only MPRIS** (won’t hijack YouTube/browser players or thicken your top bar)  
- Built with **Rust** + **libcosmic** for native COSMIC look & feel  

> Library **like (♥)** via Spotify Web API (OAuth PKCE) is planned next.

## Build from source

### Requirements

- [Pop!_OS](https://pop.system76.com/) 24.04+ with **COSMIC** desktop  
- [Spotify](https://www.spotify.com/) desktop client (MPRIS)  
- Rust (asdf recommended) — see `.tool-versions`  
- Build packages:

```bash
sudo apt install libdbus-1-dev pkg-config libssl-dev build-essential \
  libxkbcommon-dev libwayland-dev libegl1-mesa-dev
cargo install just
```

### Build & install (user-local)

```bash
git clone https://github.com/brunocasarotti/cosmictify.git
cd cosmictify
just build-release
just install-local
```

```bash
just uninstall-local   # remove
just run               # debug run
cargo test --release
just package           # make dist/*.tar.gz and dist/*.deb
```

## How it works

Cosmictify talks to the Spotify desktop app over **MPRIS on D-Bus** (same family of APIs as `playerctl`). No Spotify Premium required for local play/pause/skip on the desktop client. See the design notes in [`plans/`](plans/).

## Releases for maintainers

```bash
just release 0.1.0   # tag + GitHub release with tarball/deb
```

CI also builds on `v*` tags (`.github/workflows/release.yml`).

## Keywords / topics

`cosmic` · `cosmic-desktop` · `pop-os` · `spotify` · `mpris` · `panel-applet` · `libcosmic` · `rust` · `now-playing` · `linux-desktop` · `media-controls` · `system76`

## Related projects

- [pop-os/cosmic-applet-template](https://github.com/pop-os/cosmic-applet-template)  
- [pop-os/libcosmic](https://github.com/pop-os/libcosmic)  
- [Ebbo/cosmic-applet-music-player](https://github.com/Ebbo/cosmic-applet-music-player) (generic MPRIS applet)

## Status

Daily-driver MPRIS MVP works on Pop!_OS 24.04 COSMIC. Web API like button and packaging polish are next.

## License

[MIT](LICENSE) © Bruno Casarotti
