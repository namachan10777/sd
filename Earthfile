VERSION 0.6
FROM rust:1.65.0-slim-bullseye
RUN rustup component add rustfmt clippy
RUN cargo install cargo-chef
WORKDIR /work

plan:
    COPY Cargo.toml .
    COPY Cargo.lock .
    COPY src src
    RUN cargo chef prepare
    SAVE ARTIFACT recipe.json /recipe.json

test:
    COPY +plan/recipe.json recipe.json
    RUN cargo chef cook
    RUN cargo clippy
    COPY Cargo.toml .
    COPY Cargo.lock .
    COPY src src
    RUN cargo fmt -- --check
    RUN cargo clippy -- -D warnings
    RUN cargo test

build:
    COPY +plan/recipe.json recipe.json
    RUN cargo chef cook --release
    COPY Cargo.toml .
    COPY Cargo.lock .
    COPY src src
    RUN cargo build --release
    RUN strip target/release/sd
    SAVE ARTIFACT target/release/sd /bin

app:
    FROM debian:bullseye-slim
    COPY +build/bin /usr/local/bin/sd
    ENV RUST_LOG=Info
    ENTRYPOINT [ "/usr/local/bin/sd" ]
    CMD [ "--addr", "0.0.0.0:7000" ]
    SAVE IMAGE sd
