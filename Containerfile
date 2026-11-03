# Build: podman build -t ours -f Containerfile .
# The toolchain comes from rust-toolchain.toml; the base image carries rustup.
FROM docker.io/library/rust:1.99-bookworm AS build
WORKDIR /src
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY manifests ./manifests
COPY ours.example.toml ./
RUN rustup toolchain install --profile minimal
RUN cargo build --release --locked -p ours \
    && install -D target/release/ours /out/ours

FROM docker.io/library/debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin ours \
    && install -d -o ours -g ours /data /etc/ours
COPY --from=build /out/ours /usr/local/bin/ours
COPY ours.example.toml /usr/share/doc/ours/ours.example.toml

USER ours
WORKDIR /data
VOLUME /data
EXPOSE 7777
# Set data_dir = "/data" in /etc/ours/ours.toml.
# `run` by default; `podman run image check` or `--version` still work.
ENTRYPOINT ["ours", "--config", "/etc/ours/ours.toml"]
CMD ["run"]
