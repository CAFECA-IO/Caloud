FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --bin caloud --bin caloud-agent

FROM docker:27-cli AS dockercli

FROM debian:bookworm-slim
# git：Git 來源 App（ADR 0009）
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git && rm -rf /var/lib/apt/lists/*
COPY --from=dockercli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=dockercli /usr/local/libexec/docker/cli-plugins/docker-compose /usr/local/libexec/docker/cli-plugins/docker-compose
COPY --from=build /src/target/release/caloud /usr/local/bin/caloud
# Runner agent：安裝到 Runner 主機時由映像取出（ADR 0018）
COPY --from=build /src/target/release/caloud-agent /usr/local/bin/caloud-agent
ENV CALOUD_DATA_DIR=/data
VOLUME /data
EXPOSE 80 443 8080
ENTRYPOINT ["caloud"]
