# Maintainer: WMDE <https://wmde.fun>
# Contributor: System76 <info@system76.com> (original cosmic-comp)
#
# Builds our fork Lin-WMDE/wmde-comp (branch wmde). WMDE wayland compositor.
# Installs alongside cosmic-comp: own binary (wmde-comp), own unit (wmde-comp.service)
# and own config namespaces (fun.wmde.Comp / fun.wmde.Settings.Shortcuts / .WindowRules),
# so NO conflicts/replaces/provides of cosmic-comp. Does NOT install the wayland-session
# entry, the wmde-session[-pre].target or the session helper - wmde-session owns those.
pkgname=wmde-comp
pkgver=1.2.0
pkgrel=5
pkgdesc="WMDE wayland compositor (fork of cosmic-comp) - reads the fun.wmde.Comp config"
arch=('x86_64')
url="https://wmde.fun"
license=('GPL-3.0-only')
# Runtime libs pulled by smithay (drm/gbm/egl/libinput/libseat/udev/vulkan/x11 backends),
# xkbcommon, wayland and xwayland at runtime. Verify with namcap after first build.
depends=('glibc' 'gcc-libs' 'wayland' 'libinput' 'libxkbcommon' 'libglvnd'
         'mesa' 'seatd' 'systemd-libs' 'libdisplay-info' 'pixman' 'fontconfig'
         'freetype2' 'expat')
makedepends=('rust' 'cargo' 'git' 'make' 'clang' 'lld' 'pkgconf' 'wayland-protocols')
optdepends=('xorg-xwayland: X11 application support'
            'wmde-session: full WMDE session wiring')
# NOTE: Cargo.toml uses path deps to sibling checkouts (../libcosmic,
# ../wmde-settings-daemon). The build harness arranges them next to $srcdir;
# a standalone makepkg run without that layout will fail dependency resolution.
source=("$pkgname::git+https://github.com/Lin-WMDE/wmde-comp.git#branch=wmde")
sha256sums=('SKIP')

pkgver() {
  cd "$srcdir/$pkgname"
  # WMDE unified version: 1.5 (libcosmic base) . <commits since nearest tag> . g<short>.
  local desc
  desc=$(git describe --long --tags --abbrev=7 2>/dev/null || true)
  if [ -n "$desc" ]; then
    printf '1.5.%s.g%s' "$(printf '%s' "$desc" | sed -E 's/.*-([0-9]+)-g[0-9a-f]+$/\1/')" "$(git rev-parse --short=7 HEAD)"
  else
    printf '1.5.%s.g%s' "$(git rev-list --count HEAD)" "$(git rev-parse --short=7 HEAD)"
  fi
}

build() {
  cd "$srcdir/$pkgname"
  # x86-64-v3 (AVX2/BMI2) baseline for the WMDE repo; runs on Haswell+ (and the VM).
  export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-cpu=x86-64-v3"
  make
}

package() {
  cd "$srcdir/$pkgname"
  # installs /usr/bin/wmde-comp, /usr/lib/systemd/user/wmde-comp.service and the default
  # schemas under /usr/share/wmde/fun.wmde.Settings.{Shortcuts,WindowRules}/v1/ .
  # NO install-bare-session target - wmde-session is the sole owner of the session files.
  make DESTDIR="$pkgdir" prefix=/usr install
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
