FROM rust:1-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
COPY config ./config

RUN cargo build --release --bin agent

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        libsqlite3-0 \
        wget \
        python3 \
        tesseract-ocr \
        tesseract-ocr-eng \
        tesseract-ocr-spa \
        yt-dlp \
        openssh-client \
        ffmpeg \
        tmux \
        docker.io \
    && rm -rf /var/lib/apt/lists/*

ARG TARGETARCH=amd64
RUN wget -qO /usr/local/bin/cloudflared \
        "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-${TARGETARCH}" \
    && chmod +x /usr/local/bin/cloudflared \
    && /usr/local/bin/cloudflared --version

ENV CLOUDFLARED_BINARY=/usr/local/bin/cloudflared

WORKDIR /app
COPY --from=builder /app/target/release/agent /usr/local/bin/agent
COPY config ./config

RUN mkdir -p /app/data /run/secrets

EXPOSE 8080 9090

ENTRYPOINT ["/usr/local/bin/agent"]
CMD ["--config", "/app/config"]
