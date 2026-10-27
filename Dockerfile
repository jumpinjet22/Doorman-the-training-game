# Two images from one build:
#   --target server  -> ghcr.io/<owner>/doorman       dorman-api + dorman-ingest
#   --target jobs    -> ghcr.io/<owner>/doorman-jobs  dorman-scorer, dorman-trainer, dorman-ingest
# Static files, templates and migrations are compiled into the binaries.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p dorman-api -p dorman-ingest

# The web server. No shell, no package manager, not root.
FROM gcr.io/distroless/cc-debian12:nonroot AS server
COPY --from=build /src/target/release/dorman-api /src/target/release/dorman-ingest /usr/local/bin/
# Listen on every interface inside the container; publish the port to choose
# who can reach it. Set DORMAN_PUBLIC_ORIGIN to the address players use.
ENV DORMAN_BIND=0.0.0.0:8080
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/usr/local/bin/dorman-api"]

# The nightly jobs: scoring, the training export, and the trainer.
FROM python:3.12-slim-bookworm AS jobs
COPY --from=build /src/target/release/dorman-ingest /usr/local/bin/
COPY scorer /tmp/scorer
COPY trainer /tmp/trainer
RUN pip install --no-cache-dir /tmp/scorer /tmp/trainer && rm -rf /tmp/scorer /tmp/trainer \
    && useradd --system --uid 10001 --home-dir /var/lib/dorman --create-home dorman
USER dorman
WORKDIR /var/lib/dorman
# No default job: run `dorman-scorer`, `dorman-ingest export ...` or
# `dorman-trainer ...` explicitly.
CMD ["dorman-scorer", "--help"]
