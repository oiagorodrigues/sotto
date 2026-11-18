# Build from the repository root: docker build -f services/api/Dockerfile .
FROM rust:1.99-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml ./
COPY crates/sotto-protocol crates/sotto-protocol
COPY services/api services/api
# sotto-protocol inherits package fields from the root workspace; keep only the crates that
# exist in this image so the other members do not have to be copied.
RUN sed -i 's|^members = .*|members = ["crates/sotto-protocol"]|' Cargo.toml
RUN cd services/api && cargo build --release --locked

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --no-create-home sotto
COPY --from=build /src/services/api/target/release/sotto-api /usr/local/bin/sotto-api
USER 10001
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/sotto-api"]
