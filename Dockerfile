# ferro in Docker: browse a mounted repo with zero local install.
#   docker build -t ferro .
#   docker run -p 7777:7777 -v "$(pwd):/src:ro" ferro
FROM rust:1.98-slim-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p ferro

FROM debian:bookworm-slim
RUN useradd -m ferro
COPY --from=build /build/target/release/ferro /usr/local/bin/ferro
USER ferro
WORKDIR /src
EXPOSE 7777
ENTRYPOINT ["ferro", "-host", "0.0.0.0", "-port", "7777", "--no-open", "/src"]
