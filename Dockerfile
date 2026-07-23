FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
RUN mkdir -p /app/workspace/.apollo
COPY target/x86_64-unknown-linux-gnu/release/apollo .
COPY container-config.json /app/apollo.json
EXPOSE 8080
ENTRYPOINT ["./apollo", "mcp", "--port", "8080", "--config", "/app/apollo.json", "--workspace", "/app/workspace"]
