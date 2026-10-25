//! HTTP API. Every handler that touches the catalog runs as a job on the
//! single worker; `/api/status` and cancellation read shared state only.

use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use filestore::scan::{self, Operation, ScanRequest, Target};
use filestore::semantic::{self, NewRevision};
use filestore::{Error, advise, catalog, ledger, query};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::worker::Handle;

pub struct ApiError(Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            Error::Invalid(_) => StatusCode::BAD_REQUEST,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Conflict(_) => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(json!({"error": self.0.to_string()}))).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

fn ok<T: serde::Serialize>(v: T) -> ApiResult {
    Ok(Json(serde_json::to_value(v).map_err(Error::from)?))
}

pub fn router(h: Handle) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/tree", get(tree))
        .route("/dir", get(dir))
        .route("/location", get(location))
        .route("/search", get(search))
        .route("/blobs", get(blobs))
        .route("/blobs/{id}", get(blob))
        .route("/scans", get(scans).post(create_scan))
        .route("/scans/{id}", get(scan_detail))
        .route("/scans/{id}/cancel", post(cancel_scan))
        .route("/suggestions", get(suggestions))
        .route("/suggestions/{id}/accept", post(accept_suggestion))
        .route("/tags", get(tags).post(create_tag))
        .route("/tags/{id}", get(tag_targets).patch(rename_tag))
        .route("/tag-hierarchies", post(create_hierarchy))
        .route("/assignments", post(assign))
        .route("/lineages", get(lineages).post(create_lineage))
        .route("/lineages/{id}", get(lineage).patch(rename_lineage).delete(remove_lineage))
        .route("/lineages/{id}/revisions", post(add_revision))
        .route("/revisions/{id}", axum::routing::patch(update_revision).delete(remove_revision))
        .route("/history", get(history))
        .route("/root/adopt", post(adopt_root))
        .route("/settings", get(settings).put(put_settings))
        .with_state(h)
}

async fn status(State(h): State<Handle>) -> ApiResult {
    ok(h.status())
}

#[derive(Deserialize)]
struct PathQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    history: bool,
}

async fn tree(State(h): State<Handle>, Query(q): Query<PathQuery>) -> ApiResult {
    ok(h.call(move |w| query::tree(&w.cat, &q.path)).await?)
}

async fn dir(State(h): State<Handle>, Query(q): Query<PathQuery>) -> ApiResult {
    ok(h.call(move |w| query::dir_view(&w.cat, &q.path, q.history)).await?)
}

async fn location(State(h): State<Handle>, Query(q): Query<PathQuery>) -> ApiResult {
    ok(h.call(move |w| query::location_detail(&w.cat, &q.path)).await?)
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search(State(h): State<Handle>, Query(q): Query<SearchQuery>) -> ApiResult {
    ok(h.call(move |w| query::search(&w.cat, &q.q, 500)).await?)
}

#[derive(Deserialize)]
struct BlobsQuery {
    #[serde(default)]
    filter: String,
    q: Option<String>,
}

async fn blobs(State(h): State<Handle>, Query(q): Query<BlobsQuery>) -> ApiResult {
    ok(h.call(move |w| query::blobs(&w.cat, &q.filter, q.q.as_deref(), 2000)).await?)
}

async fn blob(State(h): State<Handle>, Path(id): Path<String>) -> ApiResult {
    ok(h.call(move |w| query::blob_detail(&w.cat, &id)).await?)
}

async fn scans(State(h): State<Handle>) -> ApiResult {
    ok(h.call(move |w| query::scans(&w.cat.conn, 500)).await?)
}

async fn scan_detail(State(h): State<Handle>, Path(id): Path<i64>) -> ApiResult {
    ok(h.call(move |w| query::scan_detail(&w.cat.conn, id)).await?)
}

#[derive(Deserialize)]
struct ScanBody {
    operation: Operation,
    targets: Vec<Target>,
    request_id: Option<String>,
}

async fn create_scan(State(h): State<Handle>, Json(b): Json<ScanBody>) -> ApiResult {
    ok(h
        .call(move |w| {
            let payload = json!({"operation": b.operation, "targets": b.targets});
            let req = ScanRequest {
                operation: b.operation,
                targets: b.targets,
            };
            let (v, fresh) = ledger::run(&w.cat, b.request_id.as_deref(), "submit_scan", &payload, |cat| {
                scan::submit(&cat.conn, &req, None)
            })?;
            if fresh {
                w.enqueue_scan(v["id"].as_i64().unwrap_or_default());
                w.changed();
            }
            Ok(v)
        })
        .await?)
}

async fn cancel_scan(State(h): State<Handle>, Path(id): Path<i64>) -> ApiResult {
    ok(json!({"cancelled": h.cancel(id)}))
}

async fn suggestions(State(h): State<Handle>) -> ApiResult {
    ok(h.call(move |w| advise::suggestions(&w.cat)).await?)
}

#[derive(Deserialize, Default)]
struct AcceptBody {
    request_id: Option<String>,
}

async fn accept_suggestion(State(h): State<Handle>, Path(id): Path<String>, body: Option<Json<AcceptBody>>) -> ApiResult {
    let b = body.map(|b| b.0).unwrap_or_default();
    ok(h
        .call(move |w| {
            let payload = json!({"suggestion": id});
            let (v, fresh) = ledger::run(&w.cat, b.request_id.as_deref(), "accept_suggestion", &payload, |cat| {
                advise::accept_scan(cat, &id)
            })?;
            if fresh {
                w.enqueue_scan(v["id"].as_i64().unwrap_or_default());
                w.changed();
            }
            Ok(v)
        })
        .await?)
}

async fn tags(State(h): State<Handle>) -> ApiResult {
    ok(h.call(move |w| semantic::list_tags(&w.cat.conn)).await?)
}

async fn tag_targets(State(h): State<Handle>, Path(id): Path<String>) -> ApiResult {
    ok(h.call(move |w| semantic::tag_targets(&w.cat.conn, &id)).await?)
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn create_hierarchy(State(h): State<Handle>, Json(b): Json<NameBody>) -> ApiResult {
    ok(h
        .call(move |w| {
            let id = semantic::create_hierarchy(&w.cat.conn, &b.name)?;
            w.changed();
            Ok(json!({"id": id}))
        })
        .await?)
}

#[derive(Deserialize)]
struct TagBody {
    hierarchy: String,
    parent: Option<String>,
    name: String,
}

async fn create_tag(State(h): State<Handle>, Json(b): Json<TagBody>) -> ApiResult {
    ok(h
        .call(move |w| {
            let id = semantic::create_tag(&w.cat.conn, &b.hierarchy, b.parent.as_deref(), &b.name)?;
            w.changed();
            Ok(json!({"id": id}))
        })
        .await?)
}

async fn rename_tag(State(h): State<Handle>, Path(id): Path<String>, Json(b): Json<NameBody>) -> ApiResult {
    ok(h
        .call(move |w| {
            semantic::rename_tag(&w.cat.conn, &id, &b.name)?;
            w.changed();
            Ok(json!({}))
        })
        .await?)
}

#[derive(Deserialize)]
struct AssignBody {
    target_kind: String,
    target: String,
    hierarchy: String,
    tag: Option<String>,
}

async fn assign(State(h): State<Handle>, Json(b): Json<AssignBody>) -> ApiResult {
    ok(h
        .call(move |w| {
            semantic::assign(&w.cat.conn, &b.target_kind, &b.target, &b.hierarchy, b.tag.as_deref())?;
            w.changed();
            Ok(json!({}))
        })
        .await?)
}

async fn lineages(State(h): State<Handle>) -> ApiResult {
    ok(h.call(move |w| semantic::list_lineages(&w.cat.conn)).await?)
}

async fn lineage(State(h): State<Handle>, Path(id): Path<String>) -> ApiResult {
    ok(h.call(move |w| semantic::lineage_detail(&w.cat.conn, &id)).await?)
}

#[derive(Deserialize)]
struct LineageBody {
    title: String,
    #[serde(default)]
    revisions: Vec<NewRevision>,
    suggestion: Option<String>,
    modifications: Option<Value>,
    request_id: Option<String>,
}

async fn create_lineage(State(h): State<Handle>, Json(b): Json<LineageBody>) -> ApiResult {
    ok(h
        .call(move |w| {
            let payload = json!({"title": b.title, "revisions": b.revisions, "suggestion": b.suggestion});
            let (v, fresh) = ledger::run(&w.cat, b.request_id.as_deref(), "create_lineage", &payload, |cat| {
                let tx = catalog::atomic(&cat.conn)?;
                let prov = match &b.suggestion {
                    Some(sid) => {
                        let s = advise::find(cat, sid)?;
                        if !matches!(s.action, advise::Action::CreateLineage { .. }) {
                            return Err(Error::Invalid("that suggestion does not create a Lineage".into()));
                        }
                        Some(advise::record_provenance(&tx, &s, b.modifications.clone())?)
                    }
                    None => None,
                };
                let id = semantic::create_lineage(&tx, &b.title, &b.revisions, prov)?;
                tx.commit()?;
                Ok(json!({"id": id}))
            })?;
            if fresh {
                w.changed();
            }
            Ok(v)
        })
        .await?)
}

#[derive(Deserialize)]
struct TitleBody {
    title: String,
}

async fn rename_lineage(State(h): State<Handle>, Path(id): Path<String>, Json(b): Json<TitleBody>) -> ApiResult {
    ok(h
        .call(move |w| {
            semantic::rename_lineage(&w.cat.conn, &id, &b.title)?;
            w.changed();
            Ok(json!({}))
        })
        .await?)
}

async fn remove_lineage(State(h): State<Handle>, Path(id): Path<String>) -> ApiResult {
    ok(h
        .call(move |w| {
            semantic::tombstone_lineage(&w.cat.conn, &id)?;
            w.changed();
            Ok(json!({}))
        })
        .await?)
}

#[derive(Deserialize)]
struct RevisionBody {
    blob: String,
    label: Option<String>,
    #[serde(default)]
    parents: Vec<String>,
    suggestion: Option<String>,
    request_id: Option<String>,
}

async fn add_revision(State(h): State<Handle>, Path(lineage): Path<String>, Json(b): Json<RevisionBody>) -> ApiResult {
    ok(h
        .call(move |w| {
            let payload = json!({"lineage": lineage, "blob": b.blob, "label": b.label, "parents": b.parents, "suggestion": b.suggestion});
            let (v, fresh) = ledger::run(&w.cat, b.request_id.as_deref(), "add_revision", &payload, |cat| {
                let tx = catalog::atomic(&cat.conn)?;
                let prov = match &b.suggestion {
                    Some(sid) => {
                        let s = advise::find(cat, sid)?;
                        Some(advise::record_provenance(&tx, &s, None)?)
                    }
                    None => None,
                };
                let rev = NewRevision {
                    blob: b.blob.clone(),
                    label: b.label.clone(),
                    parents: b.parents.iter().cloned().map(semantic::ParentRef::Id).collect(),
                };
                let id = semantic::add_revision(&tx, &lineage, &rev, prov)?;
                tx.commit()?;
                Ok(json!({"id": id}))
            })?;
            if fresh {
                w.changed();
            }
            Ok(v)
        })
        .await?)
}

#[derive(Deserialize)]
struct RevisionPatch {
    /// Absent: unchanged; `null` or empty: cleared.
    #[serde(default)]
    label: Option<Value>,
    parents: Option<Vec<String>>,
}

async fn update_revision(State(h): State<Handle>, Path(id): Path<String>, Json(b): Json<RevisionPatch>) -> ApiResult {
    ok(h
        .call(move |w| {
            let label = b.label.map(|v| v.as_str().map(String::from));
            semantic::update_revision(&w.cat.conn, &id, label, b.parents)?;
            w.changed();
            Ok(json!({}))
        })
        .await?)
}

async fn remove_revision(State(h): State<Handle>, Path(id): Path<String>) -> ApiResult {
    ok(h
        .call(move |w| {
            semantic::tombstone_revision(&w.cat.conn, &id)?;
            w.changed();
            Ok(json!({}))
        })
        .await?)
}

async fn history(State(h): State<Handle>) -> ApiResult {
    ok(h.call(move |w| semantic::history(&w.cat.conn, 500)).await?)
}

async fn adopt_root(State(h): State<Handle>) -> ApiResult {
    ok(h
        .call(move |w| {
            let root = w.cat.root().to_path_buf();
            let tx = catalog::atomic(&w.cat.conn)?;
            let id = scan::adopt_root_identity(&tx, &root)?;
            let (public, ctx, ino): (String, String, i64) = tx.query_row(
                "SELECT public_id, fs_context, root_ino FROM epochs WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            semantic::record_adoption(&tx, &public, json!({"fs_context": ctx, "root_ino": ino}))?;
            tx.commit()?;
            w.changed();
            Ok(json!({"epoch": public}))
        })
        .await?)
}

async fn settings(State(h): State<Handle>) -> ApiResult {
    ok(h
        .call(move |w| Ok(json!({"verify_horizon_days": w.cat.verify_horizon_days()})))
        .await?)
}

#[derive(Deserialize)]
struct SettingsBody {
    verify_horizon_days: i64,
}

async fn put_settings(State(h): State<Handle>, Json(b): Json<SettingsBody>) -> ApiResult {
    ok(h
        .call(move |w| {
            query::set_horizon(&w.cat, b.verify_horizon_days)?;
            w.changed();
            Ok(json!({"verify_horizon_days": b.verify_horizon_days}))
        })
        .await?)
}
