name := 'cosmictify'
appid := 'com.brunocasarotti.Cosmictify'

rootdir := ''
prefix := '/usr'

# Installation paths
base-dir := absolute_path(clean(rootdir / prefix))
cargo-target-dir := env('CARGO_TARGET_DIR', 'target')
appdata-dst := base-dir / 'share' / 'appdata' / appid + '.metainfo.xml'
bin-dst := base-dir / 'bin' / name
desktop-dst := base-dir / 'share' / 'applications' / appid + '.desktop'
icon-dst := base-dir / 'share' / 'icons' / 'hicolor' / 'scalable' / 'apps' / appid + '.svg'

# Default recipe which runs `just build-release`
default: build-release

# Runs `cargo clean`
clean:
    cargo clean

# Removes vendored dependencies
clean-vendor:
    rm -rf .cargo vendor vendor.tar

# `cargo clean` and removes vendored dependencies
clean-dist: clean clean-vendor

# Compiles with debug profile
build-debug *args:
    cargo build {{args}}

# Compiles with release profile
build-release *args: (build-debug '--release' args)

# Compiles release profile with vendored dependencies
build-vendored *args: vendor-extract (build-release '--frozen --offline' args)

# Runs a clippy check
check *args:
    cargo clippy --all-features {{args}} -- -W clippy::pedantic

# Runs a clippy check with JSON message format
check-json: (check '--message-format=json')

# Run the application for testing purposes
run *args:
    env RUST_BACKTRACE=full cargo run --release {{args}}

# Installs files (system-wide; needs write access to prefix)
install:
    install -Dm0755 {{ cargo-target-dir / 'release' / name }} {{bin-dst}}
    install -Dm0644 resources/app.desktop {{desktop-dst}}
    install -Dm0644 resources/app.metainfo.xml {{appdata-dst}}
    install -Dm0644 resources/icon.svg {{icon-dst}}

# Install into ~/.local (no sudo) for COSMIC panel testing.
# Always rebuild release first so a stale target/release is never copied.
install-local: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    prefix="${HOME}/.local"
    install -Dm0755 "{{ cargo-target-dir / 'release' / name }}" "${prefix}/bin/{{name}}"
    install -Dm0644 resources/app.desktop "${prefix}/share/applications/{{appid}}.desktop"
    install -Dm0644 resources/app.metainfo.xml "${prefix}/share/metainfo/{{appid}}.metainfo.xml"
    install -Dm0644 resources/icon.svg "${prefix}/share/icons/hicolor/scalable/apps/{{appid}}.svg"
    # COSMIC looks for applets under share/applications with X-CosmicApplet=true
    gtk-update-icon-cache -f "${prefix}/share/icons/hicolor" 2>/dev/null || true
    echo "Installed to ${prefix}. Add Cosmictify via Settings → Desktop → Panel → Applets."
    echo "Reload: remove/re-add the applet, or: pkill -x {{name}}"

# Uninstalls installed files
uninstall:
    rm -f {{bin-dst}} {{desktop-dst}} {{icon-dst}} {{appdata-dst}}

uninstall-local:
    #!/usr/bin/env bash
    set -euo pipefail
    prefix="${HOME}/.local"
    rm -f "${prefix}/bin/{{name}}"
    rm -f "${prefix}/share/applications/{{appid}}.desktop"
    rm -f "${prefix}/share/metainfo/{{appid}}.metainfo.xml"
    rm -f "${prefix}/share/icons/hicolor/scalable/apps/{{appid}}.svg"

# Build release tarball + .deb into dist/ (no Rust needed for end users of those artifacts)
package version="":
    #!/usr/bin/env bash
    set -euo pipefail
    just build-release
    bash scripts/package-release.sh {{version}}

# Publish a GitHub release: just release 0.1.0
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    ver="{{version}}"
    ver="${ver#v}"
    if [[ ! "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
      echo "usage: just release 0.1.0" >&2
      exit 1
    fi
    # bump Cargo.toml if needed
    sed -i "0,/^version = /s/^version = .*/version = \"${ver}\"/" Cargo.toml
    just package "${ver}"
    git add Cargo.toml Cargo.lock dist/.gitkeep 2>/dev/null || true
    git add Cargo.toml
    git commit -m "release: v${ver}" || true
    git tag -a "v${ver}" -m "v${ver}"
    git push origin HEAD
    git push origin "v${ver}"
    # CI builds+uploads on tag; also attach local artifacts as a backup
    gh release create "v${ver}" \
      --title "v${ver}" \
      --generate-notes \
      dist/cosmictify-${ver}-linux-x86_64.tar.gz \
      dist/cosmictify_${ver}_amd64.deb \
      dist/SHA256SUMS

# Vendor dependencies locally
vendor:
    mkdir -p .cargo
    cargo vendor --sync Cargo.toml | head -n -1 > .cargo/config.toml
    echo 'directory = "vendor"' >> .cargo/config.toml
    echo >> .cargo/config.toml
    rm -rf .cargo vendor

# Extracts vendored dependencies
vendor-extract:
    rm -rf vendor
    tar pxf vendor.tar

# Bump cargo version, create git commit, and create tag
tag version:
    find -type f -name Cargo.toml -exec sed -i '0,/^version/s/^version.*/version = "{{version}}"/' '{}' \; -exec git add '{}' \;
    cargo check
    cargo clean
    git add Cargo.lock
    git commit -m 'release: {{version}}'
    git tag -a {{version}} -m ''

