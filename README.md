# Cosmictify

**Spotify panel applet for COSMIC Desktop / Pop!_OS** — now playing, animated marquee, progress bar, and MPRIS media controls.

[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.97%2B-orange.svg)](https://www.rust-lang.org/)
[![COSMIC](https://img.shields.io/badge/desktop-COSMIC-blue.svg)](https://system76.com/cosmic)
[![Pop!_OS](https://img.shields.io/badge/Pop!_OS-24.04-6C3CE9.svg)](https://pop.system76.com/)

> COSMIC™ Spotify tray · now playing · libcosmic · MPRIS · Linux panel applet

**App ID:** `com.brunocasarotti.Cosmictify`  
**License:** MIT

## Features

- **Panel tray:** album cover + scrolling **Title — Artist** marquee + thin progress bar  
- **Popup:** large artwork, seek, play/pause, next/previous, volume, open in Spotify  
- **Shortcuts:** scroll = next/prev · middle-click = play/pause  
- **Spotify-only MPRIS** (won’t hijack YouTube/browser players or thicken your top bar)  
- Built with **Rust** + **libcosmic** for native COSMIC look & feel  

> Library **like (♥)** via Spotify Web API (OAuth PKCE) is planned next.

## Screenshots

_Add panel + popup screenshots here after capture._

## Requirements

- [Pop!_OS](https://pop.system76.com/) 24.04+ with **COSMIC** desktop  
- [Spotify](https://www.spotify.com/) desktop client (MPRIS)  
- Rust (asdf recommended) — see `.tool-versions`  
- Build packages:

```bash
sudo apt install libdbus-1-dev pkg-config libssl-dev build-essential \
  libxkbcommon-dev libwayland-dev libegl1-mesa-dev
cargo install just
```

## Build & install (user-local)

```bash
git clone https://github.com/brunocasarotti/cosmictify.git
cd cosmictify
just build-release
just install-local
```

Then: **Settings → Desktop → Panel → Configure panel applets → Cosmictify**.

```bash
just uninstall-local   # remove
just run               # debug run
cargo test --release
```

## How it works

Cosmictify talks to the Spotify desktop app over **MPRIS on D-Bus** (same family of APIs as `playerctl`). No Spotify Premium required for local play/pause/skip on the desktop client. See the design notes in [`plans/`](plans/).

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
