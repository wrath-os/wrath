//! How long a command's steps took, which `-v` prints when it ends, so a slow command says whether
//! it waited on the network or worked.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static START: OnceLock<Instant> = OnceLock::new();
static STEPS: Mutex<Vec<(&'static str, Instant)>> = Mutex::new(Vec::new());

/// Starts the clock, as the command starts.
pub fn start() {
    START.get_or_init(Instant::now);
}

/// Notes that `step`, such as `evaluating`, ended now: it took the time since the step before.
pub fn done(step: &'static str) {
    if let Ok(mut steps) = STEPS.lock() {
        steps.push((step, Instant::now()));
    }
}

/// Prints the steps on stderr with `-v`, once the command is over.
pub fn report() {
    if wrath_build::progress::mode() != wrath_build::progress::Mode::Verbose {
        return;
    }
    let Some(start) = START.get() else {
        return;
    };
    let steps = STEPS.lock().map(|steps| steps.clone()).unwrap_or_default();
    if steps.is_empty() {
        return;
    }
    eprintln!(
        "{}",
        summary(
            *start,
            &steps,
            Instant::now(),
            wrath_build::http::requests()
        )
    );
}

/// What took how long, from `start` to `end`, with what came after the last step as the rest.
fn summary(start: Instant, steps: &[(&str, Instant)], end: Instant, requests: usize) -> String {
    let mut parts = Vec::new();
    let mut since = start;
    for (step, at) in steps {
        parts.push(format!("{step} {}", seconds(at.duration_since(since))));
        since = *at;
    }
    let rest = end.duration_since(since);
    if rest >= Duration::from_millis(10) {
        parts.push(format!("the rest {}", seconds(rest)));
    }
    let asked = match requests {
        0 => String::new(),
        1 => ", 1 request over the network".to_owned(),
        n => format!(", {n} requests over the network"),
    };
    format!(
        "took {}: {}{asked}",
        seconds(end.duration_since(start)),
        parts.join(", ")
    )
}

fn seconds(duration: Duration) -> String {
    format!("{:.2}s", duration.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_step_takes_the_time_since_the_one_before() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let steps = [("fetching", at(300)), ("evaluating", at(700))];
        assert_eq!(
            summary(start, &steps, at(5800), 812),
            "took 5.80s: fetching 0.30s, evaluating 0.40s, the rest 5.10s, \
             812 requests over the network"
        );
        assert_eq!(
            summary(start, &steps, at(700), 1),
            "took 0.70s: fetching 0.30s, evaluating 0.40s, 1 request over the network"
        );
        assert_eq!(
            summary(start, &steps, at(704), 0),
            "took 0.70s: fetching 0.30s, evaluating 0.40s"
        );
    }
}
