FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --bin caloud

FROM docker:27-cli AS dockercli

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=dockercli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=dockercli /usr/local/libexec/docker/cli-plugins/docker-compose /usr/local/libexec/docker/cli-plugins/docker-compose
COPY --from=build /src/target/release/caloud /usr/local/bin/caloud
ENV CALOUD_DATA_DIR=/data
VOLUME /data
EXPOSE 80 443 8080
ENTRYPOINT ["caloud"]
