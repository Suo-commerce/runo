# Runo in a container: the binary and the profiles, nothing else. Build from the repository root:
#   docker build -f deploy/goliath/Dockerfile -t runo:local .
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p runo-cli

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/runo /usr/local/bin/runo
COPY profiles /profiles
ENTRYPOINT ["runo"]
