# syntax=docker/dockerfile:1
#
# Build pv-matter for the target aarch64 Linux box (Armbian "Bionic" — an
# Ubuntu 18.04 arm64 userland, glibc 2.27), following the d26i-matter pattern.
#
# The binary links glibc dynamically, so the build glibc must be <= the
# target's 2.27; arm64v8/ubuntu:18.04 IS Bionic. The one C dependency is
# libdns_sd (avahi-compat, from the astro-dnssd mDNS backend) — installed below
# via libavahi-compat-libdnssd-dev, which also ships the pkg-config file its
# build.rs probes; crypto is rustcrypto and rumqttc is built without its default
# TLS features. This is a plain cargo build under QEMU emulation on non-arm64
# hosts.
FROM arm64v8/ubuntu:18.04 AS builder

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        build-essential \
        curl \
        ca-certificates \
        pkg-config \
        libavahi-compat-libdnssd-dev \
 && rm -rf /var/lib/apt/lists/*

# Pinned, reproducible toolchain. Edition 2024 needs rustc >= 1.85.
ARG RUST_VERSION=1.89.0
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --no-modify-path --profile minimal --default-toolchain "${RUST_VERSION}" \
 && rustc --version && cargo --version

WORKDIR /build
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --release \
 && test -x /build/target/release/pv-matter \
 && cp /build/target/release/pv-matter /pv-matter

FROM scratch AS export
COPY --from=builder /pv-matter /pv-matter
