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
3. For the Spotify library like button (♥), see [Enable the Like Button](#enable-the-like-button)

### Uninstall

**User install** (`~/.local` / one-liner / tarball):

```bash
curl -fsSL https://raw.githubusercontent.com/brunocasarotti/cosmictify/main/install.sh | bash -s -- --uninstall
```

Or, from a downloaded tarball: `./install.sh --uninstall`

**Debian package:**

```bash
sudo apt remove cosmictify
```

**From source (dev):**

```bash
just uninstall-local
```

## Features

- **Panel tray:** album cover + scrolling **Title — Artist** marquee + thin progress bar  
- **Popup:** large artwork, seek, play/pause, next/previous, volume, open in Spotify  
- **Shortcuts:** scroll = next/prev · middle-click = play/pause  
- **Spotify-only MPRIS** (won’t hijack YouTube/browser players or thicken your top bar)  
- **Library like (♥)** via personal Spotify Web API app (OAuth PKCE + Secret Service)  
- Popup **gear** expands Spotify setup (Client ID + Connect / Disconnect) without cluttering the player UI  
- Built with **Rust** + **libcosmic** for native COSMIC look & feel  

## Enable the Like Button

> MPRIS playback controls work without this setup and do not require Spotify Premium. The like button needs a one-time personal Spotify Developer app and OAuth.

Spotify Development Mode does not provide Cosmictify with one shared public app. Each user must create a personal Spotify Developer app because a Development Mode app supports at most **five allowlisted users**, and broader access has a high eligibility threshold. Personal use is fine: the app owner is automatically the owner account; additional test accounts must be added explicitly.

### One-time Spotify Developer setup

1. Sign in to the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard) with the Spotify account that owns the app. The app owner currently needs an active **Premium** subscription.
2. Click **Create app**.
3. Enter a name such as `Cosmictify Personal` and any description.
4. Select **Web API**.
5. Add this exact Redirect URI:

   ```text
   http://127.0.0.1:43821/callback
   ```

   Do not use `localhost`, change `127.0.0.1`, change the port, or add/remove the trailing slash.
6. Accept Spotify’s terms, then create/save the app.
7. Copy only the app’s **Client ID** into Cosmictify. The Client ID is a public identifier.
8. **Never copy, share, or enter the Client Secret.** Cosmictify does not request or store a Client Secret.

The app owner needs no extra allowlist entry. To test with other accounts, open the app’s **Settings → Users Management** in the Developer Dashboard and add them; Development Mode allows a maximum of five users in total.

### Connect Cosmictify

With Cosmictify installed:

1. Open the Cosmictify popup and click the **gear** icon (bottom-right) to expand **Spotify setup**.
2. Paste the Client ID into the field (placeholder: *Spotify Client ID*).
3. Choose **Connect Spotify** (saves the Client ID and starts authorization).
4. Complete authorization in the system browser and approve the `user-library-read` and `user-library-modify` permissions.

Cosmictify checks the current Spotify track and lets the heart button save or remove it from your Spotify library. The loopback callback is local to your computer; the browser must return to the exact URI above.

### Security and reset

- The Client ID is stored in ordinary Cosmictify configuration because it is not a secret.
- OAuth access and refresh tokens are stored only in the Linux **Secret Service** (the desktop keyring), not in plain-text configuration. There is no plain-text fallback.
- MPRIS playback remains independent of Spotify Web API login, so a missing or unavailable keyring does not prevent normal local media controls.
- To disconnect, open the popup gear → expand **Spotify setup** and choose **Disconnect**. To switch Developer apps, change the Client ID and choose **Connect Spotify** again: Cosmictify clears tokens tied to the previous app so they cannot be reused with another Client ID.

### Troubleshooting

- **Invalid redirect URI:** The Dashboard value must be exactly `http://127.0.0.1:43821/callback`. Replace `localhost`, remove any trailing slash, and check the port before trying again.
- **`localhost` or port conflict:** `localhost` is not interchangeable with `127.0.0.1` for this setup. Close another process using port `43821`, restart Cosmictify, and retry the connection.
- **Keyring locked or unavailable:** Unlock/start the Linux Secret Service in your COSMIC session and retry. Cosmictify does not write tokens to a plain-text fallback.
- **403 account, allowlist, or quota error:** Confirm that the app owner has Premium, that the authorizing account is the app owner or one of the five Users Management entries, and that the app is configured for Web API.
- **Authorization was revoked:** Disconnect/reset the app connection, then choose **Connect Spotify** again and approve the requested library permissions.
- **Client Secret requested:** Stop—the setup is incorrect for Cosmictify. Only enter the Client ID; never expose the Client Secret.

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
just release 0.2.1   # tag + GitHub release with tarball/deb
```

CI also builds on `v*` tags (`.github/workflows/release.yml`).

## Keywords / topics

`cosmic` · `cosmic-desktop` · `pop-os` · `spotify` · `mpris` · `panel-applet` · `libcosmic` · `rust` · `now-playing` · `linux-desktop` · `media-controls` · `system76`

## Related projects

- [pop-os/cosmic-applet-template](https://github.com/pop-os/cosmic-applet-template)  
- [pop-os/libcosmic](https://github.com/pop-os/libcosmic)  
- [Ebbo/cosmic-applet-music-player](https://github.com/Ebbo/cosmic-applet-music-player) (generic MPRIS applet)

## Status

Daily-driver on Pop!_OS 24.04 COSMIC: MPRIS panel/popup plus optional Spotify library like via personal Developer app (released from **v0.2.0**, patch **v0.2.1**). Current in-tree UI polish: expandable Spotify setup under the popup gear.

## License

[MIT](LICENSE) © Bruno Casarotti
