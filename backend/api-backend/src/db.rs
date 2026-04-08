use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub async fn init_pool(database_url: &str) -> sqlx::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;

    tracing::info!("Database connected");
    Ok(pool)
}
