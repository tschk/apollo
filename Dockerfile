FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd --system --create-home --home-dir /home/apollo apollo
WORKDIR /app
RUN mkdir -p /app/workspace/.apollo && chown -R apollo:apollo /app/workspace
COPY target/x86_64-unknown-linux-gnu/release/apollo .
COPY container-config.json /app/apollo.json
USER apollo
EXPOSE 8080
# The container must listen on all interfaces to be reachable through a port
# mapping, so the MCP server requires a bearer token: run with
# `-e APOLLO_HTTP_TOKEN=<secret>` and send `Authorization: Bearer <secret>`.
# Startup fails without it rather than serving shell tools unauthenticated.
ENTRYPOINT ["./apollo", "mcp", "--host", "0.0.0.0", "--port", "8080", "--config", "/app/apollo.json", "--workspace", "/app/workspace"]
