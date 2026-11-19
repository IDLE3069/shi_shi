//! The AI call layer behind [`super::Model`]: archive and report requests
//! are rendered once with the adopted policy ([`invoke::render`]), measured
//! exactly as the final check measures them, and the chosen render is sent
//! through [`invoke::call`] on the slot `archive::ask` already holds.
//! Archive requests read their frozen records through [`archive_context`].
//!
//! The mode comes from the payload's `_trigger`: `manual_*` is manual,
//! `scheduled_report` automatic, every other archive trigger (`work_end`)
//! nightly with its `_archive_attempt_id` claim.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use shishi_api::{ToolFailure, ToolFailureKind, ToolResult};

use super::{Measured, Model, ModelFuture, Prepared};
use crate::ai::invoke::{self, Invocation};
use crate::ai::prompt::Policies;
use crate::ai::provider::Context;
use crate::ai::Mode;
use crate::analysis::context::{self, Meter};
use crate::analysis::layer::{attach_result, fits, progress};
use crate::exec::Cancel;
use crate::{Ctx, Db, Error, Result};

const PURPOSES: [&str; 2] = ["archive", "report"];

pub struct Layer {
    ctx: Arc<Ctx>,
    policies: BTreeMap<&'static str, Policies>,
}

/// The model of the archive domain, with the policies read once; an error
/// only when the adopted policies cannot be read.
pub async fn model(ctx: &Arc<Ctx>) -> Result<Arc<dyn Model>> {
    let policies = ctx.db.ai_policies(&PURPOSES).await?;
    Ok(Arc::new(Layer { ctx: ctx.clone(), policies }))
}

pub fn mode(purpose: &str, payload: &Value) -> Mode {
    let trigger = payload["_trigger"].as_str().unwrap_or("");
    if trigger.starts_with("manual") {
        Mode::Manual
    } else if purpose == "archive" {
        Mode::Nightly
    } else {
        Mode::Automatic
    }
}

impl Model for Layer {
    fn measure(&self, request: Prepared) -> Result<Measured<Prepared>> {
        let kind = request.purpose;
        let policies = self.policies.get(kind).ok_or_else(|| Error::new("未知请求"))?;
        let trigger = request.payload["_trigger"].as_str().unwrap_or("").to_string();
        let mode = mode(kind, &request.payload);
        let invocation =
            invoke::render(self.ctx.config.get(), policies, kind, mode, &trigger, request.payload.clone())?;
        Ok(Measured::rendered(request, invocation.prepared.total, invocation))
    }

    fn invoke<'a>(&'a self, mut request: Measured<Prepared>, cancel: Cancel) -> ModelFuture<'a> {
        Box::pin(async move {
            let mut invocation: Invocation = request.take()?;
            invocation.context = archive_context(&self.ctx.db, &invocation.kind, &invocation.payload);
            invoke::call(&self.ctx, invocation, &cancel).await
        })
    }
}

/// Cursor tokens stay inside one model request; the model never needs a message id.
#[derive(Clone, Debug)]
pub(crate) struct HistoryPage {
    pub number: i64,
    pub updates: bool,
    pub since: i64,
    pub until: i64,
    pub at: i64,
    pub key: i64,
    pub offset: usize,
}

struct ArchiveContext {
    db: Db,
    tasks: BTreeMap<i64, (String, i64)>,
    cursors: BTreeMap<usize, HistoryPage>,
    snapshot: Value,
    meter: Meter,
}

/// The retrieval context of archive requests: they read their frozen records
/// and may replace the archived memory.
pub fn archive_context(db: &Db, kind: &str, payload: &Value) -> Option<Box<dyn Context>> {
    if kind != "archive" || !payload["_local_context"].is_object() {
        return None;
    }
    let mut snapshot = payload["_local_context"].clone();
    let before = payload["_archived_memory"].as_str().unwrap_or("").to_string();
    snapshot["archived_memory"]["before"] = json!(before);
    snapshot["archived_memory"]["writable"] = json!(true);
    // The prompt shows the archived memory segment trimmed.
    if !before.trim().is_empty() {
        snapshot["archived_memory"]["inline_text"] = json!(before);
        snapshot["records"] = json!([{"id":"archived-memory","text":before}]);
    }
    let tasks = ["report_tasks", "current_tasks"]
        .into_iter()
        .flat_map(|k| payload[k].as_array().into_iter().flatten())
        .filter_map(|t| Some((t["number"].as_i64()?, (t["id"].as_str()?.to_string(), t["updated"].as_i64()?))))
        .collect();
    Some(Box::new(ArchiveContext {
        db: db.clone(),
        tasks,
        cursors: BTreeMap::new(),
        snapshot,
        meter: Meter::default(),
    }))
}

impl Context for ArchiveContext {
    fn read(
        &mut self,
        name: &str,
        arguments: &Value,
        budget: Option<usize>,
    ) -> std::result::Result<ToolResult, ToolFailure> {
        let mut staged = self.meter.clone();
        let mut cursor_change = None;
        let result = match name {
            "read_context" if arguments.get("task_number").is_some() || arguments.get("cursor").is_some() => {
                let invalid = || ToolFailure::new(ToolFailureKind::InvalidArguments);
                let horizon = self.snapshot["until"].as_i64().unwrap();
                let args = arguments.as_object().ok_or_else(invalid)?;
                let used = arguments["cursor"].as_u64().map(|n| n as usize);
                let page = if let Some(token) = used {
                    if args.len() != 1 {
                        return Err(invalid());
                    }
                    self.cursors.get(&token).cloned().ok_or_else(invalid)?
                } else {
                    if args.keys().any(|k| !matches!(k.as_str(), "task_number" | "kind" | "since" | "until")) {
                        return Err(invalid());
                    }
                    let endpoint = |k: &str, default| match args.get(k) {
                        Some(v) => v.as_i64().filter(|v| *v >= 0).ok_or_else(invalid),
                        None => Ok(default),
                    };
                    let since = endpoint("since", self.snapshot["since"].as_i64().unwrap())?;
                    let until = endpoint("until", horizon)?.min(horizon);
                    if since > until {
                        return Err(invalid());
                    }
                    let number = arguments["task_number"].as_i64().ok_or_else(invalid)?;
                    let updates = arguments["kind"] == "update";
                    HistoryPage { number, updates, since, until, at: until, key: i64::MAX, offset: 0 }
                };
                let (id, updated) =
                    self.tasks.get(&page.number).ok_or_else(|| ToolFailure::new(ToolFailureKind::Denied))?;
                let token = staged.calls + 1;
                staged.pages.insert(format!("{page:?}"));
                let (text, next) = self
                    .db
                    .archive_history_page(
                        id.clone(),
                        *updated,
                        page,
                        horizon,
                        self.snapshot["max_seq"].as_i64().unwrap(),
                        token,
                        budget,
                    )
                    .map_err(|e| ToolFailure::with_message(ToolFailureKind::Unavailable, e.0))?
                    .ok_or_else(|| ToolFailure::new(ToolFailureKind::BudgetExhausted))?;
                staged.calls += 1;
                staged.chars += shishi_api::json::words(&text);
                cursor_change = Some((used, token, next));
                ToolResult::Text(text)
            }
            "read_context" => ToolResult::Text(context::query(&self.snapshot, &mut staged, arguments, budget)),
            "read_image" => return Err(ToolFailure::new(ToolFailureKind::Unavailable)),
            _ => return Err(ToolFailure::new(ToolFailureKind::Denied)),
        };
        fits(&mut self.meter, staged, &result, budget)?;
        if let Some((used, token, next)) = cursor_change {
            if let Some(used) = used {
                self.cursors.remove(&used);
            }
            if let Some(next) = next {
                self.cursors.insert(token, next);
            }
        }
        Ok(result)
    }

    fn progress(&self) -> usize {
        progress(&self.meter)
    }

    fn finish(&mut self, value: &mut Value) {
        attach_result(value, &self.snapshot, &self.meter);
        if let Some(map) = value.as_object_mut() {
            map.remove("_image_notes");
            map.remove("_memory_update");
            map.remove("_memory_error");
        }
    }
}
