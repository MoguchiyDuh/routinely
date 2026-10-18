FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN useradd --system --uid 10001 --home-dir /app --shell /usr/sbin/nologin routinely \
    && mkdir -p /app/data \
    && chown -R routinely:routinely /app
WORKDIR /app
COPY --from=build /src/target/release/routinely /usr/local/bin/routinely
COPY --chown=routinely:routinely static ./static
USER routinely
ENV ROUTINELY_BIND=0.0.0.0:8080 \
    ROUTINELY_DB_PATH=/app/data/routinely.db
VOLUME /app/data
EXPOSE 8080
ENTRYPOINT ["routinely"]
