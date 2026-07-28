# Cosmictify

Spotify control applet for the [COSMIC™](https://system76.com/cosmic) desktop (Pop!_OS 24.04+).

**License:** MIT  
**App ID:** `com.brunocasarotti.Cosmictify`

Shows what's playing (cover + title + progress), transport controls via **MPRIS/D-Bus**, and library like via the **Spotify Web API** (OAuth PKCE).

See [`plans/2026-07-28-cosmictify-applet.md`](plans/2026-07-28-cosmictify-applet.md) for the full plan.

## Requirements

- Pop!_OS 24.04 / COSMIC
- Rust (via [asdf](https://asdf-vm.com/) recommended): see `.tool-versions`
- Build deps: `libdbus-1-dev pkg-config libssl-dev build-essential`
- Spotify desktop client (for MPRIS)
- [`just`](https://github.com/casey/just)

```bash
# asdf
cd ~/Projects/cosmictify
# ensures rust stable from .tool-versions
rustc --version

sudo apt install libdbus-1-dev pkg-config libssl-dev build-essential
cargo install just
```

## Build

```bash
just build-release
# or
just run
```

## Install (user-local, no sudo)

```bash
just build-release
just install-local
```

Then: **Settings → Desktop → Panel → Configure panel applets → Cosmictify**.

Uninstall: `just uninstall-local`.

## Status

Phase 0 scaffold from [cosmic-applet-template](https://github.com/pop-os/cosmic-applet-template). MPRIS + like UI coming next.
