//! Integration tests for refresh-token rotation against a real Redis.
//!
//! The unit tests in `sessions.rs` cover the decision logic; they cannot cover
//! what actually matters here — that the Lua script is atomic, that the 30-day
//! `EXAT` cap does not creep forward on rotation, and that two simultaneous
//! refreshes produce exactly one winner. Those are properties of Redis
//! executing the script, so they need Redis.
//!
//! Skipped (with a message, not silently) when `REDIS_URL` is unset, and run in
//! CI where it always is. Every test namespaces its keys with a random prefix
//! so the suite is safe to run in parallel and against a shared instance.

use api_backend::sessions::{
    self, RefreshSession, RotationOutcome, RotationReply, SessionKeys, decide_rotation, new_session,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use redis::AsyncCommands;

const GRACE: i64 = 10;
const NOW: i64 = 1_800_000_000;

fn keys() -> SessionKeys {
    SessionKeys::new([1u8; 32], [2u8; 32])
}

/// Unique per test so parallel runs cannot collide.
fn unique_sid() -> String {
    let mut b = [0u8; 12];
    rand::fill(&mut b);
    format!("test-{}", URL_SAFE_NO_PAD.encode(b))
}

fn session_key(sid: &str) -> String {
    format!("auth:session:v1:{sid}")
}

fn rotation_key(sid: &str) -> String {
    format!("auth:rotation:v1:{sid}")
}

async fn connect() -> Option<redis::aio::MultiplexedConnection> {
    let url = match std::env::var("REDIS_URL") {
        Ok(u) if !u.trim().is_empty() => u,
        _ => {
            // Skipping locally is a convenience; skipping in CI would quietly
            // turn this whole file into a no-op, which is worse than a failure.
            assert!(
                std::env::var("CI").is_err(),
                "REDIS_URL must be set in CI: these tests are the only coverage of \
                 the rotation Lua script's atomicity and TTL behaviour, and passing \
                 them by skipping would be meaningless"
            );
            eprintln!(
                "SKIP: REDIS_URL is not set. These tests exercise the rotation Lua \
                 script against a real Redis; run e.g. \
                 `REDIS_URL=redis://127.0.0.1:6379 cargo test`."
            );
            return None;
        }
    };
    match redis::Client::open(url.as_str()) {
        Ok(c) => match c.get_multiplexed_async_connection().await {
            Ok(conn) => Some(conn),
            Err(e) => panic!("REDIS_URL is set but unreachable: {e}"),
        },
        Err(e) => panic!("REDIS_URL is not a valid Redis URL: {e}"),
    }
}

/// Runs the body only when Redis is available.
macro_rules! with_redis {
    ($conn:ident, $body:block) => {
        let Some(mut $conn) = connect().await else {
            return;
        };
        $body
    };
}

async fn store(conn: &mut redis::aio::MultiplexedConnection, sid: &str, s: &RefreshSession) {
    let payload = serde_json::to_string(s).unwrap();
    let _: () = redis::cmd("SET")
        .arg(session_key(sid))
        .arg(payload)
        .arg("EXAT")
        .arg(s.exp)
        .query_async(conn)
        .await
        .unwrap();
}

async fn load(conn: &mut redis::aio::MultiplexedConnection, sid: &str) -> Option<RefreshSession> {
    let raw: Option<String> = conn.get(session_key(sid)).await.unwrap();
    raw.map(|r| serde_json::from_str(&r).unwrap())
}

/// One rotation attempt, exactly as the handler performs it.
async fn rotate(
    conn: &mut redis::aio::MultiplexedConnection,
    k: &SessionKeys,
    sid: &str,
    presented_secret: &str,
    now: i64,
) -> (RotationReply, sessions::NewToken) {
    let next = sessions::generate_token(k, sid);
    let sealed = k.seal_secret(sid, &next.hash, &next.secret);
    let parts: Vec<redis::Value> = redis::Script::new(sessions::ROTATE_SCRIPT)
        .key(session_key(sid))
        .key(rotation_key(sid))
        .arg(k.hash_secret(presented_secret))
        .arg(now)
        .arg(GRACE)
        .arg(&next.hash)
        .arg(&sealed)
        .invoke_async(conn)
        .await
        .expect("rotation script must not error");
    (
        RotationReply::from_parts(parts).expect("reply must decode"),
        next,
    )
}

/// Fresh session plus the secret that currently opens it.
async fn seed(
    conn: &mut redis::aio::MultiplexedConnection,
    k: &SessionKeys,
    sid: &str,
    now: i64,
) -> String {
    let token = sessions::generate_token(k, sid);
    store(conn, sid, &new_session("user-1", token.hash.clone(), now)).await;
    token.secret
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn rotating_advances_the_generation_and_returns_the_user() {
    with_redis!(conn, {
        let (k, sid) = (keys(), unique_sid());
        let secret = seed(&mut conn, &k, &sid, NOW).await;

        let (reply, next) = rotate(&mut conn, &k, &sid, &secret, NOW).await;
        assert_eq!(reply.outcome, RotationOutcome::Rotated);
        assert_eq!(reply.user_id, "user-1");

        let stored = load(&mut conn, &sid).await.unwrap();
        assert_eq!(stored.rotation, 1);
        assert_eq!(stored.current_hash, next.hash);
        assert_eq!(
            stored.previous_hash.as_deref(),
            Some(k.hash_secret(&secret).as_str())
        );
        assert_eq!(stored.previous_expires_at, Some(NOW + GRACE));

        let _: () = conn.del(session_key(&sid)).await.unwrap();
    });
}

#[tokio::test]
async fn concurrent_refreshes_produce_exactly_one_winner() {
    with_redis!(conn, {
        let (k, sid) = (keys(), unique_sid());
        let secret = seed(&mut conn, &k, &sid, NOW).await;

        // Two tabs refreshing at the same instant with the same cookie.
        let mut c2 = connect().await.unwrap();
        let (a, b) = tokio::join!(
            rotate(&mut conn, &k, &sid, &secret, NOW),
            rotate(&mut c2, &k, &sid, &secret, NOW),
        );

        let mut outcomes = [a.0.outcome, b.0.outcome];
        outcomes.sort_by_key(|o| format!("{o:?}"));
        assert_eq!(
            outcomes,
            [RotationOutcome::Grace, RotationOutcome::Rotated],
            "exactly one refresh may rotate; the other must be treated as \
             concurrency, not as token theft"
        );

        // Only one generation advanced, despite two concurrent calls.
        assert_eq!(load(&mut conn, &sid).await.unwrap().rotation, 1);

        let _: () = conn.del(session_key(&sid)).await.unwrap();
    });
}

#[tokio::test]
async fn a_lost_rotation_response_is_recoverable() {
    // The scenario this whole grace/seal mechanism exists for: the rotation
    // succeeds, the response never reaches the browser, and the client retries
    // with the only token it still has.
    with_redis!(conn, {
        let (k, sid) = (keys(), unique_sid());
        let secret = seed(&mut conn, &k, &sid, NOW).await;

        let (first, winner) = rotate(&mut conn, &k, &sid, &secret, NOW).await;
        assert_eq!(first.outcome, RotationOutcome::Rotated);
        drop(winner); // response lost in transit

        // Retry with the old cookie.
        let (retry, _) = rotate(&mut conn, &k, &sid, &secret, NOW + 1).await;
        assert_eq!(retry.outcome, RotationOutcome::Grace);

        let recovered = k
            .open_secret(
                &sid,
                &retry.current_hash,
                retry.sealed_secret.as_ref().unwrap(),
            )
            .expect("the winner's secret must be recoverable inside the grace window");

        // And the recovered token really is live: the next refresh rotates.
        let (third, _) = rotate(&mut conn, &k, &sid, &recovered, NOW + 2).await;
        assert_eq!(
            third.outcome,
            RotationOutcome::Rotated,
            "the recovered token must be the current one, or the user is stranded"
        );

        let _: () = conn.del(session_key(&sid)).await.unwrap();
    });
}

#[tokio::test]
async fn recovery_degrades_to_access_only_once_the_seal_expires() {
    with_redis!(conn, {
        let (k, sid) = (keys(), unique_sid());
        let secret = seed(&mut conn, &k, &sid, NOW).await;

        let _ = rotate(&mut conn, &k, &sid, &secret, NOW).await;
        // Simulate the recovery key's TTL elapsing while the grace window,
        // which is clock-based, is still open.
        let _: () = conn.del(rotation_key(&sid)).await.unwrap();

        let (retry, _) = rotate(&mut conn, &k, &sid, &secret, NOW + 1).await;
        assert_eq!(retry.outcome, RotationOutcome::Grace);
        assert!(
            retry.sealed_secret.is_none(),
            "without a seal the handler must fall back to issuing an access token only"
        );

        let _: () = conn.del(session_key(&sid)).await.unwrap();
    });
}

#[tokio::test]
async fn a_sealed_secret_cannot_be_moved_between_sessions() {
    with_redis!(conn, {
        let (k, sid_a, sid_b) = (keys(), unique_sid(), unique_sid());
        let secret_a = seed(&mut conn, &k, &sid_a, NOW).await;
        let secret_b = seed(&mut conn, &k, &sid_b, NOW).await;

        let _ = rotate(&mut conn, &k, &sid_a, &secret_a, NOW).await;
        let _ = rotate(&mut conn, &k, &sid_b, &secret_b, NOW).await;

        // Splice A's ciphertext into B's recovery slot.
        let stolen: String = conn.get(rotation_key(&sid_a)).await.unwrap();
        let _: () = conn.set(rotation_key(&sid_b), &stolen).await.unwrap();

        let (retry, _) = rotate(&mut conn, &k, &sid_b, &secret_b, NOW + 1).await;
        assert_eq!(retry.outcome, RotationOutcome::Grace);
        assert!(
            k.open_secret(
                &sid_b,
                &retry.current_hash,
                retry.sealed_secret.as_ref().unwrap()
            )
            .is_none(),
            "AAD binds the ciphertext to its own session; it must not open here"
        );

        let _: () = conn.del(session_key(&sid_a)).await.unwrap();
        let _: () = conn.del(session_key(&sid_b)).await.unwrap();
    });
}

#[tokio::test]
async fn replaying_a_superseded_token_after_the_grace_window_revokes_the_session() {
    with_redis!(conn, {
        let (k, sid) = (keys(), unique_sid());
        let secret = seed(&mut conn, &k, &sid, NOW).await;

        let _ = rotate(&mut conn, &k, &sid, &secret, NOW).await;
        let (replay, _) = rotate(&mut conn, &k, &sid, &secret, NOW + GRACE + 1).await;

        assert_eq!(replay.outcome, RotationOutcome::ReuseDetected);
        assert!(
            load(&mut conn, &sid).await.is_none(),
            "reuse must revoke the whole session"
        );
        let leftover: Option<String> = conn.get(rotation_key(&sid)).await.unwrap();
        assert!(leftover.is_none(), "the recovery key must be revoked too");
    });
}

#[tokio::test]
async fn an_unknown_secret_never_revokes_the_session() {
    // `sid` is not secret, so revoking on any bad secret would be a
    // log-anyone-out DoS.
    with_redis!(conn, {
        let (k, sid) = (keys(), unique_sid());
        let secret = seed(&mut conn, &k, &sid, NOW).await;

        let (reply, _) = rotate(&mut conn, &k, &sid, "not-the-right-secret", NOW).await;
        assert_eq!(reply.outcome, RotationOutcome::UnknownHash);

        let survivor = load(&mut conn, &sid).await.expect("session must survive");
        assert_eq!(survivor.rotation, 0);

        // The genuine token still works afterwards.
        let (ok, _) = rotate(&mut conn, &k, &sid, &secret, NOW + 1).await;
        assert_eq!(ok.outcome, RotationOutcome::Rotated);

        let _: () = conn.del(session_key(&sid)).await.unwrap();
    });
}

#[tokio::test]
async fn rotation_never_extends_the_thirty_day_cap() {
    with_redis!(conn, {
        let (k, sid) = (keys(), unique_sid());
        // Anchor on real time: TTLs are measured against Redis's clock.
        let now = common::time::unix_now_secs();
        let secret = seed(&mut conn, &k, &sid, now).await;

        let before = load(&mut conn, &sid).await.unwrap().exp;
        let ttl_before: i64 = conn.ttl(session_key(&sid)).await.unwrap();

        // Rotate repeatedly, as a busy client would over a long-lived login.
        let mut secret = secret;
        for i in 1..=3 {
            let (reply, next) = rotate(&mut conn, &k, &sid, &secret, now + i).await;
            assert_eq!(reply.outcome, RotationOutcome::Rotated);
            assert_eq!(reply.exp, before, "exp must not move");
            secret = next.secret;
        }

        let after = load(&mut conn, &sid).await.unwrap();
        assert_eq!(after.exp, before);
        assert_eq!(after.rotation, 3);

        let ttl_after: i64 = conn.ttl(session_key(&sid)).await.unwrap();
        assert!(
            ttl_after <= ttl_before,
            "TTL must decay ({ttl_after} > {ttl_before}); EXAT, not EX"
        );

        let _: () = conn.del(session_key(&sid)).await.unwrap();
    });
}

#[tokio::test]
async fn an_expired_session_is_rejected_and_cleaned_up() {
    with_redis!(conn, {
        let (k, sid) = (keys(), unique_sid());
        let token = sessions::generate_token(&k, &sid);
        let now = common::time::unix_now_secs();

        // Past its hard cap, but still present in Redis.
        let mut s = new_session("user-1", token.hash.clone(), now);
        s.exp = now - 1;
        let payload = serde_json::to_string(&s).unwrap();
        let _: () = redis::cmd("SET")
            .arg(session_key(&sid))
            .arg(payload)
            .arg("EX")
            .arg(60)
            .query_async(&mut conn)
            .await
            .unwrap();

        let (reply, _) = rotate(&mut conn, &k, &sid, &token.secret, now).await;
        assert_eq!(reply.outcome, RotationOutcome::Expired);
        assert!(load(&mut conn, &sid).await.is_none());
    });
}

#[tokio::test]
async fn a_missing_session_reports_not_found() {
    with_redis!(conn, {
        let (k, sid) = (keys(), unique_sid());
        let (reply, _) = rotate(&mut conn, &k, &sid, "anything", NOW).await;
        assert_eq!(reply.outcome, RotationOutcome::NotFound);
    });
}

#[tokio::test]
async fn logout_makes_the_refresh_token_unusable() {
    with_redis!(conn, {
        let (k, sid) = (keys(), unique_sid());
        let secret = seed(&mut conn, &k, &sid, NOW).await;

        // What the logout handler does.
        let _: () = redis::pipe()
            .del(session_key(&sid))
            .del(rotation_key(&sid))
            .query_async(&mut conn)
            .await
            .unwrap();

        let (reply, _) = rotate(&mut conn, &k, &sid, &secret, NOW).await;
        assert_eq!(reply.outcome, RotationOutcome::NotFound);
    });
}

#[tokio::test]
async fn oauth_state_can_only_be_consumed_once() {
    with_redis!(conn, {
        let state = unique_sid();
        let key = format!("auth:oauth:discord:v1:{state}");
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg("ticket")
            .arg("NX")
            .arg("EX")
            .arg(300)
            .query_async(&mut conn)
            .await
            .unwrap();

        // Two callbacks racing on the same state value.
        let mut c2 = connect().await.unwrap();
        let (a, b): (
            redis::RedisResult<Option<String>>,
            redis::RedisResult<Option<String>>,
        ) = tokio::join!(async { conn.get_del(&key).await }, async {
            c2.get_del(&key).await
        },);

        let winners = [a.unwrap(), b.unwrap()].into_iter().flatten().count();
        assert_eq!(
            winners, 1,
            "GETDEL must hand the ticket to exactly one caller"
        );
    });
}

#[tokio::test]
async fn the_lua_script_agrees_with_the_rust_reference_implementation() {
    // `decide_rotation` is what the unit tests exercise; if it ever drifts from
    // the script, those tests would be verifying the wrong thing.
    with_redis!(conn, {
        let k = keys();

        for (label, mutate, presented, at) in [
            ("current", None::<fn(&mut RefreshSession)>, "current", 0),
            ("grace", None, "previous", 1),
            ("after grace", None, "previous", GRACE + 1),
            ("unknown", None, "bogus", 0),
        ] {
            let sid = unique_sid();
            let first = sessions::generate_token(&k, &sid);
            let mut s = new_session("user-1", first.hash.clone(), NOW);
            if let Some(f) = mutate {
                f(&mut s);
            }
            store(&mut conn, &sid, &s).await;

            // Establish a previous generation for the cases that need one.
            let (secret, base) = if presented == "previous" {
                let (_, next) = rotate(&mut conn, &k, &sid, &first.secret, NOW).await;
                drop(next);
                (first.secret.clone(), NOW)
            } else if presented == "bogus" {
                ("definitely-not-valid".to_string(), NOW)
            } else {
                (first.secret.clone(), NOW)
            };

            let expected = {
                let stored = load(&mut conn, &sid).await.unwrap();
                decide_rotation(&stored, &k.hash_secret(&secret), base + at)
            };
            let (reply, _) = rotate(&mut conn, &k, &sid, &secret, base + at).await;

            assert_eq!(
                reply.outcome, expected,
                "{label}: Lua and decide_rotation must agree"
            );

            let _: () = conn.del(session_key(&sid)).await.unwrap();
            let _: () = conn.del(rotation_key(&sid)).await.unwrap();
        }
    });
}
