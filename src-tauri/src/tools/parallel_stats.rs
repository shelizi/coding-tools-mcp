use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::BufRead;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

const TOOL_USAGE_LOG_FILE: &str = "mcp-tool-usage.jsonl";
const RECENT_HISTORY_FILES: usize = 2;
const MIN_CONFIDENT_SAMPLES: u64 = 5;
const SAFE_LOWER_BOUND: f64 = 0.70;
const SERIALIZATION_RATE_LIMIT: f64 = 0.60;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ParallelPairStats {
    pub attempts: u64,
    pub successes: u64,
    pub conflicts: u64,
    pub serialized: u64,
    pub failures: u64,
    pub not_overlapped: u64,
    pub overlap_ms: u128,
    pub lock_wait_ms: u128,
}

impl ParallelPairStats {
    pub(crate) fn safety_samples(&self) -> u64 {
        self.successes.saturating_add(self.conflicts)
    }

    pub(crate) fn serialization_rate(&self) -> f64 {
        ratio(self.serialized, self.attempts)
    }

    pub(crate) fn conflict_rate(&self) -> f64 {
        ratio(self.conflicts, self.safety_samples())
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ParallelHistory {
    pub pairs: BTreeMap<String, ParallelPairStats>,
    pub total_observations: u64,
}

impl ParallelHistory {
    pub(crate) fn accumulate_record(&mut self, record: &Value) {
        let Some(observations) = record
            .get("parallelism_observations")
            .and_then(Value::as_array)
        else {
            return;
        };
        self.apply_observations(observations);
    }

    pub(crate) fn apply_observations(&mut self, observations: &[Value]) {
        for observation in observations {
            self.apply_observation(observation);
        }
    }

    fn apply_observation(&mut self, observation: &Value) {
        let Some(pair) = observation
            .get("pair")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|pair| !pair.is_empty() && pair.len() <= 320)
        else {
            return;
        };
        let Some(outcome) = observation
            .get("outcome")
            .and_then(Value::as_str)
            .filter(|outcome| {
                matches!(
                    *outcome,
                    "success" | "conflict" | "serialized" | "failure" | "not_overlapped"
                )
            })
        else {
            return;
        };
        let stats = self.pairs.entry(pair.to_string()).or_default();
        stats.attempts = stats.attempts.saturating_add(1);
        stats.overlap_ms = stats.overlap_ms.saturating_add(
            observation
                .get("overlap_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u128,
        );
        stats.lock_wait_ms = stats.lock_wait_ms.saturating_add(
            observation
                .get("lock_wait_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u128,
        );
        match outcome {
            "success" => stats.successes = stats.successes.saturating_add(1),
            "conflict" => stats.conflicts = stats.conflicts.saturating_add(1),
            "serialized" => stats.serialized = stats.serialized.saturating_add(1),
            "failure" => stats.failures = stats.failures.saturating_add(1),
            "not_overlapped" => stats.not_overlapped = stats.not_overlapped.saturating_add(1),
            _ => unreachable!("outcome validated above"),
        }
        self.total_observations = self.total_observations.saturating_add(1);
    }
}

static PARALLEL_HISTORY_CACHE: OnceLock<Mutex<HashMap<String, ParallelHistory>>> = OnceLock::new();

pub(crate) fn parallel_pair_history(
    profile_id: &str,
    pair_keys: &[String],
) -> BTreeMap<String, ParallelPairStats> {
    let cache = PARALLEL_HISTORY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let history = cache
        .entry(profile_id.to_string())
        .or_insert_with(|| load_recent_history(profile_id));
    pair_keys
        .iter()
        .filter_map(|pair| {
            history
                .pairs
                .get(pair)
                .cloned()
                .map(|stats| (pair.clone(), stats))
        })
        .collect()
}

pub(crate) fn record_parallel_observations(profile_id: &str, observations: &[Value]) {
    if observations.is_empty() {
        return;
    }
    let cache = PARALLEL_HISTORY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .entry(profile_id.to_string())
        .or_insert_with(|| load_recent_history(profile_id))
        .apply_observations(observations);
}

pub(crate) fn parallel_safety_lower_bound(stats: &ParallelPairStats) -> f64 {
    let samples = stats.safety_samples();
    if samples == 0 {
        return 0.0;
    }
    // One-sided 80% Wilson lower confidence bound. This is deliberately
    // conservative for small samples while remaining easy to explain.
    let z = 1.281_551_565_544_600_4_f64;
    let n = samples as f64;
    let p = stats.successes as f64 / n;
    let z2 = z * z;
    let center = p + z2 / (2.0 * n);
    let margin = z * ((p * (1.0 - p) + z2 / (4.0 * n)) / n).sqrt();
    ((center - margin) / (1.0 + z2 / n)).clamp(0.0, 1.0)
}

pub(crate) fn parallelism_report(history: &ParallelHistory, top: usize) -> Value {
    let mut pairs = history
        .pairs
        .iter()
        .map(|(pair, stats)| {
            let lower_bound = parallel_safety_lower_bound(stats);
            let confident_safe = stats.safety_samples() >= MIN_CONFIDENT_SAMPLES
                && lower_bound >= SAFE_LOWER_BOUND
                && stats.conflicts < 2;
            let conflict_prone = stats.conflicts >= 2
                || (stats.safety_samples() >= 3 && stats.conflict_rate() >= 0.10);
            let serialization_prone =
                stats.attempts >= 3 && stats.serialization_rate() >= SERIALIZATION_RATE_LIMIT;
            json!({
                "pair": pair,
                "attempts": stats.attempts,
                "successes": stats.successes,
                "conflicts": stats.conflicts,
                "serialized": stats.serialized,
                "failures": stats.failures,
                "not_overlapped": stats.not_overlapped,
                "safety_samples": stats.safety_samples(),
                "success_rate": rounded_ratio(stats.successes, stats.safety_samples()),
                "conflict_rate": rounded_ratio(stats.conflicts, stats.safety_samples()),
                "serialization_rate": rounded_ratio(stats.serialized, stats.attempts),
                "safety_lower_bound_80": round3(lower_bound),
                "overlap_ms": stats.overlap_ms,
                "lock_wait_ms": stats.lock_wait_ms,
                "confident_safe": confident_safe,
                "conflict_prone": conflict_prone,
                "serialization_prone": serialization_prone
            })
        })
        .collect::<Vec<_>>();
    pairs.sort_by(|left, right| {
        right["conflicts"]
            .as_u64()
            .cmp(&left["conflicts"].as_u64())
            .then_with(|| right["attempts"].as_u64().cmp(&left["attempts"].as_u64()))
            .then_with(|| left["pair"].as_str().cmp(&right["pair"].as_str()))
    });
    pairs.truncate(top);

    let confident_safe_pairs = history
        .pairs
        .values()
        .filter(|stats| {
            stats.safety_samples() >= MIN_CONFIDENT_SAMPLES
                && parallel_safety_lower_bound(stats) >= SAFE_LOWER_BOUND
                && stats.conflicts < 2
        })
        .count();
    let conflict_pairs = history
        .pairs
        .values()
        .filter(|stats| {
            stats.conflicts >= 2 || (stats.safety_samples() >= 3 && stats.conflict_rate() >= 0.10)
        })
        .count();
    let serialized_pairs = history
        .pairs
        .values()
        .filter(|stats| {
            stats.attempts >= 3 && stats.serialization_rate() >= SERIALIZATION_RATE_LIMIT
        })
        .count();

    let mut recommendations = Vec::<String>::new();
    if conflict_pairs > 0 {
        recommendations.push(format!(
            "Keep {conflict_pairs} conflict-prone command pair(s) sequential or assign an explicit shared lock_group."
        ));
    }
    if serialized_pairs > 0 {
        recommendations.push(format!(
            "Reduce max_parallel or separate {serialized_pairs} pair(s) that mostly wait on the same resource lock."
        ));
    }
    if confident_safe_pairs > 0 {
        recommendations.push(format!(
            "The LLM may batch {confident_safe_pairs} statistically supported pair(s) with exec_many mode=auto."
        ));
    }
    if history.total_observations == 0 {
        recommendations.push(
            "Collect exec_many observations before allowing evidence-required command pairs to run in parallel."
                .into(),
        );
    }

    json!({
        "decision_method": "hard_rules_plus_wilson_statistics",
        "machine_learning_enabled": false,
        "minimum_confident_samples": MIN_CONFIDENT_SAMPLES,
        "safe_lower_bound_threshold": SAFE_LOWER_BOUND,
        "observed_pairs": history.pairs.len(),
        "total_observations": history.total_observations,
        "confident_safe_pairs": confident_safe_pairs,
        "conflict_pairs": conflict_pairs,
        "serialization_prone_pairs": serialized_pairs,
        "pairs": pairs,
        "llm_recommendations": recommendations,
        "future_model_note": "Consider a contextual bandit only after stable command signatures, explicit conflict labels, and enough observations per context are available."
    })
}

fn load_recent_history(profile_id: &str) -> ParallelHistory {
    let log_dir = crate::tunnel::log_dir_for_profile(profile_id);
    let paths = [
        log_dir.join(format!("{TOOL_USAGE_LOG_FILE}.1")),
        log_dir.join(TOOL_USAGE_LOG_FILE),
    ];
    let mut history = ParallelHistory::default();
    for path in paths.into_iter().take(RECENT_HISTORY_FILES) {
        let _ = visit_records(&path, |record| history.accumulate_record(&record));
    }
    history
}

fn visit_records<F>(path: &Path, mut visit: F) -> std::io::Result<()>
where
    F: FnMut(Value),
{
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut reader = std::io::BufReader::new(file);
    let mut buffer = Vec::with_capacity(8 * 1024);
    loop {
        buffer.clear();
        if reader.read_until(b'\n', &mut buffer)? == 0 {
            break;
        }
        if !buffer.ends_with(b"\n") {
            break;
        }
        while buffer
            .last()
            .is_some_and(|byte| matches!(*byte, b'\n' | b'\r'))
        {
            buffer.pop();
        }
        if let Ok(record) = serde_json::from_slice::<Value>(&buffer) {
            visit(record);
        }
    }
    Ok(())
}

fn ratio(value: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        value as f64 / total as f64
    }
}

fn rounded_ratio(value: u64, total: u64) -> f64 {
    round3(ratio(value, total))
}

fn round3(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observations_are_aggregated_without_raw_command_data() {
        let mut history = ParallelHistory::default();
        history.apply_observations(&[
            json!({"pair":"cargo:test@a|node:test@b","outcome":"success","overlap_ms":500,"lock_wait_ms":0}),
            json!({"pair":"cargo:test@a|node:test@b","outcome":"conflict","overlap_ms":25,"lock_wait_ms":10}),
            json!({"pair":"cargo:test@a|git:status@a","outcome":"serialized","overlap_ms":0,"lock_wait_ms":200}),
        ]);
        let stats = &history.pairs["cargo:test@a|node:test@b"];
        assert_eq!(stats.attempts, 2);
        assert_eq!(stats.successes, 1);
        assert_eq!(stats.conflicts, 1);
        assert_eq!(stats.overlap_ms, 525);
        assert_eq!(stats.lock_wait_ms, 10);
        assert_eq!(history.total_observations, 3);
        history.apply_observations(&[json!({
            "pair":"ignored|pair",
            "outcome":"future-unknown-label"
        })]);
        assert!(!history.pairs.contains_key("ignored|pair"));
        assert_eq!(history.total_observations, 3);
    }

    #[test]
    fn wilson_bound_requires_repeated_success() {
        let three = ParallelPairStats {
            attempts: 3,
            successes: 3,
            ..Default::default()
        };
        let five = ParallelPairStats {
            attempts: 5,
            successes: 5,
            ..Default::default()
        };
        assert!(parallel_safety_lower_bound(&three) < SAFE_LOWER_BOUND);
        assert!(parallel_safety_lower_bound(&five) > SAFE_LOWER_BOUND);
    }

    #[test]
    fn report_surfaces_safe_conflicting_and_serialized_pairs() {
        let mut history = ParallelHistory::default();
        for _ in 0..5 {
            history.apply_observations(&[json!({
                "pair":"python:version@a|node:version@b",
                "outcome":"success"
            })]);
        }
        for _ in 0..2 {
            history.apply_observations(&[json!({
                "pair":"git:commit@a|git:commit@a",
                "outcome":"conflict"
            })]);
        }
        for _ in 0..3 {
            history.apply_observations(&[json!({
                "pair":"cargo:test@a|cargo:check@a",
                "outcome":"serialized",
                "lock_wait_ms":100
            })]);
        }
        let report = parallelism_report(&history, 20);
        assert_eq!(report["machine_learning_enabled"], false);
        assert_eq!(report["confident_safe_pairs"], 1);
        assert_eq!(report["conflict_pairs"], 1);
        assert_eq!(report["serialization_prone_pairs"], 1);
        assert_eq!(report["llm_recommendations"].as_array().unwrap().len(), 3);
    }
}
