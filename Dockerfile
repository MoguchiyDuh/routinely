FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
    && cp target/release/routinely /usr/local/bin/routinely

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --home-dir /app routinely \
    && mkdir -p /app/data/uploads \
    && chown -R routinely:routinely /app
WORKDIR /app
COPY --from=build /usr/local/bin/routinely /usr/local/bin/routinely
COPY static ./static
USER routinely
ENV ROUTINELY_BIND=0.0.0.0:8080
EXPOSE 8080
CMD ["routinely"]
