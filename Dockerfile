# Build from this repository root. The server depends only on published Seiza
# crates, so this image is independent of a sibling checkout.
FROM node:24-bookworm-slim AS web-build
WORKDIR /web
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM rust:1.96-bookworm AS rust-build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
ARG CARGO_FEATURES=""
RUN cargo build --release --locked $CARGO_FEATURES

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 seiza
WORKDIR /app
COPY --from=rust-build /src/target/release/seiza-server /usr/local/bin/seiza-server
COPY --from=web-build /web/dist ./frontend/dist
USER seiza
ENV SEIZA_BIND_ADDR=0.0.0.0:8080 \
    SEIZA_FRONTEND_DIR=/app/frontend/dist \
    SEIZA_DATA_DIR=/app/data
EXPOSE 8080
ENTRYPOINT ["seiza-server"]
