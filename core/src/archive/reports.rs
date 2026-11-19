//! Reports: the manual report and the morning report.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{event, task_brief, Mode, Model, MorningJob, Prepared};
use crate::ai::prompt::require_purpose;
use crate::analysis::batch::fit_prefix;
use crate::{exec, time, Ctx, Error, Result};

/// Only tasks and changes can be split between report calls.
pub(crate) fn row_count(payload: &Value) -> usize {
    ["report_tasks", "delta"].into_iter().filter_map(|key| payload[key].as_array()).map(Vec::len).sum()
}

/// What the report model reads: the selected tasks and their new changes.
/// It cannot read messages, so it gets no message ids.
pub fn request(r: &Value, mode: Mode) -> Prepared {
    let rows = |section: &str| r[section].as_array().map_or(&[][..], Vec::as_slice);
    let tasks: Vec<Value> = rows("tasks").iter().map(task_brief).collect();
    let delta: Vec<Value> = rows("delta").iter().map(event).collect();
    Prepared {
        purpose: "report",
        payload: json!({
            "report_tasks": tasks, "delta": delta, "label": r["label"], "time": time::stamp(time::now()),
            "_trigger": if mode == Mode::Manual { "manual_report" } else { "scheduled_report" },
        }),
    }
}

/// Map the model result onto the report.
pub fn apply(r: &mut Value, result: Result<Value>) {
    match result {
        Ok(v) => {
            r["ai_summary"] = v;
            r["summary_status"] = json!("done");
            r.as_object_mut().map(|o| o.remove("summary_error"));
        }
        Err(e) if e.0 == exec::CANCELLED => {
            r["summary_status"] = json!("interrupted");
            r["summary_error"] = json!(e.0);
        }
        Err(e) => {
            r["summary_status"] = json!("failed");
            r["summary_error"] = json!(e.0);
        }
    }
}

async fn summarize(ctx: &Ctx, model: &dyn Model, mut r: Value, mode: Mode) -> Result<Value> {
    let Some(_release) = exec::Flag::claim(&ctx.archive.summary_busy) else { return Ok(r) };
    let result = async {
        super::archive_blocks(ctx).await?;
        let config = ctx.config.get();
        let limit = config.max_input_chars;
        let timeout = Duration::from_secs(config.ai_timeout_seconds.max(1));
        let mut base = {
            let measured = model.measure(request(&r, mode))?;
            if measured.total <= limit || row_count(&measured.request.payload) <= 1 {
                return super::ask(ctx, model, measured, timeout).await;
            }
            measured.request
        };
        let started = Instant::now();
        let all_tasks = base.payload["report_tasks"].take();
        let mut all_delta = base.payload["delta"].take();
        let mut tasks = all_tasks.as_array().unwrap().as_slice();
        let delta = all_delta.as_array_mut().unwrap();
        let titles: HashMap<_, _> = tasks.iter().map(|t| (t["id"].as_str().unwrap(), &t["title"])).collect();
        for e in delta.iter_mut() {
            if let Some(title) = e["task_id"].as_str().and_then(|id| titles.get(id)) {
                e["task_title"] = (*title).clone();
            }
        }
        let mut delta = delta.as_slice();
        loop {
            let remaining = tasks.len() + delta.len();
            let measure = |n: usize| {
                let t = n.min(tasks.len());
                let mut request = base.clone();
                request.payload["report_tasks"] = json!(&tasks[..t]);
                request.payload["delta"] = json!(&delta[..n - t]);
                model.measure(request)
            };
            let (n, request) = fit_prefix(remaining, |n| {
                let m = measure(n)?;
                Ok((n == 1 || m.total <= limit).then_some(m))
            })?
            .unwrap();
            let left = timeout.checked_sub(started.elapsed()).ok_or_else(|| Error::new("报表生成超过总时限"))?;
            let summary = super::ask(ctx, model, request, left).await?;
            if n == remaining {
                return Ok(summary);
            }
            base.payload["previous_summary"] = summary;
            let t = n.min(tasks.len());
            tasks = &tasks[t..];
            delta = &delta[n - t..];
        }
    }
    .await;
    if result.as_ref().is_err_and(|e| e.0 == crate::ai::DEFERRED) {
        return Ok(r);
    }
    apply(&mut r, result);
    ctx.db.finish_report(r).await
}

/// A report with its summary attempt; failures stay in the report.
pub async fn report_result(ctx: &Ctx, model: &dyn Model, label: &str, mode: Mode) -> Result<Value> {
    require_purpose(&ctx.config.get(), "report")?;
    let r = ctx.db.prepare_report(label.to_string(), time::now()).await?;
    if r["summary_status"] == "pending" {
        return summarize(ctx, model, r, mode).await;
    }
    Ok(r)
}

/// `report`: an interrupted summary is an error, other failures are shown
/// in the report.
pub async fn report(ctx: &Ctx, model: &dyn Model, label: &str, mode: Mode) -> Result<Value> {
    let r = report_result(ctx, model, label, mode).await?;
    if r["summary_error"] == exec::CANCELLED {
        return Err(Error::new(exec::CANCELLED));
    }
    Ok(r)
}

/// Run the first queued morning job once; failures are not retried.
pub async fn morning_tick(ctx: &Arc<Ctx>, model: &dyn Model) -> Result<Option<MorningJob>> {
    if ctx.exec.ai_busy() {
        return Ok(None);
    }
    let job = ctx.db.claim_morning_job().await?;
    let Some(mut job) = job else {
        return Ok(None);
    };
    let result = report_result(ctx, model, "早报", Mode::Automatic).await;
    let (status, error, finished) = match &result {
        Ok(r) if ["done", "reused", "fallback"].contains(&r["summary_status"].as_str().unwrap_or("")) => {
            ("succeeded", String::new(), r["finished"].as_i64().unwrap_or(0))
        }
        Ok(r) => (
            "failed",
            r["summary_error"]
                .as_str()
                .filter(|e| !e.is_empty())
                .unwrap_or("早报已生成，但汇总未完成；不会自动重试")
                .to_string(),
            r["finished"].as_i64().unwrap_or(0),
        ),
        Err(e) => ("failed", e.0.clone(), 0),
    };
    job.status = status.into();
    job.error = error;
    let job = ctx.db.finish_morning_job(job, finished).await?;
    Ok(Some(job))
}
