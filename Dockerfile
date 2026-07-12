FROM rust:1.75-alpine3.19 AS builder

WORKDIR /app

RUN apk add --no-cache musl-dev

# Build dependencies in their own layer, keyed only on the manifest files, so
# unrelated source changes don't bust the cache and force a full recompile of
# every dependency on each build.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
RUN rm -rf src

COPY . .

RUN touch src/main.rs && cargo build --release

FROM alpine:3.19

HEALTHCHECK CMD sh -c 'wget --no-verbose --tries=1 --spider http://$SERVER_ADDR/up || exit 1'

COPY --from=builder /app/target/release/autocompleted /app/autocompleted
