#!/usr/bin/env bash
#
# Build + package pv-matter for the aarch64 Linux target (Armbian/Ubuntu 18.04
# "Bionic", glibc 2.27).
#
# Default path: cross-compile with cargo-zigbuild (Zig as the cross-linker),
# pinning the glibc symbol floor to the target's 2.27. The only C dependency is
# libdns_sd (avahi-compat, pulled in by the astro-dnssd mDNS backend); crypto is
# rustcrypto. We cross-link libdns_sd from the aarch64 shared object copied off
# the target (packaging/libdns_sd.so.1.0.0) via a generated pkg-config shim —
# see build_with_zig below — so still no sysroot or emulation is needed.
#
#   Requirements:  brew install zig cargo-zigbuild pkg-config
#                  rustup target add aarch64-unknown-linux-gnu
#                  packaging/libdns_sd.so.1.0.0 copied from the target box
#
# Fallback path (`--docker`): build natively-under-emulation inside an arm64
# Bionic image via packaging/Dockerfile — the d26i-matter pattern. Slower
# (QEMU), but needs only Docker; use it on hosts without Zig, or to
# double-check the zigbuild artifact against a real Bionic userland.
#
#   Requirements:  Docker with buildx; QEMU/binfmt for linux/arm64 on
#                  non-arm64 hosts (Docker Desktop ships it).
#
# Both paths produce, under dist/:
#   pv-matter                                      the stripped aarch64 binary
#   pv-matter-<version>-aarch64-linux-gnu.tar.gz   install bundle
#   <bundle>.tar.gz.sha256                         checksum
#
# Usage:  packaging/build-aarch64.sh [--docker] [--debug]
#
#   --debug   build without --release (zigbuild path only)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${ROOT}"

TARGET_TRIPLE="aarch64-linux-gnu"
RUST_TARGET="aarch64-unknown-linux-gnu"
GLIBC_VERSION="2.27"
DIST="${ROOT}/dist"
BIN_NAME="pv-matter"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "${ROOT}/Cargo.toml" | head -n1)"
if [[ -z "${VERSION}" ]]; then
  echo "error: could not read version from Cargo.toml" >&2
  exit 1
fi

BUNDLE="${BIN_NAME}-${VERSION}-${TARGET_TRIPLE}"
STAGE="${DIST}/${BUNDLE}"
mkdir -p "${DIST}"

build_with_zig() {
  local profile_dir="release"
  if [[ "${DEBUG}" == "1" ]]; then
    profile_dir="debug"
  fi

  echo ">> building ${BIN_NAME} ${VERSION} for ${RUST_TARGET} (glibc ${GLIBC_VERSION}) with cargo-zigbuild"
  if ! command -v cargo-zigbuild >/dev/null 2>&1 || ! command -v zig >/dev/null 2>&1; then
    echo "error: cargo-zigbuild and zig are required (brew install zig cargo-zigbuild)," >&2
    echo "       or use the Docker fallback: packaging/build-aarch64.sh --docker" >&2
    exit 1
  fi
  if ! command -v pkg-config >/dev/null 2>&1; then
    echo "error: pkg-config is required for the libdns_sd cross-link (brew install pkg-config)" >&2
    exit 1
  fi
  rustup target add "${RUST_TARGET}" >/dev/null

  # astro-dnssd's build.rs probes pkg-config for `avahi-compat-libdns_sd`, then
  # the link emits `-ldns_sd`. Cross-linking from macOS we have neither, so:
  #   1. symlink the linker name `libdns_sd.so` onto the aarch64 .so copied off
  #      the target (its SONAME is libdns_sd.so.1, which the target's
  #      libavahi-compat-libdnssd1 provides at runtime), and
  #   2. hand pkg-config a generated .pc pointing `-L` at that dir.
  # PKG_CONFIG_ALLOW_CROSS lets the probe run despite host != target.
  local libdns="${SCRIPT_DIR}/libdns_sd.so.1.0.0"
  if [[ ! -f "${libdns}" ]]; then
    echo "error: ${libdns} not found — copy /usr/lib/*/libdns_sd.so.1.0.0 from the target box" >&2
    exit 1
  fi
  ln -sf libdns_sd.so.1.0.0 "${SCRIPT_DIR}/libdns_sd.so"
  cat > "${SCRIPT_DIR}/avahi-compat-libdns_sd.pc" <<EOF
libdir=${SCRIPT_DIR}
Name: avahi-compat-libdns_sd
Description: Avahi DNS-SD compat shim for the pv-matter aarch64 cross build
Version: 0.7
Libs: -L\${libdir} -ldns_sd
Cflags:
EOF
  export PKG_CONFIG_ALLOW_CROSS=1
  export PKG_CONFIG_PATH="${SCRIPT_DIR}"

  # Seeded with --target so the array is never empty — macOS's bash 3.2
  # errors on "${arr[@]}" under `set -u` when arr has zero elements.
  local -a cargo_flags=(--target "${RUST_TARGET}.${GLIBC_VERSION}")
  if [[ "${DEBUG}" != "1" ]]; then
    cargo_flags=(--release "${cargo_flags[@]}")
  fi
  cargo zigbuild "${cargo_flags[@]}"
  cp "${ROOT}/target/${RUST_TARGET}/${profile_dir}/${BIN_NAME}" "${DIST}/${BIN_NAME}"
}

build_with_docker() {
  echo ">> building ${BIN_NAME} ${VERSION} for linux/arm64 (Bionic image) with Docker"
  docker buildx build \
    --platform linux/arm64 \
    --file "${SCRIPT_DIR}/Dockerfile" \
    --target export \
    --output "type=local,dest=${DIST}" \
    "${ROOT}"
}

DOCKER=0
DEBUG=0
for arg in "$@"; do
  case "${arg}" in
    --docker) DOCKER=1 ;;
    --debug) DEBUG=1 ;;
    *)
      echo "usage: packaging/build-aarch64.sh [--docker] [--debug]" >&2
      exit 2
      ;;
  esac
done

if [[ "${DOCKER}" == "1" ]]; then
  if [[ "${DEBUG}" == "1" ]]; then
    echo "error: --debug is not supported with --docker" >&2
    exit 2
  fi
  build_with_docker
else
  build_with_zig
fi

if [[ ! -f "${DIST}/${BIN_NAME}" ]]; then
  echo "error: build did not produce ${DIST}/${BIN_NAME}" >&2
  exit 1
fi

echo ">> assembling bundle ${BUNDLE}.tar.gz"
rm -rf "${STAGE}"
mkdir -p "${STAGE}"
cp "${DIST}/${BIN_NAME}"                  "${STAGE}/${BIN_NAME}"
cp "${SCRIPT_DIR}/install.sh"             "${STAGE}/"
cp "${SCRIPT_DIR}/pv-matter-run"          "${STAGE}/"
cp "${SCRIPT_DIR}/pv-matter.service"      "${STAGE}/"
cp "${SCRIPT_DIR}/config.env.example"     "${STAGE}/"
cp "${SCRIPT_DIR}/INSTALL.md"             "${STAGE}/"
chmod 0755 "${STAGE}/${BIN_NAME}" "${STAGE}/install.sh" "${STAGE}/pv-matter-run"

tar -C "${DIST}" -czf "${DIST}/${BUNDLE}.tar.gz" "${BUNDLE}"
rm -rf "${STAGE}"

( cd "${DIST}"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${BUNDLE}.tar.gz" > "${BUNDLE}.tar.gz.sha256"
  else
    shasum -a 256 "${BUNDLE}.tar.gz" > "${BUNDLE}.tar.gz.sha256"
  fi )

echo ">> done:"
echo "   ${DIST}/${BUNDLE}.tar.gz"
echo "   ${DIST}/${BUNDLE}.tar.gz.sha256"
echo "   ${DIST}/${BIN_NAME} (raw binary)"
