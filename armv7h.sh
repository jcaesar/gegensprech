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
        musl-tools \
        sudo \
        crossbuild-essential-armhf \
        gcc-arm-linux-gnueabihf \
        binutils-arm-linux-gnueabi \
        && \
    apt-get clean && rm -rf /var/lib/apt/lists/*
ENV PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
RUN curl https://sh.rustup.rs -sSf | sh -s -- -y --default-toolchain nightly --profile default --no-modify-path && \
    rustup target add armv7-unknown-linux-musleabi && \
    rustup component add rust-src --toolchain nightly-x86_64-unknown-linux-gnu
RUN curl -O https://musl.cc/armv7l-linux-musleabihf-cross.tgz \
       && echo 77a7e3ec4df13f33ca1e93505504ef888d8980726f26a62a3c79d39573668143 \ armv7l-linux-musleabihf-cross.tgz | sha256sum -c \
       && tar xvf armv7l-linux-musleabihf-cross.tgz -C /usr/local \
       && rm armv7l-linux-musleabihf-cross.tgz
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
rustflags = ["-L/usr/local/armv7l-linux-musleabihf-cross/armv7l-linux-musleabihf/lib/", "-L/usr/local/armv7l-linux-musleabihf-cross/lib/gcc/armv7l-linux-musleabihf/10.2.1/"]
linker = "/usr/local/armv7l-linux-musleabihf-cross/bin/armv7l-linux-musleabihf-gcc"
'> "$root/emk-cargo/config.toml"

#set -x
podman run --rm \
    -w "/root/src/serialsensors" \
    -v "$root:/root/src/serialsensors:ro" \
    -v "$root/emk-target:/root/src/serialsensors/target" \
    -v "$root/emk-cargo:/root/src/serialsensors/.cargo:ro" \
    -v "$root/emk-cache:/root/.cargo/registry" \
    -e CC=arm-linux-gnueabihf-gcc \
    $tag cargo +nightly build -Z build-std --release --locked --target=armv7-unknown-linux-musleabihf

