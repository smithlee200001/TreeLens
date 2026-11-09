# Debian / Ubuntu desktop support

Tree Lens 0.5 supports Linux desktop integration alongside macOS and Windows.
The intended distro matrix is Debian 12/13 and Ubuntu 22.04/24.04, with glibc,
an active X11 or Wayland desktop, a working Vulkan/EGL graphics driver and a user
D-Bus session. The automated workflow builds each distro separately on amd64.
Native arm64 packaging is supported by the script but is not covered by that CI
matrix. A workflow definition is not evidence of a passing run; see TESTING.md
for the checks actually executed for this revision.

## Build from source

Install Rust stable 1.92 or newer from https://rustup.rs. Review the dependency
helper, then explicitly run it as administrator (the application and package
builder themselves do not need root):

```sh
bash scripts/install-linux-deps.sh --print
sudo bash scripts/install-linux-deps.sh
cargo test --locked --no-default-features --lib
cargo test --locked --features ui-tests --all-targets
cargo clippy --locked --features ui-tests --all-targets -- -D warnings
cargo run --locked --release -- "$HOME"
```

The helper uses official distro packages. It installs the native C/C++ toolchain,
Fontconfig/FreeType, Wayland/X11 keyboard and display headers, EGL/Vulkan,
OpenSSL, packaging tools, desktop portals, and CJK fonts. It does not install
Rust, change security settings or start the application as root. A headless
server can run the core tests but is not a substitute for a graphical session.

## Packages

```sh
bash scripts/bundle-linux.sh
```

The script builds the release binary on the current supported distro and native
architecture. It honors Cargo's target directory through metadata, explicitly
selects the native GNU target, and creates distro/version/architecture-labelled
`.deb`, `.tar.gz` and SHA-256 files under `dist/`. It validates the desktop entry,
derives minimum linked-library ABI versions with `dpkg-shlibdeps`, and adds
runtime-loaded graphics and desktop-service dependencies explicitly. Third-party
license files and an inventory accompany both formats. Packaging never invokes
sudo, installs the package, or deletes an existing directory. Temporary staging
is unique and retained for inspection.

Install the `.deb` built for **your distro release and architecture**:

```sh
sudo apt install ./dist/Tree-Lens-0.5.0-ubuntu-24.04-amd64.deb
```

Adjust the filename to the actual artifact. Launch **Tree Lens** from the desktop
menu or run `tree-lens`. The package installs under `/usr`, contains no maintainer
scripts, and is an unsigned development package, not a signed apt repository.
Use `sudo apt remove tree-lens` to uninstall it. No user files are removed by the
package.

The tarball is relocatable, **not self-contained**: extract it and run
`./Tree-Lens-…/usr/bin/tree-lens`. It still needs the libraries and desktop tools
listed in `usr/share/doc/tree-lens/BUILD.txt`. Its desktop entry assumes a system
installation; extraction alone does not add a menu entry. A binary built on
Debian 13 is not promised to run on Debian 12 or Ubuntu 22.04. Always use the
matching package or build locally. Neither format bundles GPU drivers.

### Archive-only builds on restricted hosts

If the build host lacks a usable dpkg installed-library database, the default
package build fails rather than guessing `.deb` ABI dependencies. You may
explicitly request a tarball only:

```sh
bash scripts/bundle-linux.sh --tar-only
```

This still compiles the same native release target and includes the desktop
files, licenses and a `.tar.gz.sha256` checksum. It does not call
`dpkg-shlibdeps` or create a `.deb`. `BUILD.txt` identifies the actual host and
additional runtime packages, while `ELF-VERSIONS.txt` records the binary's actual
ELF version requirements. Minimum distro package versions have **not** been
resolved. The archive is not self-contained, does not promise compatibility
with other distro releases, and must be tested on the intended target system.
No root access or package installation is needed to create it.

## Desktop integration and safety

- Folder selection uses GPUI's desktop portal. Install and run the portal backend
  for your desktop, such as `xdg-desktop-portal-gtk` for GTK desktops or
  `xdg-desktop-portal-kde` for KDE. A D-Bus service and graphical session are
  required. A missing or unavailable portal can prevent the chooser opening.
- Standard folder shortcuts use `xdg-user-dir` (`xdg-user-dirs`) to respect
  localized and redirected user folders. Reveal/open uses `xdg-open`
  (`xdg-utils`). These tools are passed paths as arguments, not shell commands.
- Allocated space uses Unix allocated blocks × 512. Hard links are deduplicated
  by device/inode. Symlinks are not followed. CoW/reflinks, snapshots, compression
  and filesystems reporting unusual block allocation mean this is not an exact
  prediction of recoverable free space.
- Moving a folder to Trash requires the application's explicit confirmation and
  path/identity revalidation. The Linux backend uses `/usr/bin/gio trash`, which
  delegates FreeDesktop Trash handling to GIO (`libglib2.0-bin`). There is no
  fallback to permanent deletion. Failures such as missing GIO, unsupported
  volumes, permission errors or unavailable Trash must be reported, not ignored.
- Restore through the desktop file manager's Trash. The application does not
  restore files. Avoid running it as root: that uses a different home/Trash and
  can reach system files. Do not use a production folder for acceptance tests.
- Path checks cannot make pathname-based Trash operations atomic against another
  process concurrently replacing paths. Treat the filesystem as non-hostile.

## Verification and remaining desktop acceptance

`.github/workflows/linux.yml` provisions real Debian 12/13 and Ubuntu 22.04/24.04
userspaces in separate containers. It runs core tests, headless GPUI interaction
tests, strict Clippy, release builds, desktop-entry validation, package extraction,
ELF dependency checks and package installation, then runs the installed binary's
`--version` and `--help` without opening a graphical window and uploads distinct
artifacts. CI also explicitly opts into the isolated real GIO Trash round-trip
and Unix socket scan tests; the GIO fixture uses its own HOME and XDG data
directory, never the user's normal Trash.
Containers share the runner kernel and are not complete desktop VMs. No real
user folder is trashed by the automated tests. The isolated GIO test does move
and restore its own synthetic fixture through the real GIO backend.

Before distributing, manually test on each intended desktop: X11 and Wayland,
HiDPI scaling, Chinese text/font rendering, folder picker cancellation/retry,
file-manager opening, localized/redirected home folders, scan cancellation and
refresh, and synthetic Trash success/restoration, permission failure, full or
unsupported filesystem and concurrent path replacement. Confirm root, symlink,
mount-point and stale-target protections using disposable fixtures. These UI,
portal/GPU and actual Trash behaviors are not certified by headless test success.
