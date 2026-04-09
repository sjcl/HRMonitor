#[tokio::main]
async fn main() {
    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");

    let pool = {
        let max_retries = 10u32;
        let mut delay = std::time::Duration::from_secs(1);
        let max_delay = std::time::Duration::from_secs(30);
        let mut result = None;

        for attempt in 1..=max_retries {
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(2)
                .connect(&database_url)
                .await
            {
                Ok(pool) => {
                    if attempt > 1 {
                        println!("Connected to database on attempt {attempt}");
                    }
                    result = Some(pool);
                    break;
                }
                Err(e) => {
                    if attempt == max_retries {
                        panic!("Failed to connect to database after {max_retries} attempts: {e}");
                    }
                    println!(
                        "Database connection attempt {attempt}/{max_retries} failed: {e}. \
                         Retrying in {}s...",
                        delay.as_secs()
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(max_delay);
                }
            }
        }

        result.unwrap()
    };

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    println!("Migrations applied successfully");
}
