use super::*;
use crate::analytics::{bytes_bucket, count_bucket};
use serde_json::json;

pub(super) fn count(p: &mut Map<String, Value>, key: &str, n: Option<u64>) {
    if let Some(n) = n {
        p.insert(key.into(), json!(count_bucket(n).as_str()));
    }
}
pub(super) fn timings(p: &mut Map<String, Value>, total: Duration, times: ProductTimings) {
    p.insert(
        "native_total_duration_bucket".into(),
        json!(native_duration_bucket(total)),
    );
    for (key, value) in [
        ("prepare_duration_bucket", times.prepare),
        ("work_duration_bucket", times.work),
        ("output_duration_bucket", times.output),
    ] {
        if let Some(value) = value {
            p.insert(key.into(), json!(native_duration_bucket(value)));
        }
    }
}
pub(super) fn completion(c: ProductCompletion) -> Map<String, Value> {
    let mut p = Map::new();
    timings(&mut p, c.elapsed, c.timings);
    p.insert(
        "execution_result".into(),
        json!(if c.execution.is_ok() {
            "success"
        } else {
            "failure"
        }),
    );
    p.insert("output_delivery".into(), json!(c.delivery.as_str()));
    p.insert("output_kind".into(), json!(c.output.as_str()));
    let failure = c.execution.err().or_else(|| {
        (c.delivery == DeliveryEvidence::Failed).then_some(ProductFailure {
            stage: ProductFailureStage::Output,
            class: ProductFailureClass::Io,
        })
    });
    if let Some(failure) = failure {
        insert_failure(&mut p, failure);
    }
    p
}
fn insert_failure(p: &mut Map<String, Value>, f: ProductFailure) {
    p.insert("product_failure_stage".into(), json!(f.stage.as_str()));
    p.insert("product_failure_class".into(), json!(f.class.as_str()));
}
pub(super) fn result(p: &mut Map<String, Value>, result: Option<ProductResultFacts>) {
    if let Some(result) = result {
        count(p, "result_count_bucket", Some(result.count));
        p.insert("result_empty".into(), json!(result.count == 0));
        if let Some(truncated) = result.truncated {
            p.insert("result_truncated".into(), json!(truncated));
        }
    }
}
pub(super) fn graph(e: &GraphCompletedV1) -> EngineWire {
    let mut p = completion(e.completion);
    p.insert("graph_operation".into(), json!(e.operation.as_str()));
    result(&mut p, e.result);
    e.details.insert(&mut p);
    count(&mut p, "graph_node_count_bucket", e.nodes);
    count(&mut p, "graph_edge_count_bucket", e.edges);
    count(&mut p, "graph_files_processed_bucket", e.files_processed);
    (
        "operation_completed",
        match e.surface {
            GraphSurface::Cli => Surface::Cli,
            GraphSurface::Mcp => Surface::Mcp,
        },
        "graph",
        e.completion.outcome(),
        duration_bucket(e.completion.elapsed),
        p,
    )
}
pub(super) fn server(e: &ServerRequestCompletedV1) -> EngineWire {
    let mut p = completion(e.completion);
    p.insert("server_operation".into(), json!(e.operation.as_str()));
    p.insert("response_class".into(), json!(e.response_class.as_str()));
    p.insert(
        "response_handoff".into(),
        json!(e.response_handoff.as_str()),
    );
    result(&mut p, e.result);
    (
        "operation_completed",
        Surface::Server,
        "server_request",
        e.completion.outcome(),
        duration_bucket(e.completion.elapsed),
        p,
    )
}
pub(super) fn runtime(e: &ProductRuntimeV1) -> EngineWire {
    let mut p = Map::new();
    p.insert("runtime_kind".into(), json!(e.kind.as_str()));
    p.insert("runtime_phase".into(), json!(e.phase.as_str()));
    p.insert(
        "uptime_bucket".into(),
        json!(match e.uptime.as_secs() {
            0..=3599 => "lt_1h",
            3600..=86399 => "1h-1d",
            86400..=604799 => "1d-7d",
            _ => "7d+",
        }),
    );
    count(&mut p, "active_requests_bucket", e.active_requests);
    count(&mut p, "pending_work_bucket", e.pending_work);
    if let Some(f) = e.failure {
        insert_failure(&mut p, f);
    }
    let surface = match e.kind {
        ProductRuntimeKind::Server => Surface::Server,
        ProductRuntimeKind::GraphServe => Surface::Mcp,
        ProductRuntimeKind::GraphWatch => Surface::Cli,
    };
    (
        "runtime_observation",
        surface,
        "product_runtime",
        if e.failure.is_some() {
            Outcome::Failure
        } else {
            Outcome::Success
        },
        duration_bucket(e.uptime),
        p,
    )
}
pub(super) fn pair(p: &mut Map<String, Value>, unit: &str, v: PresentedTotals) {
    count(
        p,
        &format!("sift_{unit}_measured_count_bucket"),
        Some(v.samples),
    );
    for (label, n) in [
        ("input", v.input),
        ("output", v.output),
        ("delta", v.input.abs_diff(v.output)),
    ] {
        let bucket = if unit == "bytes" {
            bytes_bucket(n).as_str()
        } else {
            count_bucket(n).as_str()
        };
        p.insert(format!("sift_{unit}_{label}_bucket"), json!(bucket));
    }
    p.insert(
        format!("sift_{unit}_change"),
        json!(if v.output > v.input {
            "increased"
        } else if v.output < v.input {
            "reduced"
        } else {
            "unchanged"
        }),
    );
    if v.input != 0 {
        let fraction = if v.output > v.input {
            "increased"
        } else if v.output == v.input {
            "unchanged"
        } else if v.output == 0 {
            "100pct"
        } else {
            let pct = u128::from(v.input - v.output) * 100 / u128::from(v.input);
            match pct {
                0..=9 => "lt_10pct",
                10..=24 => "10pct-25pct",
                25..=49 => "25pct-50pct",
                50..=74 => "50pct-75pct",
                75..=89 => "75pct-90pct",
                _ => "90pct-100pct",
            }
        };
        p.insert(
            format!("sift_{unit}_savings_fraction_bucket"),
            json!(fraction),
        );
    }
}
