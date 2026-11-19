//! Read-side task queries: the console list, one task, the task page's
//! active and archive lists, a task's change history and the snapshot rows.

use std::collections::{BTreeMap, HashMap};

use rusqlite::types::Value as Sql;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde_json::{json, Map, Value};

use super::tasks::get;
use super::{kv, pages, rows, schedules, Db};
use crate::state::Scope;
use crate::tasks::model::{from_json, new_id, Task, STATUSES};
use crate::tasks::query::{label, matches, record_ascending, record_range, snapshot_json, terms};
use crate::tasks::tags::{self, Tag};
use crate::{time, Result};

impl Db {
    pub async fn query_tasks(&self, p: Value) -> Result<Value> {
        self.read(move |c| tasks(c, &p)).await
    }

    pub async fn task_payload(&self, p: Value) -> Result<Value> {
        self.read(move |c| task(c, &p)).await
    }

    pub async fn archived_tasks(&self, p: Value) -> Result<Value> {
        self.read(move |c| archived(c, &p)).await
    }

    pub async fn active_tasks(&self, p: Value) -> Result<Value> {
        self.read(move |c| active(c, &p)).await
    }

    pub async fn edit_focus_view(&self, p: Value) -> Result<Value> {
        self.write(move |tx| {
            let mut views = focus_views(tx)?;
            let name = p["name"].as_str().unwrap_or("").trim();
            if matches!(p["action"].as_str(), Some("add" | "rename")) && name.is_empty() {
                crate::bail!("请填写关注名称");
            }
            if p["action"] == "add" {
                let f = &p["filters"];
                let filters = json!({
                    "scope": from_json::<Vec<String>>(&f["scope"])?, "view": from_json::<String>(&f["view"])?,
                    "query": from_json::<String>(&f["query"])?, "priorities": from_json::<Vec<String>>(&f["priorities"])?,
                    "tag_filters": from_json::<BTreeMap<String, Vec<String>>>(&f["tag_filters"])?,
                });
                views.push(json!({"id": new_id(), "name": name, "filters": filters}));
            } else {
                let position = |id: &Value| views.iter().position(|v| &v["id"] == id).ok_or("找不到这条关注");
                let i = position(&p["id"])?;
                match p["action"].as_str() {
                    Some("rename") => views[i]["name"] = name.into(),
                    Some("delete") => { views.remove(i); }
                    Some("move") => {
                        let slot = position(&p["target"])? + usize::from(p["after"] == true);
                        let to = slot - usize::from(i < slot);
                        if to == i { return Ok(views.into()); }
                        let view = views.remove(i);
                        views.insert(to, view);
                    }
                    _ => crate::bail!("关注操作无效"),
                }
            }
            kv::set(tx, "focus_views", &views)?;
            Ok(views.into())
        }).await
    }

    pub async fn merge_candidates(&self, p: Value) -> Result<Value> {
        self.read(move |c| merge_candidates(c, &p)).await
    }

    pub async fn task_changes(&self, p: Value) -> Result<Value> {
        self.read(move |c| changes(c, &p)).await
    }
}

/// The level group (name, values as JSON) the ranked list queries take.
fn level(catalog: &[tags::Group], kind: &str) -> String {
    let values = catalog.iter().find(|g| g.name == kind).map(|g| g.values.clone()).unwrap_or_default();
    Value::from(values).to_string()
}

const CONSOLE: &str = concat!(include_str!("sql/tasks/ranked.sql"), include_str!("sql/tasks/console.sql"));

/// `tasks_query`: the CLI list, in priority order with deferred tasks
/// reordered among their own slots by when they return.
pub fn tasks(conn: &Connection, p: &Value) -> Result<Value> {
    let limit = p["limit"].as_u64().unwrap_or(20).clamp(1, 100) as i64;
    let offset = p["offset"].as_u64().unwrap_or(0).min(i64::MAX as u64) as i64;
    let query = p["query"].as_str().unwrap_or("").to_lowercase();
    let focus = p["focus"].as_str();
    if p.get("focus").is_some() && !matches!(focus, Some("待处理" | "处理中" | "被阻塞" | "延后处理")) {
        crate::bail!("关注范围应为待处理、处理中、被阻塞或延后处理");
    }
    let (catalog, kind) = super::tags::catalog(conn)?;
    let args = params![
        time::now(),
        kind,
        level(&catalog, &kind),
        p["archived"].as_bool(),
        p["status"].as_str(),
        focus,
        query,
        limit,
        offset
    ];
    let mut items = rows::objects(conn, CONSOLE, args, &json!({"tags": [], "archived": false}))?;
    let total = items.first().map_or(json!(0), |r| r["total"].clone());
    // An empty page is the one `total` row with a NULL id.
    items.retain(|r| !r["id"].is_null());
    items.iter_mut().for_each(|r| drop(r.as_object_mut().unwrap().remove("total")));
    Ok(json!({"total": total, "offset": offset, "limit": limit, "items": items}))
}

pub fn task(conn: &Connection, p: &Value) -> Result<Value> {
    let id = p["id"].as_str().ok_or("缺少事项编号")?;
    Ok(get(conn, id)?.ok_or("事项不存在")?.payload())
}

/// History rows the change dialog shows (archive memory is internal).
const HISTORY: &str = "task_id = ?1 AND substr(kind, 1, 14) <> 'archive_memory'";

pub fn change_count(conn: &Connection, id: &str) -> Result<i64> {
    Ok(conn.query_row(&format!("SELECT count(*) FROM task_audit WHERE {HISTORY}"), [id], |r| r.get(0))?)
}

/// SQL `task_focus(archived, merged_into, status, deferred_until, blocker,
/// waiting_for, at)`: [`Task::focus`], so the lists and counts share it.
pub(super) fn focus_of(ctx: &rusqlite::functions::Context) -> rusqlite::Result<Option<&'static str>> {
    let t = Task {
        archived: ctx.get(0)?,
        merged_into: ctx.get(1)?,
        status: ctx.get(2)?,
        deferred_until: ctx.get(3)?,
        blocker: ctx.get(4)?,
        waiting_for: ctx.get(5)?,
        ..Task::default()
    };
    Ok(t.focus(ctx.get(6)?))
}

/// What a task list row adds: evidence still shown, visible history rows,
/// whether a tool call names it.
type Counts = (i64, i64, bool);

/// [`Counts`] and stored tags of each task in `ids` (extras.sql).
fn extras(conn: &Connection, ids: &[&str]) -> Result<HashMap<String, (Counts, Vec<Tag>)>> {
    let mut stmt = conn.prepare_cached(include_str!("sql/tasks/extras.sql"))?;
    let mut rows = stmt.query([Value::from(ids).to_string()])?;
    let mut out = HashMap::new();
    while let Some(r) = rows.next()? {
        let tags: String = r.get("tags")?;
        out.insert(
            r.get("id")?,
            (
                (r.get("evidence_count")?, r.get("change_count")?, r.get("has_tool_calls")?),
                serde_json::from_str(&tags)?,
            ),
        );
    }
    Ok(out)
}

/// How a task was created: the kind of its first history row.
fn origin(conn: &Connection, id: &str) -> Result<String> {
    let mut stmt = conn.prepare_cached("SELECT kind FROM task_audit WHERE task_id = ?1 ORDER BY seq LIMIT 1")?;
    Ok(stmt.query_row([id], |r| r.get(0)).optional()?.unwrap_or_default())
}

/// The `tasks` rows of the snapshot: only the tasks the page points at (its
/// handling window and selection), each with its full payload plus the
/// fields the list adds. Lists page through `active_tasks_query` and
/// `archived_tasks_query` instead.
pub fn snapshot_rows(conn: &Connection, scope: &Scope) -> Result<Vec<Value>> {
    let mut ids = vec![scope.task.as_str()];
    if scope.selected != scope.task {
        ids.push(&scope.selected);
    }
    ids.retain(|id| !id.is_empty());
    let mut extra = extras(conn, &ids)?;
    let (catalog, kind) = super::tags::catalog(conn)?;
    let at = time::now();
    let mut rows = Vec::new();
    for id in ids {
        let (Some(t), Some((counts, _))) = (get(conn, id)?, extra.remove(id)) else { continue };
        let tags = tags::effective_tags(&t, &catalog, &kind, at);
        let mut row = t.payload();
        row["priority_rank"] = json!(tags::priority_rank(&tags, &catalog, &kind));
        row["effective_tags"] = json!(tags);
        row["evidence_count"] = json!(counts.0);
        row["change_count"] = json!(counts.1);
        row["has_tool_calls"] = json!(counts.2);
        row["origin"] = json!(origin(conn, id)?);
        rows.push(row);
    }
    Ok(rows)
}

/// The fields a listed.sql row carries.
fn listed(r: &Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get("id")?,
        number: r.get("number")?,
        title: r.get("title")?,
        status: r.get("status")?,
        generation: r.get("generation")?,
        manual: r.get("manual")?,
        blocker: r.get("blocker")?,
        waiting_for: r.get("waiting_for")?,
        due: r.get("due")?,
        follow_deadline: r.get("follow_deadline")?,
        deferred_until: r.get("deferred_until")?,
        archived: r.get("archived")?,
        merged_into: r.get("merged_into")?,
        status_changed_at: r.get("status_changed_at")?,
        updated: r.get("updated")?,
        ..Task::default()
    })
}

macro_rules! listed_sql {
    ($($tail:expr),+) => { concat!(include_str!("sql/tasks/ranked.sql"), include_str!("sql/tasks/listed.sql"), $($tail),+) };
}
const ARCHIVED: &str = listed_sql!(include_str!("sql/tasks/archived.sql"));
const ACTIVE: &str = listed_sql!(include_str!("sql/tasks/picked.sql"), include_str!("sql/tasks/active.sql"));
const FOCUS_TOTAL: &str = listed_sql!(include_str!("sql/tasks/picked.sql"), "SELECT count(*) FROM picked");
const FOCUS: [&str; 4] = ["待处理", "处理中", "被阻塞", "延后处理"];

/// `archived_tasks_query`: archived, unmerged, generated tasks filtered by
/// time, tags, level and text, 50 per page in priority then recency order.
pub fn archived(conn: &Connection, p: &Value) -> Result<Value> {
    let (since, until) = record_range(p)?;
    let mut result = list(conn, p, ARCHIVED, 3, [since.into(), until.into()])?;
    result["all"] = conn
        .query_row("SELECT count(*) FROM tasks WHERE archived = 1 AND merged_into = '' AND generation = ''", [], |r| {
            r.get::<_, i64>(0)
        })?
        .into();
    Ok(result)
}

/// Selected scopes form a union before quick-view filters, priority sorting and pagination.
pub fn active(conn: &Connection, p: &Value) -> Result<Value> {
    let scopes = match &p["scope"] {
        Value::Array(items) => items.as_slice(),
        Value::Null => &[],
        item => std::slice::from_ref(item),
    };
    if !scopes.iter().all(|s| {
        s.as_str()
            .is_some_and(|s| ["全部", "未完成", "归档"].contains(&s) || FOCUS.contains(&s) || STATUSES.contains(&s))
    }) {
        crate::bail!("事项范围无效");
    }
    let view = p["view"].as_str().unwrap_or("");
    if !view.is_empty() && view != "today" && !FOCUS.contains(&view) {
        crate::bail!("快速视图无效");
    }
    list(conn, p, ACTIVE, 1, [serde_json::to_string(scopes)?.into(), view.to_string().into()])
}

fn list_catalog(catalog: &[tags::Group], kind: &str, at: i64) -> Result<[Sql; 4]> {
    Ok([at.into(), kind.to_string().into(), level(catalog, kind).into(), serde_json::to_string(catalog)?.into()])
}

fn list_filters(p: &Value, query: String, [a, b]: [Sql; 2]) -> [Sql; 5] {
    let json_or_null = |v: Option<&Value>| v.map(Value::to_string);
    [
        a,
        b,
        query.into(),
        json_or_null(p["tag_filters"].as_object().map(|_| &p["tag_filters"])).into(),
        json_or_null(p["priorities"].as_array().map(|_| &p["priorities"])).into(),
    ]
}

/// One page of a listed.sql source whose own parameters are ?5 and ?6.
/// `selected` names a task the page keeps shown: when it passes the
/// filters but is not on this page, it comes back as `selected`.
fn list(conn: &Connection, p: &Value, source: &str, keys: usize, own: [Sql; 2]) -> Result<Value> {
    let query = p["query"].as_str().unwrap_or("").trim().to_lowercase();
    if query.chars().count() > 200 {
        crate::bail!("搜索词最多200字");
    }
    let (catalog, kind) = super::tags::catalog(conn)?;
    let at = time::now();
    let mut params = Vec::from(list_catalog(&catalog, &kind, at)?);
    params.extend(list_filters(p, query, own));
    let mut found = Vec::new();
    let mut result = pages::Page {
        source,
        params: params.clone(),
        keys,
        descending: false,
        cursor: p.get("cursor"),
        page: p["page"].as_u64().unwrap_or(1) as usize,
        size: 50,
    }
    .run(conn, |r: &Row| {
        found.push(listed(r)?);
        Ok(Value::Null)
    })?;
    let shown = found.len();
    if let Some(id) = p["selected"].as_str().filter(|id| !found.iter().any(|t| t.id == *id)) {
        let mut args = params;
        args.push(id.to_string().into());
        let pinned = conn
            .query_row(&format!("SELECT * FROM ({source}) WHERE id = ?10"), params_from_iter(args), listed)
            .optional()?;
        found.extend(pinned);
    }
    let mut extra = extras(conn, &found.iter().map(|t| t.id.as_str()).collect::<Vec<_>>())?;
    let mut rows = Vec::with_capacity(found.len());
    for mut t in found {
        let ((evidence, _, _), stored) = extra.remove(&t.id).unwrap_or_default();
        t.tags = stored;
        let tags = tags::effective_tags(&t, &catalog, &kind, at);
        rows.push(json!({
            "id": t.id, "number": t.number, "title": t.title, "status": t.status, "generation": t.generation,
            "manual": t.manual, "blocker": t.blocker, "waiting_for": t.waiting_for, "due": t.due,
            "deferred_until": t.deferred_until, "archived": t.archived, "merged_into": t.merged_into,
            "status_changed_at": t.status_changed_at, "updated": t.updated, "evidence_count": evidence,
            "priority_rank": tags::priority_rank(&tags, &catalog, &kind), "effective_tags": tags,
        }));
    }
    result["selected"] = rows.get(shown).cloned().unwrap_or(Value::Null);
    rows.truncate(shown);
    result["rows"] = json!(rows);
    Ok(result)
}

/// `audit_matches(kind, task_id, detail, before, after, number, title,
/// terms)`: [`matches`] over one stored history row of task `number`, so
/// the change dialog filters, counts and pages in one query.
pub(super) fn audit_matches(ctx: &rusqlite::functions::Context) -> rusqlite::Result<bool> {
    let text = |i| ctx.get::<Option<String>>(i).map(Option::unwrap_or_default);
    let number: i64 = ctx.get(5)?;
    let row = json!({
        "kind": text(0)?, "task_id": text(1)?, "detail": text(2)?,
        "before": snapshot_json(ctx.get(3)?), "after": snapshot_json(ctx.get(4)?),
        "task_number": number, "task_title": text(6)?, "task_label": format!("#{number}"),
    });
    let terms: Vec<String> = text(7)?.split_whitespace().map(str::to_owned).collect();
    Ok(matches(&row, &terms))
}

/// An audit row as the page shows it: `index` is the `seq`, `id` the token.
fn audit_row(r: &Row) -> Result<Value> {
    let kind: String = r.get("kind")?;
    Ok(json!({
        "index": r.get::<_, i64>("seq")?, "id": r.get::<_, String>("token")?, "time": r.get::<_, i64>("time")?,
        "kind_label": label(&kind), "kind": kind, "task_id": r.get::<_, String>("task_id")?,
        "detail": r.get::<_, String>("detail")?, "change_summary": r.get::<_, Option<String>>("change_summary")?,
        "before": snapshot_json(r.get("before")?), "after": snapshot_json(r.get("after")?),
    }))
}

/// `task_changes`: one task's history, 50 per page by time then `seq`.
pub fn changes(conn: &Connection, p: &Value) -> Result<Value> {
    let id = p["id"].as_str().ok_or("缺少事项编号")?;
    let terms = terms(p)?;
    let ascending = record_ascending(p)?;
    let (since, until) = record_range(p)?;
    let (number, title): (i64, String) = conn
        .query_row("SELECT number, title FROM tasks WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
        .ok_or("事项不存在")?;
    let all = change_count(conn, id)?;
    let source = format!(
        "SELECT *, time AS k0, seq AS k1 FROM task_audit WHERE {HISTORY} AND time >= ?2 AND time <= ?3 \
         AND (?4 = '' OR audit_matches(kind, task_id, detail, before, after, ?5, ?6, ?4))"
    );
    let args: Vec<rusqlite::types::Value> =
        vec![id.to_string().into(), since.into(), until.into(), terms.join(" ").into(), number.into(), title.into()];
    let mut result = pages::Page {
        source: &source,
        params: args,
        keys: 2,
        descending: !ascending,
        cursor: p.get("cursor"),
        page: p["page"].as_u64().unwrap_or(1).max(1) as usize,
        size: 50,
    }
    .run(conn, audit_row)?;
    result["all"] = json!(all);
    Ok(result)
}

/// `merge_candidates`: what the merge dialog offers next to task `id`,
/// unhandled tasks first, then most recently updated.
pub fn merge_candidates(conn: &Connection, p: &Value) -> Result<Value> {
    let id = p["id"].as_str().unwrap_or("");
    let sql = "SELECT t.id, t.title, t.description, t.status, t.updated, (
            SELECT json_group_array(json_object('kind', kind, 'value', value)) FROM (
                SELECT kind, value FROM task_tags WHERE task_id = t.id ORDER BY kind, value)) AS tags
        FROM tasks t WHERE t.id <> ?1 AND t.archived = 0 AND t.merged_into = '' AND t.generation = ''
        ORDER BY t.status <> '待处理', t.updated DESC, t.number";
    Ok(json!({ "rows": rows::objects(conn, sql, [id], &json!({"tags": []}))? }))
}

/// Changes when any task is added, edited or removed, when a deadline
/// crosses the hour marks that raise a level, and when messages change
/// (withdrawn or removed evidence), so a list knows to reload what it
/// shows. Every write moves a task's `updated` forward and numbers are
/// unique, so these sums cover add, edit, merge and removal without
/// hashing every row.
pub fn tasks_revision(conn: &Connection, at: i64) -> Result<String> {
    let tasks: String = conn.query_row(
        "SELECT count(*) || ':' || total(number) || ':' || total(updated) || ':' || coalesce(max(updated), 0)
            || ':' || total(merged_into <> '') || ':' || total(near AND due_ms - ?1 <= 7200000)
            || ':' || total(near AND due_ms - ?1 <= 86400000)
        FROM (SELECT *, follow_deadline = 1 AND status IN ('待处理', '处理中') AND due_ms IS NOT NULL AS near FROM tasks)",
        [at],
        |r| r.get(0),
    )?;
    Ok(format!("{tasks}:{}", kv::get_i64(conn, crate::messages::REVISION)?))
}

fn focus_views(conn: &Connection) -> Result<Vec<Value>> {
    Ok(kv::get(conn, "focus_views")?.unwrap_or_else(|| FOCUS.iter().map(|s| json!({"id": s, "name": s})).collect()))
}

/// Counts by focus ID and the next deferred task's return time.
pub fn focus_counts(conn: &Connection, at: i64, views: &[Value]) -> Result<(Map<String, Value>, Option<i64>)> {
    let mut counts: Map<String, Value> = FOCUS.iter().map(|f| (f.to_string(), json!(0))).collect();
    let mut next = None;
    let mut stmt = conn.prepare_cached(
        "SELECT focus, count(*), min(deferred_until) FROM (
            SELECT task_focus(archived, merged_into, status, deferred_until, blocker, waiting_for, ?1) AS focus,
                deferred_until
            FROM tasks WHERE status IN ('待处理', '处理中') AND archived = 0 AND merged_into = '')
        GROUP BY focus",
    )?;
    let mut rows = stmt.query([at])?;
    while let Some(r) = rows.next()? {
        let focus: String = r.get(0)?;
        if focus == "延后处理" {
            next = Some(r.get(2)?);
        }
        counts.insert(focus, json!(r.get::<_, i64>(1)?));
    }
    let mut filtered = Vec::new();
    for v in views {
        let Some(f) = v.get("filters") else { continue };
        let id = v["id"].as_str().unwrap();
        let scopes = f["scope"].as_array().unwrap();
        if f["query"] == ""
            && f["view"] == ""
            && f["tag_filters"] == json!({})
            && f["priorities"] == json!([])
            && !scopes.is_empty()
            && scopes.iter().all(|s| FOCUS.contains(&s.as_str().unwrap()))
        {
            let n: i64 =
                FOCUS.iter().filter(|k| scopes.iter().any(|s| s == **k)).map(|k| counts[*k].as_i64().unwrap()).sum();
            counts.insert(id.into(), n.into());
        } else {
            filtered.push((id, f));
        }
    }
    if !filtered.is_empty() {
        let (catalog, kind) = super::tags::catalog(conn)?;
        let common = list_catalog(&catalog, &kind, at)?;
        let mut stmt = conn.prepare_cached(FOCUS_TOTAL)?;
        for (id, f) in filtered {
            let view = if f["scope"] == json!(["归档"]) { "" } else { f["view"].as_str().unwrap() };
            let args = list_filters(
                f,
                f["query"].as_str().unwrap().trim().to_lowercase(),
                [f["scope"].to_string().into(), view.to_string().into()],
            );
            let total: i64 = stmt.query_row(params_from_iter(common.iter().chain(&args)), |r| r.get(0))?;
            counts.insert(id.into(), total.into());
        }
    }
    Ok((counts, next))
}

/// Snapshot keys of the task domain.
pub(super) fn part(conn: &Connection, scope: &Scope, out: &mut Map<String, Value>) -> Result<()> {
    let at = time::now();
    let views = focus_views(conn)?;
    let (counts, next) = focus_counts(conn, at, if scope.page == "panel" { &[] } else { &views })?;
    out.insert("tasks".into(), snapshot_rows(conn, scope)?.into());
    out.insert("open_task_count".into(), FOCUS.iter().map(|k| counts[*k].as_i64().unwrap()).sum::<i64>().into());
    out.insert("task_focus_counts".into(), counts.into());
    out.insert("focus_views".into(), views.into());
    out.insert("next_deferral_at".into(), next.into());
    out.insert("tag_state".into(), super::tags::snapshot(conn)?);
    out.insert("rule_version".into(), kv::get_i64(conn, super::rules::RULE_VERSION)?.into());
    out.insert("tasks_revision".into(), tasks_revision(conn, at)?.into());
    if scope.page == "tasks" {
        let (seq, count): (i64, i64) =
            conn.query_row("SELECT coalesce(max(seq), 0), count(*) FROM task_audit", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        out.insert("task_history_revision".into(), json!([seq, count]));
    }
    if ["prompts", "learning"].contains(&scope.page.as_str()) {
        out.insert("suggestions".into(), super::rules::list(conn)?.into());
    }
    if scope.page == "schedules" {
        out.insert("schedules".into(), schedules::snapshot(conn, time::now())?);
    }
    Ok(())
}
