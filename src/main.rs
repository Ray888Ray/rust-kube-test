use std::{env, io};

use actix_web::{App, HttpResponse, HttpServer, Responder, error::ErrorInternalServerError, web};
use redis::AsyncCommands;
use serde::Serialize;
use sqlx::{PgPool, postgres::PgPoolOptions};

#[derive(Clone)]
struct AppState {
    pg_pool: PgPool,
    redis_client: redis::Client,
}

struct AppConfig {
    port: u16,
    postgres_url: String,
    redis_url: String,
}

impl AppConfig {
    fn from_env() -> Self {
        let port = env::var("APP_PORT")
            .ok()
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or(8080);

        let postgres_url = env::var("POSTGRES_URL")
            .unwrap_or_else(|_| "postgres://postgres:postgres@postgres:5432/postgres".to_owned());

        let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://redis:6379".to_owned());

        Self {
            port,
            postgres_url,
            redis_url,
        }
    }
}

#[derive(Serialize)]
struct GreetResponse {
    message: &'static str,
    postgres_time: String,
    redis_visits: u64,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    postgres: &'static str,
    redis: &'static str,
}

#[actix_web::get("/greet")]
async fn greet(state: web::Data<AppState>) -> Result<impl Responder, actix_web::Error> {
    let postgres_time: String = sqlx::query_scalar("SELECT NOW()::text")
        .fetch_one(&state.pg_pool)
        .await
        .map_err(ErrorInternalServerError)?;

    let mut redis_connection = state
        .redis_client
        .get_multiplexed_async_connection()
        .await
        .map_err(ErrorInternalServerError)?;

    let redis_visits: u64 = redis_connection
        .incr("greet:visits", 1_u64)
        .await
        .map_err(ErrorInternalServerError)?;

    Ok(web::Json(GreetResponse {
        message: "Hello, world!",
        postgres_time,
        redis_visits,
    }))
}

#[actix_web::get("/health")]
async fn health(state: web::Data<AppState>) -> impl Responder {
    let postgres_ok = sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.pg_pool)
        .await
        .is_ok();

    let redis_ok = async {
        let mut redis_connection = state
            .redis_client
            .get_multiplexed_async_connection()
            .await?;
        redis::cmd("PING")
            .query_async::<String>(&mut redis_connection)
            .await
    }
    .await
    .is_ok();

    if postgres_ok && redis_ok {
        HttpResponse::Ok().json(HealthResponse {
            status: "ok",
            postgres: "up",
            redis: "up",
        })
    } else {
        HttpResponse::ServiceUnavailable().json(HealthResponse {
            status: "degraded",
            postgres: if postgres_ok { "up" } else { "down" },
            redis: if redis_ok { "up" } else { "down" },
        })
    }
}

#[actix_web::main]
async fn main() -> io::Result<()> {
    let config = AppConfig::from_env();

    let pg_pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.postgres_url)
        .await
        .map_err(|err| io::Error::other(format!("Postgres connection failed: {err}")))?;

    let redis_client = redis::Client::open(config.redis_url.as_str())
        .map_err(|err| io::Error::other(format!("Redis client init failed: {err}")))?;

    {
        let mut redis_connection = redis_client
            .get_multiplexed_async_connection()
            .await
            .map_err(|err| io::Error::other(format!("Redis connection failed: {err}")))?;
        let _: String = redis::cmd("PING")
            .query_async(&mut redis_connection)
            .await
            .map_err(|err| io::Error::other(format!("Redis ping failed: {err}")))?;
    }

    let app_state = web::Data::new(AppState {
        pg_pool,
        redis_client,
    });

    println!("Starting server on port {}", config.port);

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone())
            .service(greet)
            .service(health)
    })
    .bind(("0.0.0.0", config.port))?
    .workers(4)
    .run()
    .await
}
