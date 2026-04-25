FROM rust:1-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
COPY config ./config

# Build the renamed `nexo` bin. The legacy `agent` binary name was
# retired in commit 4bccdc3 (rename: agent_* crates → nexo_*, agent
# bin → nexo).
RUN cargo build --release --bin nexo

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
        dumb-init \
        fonts-liberation \
        libasound2 \
        libatk-bridge2.0-0 \
        libatk1.0-0 \
        libcairo2 \
        libcups2 \
        libdbus-1-3 \
        libexpat1 \
        libfontconfig1 \
        libgbm1 \
        libglib2.0-0 \
        libgtk-3-0 \
        libnspr4 \
        libnss3 \
        libpango-1.0-0 \
        libx11-6 \
        libxcb1 \
        libxcomposite1 \
        libxdamage1 \
        libxext6 \
        libxfixes3 \
        libxrandr2 \
        libxshmfence1 \
        xdg-utils \
    && rm -rf /var/lib/apt/lists/*

ARG TARGETARCH=amd64
RUN wget -qO /usr/local/bin/cloudflared \
        "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-${TARGETARCH}" \
    && chmod +x /usr/local/bin/cloudflared \
    && /usr/local/bin/cloudflared --version

ENV CLOUDFLARED_BINARY=/usr/local/bin/cloudflared

# Google Chrome — treated like cloudflared, yt-dlp, ffmpeg: a runtime
# binary the agent's browser plugin launches directly. Real Chrome (not
# Chromium) for Google OAuth / 2FA acceptance, Widevine, and H.264
# codecs. amd64 only — .deb is x86_64 exclusive; arm64 hosts fall back
# to the Debian `chromium` package (Chrome isn't published for arm64).
RUN if [ "${TARGETARCH}" = "amd64" ]; then \
        wget -qO /tmp/chrome.deb \
            "https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb" \
        && apt-get update \
        && apt-get install -y --no-install-recommends /tmp/chrome.deb \
        && rm -f /tmp/chrome.deb \
        && rm -rf /var/lib/apt/lists/* \
        && /usr/bin/google-chrome --version; \
    else \
        apt-get update \
        && apt-get install -y --no-install-recommends chromium \
        && rm -rf /var/lib/apt/lists/* \
        && chromium --version \
        && ln -sf /usr/bin/chromium /usr/bin/google-chrome; \
    fi

WORKDIR /app
COPY --from=builder /app/target/release/nexo /usr/local/bin/nexo
COPY config ./config

RUN mkdir -p /app/data /run/secrets

# OCI labels for ghcr.io (filled in by the build workflow via --label).
LABEL org.opencontainers.image.source="https://github.com/lordmacu/nexo-rs" \
      org.opencontainers.image.description="Nexo — multi-agent Rust framework" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"

EXPOSE 8080 9090

ENTRYPOINT ["/usr/bin/dumb-init", "--", "/usr/local/bin/nexo"]
CMD ["--config", "/app/config"]
