use serde::Serialize;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::tools::workspace::WorkspaceError;

#[derive(Serialize)]
struct Bucket {
    started_ts_ms: u64,
    calls: u64,
    errors: u64,
}

pub(super) struct UsageStatistics {
    period: String,
    interval_ms: u64,
    start_ts_ms: u64,
    until_ts_ms: u64,
    buckets: Vec<Bucket>,
}

impl UsageStatistics {
    pub(super) fn from_args(args: &Value) -> Result<Option<Self>, WorkspaceError> {
        let Some(period) = args
            .get("statistics_period")
            .filter(|value| !value.is_null())
        else {
            return Ok(None);
        };
        let (interval_ms, count) = match period.as_str() {
            Some("minute") => (60_000, 60),
            Some("hour") => (3_600_000, 24),
            Some("day") => (86_400_000, 30),
            _ => return Err(invalid_statistics("Unsupported statistics period.")),
        };
        let until_ts_ms = match args
            .get("statistics_until_ts_ms")
            .filter(|value| !value.is_null())
        {
            Some(value) => value
                .as_u64()
                .filter(|value| *value > 0 && *value <= 8_640_000_000_000_000)
                .ok_or_else(|| invalid_statistics("Invalid statistics cutoff timestamp."))?,
            None => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        };
        let last = (until_ts_ms - 1) / interval_ms * interval_ms;
        let start_ts_ms = last.saturating_sub((count - 1) * interval_ms);
        let bucket_count = count.min(last / interval_ms + 1);
        Ok(Some(Self {
            period: period.as_str().unwrap().to_string(),
            interval_ms,
            start_ts_ms,
            until_ts_ms,
            buckets: (0..bucket_count)
                .map(|index| Bucket {
                    started_ts_ms: start_ts_ms + index * interval_ms,
                    calls: 0,
                    errors: 0,
                })
                .collect(),
        }))
    }

    pub(super) fn includes(&self, record: &Value) -> bool {
        record
            .get("started_ts_ms")
            .and_then(Value::as_u64)
            .is_some_and(|timestamp| timestamp >= self.start_ts_ms && timestamp < self.until_ts_ms)
    }

    pub(super) fn accumulate(&mut self, record: &Value, is_error: bool) {
        let timestamp = record["started_ts_ms"].as_u64().unwrap();
        let index = ((timestamp - self.start_ts_ms) / self.interval_ms) as usize;
        self.buckets[index].calls += 1;
        self.buckets[index].errors += u64::from(is_error);
    }

    pub(super) fn report(self) -> Value {
        json!({ "period": self.period, "interval_ms": self.interval_ms, "start_ts_ms": self.start_ts_ms,
            "until_ts_ms": self.until_ts_ms, "buckets": self.buckets })
    }
}

fn invalid_statistics(message: &str) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "INVALID_QUERY",
        message: message.to_string(),
        category: "validation",
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_statistics_cover_all_periods_and_boundaries() {
        let until: u64 = 1_769_862_870_000;
        for (period, interval, count) in [
            ("minute", 60_000, 60),
            ("hour", 3_600_000, 24),
            ("day", 86_400_000, 30),
        ] {
            let mut statistics = UsageStatistics::from_args(&json!({
                "statistics_period": period, "statistics_until_ts_ms": until,
            }))
            .unwrap()
            .unwrap();
            let start = (until - 1) / interval * interval - (count - 1) * interval;
            for timestamp in [start, start + interval, until - 1] {
                let record = json!({ "started_ts_ms": timestamp });
                assert!(statistics.includes(&record));
                statistics.accumulate(&record, timestamp == start + interval);
            }
            for timestamp in [
                json!(start - 1),
                json!(until),
                json!(-1),
                Value::Null,
                json!("invalid"),
            ] {
                assert!(!statistics.includes(&json!({ "started_ts_ms": timestamp })));
            }
            let report = statistics.report();
            assert_eq!(report["buckets"].as_array().unwrap().len(), count as usize);
            assert_eq!(report["buckets"][0]["calls"], 1);
            assert_eq!(report["buckets"][1]["errors"], 1);
            assert_eq!(report["buckets"][2]["calls"], 0);
            assert_eq!(report["buckets"][count as usize - 1]["calls"], 1);
        }
    }

    #[test]
    fn statistics_validate_options_and_leave_legacy_queries_opted_out() {
        assert!(UsageStatistics::from_args(&json!({})).unwrap().is_none());
        for args in [
            json!({ "statistics_period": "week" }),
            json!({ "statistics_period": "minute", "statistics_until_ts_ms": -1 }),
            json!({ "statistics_period": "minute", "statistics_until_ts_ms": "100" }),
        ] {
            assert!(UsageStatistics::from_args(&args).is_err());
        }
        let statistics = UsageStatistics::from_args(
            &json!({ "statistics_period": "day", "statistics_until_ts_ms": 1 }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(statistics.report()["buckets"].as_array().unwrap().len(), 1);
    }
}
