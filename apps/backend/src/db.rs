use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::token_encryption::TokenEncryption;

pub async fn init_pool(database_url: &str) -> sqlx::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;

    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    tracing::info!("Database initialized (migrations applied)");
    Ok(pool)
}

/// Migrate legacy plaintext pulsoid_access_token from users table to pulsoid_connections.
/// Each user is migrated in its own transaction so a partial failure doesn't lose tokens.
pub async fn migrate_legacy_pulsoid_tokens(db: &PgPool, encryption: &TokenEncryption) {
    let rows: Vec<(String, Option<String>)> = match sqlx::query_as(
        "SELECT id, pulsoid_access_token FROM users WHERE pulsoid_access_token IS NOT NULL",
    )
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to query legacy pulsoid tokens: {e}");
            return;
        }
    };

    if rows.is_empty() {
        return;
    }

    let mut migrated = 0u32;
    let mut skipped = 0u32;
    let mut failed = 0u32;

    for (user_id, token_opt) in &rows {
        let token = match token_opt {
            Some(t) if !t.is_empty() => t,
            _ => {
                skipped += 1;
                continue;
            }
        };

        // Check if user already has a pulsoid_connections row
        let exists: bool = match sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pulsoid_connections WHERE user_id = $1)",
        )
        .bind(user_id)
        .fetch_one(db)
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(user_id, "Failed to check existing connection: {e}");
                failed += 1;
                continue;
            }
        };

        if exists {
            skipped += 1;
            continue;
        }

        let (enc_access, key_version) = encryption.encrypt(token);

        // INSERT + clear old column in a single transaction
        let mut tx = match db.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                tracing::warn!(user_id, "Failed to begin transaction: {e}");
                failed += 1;
                continue;
            }
        };

        if let Err(e) = sqlx::query(
            "INSERT INTO pulsoid_connections (user_id, source, access_token, key_version)
             VALUES ($1, 'manual', $2, $3)",
        )
        .bind(user_id)
        .bind(&enc_access)
        .bind(key_version as i32)
        .execute(&mut *tx)
        .await
        {
            tracing::warn!(user_id, "Failed to insert pulsoid connection: {e}");
            failed += 1;
            continue;
        }

        if let Err(e) = sqlx::query(
            "UPDATE users SET pulsoid_access_token = NULL WHERE id = $1",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await
        {
            tracing::warn!(user_id, "Failed to clear legacy token: {e}");
            failed += 1;
            continue;
        }

        if let Err(e) = tx.commit().await {
            tracing::warn!(user_id, "Failed to commit migration transaction: {e}");
            failed += 1;
            continue;
        }

        migrated += 1;
    }

    tracing::info!(
        migrated,
        skipped,
        failed,
        "Legacy pulsoid token migration complete"
    );
}
