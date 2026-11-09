pkgname=wl-dropdown-git
pkgver=0.1.0
pkgrel=1
pkgdesc="Yakuake-style dropdown terminal for wlr-layer-shell compositors"
arch=('x86_64' 'aarch64')
url="https://github.com/BlackPhlox/wl-dropdown"
license=('MIT')
depends=('libxkbcommon' 'wayland' 'libglvnd')
makedepends=('cargo' 'git')
provides=('wl-dropdown')
conflicts=('wl-dropdown')
source=("$pkgname::git+$url.git")
sha256sums=('SKIP')

pkgver() {
  cd "$pkgname"
  local base
  base=$(grep -m1 -oP '^version = "\K[0-9.]+' Cargo.toml)
  printf '%s.r%s.g%s' "$base" "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}

prepare() {
  cd "$pkgname"
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "$pkgname"
  cargo build --frozen --release
}

check() {
  cd "$pkgname"
  cargo test --frozen
}

package() {
  cd "$pkgname"
  install -Dm755 target/release/wl-dropdown "$pkgdir/usr/bin/wl-dropdown"
  install -Dm644 share/templates/wl-dropdown.conf "$pkgdir/usr/share/wl-dropdown/templates/wl-dropdown.conf"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
