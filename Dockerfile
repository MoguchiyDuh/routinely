FROM rust:1-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
    && cp target/release/routinely /usr/local/bin/routinely

FROM debian:trixie-slim
RUN useradd --system --uid 1000 --home-dir /app routinely \
    && mkdir -p /app/data/uploads \
    && chown -R routinely:routinely /app
WORKDIR /app
COPY --from=build /usr/local/bin/routinely /usr/local/bin/routinely
COPY static ./static
USER routinely
ENV ROUTINELY_BIND=0.0.0.0:8080
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 CMD ["routinely", "healthcheck"]
CMD ["routinely"]
