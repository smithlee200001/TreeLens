#!/usr/bin/env bash
# Native distro package: no root, installation, maintainer scripts or cross build.
set -euo pipefail
cd "$(dirname "$0")/.."
tar_only=false
if [[ $# == 1 && $1 == --tar-only ]]; then
  tar_only=true
elif [[ $# != 0 ]]; then
  echo "Usage: $0 [--tar-only] (native Debian/Ubuntu build only)" >&2; exit 1
fi
if [[ $(uname -s) != Linux || ! -r /etc/os-release ]]; then
  echo 'Build this package on Debian or Ubuntu.' >&2; exit 1
fi
. /etc/os-release
case "${ID}:${VERSION_ID}" in
  debian:12|debian:13|ubuntu:22.04|ubuntu:24.04) ;;
  *) echo "Unsupported build host: ${ID} ${VERSION_ID}" >&2; exit 1 ;;
esac
tools=(cargo rustc python3 dpkg desktop-file-validate readelf)
if [[ $tar_only == false ]]; then tools+=(dpkg-deb dpkg-shlibdeps); fi
for tool in "${tools[@]}"; do
  command -v "$tool" >/dev/null || { echo "Missing $tool. See docs/LINUX.md." >&2; exit 1; }
done
arch=$(dpkg --print-architecture)
case "$arch" in
  amd64) target=x86_64-unknown-linux-gnu ;;
  arm64) target=aarch64-unknown-linux-gnu ;;
  *) echo "Unsupported package architecture: $arch" >&2; exit 1 ;;
esac
host=$(rustc -vV | sed -n 's/^host: //p')
[[ "$host" == "$target" ]] || { echo "Native Rust host $target required; found $host" >&2; exit 1; }
# Explicit target avoids accidentally packaging a CARGO_BUILD_TARGET cross build.
cargo build --locked --release --bin tree-lens --target "$target"
stage=$(mktemp -d "${TMPDIR:-/tmp}/tree-lens-linux.XXXXXXXX")
# Retain the unique staging directory on failure for diagnosis. Never remove user paths.
cargo metadata --locked --format-version 1 --filter-platform "$target" > "$stage/metadata.json"
readarray -t metadata < <(python3 - "$stage/metadata.json" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
p = next(p for p in m['packages'] if p['name'] == 'tree-lens' and p['id'] in m['workspace_members'])
print(p['version']); print(m['target_directory'])
PY
)
version=${metadata[0]}
binary=${metadata[1]}/$target/release/tree-lens
[[ -f "$binary" && -x "$binary" ]] || { echo "Missing executable: $binary" >&2; exit 1; }
readelf -h "$binary" > "$stage/elf-header.txt"
bundle="Tree-Lens-${version}-${ID}-${VERSION_ID}-${arch}"
root="$stage/$bundle"
mkdir -p "$root/usr/bin" "$root/usr/share/applications" "$root/usr/share/icons/hicolor/scalable/apps" "$root/usr/share/doc/tree-lens"
install -m755 "$binary" "$root/usr/bin/tree-lens"
install -m644 packaging/linux/local.treelens.app.desktop "$root/usr/share/applications/"
install -m644 packaging/linux/local.treelens.app.svg "$root/usr/share/icons/hicolor/scalable/apps/"
desktop-file-validate "$root/usr/share/applications/local.treelens.app.desktop"
cp LICENSE "$root/usr/share/doc/tree-lens/copyright"
cp README.md TESTING*.md CHANGELOG.md "$root/usr/share/doc/tree-lens/"
cp -R docs "$root/usr/share/doc/tree-lens/docs"
python3 - "$stage/metadata.json" "$root/usr/share/doc/tree-lens" <<'PY'
import json, pathlib, re, shutil, sys
metadata = json.load(open(sys.argv[1]))
out = pathlib.Path(sys.argv[2])
notices = ['# Dependency license inventory', 'Conservative native-target Cargo metadata inventory; may include build/test dependencies. Original relative license paths are preserved.']
for p in sorted(metadata['packages'], key=lambda p: (p['name'], p['version'])):
    if p['name'] == 'tree-lens': continue
    ident = p['name'] + '-' + p['version']
    notices.append(f"\n## {ident}\nLicense: {p.get('license')}\nSource: {p.get('source')}\nRepository: {p.get('repository')}")
    base = pathlib.Path(p['manifest_path']).parent
    files = {f for f in base.rglob('*') if f.is_file() and re.match(r'^(LICENSE|LICENCE|COPYING|NOTICE|COPYRIGHT|OFL)([._-].*)?$', f.name, re.I)}
    if p.get('license_file'): files.add(base / p['license_file'])
    for f in sorted(files):
        # Reject escaping paths/symlinks rather than copying unrelated host data.
        f.resolve().relative_to(base.resolve())
        relative = f.relative_to(base)
        dest = out / 'licenses' / ident / relative
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(f, dest)
(out / 'THIRD-PARTY-NOTICES.md').write_text('\n'.join(notices) + '\n')
PY
# Libraries/services loaded at runtime are absent from ELF NEEDED.
runtime_deps="libfontconfig1, libvulkan1, libegl1, libwayland-client0, libwayland-cursor0, libwayland-egl1, libxkbcommon0, libxkbcommon-x11-0, libxcb1, libxcb-xkb1, libglib2.0-bin, xdg-utils, xdg-user-dirs, xdg-desktop-portal"
if [[ $tar_only == true ]]; then
  # Explicit archive-only mode makes no unverified Debian dependency claim.
  # Include actual ELF SONAME/version requirements for diagnosis, not an ABI promise.
  {
    printf 'Built on %s %s (%s), Rust target %s\n' "$ID" "$VERSION_ID" "$arch" "$target"
    printf 'Archive-only build: minimum distro package/ABI versions were not resolved.\n'
    printf 'This archive is not self-contained and is not guaranteed across distro releases.\n'
    printf 'Additional runtime packages: %s\n' "$runtime_deps"
    printf 'A working GPU driver, user D-Bus session and desktop portal backend are required.\n'
    printf '\nActual ELF linked libraries:\n'
    readelf -d "$binary" | grep '(NEEDED)' || true
  } > "$root/usr/share/doc/tree-lens/BUILD.txt"
  readelf --version-info "$binary" > "$root/usr/share/doc/tree-lens/ELF-VERSIONS.txt"
  mkdir -p dist
  tar -C "$stage" -czf "dist/$bundle.tar.gz" "$bundle"
  (cd dist && sha256sum "$bundle.tar.gz" > "$bundle.tar.gz.sha256")
  printf 'Created archive only: %s/dist/%s.tar.gz\nNo .deb was generated. Staging retained: %s\n' "$PWD" "$bundle" "$stage"
  exit 0
fi
# dpkg-shlibdeps derives minimum ABI versions from the actual native ELF.
mkdir -p "$stage/debian"
cat > "$stage/debian/control" <<CONTROL
Source: tree-lens
Section: utils
Priority: optional
Maintainer: Tree Lens contributors <noreply@users.noreply.github.com>

Package: tree-lens
Architecture: $arch
Description: Native disk usage analyzer
CONTROL
if ! deps=$(cd "$stage" && dpkg-shlibdeps -O -e"$root/usr/bin/tree-lens"); then
  echo 'Cannot resolve distro ABI dependencies. Build in a native Debian/Ubuntu installation with its dpkg library metadata intact; no package was emitted.' >&2
  exit 1
fi
deps=${deps#shlibs:Depends=}
[[ -n "$deps" && "$deps" != *$'\n'* ]] || { echo 'Cannot determine ELF dependencies' >&2; exit 1; }
# These are runtime-loaded libraries/services and thus absent from ELF NEEDED.
deps+=", $runtime_deps"
printf 'Built on %s %s (%s)\nRuntime dependencies: %s\n' "$ID" "$VERSION_ID" "$arch" "$deps" > "$root/usr/share/doc/tree-lens/BUILD.txt"
mkdir -p dist
# Tarball contains no DEBIAN metadata; it is relocatable but not self-contained.
tar -C "$stage" -czf "dist/$bundle.tar.gz" "$bundle"
mkdir -p "$root/DEBIAN"
cat > "$root/DEBIAN/control" <<CONTROL
Package: tree-lens
Version: $version
Architecture: $arch
Section: utils
Priority: optional
Maintainer: Tree Lens contributors <noreply@users.noreply.github.com>
Depends: $deps
Recommends: xdg-desktop-portal-gtk | xdg-desktop-portal-kde, fonts-noto-cjk, mesa-vulkan-drivers
Installed-Size: $(du -sk "$root/usr" | cut -f1)
Homepage: https://github.com/smithlee200001/TreeLens
Description: Native GPUI disk usage analyzer
 Explore folder sizes, allocated disk space and hard-link aware disk usage.
CONTROL
chmod 755 "$root/DEBIAN"
chmod 644 "$root/DEBIAN/control"
dpkg-deb --root-owner-group --build "$root" "dist/$bundle.deb"
dpkg-deb --info "dist/$bundle.deb"
(cd dist && sha256sum "$bundle.deb" "$bundle.tar.gz" > "$bundle.sha256")
printf 'Created: %s/dist/%s.{deb,tar.gz,sha256}\nStaging retained: %s\n' "$PWD" "$bundle" "$stage"
