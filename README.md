# autocompleted

A tag autocomplete API service for GayFurCity, written in Rust using [actix-web](https://actix.rs/). It queries a PostgreSQL database for matching tags and caches results in memory. Used for [GayFurCity](https://github.com/GayFurCity/GayFurCity).

## Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/`  | Autocomplete tags |
| `GET`  | `/up` | Health check (returns 204) |

Prometheus metrics are served separately, on `METRICS_ADDR`, so `/metrics` is never reachable through the public reverse proxy:

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/metrics` | Prometheus metrics (on `METRICS_ADDR`, not `SERVER_ADDR`) |

Exposes standard `actix-web-prom` HTTP metrics (`autocompleted_http_requests_total`, `autocompleted_http_requests_duration_seconds`) plus:

- `autocompleted_cache_hits_total` / `autocompleted_cache_misses_total` — in-memory cache effectiveness
- `autocompleted_db_query_duration_seconds` — tag lookup query latency

### `GET /?search[name_matches]=<prefix>`

Returns a JSON array of matching tags. The prefix must be 3–100 characters.

**Example:**
```
GET /?search[name_matches]=cani
```

**Response:**
```json
[
  {"id": 1, "name": "canine", "post_count": 12345, "category": 0, "antecedent_name": null}
]
```

Responses are cached for 6 hours (up to 15,000 entries). Supports `*` as a wildcard in the prefix.

## Configuration

Copy `.env.sample` to `.env` and fill in the values:

```env
SERVER_ADDR=0.0.0.0:3000
METRICS_ADDR=0.0.0.0:9396
PG__USER=postgres
PG__PASSWORD=secret
PG__HOST=localhost
PG__PORT=5432
PG__DBNAME=mydb
PG__POOL__MAX_SIZE=16
```

## Running

```bash
# Development
cargo run

# Docker
docker compose up
```

## Building

```bash
cargo build --release
```

## License

See [LICENSE](LICENSE).
