CREATE MATERIALIZED VIEW heart_rate_1m
WITH (timescaledb.continuous) AS
SELECT
    user_id,
    time_bucket(INTERVAL '1 minute', recorded_at) AS bucket,
    AVG(bpm)::FLOAT8                               AS avg_bpm,
    MIN(bpm)                                        AS min_bpm,
    MAX(bpm)                                        AS max_bpm,
    COUNT(*)::BIGINT                                AS sample_count
FROM heart_rate_records
GROUP BY user_id, bucket
WITH NO DATA;

SELECT add_continuous_aggregate_policy('heart_rate_1m',
    start_offset    => INTERVAL '10 minutes',
    end_offset      => INTERVAL '1 minute',
    schedule_interval => INTERVAL '1 minute'
);

CALL refresh_continuous_aggregate('heart_rate_1m', NULL, NULL);
