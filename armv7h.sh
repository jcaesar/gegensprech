#!/usr/bin/env bash

set -euo pipefail

df='
FROM docker.io/library/debian:bullseye
RUN dpkg --add-architecture armhf && \
    apt-get update && \
    export DEBIAN_FRONTEND=noninteractive && \
    apt-get install -yq \
        build-essential \
        clang \
        cmake \
        curl \
        file \
        git \
        musl-dev \
        musl-dev:armhf \
        musl-tools \
        sudo \
        crossbuild-essential-armhf \
        libunwind-dev:armhf \
        libunwind8-dev:armhf \
        && \
    apt-get clean && rm -rf /var/lib/apt/lists/*
ENV PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
RUN curl https://sh.rustup.rs -sSf | sh -s -- -y --default-toolchain nightly --profile default --no-modify-path && \
    rustup target add armv7-unknown-linux-musleabihf && \
    rustup component add rust-src --toolchain nightly-x86_64-unknown-linux-gnu
RUN curl -O https://liftm.de/Misc/armv7l-linux-musleabihf-cross-21-11-23.tgz \
       && echo f49f1a15ec62364ef5e4edb4e3990c0e1d2d1a54c90153b8f3869dad63328a10 \ armv7l-linux-musleabihf-cross-21-11-23.tgz | sha256sum -c \
       && tar xvf armv7l-linux-musleabihf-cross-21-11-23.tgz -C /usr/local \
       && rm armv7l-linux-musleabihf-cross-21-11-23.tgz
ENV CC_armv7_unknown_linux_musleabihf=/usr/local/armv7l-linux-musleabihf-cross/bin/armv7l-linux-musleabihf-gcc
ENV CFLAGS_armv7_unknown_linux_musleabihf="-mfpu=neon"
WORKDIR /root/src
'

# Good part about podman? no need to worry about PIDs and file owners.
# Bad part? Build cache is dead slow...
dfh=$(sha256sum <<<"$df" - | cut -d\  -f1)
tag=rust-wasi-builder-$dfh
if ! podman image list --format "{{.Repository}}" | grep -q $tag; then
    podman build -t $tag - <<<"$df"
fi

root="$(realpath "$(dirname "$0")")"

mkdir -p "$root/emk-target" "$root/emk-cache" "$root/emk-cargo"

echo '
[target.armv7-unknown-linux-musleabihf]
#rustflags = ["-L/usr/arm-linux-gcceabihf/lib/", "-L/usr/lib/arm-linux-gcceabihf/", "-L/usr/lib/gcc-cross/arm-linux-gcceabihf/10/"]
#linker = "/usr/bin/arm-linux-gcceabihf-g++"
rustflags = ["-L/usr/local/armv7l-linux-musleabihf-cross/armv7l-linux-musleabihf/lib/", "-C", "link-args=-lm -lc"]
linker = "/usr/local/armv7l-linux-musleabihf-cross/armv7l-linux-musleabihf/bin/ld"
'> "$root/emk-cargo/config.toml"

set -x
podman run --rm \
    -w "/root/src/serialsensors" \
    -v "$root:/root/src/serialsensors:ro" \
    -v "$root/emk-target:/root/src/serialsensors/target" \
    -v "$root/emk-cargo:/root/src/serialsensors/.cargo:ro" \
    -v "$root/emk-cache:/root/.cargo/registry" \
    $tag cargo +nightly build --release --locked --target=armv7-unknown-linux-musleabihf -Zbuild-std  

