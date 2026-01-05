FROM ubuntu:24.04

RUN apt-get update && apt-get install -y \
    curl \
    build-essential \
    sudo \
    pkg-config \
    libssl-dev \
    && curl https://sh.rustup.rs -sSf | sh -s -- -y

ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /usr/src/app
COPY . .

RUN cargo build --release
