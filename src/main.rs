use actix_web::{
    error, get,
    http::header,
    http::StatusCode,
    middleware::DefaultHeaders,
    web::{self, Data},
    HttpResponse, HttpResponseBuilder,
};
use actix_web_prom::PrometheusMetricsBuilder;
use deadpool_postgres::{Pool, Runtime};
use derive_more::{Display, Error, From};
use log::error;
use moka::future::Cache;
use prometheus::{Encoder, Histogram, HistogramOpts, IntCounter, Registry, TextEncoder};
use serde::Deserialize;
use std::time::Instant;

mod config {
    use serde::Deserialize;

    #[derive(Deserialize)]
    pub struct Config {
        pub server_addr: String,
        pub metrics_addr: String,
        pub pg: deadpool_postgres::Config,
    }

    impl Config {
        pub fn from_env() -> Result<Self, config::ConfigError> {
            config::Config::builder()
                .set_default("metrics_addr", "0.0.0.0:9396")?
                .add_source(config::Environment::default().separator("__"))
                .build()?
                .try_deserialize()
        }
    }
}

mod models {
    use serde::{Deserialize, Serialize};
    use tokio_pg_mapper_derive::PostgresMapper;

    #[derive(Deserialize, PostgresMapper, Serialize)]
    #[pg_mapper(table = "tags")] // singular 'user' is a keyword..
    pub struct Tag {
        pub id: i64,
        pub name: String,
        pub post_count: i32,
        pub category: i16,
        pub antecedent_name: Option<String>,
    }
}

mod db {
    use deadpool_postgres::Client;
    use tokio_pg_mapper::FromTokioPostgresRow;

    use crate::models::Tag;

    fn escape_like(stuff: &str) -> String {
        stuff
            .replace('%', "\\%")
            .replace('_', "\\_")
            .replace('*', "%")
            .replace("\\*", "*")
    }

    pub async fn get_tags(
        client: &Client,
        tag_prefix: &String,
    ) -> Result<Vec<Tag>, tokio_postgres::Error> {
        let escape_prefix = escape_like(&(tag_prefix.to_owned() + "*"));
        let _stmt = "set statement_timeout = 3000";
        let stmt = client.prepare(_stmt).await?;
        client.execute(&stmt, &[]).await?;
        let _stmt = include_str!("../sql/fetch_tags_a.sql");
        let stmt = client.prepare(_stmt).await?;
        let rows = client
            .query(&stmt, &[&escape_prefix])
            .await?
            .iter()
            .map(|row| Tag::from_row_ref(row).unwrap())
            .collect::<Vec<Tag>>();
        if !rows.is_empty() {
            return Ok(rows);
        }
        let _stmt = include_str!("../sql/fetch_tags_b.sql");
        let stmt = client.prepare(_stmt).await?;
        let rows = client
            .query(&stmt, &[&tag_prefix])
            .await?
            .iter()
            .map(|row| Tag::from_row_ref(row).unwrap())
            .collect::<Vec<Tag>>();
        Ok(rows)
    }
}

struct AutocompleteState {
    pool: Pool,
    cache: Cache<String, String>,
    cache_hits: IntCounter,
    cache_misses: IntCounter,
    db_query_duration: Histogram,
}

#[derive(Debug, Display, Error, From)]
enum AutocompleteError {
    #[display(fmt = "bad request")]
    BadRequest,
    #[display(fmt = "internal error")]
    ServerError,
}

impl error::ResponseError for AutocompleteError {
    fn error_response(&self) -> HttpResponse {
        match *self {
            AutocompleteError::BadRequest => HttpResponseBuilder::new(self.status_code())
                .insert_header((header::CONTENT_TYPE, "application/json; charset=utf-8"))
                .insert_header((header::CACHE_CONTROL, "private; max-age=0"))
                .body("{\"error\":\"bad request\"}"),
            AutocompleteError::ServerError => HttpResponseBuilder::new(self.status_code())
                .insert_header((header::CONTENT_TYPE, "application/json; charset=utf-8"))
                .insert_header((header::CACHE_CONTROL, "private; max-age=0"))
                .body("{\"error\":\"internal error\"}"),
        }
    }

    fn status_code(&self) -> StatusCode {
        match *self {
            AutocompleteError::BadRequest => StatusCode::BAD_REQUEST,
            AutocompleteError::ServerError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

fn validate_transform_tag(tag: &str) -> Result<String, AutocompleteError> {
    use unicode_normalization::UnicodeNormalization;
    if tag.len() > 100 {
        return Err(AutocompleteError::BadRequest);
    }
    if tag.len() < 3 {
        return Err(AutocompleteError::BadRequest);
    }
    let tag_str = tag
        .nfc()
        .collect::<String>()
        .to_lowercase()
        .replace(['*', '%', '\0'], "")
        .chars()
        .filter(|x| !x.is_whitespace())
        .collect();
    Ok(tag_str)
}

#[derive(Deserialize)]
struct Req {
    #[serde(rename(deserialize = "search[name_matches]"))]
    tag_prefix: String,
}

#[get("/")]
async fn autocomplete(
    data: web::Data<AutocompleteState>,
    req: web::Query<Req>,
) -> Result<HttpResponse, AutocompleteError> {
    let prefix: String = validate_transform_tag(req.tag_prefix.as_str())?;
    let cached = data.cache.get(&prefix).await;
    return if let Some(cached_json) = cached {
        data.cache_hits.inc();
        Ok(HttpResponse::Ok()
            .insert_header((header::CONTENT_TYPE, "application/json; charset=utf-8"))
            .insert_header((header::CACHE_CONTROL, "public, max-age=604800"))
            .body(cached_json))
    } else {
        data.cache_misses.inc();
        let client = match data.pool.get().await {
            Ok(x) => x,
            Err(x) => {
                error!("{}", x.to_string());
                return Err(AutocompleteError::ServerError);
            }
        };
        let query_start = Instant::now();
        let results = match db::get_tags(&client, &prefix).await {
            Ok(x) => x,
            Err(x) => {
                data.db_query_duration.observe(query_start.elapsed().as_secs_f64());
                error!("{}", x.to_string());
                return Err(AutocompleteError::ServerError);
            }
        };
        data.db_query_duration.observe(query_start.elapsed().as_secs_f64());
        let serialized = serde_json::to_string(&results).unwrap_or_else(|_| "[]".to_string());
        let serialized_copy = serialized.clone();
        data.cache.insert(prefix, serialized).await;
        Ok(HttpResponse::Ok()
            .insert_header((header::CONTENT_TYPE, "application/json; charset=utf-8"))
            .insert_header((header::CACHE_CONTROL, "public, max-age=604800"))
            .body(serialized_copy))
    };
}

#[get("/up")]
async fn healthcheck() -> HttpResponse {
    HttpResponse::NoContent().finish()
}

async fn metrics(registry: web::Data<Registry>) -> HttpResponse {
    let encoder = TextEncoder::new();
    let mut buffer = Vec::new();
    encoder
        .encode(&registry.gather(), &mut buffer)
        .unwrap_or_default();
    HttpResponse::Ok()
        .content_type(encoder.format_type())
        .body(buffer)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    use actix_web::{App, HttpServer};
    use moka::future::CacheBuilder;
    use std::time::Duration;
    use tokio_postgres::NoTls;
    env_logger::init();

    let config = crate::config::Config::from_env().unwrap();
    let pool = config.pg.create_pool(Some(Runtime::Tokio1), NoTls).unwrap();
    let cache = CacheBuilder::new(15_000)
        .time_to_live(Duration::from_secs(6 * 60 * 60))
        .build();

    // http_requests_total / http_requests_duration_seconds instrumentation, wrapped around
    // the public app below. Not served on the public port: /metrics is only exposed on
    // metrics_addr, an internal port not reachable through the reverse-proxied domain.
    let http_metrics = PrometheusMetricsBuilder::new("autocompleted")
        .build()
        .unwrap();
    let registry = http_metrics.registry.clone();

    let cache_hits = IntCounter::new(
        "autocompleted_cache_hits_total",
        "Number of autocomplete requests served from the in-memory cache",
    )
    .unwrap();
    let cache_misses = IntCounter::new(
        "autocompleted_cache_misses_total",
        "Number of autocomplete requests that required a database query",
    )
    .unwrap();
    let db_query_duration = Histogram::with_opts(HistogramOpts::new(
        "autocompleted_db_query_duration_seconds",
        "Duration of tag lookup queries against postgres",
    ))
    .unwrap();
    registry.register(Box::new(cache_hits.clone())).unwrap();
    registry.register(Box::new(cache_misses.clone())).unwrap();
    registry
        .register(Box::new(db_query_duration.clone()))
        .unwrap();

    let app_server = HttpServer::new(move || {
        App::new()
            .wrap(
                DefaultHeaders::new()
                    .add((header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"))
                    .add((header::ACCESS_CONTROL_ALLOW_HEADERS, "Authorization")),
            )
            .wrap(http_metrics.clone())
            .app_data(Data::new(AutocompleteState {
                pool: pool.clone(),
                cache: cache.clone(),
                cache_hits: cache_hits.clone(),
                cache_misses: cache_misses.clone(),
                db_query_duration: db_query_duration.clone(),
            }))
            .service(autocomplete)
            .service(healthcheck)
    })
    .bind(config.server_addr.clone())?
    .run();

    let metrics_server = HttpServer::new(move || {
        App::new()
            .app_data(Data::new(registry.clone()))
            .route("/metrics", web::get().to(metrics))
    })
    .bind(config.metrics_addr.clone())?
    .run();

    tokio::try_join!(app_server, metrics_server)?;
    Ok(())
}
