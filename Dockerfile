FROM python:3.12-slim

COPY --from=ghcr.io/astral-sh/uv:0.8.22 /uv /uvx /bin/

ENV PATH="/app/.venv/bin:$PATH" \
    UV_COMPILE_BYTECODE=1 \
    UV_LINK_MODE=copy

WORKDIR /app

COPY pyproject.toml uv.lock README.md ./
COPY pengepul ./pengepul

RUN uv sync --locked --no-dev
RUN useradd --create-home --home-dir /home/pengepul pengepul \
    && mkdir -p /home/pengepul/.pengepul \
    && chown -R pengepul:pengepul /home/pengepul /app

USER pengepul

ENV HOME=/home/pengepul

EXPOSE 8317

ENTRYPOINT ["pengepul"]
CMD ["serve", "--host", "0.0.0.0"]
