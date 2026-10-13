# The Pinakes service. Built on the official Rust image, run on a slim Debian of the same
# release. fastembed links a prebuilt ONNX Runtime into the binary, which needs glibc 2.38 and
# GCC 13's libstdc++: Debian 13 (trixie), not 12. `docker compose up` builds it.
FROM rust:1-trixie AS build
WORKDIR /src
COPY . .
# The commit the image is built from, for search traces (there is no .git in the context).
ARG PINAKES_GIT_COMMIT=unknown
ENV PINAKES_GIT_COMMIT=${PINAKES_GIT_COMMIT}
# Parallel compile jobs; fewer use less memory (a release build of every core can take
# several GB). Cargo's default, the number of cores, when not given.
ARG CARGO_BUILD_JOBS
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin pinakes --bin pinakes-pdf \
    && cp target/release/pinakes target/release/pinakes-pdf /usr/local/bin/

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /app pinakes \
    && mkdir /data && chown pinakes /data
# The service and its PDF reader, which it runs beside itself for every PDF.
COPY --from=build /usr/local/bin/pinakes /usr/local/bin/pinakes-pdf /usr/local/bin/
WORKDIR /app
COPY docker/config.toml ./config.toml
USER pinakes
# Wiki files, SQLite, indexes, history, the embedding model (when turned on) and the service's
# config: the image's config.toml is copied there on first start, and settings changed from the
# console are written to that copy, so they outlive the container.
VOLUME ["/data"]
EXPOSE 8080
ENV PINAKES_CONFIG=/data/config.toml
ENV PINAKES_CONFIG_DEFAULT=/app/config.toml
CMD ["pinakes"]
