# Container image for harness sandboxes that cannot install to the host.
#
# Build stage is pinned to the workspace MSRV (Rust 1.89). The runtime stage
# keeps only the release binary, CA certificates for HTTPS sources, and an
# unprivileged user.

FROM rust:1.89-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
# skills/prog/SKILL.md is embedded into the binary via include_str!.
COPY skills/ skills/
COPY crates/ crates/
RUN cargo build --release --locked -p prog-cli \
    && cp target/release/prog /usr/local/bin/prog

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 1000 --create-home prog
COPY --from=build /usr/local/bin/prog /usr/local/bin/prog
WORKDIR /work
RUN chown prog:prog /work
USER prog
# The local store defaults to ./.prog relative to the working directory.
ENV PROG_DIR=/work/.prog
ENTRYPOINT ["/usr/local/bin/prog"]
CMD ["--help"]
