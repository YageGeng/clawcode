//! Reads captured log lines and the Run correlation in tracing-subscriber's
//! default span-chain output.

use protocol::{RUN_ID_FIELD, TRACE_ID_FIELD};

/// Returns the first line containing `needle`, or the whole output as the error.
pub fn line_with(output: &str, needle: &str) -> Result<String, String> {
    output
        .lines()
        .find(|line| line.contains(needle))
        .map(str::to_string)
        .ok_or_else(|| format!("missing log line: {needle}\n{output}"))
}

/// Returns the last trace and the last Run declared by the `run{…}` spans of one line.
///
/// The default formatter prints the chain outermost first, for example
/// `run{trace_id=trace-a run_id=run-1}:run{run_id=run-2}: …`, so the last value
/// of each field is the one nearest to the event.
pub fn run_fields(line: &str) -> (Option<String>, Option<String>) {
    let trace_key = format!("{TRACE_ID_FIELD}=");
    let run_key = format!("{RUN_ID_FIELD}=");
    let mut trace = None;
    let mut run = None;
    for segment in line.split("run{").skip(1) {
        let fields = segment.split('}').next().unwrap_or_default();
        for field in fields.split(' ') {
            if let Some(value) = field.strip_prefix(&trace_key) {
                trace = Some(value.to_string());
            }
            if let Some(value) = field.strip_prefix(&run_key) {
                run = Some(value.to_string());
            }
        }
    }
    (trace, run)
}
