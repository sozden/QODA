#!/bin/bash
# Builds the Linux release files of QODA:
#   dist/qoda_<version>_amd64.deb             (Debian/Ubuntu; apt installs the dependencies)
#   dist/QODA-<version>-linux-x86_64.tar.gz   (other distributions; install the libraries yourself)
# Needs: cargo, dpkg-dev (dpkg-deb, dpkg-shlibdeps), libsoapysdr-dev and the GUI -dev packages.
set -euo pipefail
cd "$(dirname "$0")/../.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
dist=dist
rm -rf "$dist/linux"
mkdir -p "$dist/linux"

cargo build --release --locked

# Files shared by both packages, laid out as installed under /usr.
root="$dist/linux/root"
install -Dm755 target/release/qoda "$root/usr/bin/qoda"
install -Dm755 target/release/qoda-analyze "$root/usr/bin/qoda-analyze"
install -Dm644 tools/linux/qoda.desktop "$root/usr/share/applications/qoda.desktop"
install -Dm644 assets/qoda-logo.svg "$root/usr/share/icons/hicolor/scalable/apps/qoda.svg"
install -Dm644 tools/linux/60-qoda-sdr.rules "$root/usr/lib/udev/rules.d/60-qoda-sdr.rules"
install -Dm644 README.md "$root/usr/share/doc/qoda/README.md"
install -Dm644 CHANGELOG.md "$root/usr/share/doc/qoda/CHANGELOG.md"
install -Dm644 LICENSE "$root/usr/share/doc/qoda/copyright"
strip "$root/usr/bin/qoda" "$root/usr/bin/qoda-analyze"

# .deb: shared library dependencies come from dpkg-shlibdeps; libraries that are loaded at run time
# (window system, libiio, SoapySDR modules, FFmpeg) are listed by hand.
deb="$dist/linux/deb"
cp -a "$root" "$deb"
mkdir -p "$dist/linux/shlib/debian"
touch "$dist/linux/shlib/debian/control"
shlibs=$(cd "$dist/linux/shlib" && dpkg-shlibdeps -O "../deb/usr/bin/qoda" "../deb/usr/bin/qoda-analyze" 2>/dev/null \
    | sed -n 's/^shlibs:Depends=//p')
size=$(du -sk "$deb/usr" | cut -f1)
mkdir -p "$deb/DEBIAN"
cat > "$deb/DEBIAN/control" <<EOF
Package: qoda
Version: $version
Section: hamradio
Priority: optional
Architecture: amd64
Maintainer: şadan özden <sadanozden@gmail.com>
Installed-Size: $size
Depends: $shlibs, libgl1, libegl1, libxkbcommon0, libxkbcommon-x11-0, libx11-6, libxcursor1, libxrandr2, libxi6, libwayland-client0, libwayland-egl1
Recommends: soapysdr-module-rtlsdr, soapysdr-module-hackrf, libiio0, ffmpeg
Suggests: soapysdr-tools
Homepage: https://github.com/sozden/QODA
Description: QO-100 DATV (DVB-S2) receiver
 QODA shows the spectrum of the QO-100 wideband transponder, finds the
 DATV broadcasts in it, decodes the selected one with its own DVB-S2
 decoder and plays the video. It works with RTL-SDR and HackRF through
 SoapySDR and with ADALM-Pluto / LibreSDR through libiio.
EOF
cat > "$deb/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = configure ] && command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules 2>/dev/null || true
    udevadm trigger --subsystem-match=usb 2>/dev/null || true
fi
EOF
chmod 755 "$deb/DEBIAN/postinst"
dpkg-deb --root-owner-group --build "$deb" "$dist/qoda_${version}_amd64.deb"

# tar.gz: the same files in one folder.
name="QODA-$version-linux-x86_64"
tree="$dist/linux/$name"
mkdir -p "$tree"
cp "$root/usr/bin/qoda" "$root/usr/bin/qoda-analyze" tools/linux/qoda.desktop \
    tools/linux/60-qoda-sdr.rules assets/qoda-logo.svg README.md CHANGELOG.md LICENSE "$tree/"
cat > "$tree/INSTALL.txt" <<'EOF'
QODA for Linux (x86_64)

1. Install the libraries with your package manager: SoapySDR 0.8 with the
   RTL-SDR and HackRF modules, libiio (for Pluto/LibreSDR) and FFmpeg.
   Debian/Ubuntu: use the .deb package instead, it installs everything.
2. Run ./qoda

To use USB receivers without root, copy 60-qoda-sdr.rules to
/etc/udev/rules.d/ and replug the receiver. ./qoda --diagnose shows what
the program finds.
EOF
tar -C "$dist/linux" -czf "$dist/$name.tar.gz" "$name"

ls -l "$dist/qoda_${version}_amd64.deb" "$dist/$name.tar.gz"
