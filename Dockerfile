# Stage 1: Build Game Master (server)
FROM rust:slim AS server-build
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY crates/questlib crates/questlib
COPY crates/gamemaster crates/gamemaster
COPY adventures adventures
RUN cargo build --release -p gamemaster

# Stage 2: Build WASM client
FROM rust:slim AS wasm-build
WORKDIR /app
RUN rustup target add wasm32-unknown-unknown
RUN cargo install wasm-bindgen-cli --version 0.2.114
COPY Cargo.toml Cargo.lock ./
COPY crates/questlib crates/questlib
COPY crates/gamemaster crates/gamemaster
COPY crates/gameclient crates/gameclient
COPY adventures adventures
RUN cd crates/gameclient && cargo build --release --target wasm32-unknown-unknown
RUN wasm-bindgen crates/gameclient/target/wasm32-unknown-unknown/release/gameclient.wasm \
    --out-dir /app/web --target web --no-typescript

# Stage 3: Runtime
FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

# Copy server binary
COPY --from=server-build /app/target/release/gamemaster /app/gamemaster

# Copy WASM client + favicon
COPY --from=wasm-build /app/web /app/crates/gameclient/web
COPY --from=wasm-build /app/crates/gameclient/index.html /app/crates/gameclient/index.html
COPY --from=wasm-build /app/crates/gameclient/favicon.png /app/crates/gameclient/favicon.png
COPY --from=wasm-build /app/crates/gameclient/assets /app/crates/gameclient/assets

# Copy adventure data
COPY adventures /app/adventures

ENV MAP_SEED=12345
ENV EVENTS_PATH=adventures/seed12345_events.json
ENV RUST_LOG=gamemaster=info

EXPOSE 3001
CMD ["/app/gamemaster"]
