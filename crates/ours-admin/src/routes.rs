//! The router and its handlers.

use crate::auth::require_token;
use crate::compliance;
use crate::source::{AdminEvent, AdminSource, GroupSummary};
use axum::extract::{Path, Query, Request, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY,
    WWW_AUTHENTICATE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{from_fn_with_state, map_response};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{Stream, StreamExt, stream};
use ours_types::GroupId;
use serde::Deserialize;
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::broadcast::{self, error::RecvError};
use tower_http::trace::TraceLayer;

const INDEX_HTML: &str = include_str!("../assets/index.html");
const FAVICON_SVG: &str = include_str!("../assets/favicon.svg");
const PAGE_POLICY: &str = "default-src 'none'; script-src 'unsafe-inline'; \
    style-src 'unsafe-inline'; connect-src 'self'; img-src data:; \
    base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const MARKDOWN: &str = "text/markdown; charset=utf-8";
const JSON_TYPE: &str = "application/json";
const CALENDAR: &str = "text/calendar; charset=utf-8";
const ICS_SUFFIX: &str = ".ics";
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;

#[derive(Clone)]
pub(crate) struct AppState {
    source: Arc<dyn AdminSource>,
    pub(crate) token: Arc<str>,
}

#[derive(Debug)]
pub(crate) enum ApiError {
    Unauthorized,
    UnknownGroup,
    NoCharter,
    NoDecisionLog,
    NotFound,
    NoPersona,
    NoProfile,
    WrongConfirmation,
    Failed(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                [(WWW_AUTHENTICATE, "Bearer")],
                Json(json!({
                    "error": "unauthorized",
                    "hint": "open the link or scan the QR code the host's terminal printed",
                })),
            )
                .into_response(),
            Self::UnknownGroup => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "unknown group" })),
            )
                .into_response(),
            Self::NoCharter => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "no charter yet" })),
            )
                .into_response(),
            Self::NoDecisionLog => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "no decision log yet" })),
            )
                .into_response(),
            Self::NoPersona => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "no persona" })),
            )
                .into_response(),
            Self::NoProfile => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "no profile" })),
            )
                .into_response(),
            Self::WrongConfirmation => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "type the group id to confirm" })),
            )
                .into_response(),
            Self::Failed(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": error })),
            )
                .into_response(),
            Self::NotFound => {
                (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
            }
        }
    }
}

/// The whole admin app. Everything needs the token except `/healthz` and
/// the calendar feeds under `/ics/`, whose secret is their credential.
pub fn router(source: Arc<dyn AdminSource>, token: impl Into<String>) -> Router {
    let state = AppState {
        source,
        token: Arc::from(token.into()),
    };
    let guarded = Router::new()
        .route("/", get(page))
        .route("/api/groups", get(groups))
        .route("/api/groups/{id}/state", get(group_state))
        .route("/api/groups/{id}/charter", get(group_charter))
        .route("/api/groups/{id}/facts", get(group_facts))
        .route("/api/groups/{id}/decisions", get(group_decisions))
        .route("/api/groups/{id}/decisions.md", get(group_decision_log))
        .route("/api/groups/{id}/ledger", get(group_ledger))
        .route("/api/groups/{id}/actions", get(group_actions))
        .route("/compliance", get(compliance_page))
        .route("/api/groups/{id}/export.json", get(export_json))
        .route("/api/groups/{id}/export.md", get(export_markdown))
        .route("/api/groups/{id}/legal-hold", post(legal_hold))
        .route("/api/groups/{id}/wipe", post(wipe))
        .route("/api/groups/{id}/persona", get(group_persona))
        .route("/api/groups/{id}/profile", get(group_profile))
        .route("/api/catalogue", get(catalogue))
        .route("/api/groups/{id}/events", get(group_events))
        .route("/api/groups/{id}/stream", get(group_stream))
        .route_layer(from_fn_with_state(state.clone(), require_token));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/favicon.svg",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
                    FAVICON_SVG,
                )
            }),
        )
        .route("/ics/{id}/{file}", get(ics_group))
        .route("/ics/{id}/{secret}/{file}", get(ics_member))
        .merge(guarded)
        .fallback(|| async { ApiError::NotFound })
        .with_state(state)
        .layer(map_response(harden))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request| {
                // The path only: the query can carry the token.
                tracing::info_span!(
                    "admin_request",
                    method = %request.method(),
                    path = %logged_path(request.uri().path()),
                )
            }),
        )
}

/// The request path for logs. A calendar path carries the secret.
fn logged_path(path: &str) -> &str {
    if path.starts_with("/ics/") {
        "/ics/<hidden>"
    } else {
        path
    }
}

/// No caching and no referrer, so the token and the feed stay off disk
/// and out of other sites' logs.
async fn harden(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

async fn page() -> impl IntoResponse {
    ([(CONTENT_SECURITY_POLICY, PAGE_POLICY)], Html(INDEX_HTML))
}

async fn compliance_page(State(state): State<AppState>) -> impl IntoResponse {
    let report = state.source.compliance().await;
    (
        [(CONTENT_SECURITY_POLICY, PAGE_POLICY)],
        Html(compliance::render(&report)),
    )
}

/// A file name part that is safe in a header: letters, digits, `-`, `_`.
fn file_stem(id: &str) -> String {
    let stem: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("ours-{stem}-export")
}

fn download(stem: &str, extension: &str, content_type: &'static str, body: String) -> Response {
    let disposition = format!("attachment; filename=\"{stem}.{extension}\"");
    let mut response = ([(CONTENT_TYPE, content_type)], body).into_response();
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        response.headers_mut().insert(CONTENT_DISPOSITION, value);
    }
    response
}

async fn export_json(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let stem = file_stem(&id);
    let export = state
        .source
        .export(&GroupId::new(id))
        .await
        .ok_or(ApiError::UnknownGroup)?;
    Ok(download(&stem, "json", JSON_TYPE, export.json))
}

async fn export_markdown(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let stem = file_stem(&id);
    let export = state
        .source
        .export(&GroupId::new(id))
        .await
        .ok_or(ApiError::UnknownGroup)?;
    Ok(download(&stem, "md", MARKDOWN, export.markdown))
}

#[derive(Deserialize)]
struct HoldBody {
    on: bool,
}

async fn legal_hold(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<HoldBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let id = known_group(&state, id).await?;
    if state.source.set_legal_hold(&id, body.on).await {
        Ok(Json(json!({ "legal_hold": body.on })))
    } else {
        Err(ApiError::Failed("could not change the legal hold".into()))
    }
}

#[derive(Deserialize)]
struct WipeBody {
    confirm: String,
}

/// Wipes the group, but only when the body repeats the group id.
async fn wipe(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<WipeBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let id = known_group(&state, id).await?;
    if body.confirm != id.as_str() {
        return Err(ApiError::WrongConfirmation);
    }
    tracing::warn!(group = %id.as_str(), "wiping a group at the host's request");
    state.source.wipe(&id).await.map_err(ApiError::Failed)?;
    Ok(Json(json!({ "wiped": true })))
}

async fn group_persona(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let value = state.source.persona(&GroupId::new(id)).await;
    value.map(Json).ok_or(ApiError::NoPersona)
}

async fn group_profile(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let value = state.source.profile(&GroupId::new(id)).await;
    value.map(Json).ok_or(ApiError::NoProfile)
}

async fn groups(State(state): State<AppState>) -> Json<Vec<GroupSummary>> {
    Json(state.source.groups().await)
}

async fn group_state(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let value = state.source.state(&GroupId::new(id)).await;
    value.map(Json).ok_or(ApiError::UnknownGroup)
}

async fn group_charter(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id = known_group(&state, id).await?;
    let charter = state.source.charter(&id).await.ok_or(ApiError::NoCharter)?;
    Ok(([(CONTENT_TYPE, MARKDOWN)], charter))
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

impl LimitQuery {
    fn get(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT)
    }
}

async fn group_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<Vec<AdminEvent>>, ApiError> {
    let id = known_group(&state, id).await?;
    Ok(Json(state.source.recent(&id, query.get()).await))
}

#[derive(Deserialize)]
struct FactsQuery {
    include_retracted: Option<bool>,
}

/// The group's facts as a JSON array. A fact is retracted when its
/// `retracted` key holds anything but null.
async fn group_facts(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<FactsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let id = known_group(&state, id).await?;
    let mut facts = state.source.facts(&id).await;
    if !query.include_retracted.unwrap_or(false) {
        facts.retain(|fact| fact.get("retracted").is_none_or(serde_json::Value::is_null));
    }
    Ok(Json(facts))
}

/// Open proposals, closed decisions and upcoming calendar entries, as the
/// source gives them.
async fn group_decisions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let id = known_group(&state, id).await?;
    Ok(Json(state.source.decisions(&id).await))
}

/// Open and past promises, the tab's lines and the balances, as the source
/// gives them.
async fn group_ledger(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let id = known_group(&state, id).await?;
    Ok(Json(state.source.ledger(&id).await))
}

/// The group's action records, newest first, as the source gives them.
async fn group_actions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let id = known_group(&state, id).await?;
    Ok(Json(state.source.actions(&id).await))
}

/// Every action class with its tier and how it is taken back.
async fn catalogue(State(state): State<AppState>) -> Json<Vec<serde_json::Value>> {
    Json(state.source.catalogue().await)
}

async fn group_decision_log(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id = known_group(&state, id).await?;
    let log = state
        .source
        .decision_log(&id)
        .await
        .ok_or(ApiError::NoDecisionLog)?;
    Ok(([(CONTENT_TYPE, MARKDOWN)], log))
}

/// The group's calendar. A wrong secret and an unknown group look alike.
async fn ics_group(
    State(state): State<AppState>,
    Path((id, file)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let secret = ics_name(&file)?;
    calendar(&state, id, secret, None).await
}

/// One member's view of the group's calendar.
async fn ics_member(
    State(state): State<AppState>,
    Path((id, secret, file)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let member = ics_name(&file)?;
    calendar(&state, id, &secret, Some(member)).await
}

/// The name before `.ics`, or not found when it has no suffix or no name.
fn ics_name(file: &str) -> Result<&str, ApiError> {
    file.strip_suffix(ICS_SUFFIX)
        .filter(|name| !name.is_empty())
        .ok_or(ApiError::NotFound)
}

async fn calendar(
    state: &AppState,
    id: String,
    secret: &str,
    member: Option<&str>,
) -> Result<Response, ApiError> {
    let text = state
        .source
        .ics(&GroupId::new(id), secret, member)
        .await
        .ok_or(ApiError::NotFound)?;
    Ok(([(CONTENT_TYPE, CALENDAR)], text).into_response())
}

async fn group_stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<LimitQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let id = known_group(&state, id).await?;
    // Subscribe before reading the backlog so no event falls in between.
    let receiver = state.source.subscribe();
    let backlog = state.source.recent(&id, query.get()).await;
    let replay = stream::iter(backlog.clone().into_iter().map(Item::Event));
    let feed = replay
        .chain(live(receiver, id, backlog))
        .filter_map(|item| async move { item.into_sse() })
        .map(Ok);
    Ok(Sse::new(feed).keep_alive(KeepAlive::default()))
}

async fn known_group(state: &AppState, id: String) -> Result<GroupId, ApiError> {
    let id = GroupId::new(id);
    let groups = state.source.groups().await;
    if groups.iter().any(|group| group.id == id) {
        Ok(id)
    } else {
        Err(ApiError::UnknownGroup)
    }
}

enum Item {
    Event(AdminEvent),
    Lagged(u64),
}

impl Item {
    fn into_sse(self) -> Option<Event> {
        match self {
            Self::Event(event) => match Event::default().json_data(&event) {
                Ok(sse) => Some(sse),
                Err(error) => {
                    tracing::warn!(%error, "dropping an event that cannot be encoded");
                    None
                }
            },
            Self::Lagged(missed) => Some(Event::default().event("lagged").data(missed.to_string())),
        }
    }
}

/// Live events for one group. Events already in `backlog` are skipped so
/// the replay and the channel do not show a line twice.
fn live(
    receiver: broadcast::Receiver<AdminEvent>,
    group: GroupId,
    backlog: Vec<AdminEvent>,
) -> impl Stream<Item = Item> {
    stream::unfold(
        (receiver, group, backlog),
        |(mut receiver, group, mut backlog)| async move {
            loop {
                match receiver.recv().await {
                    Ok(event) if event.group != group => {}
                    Ok(event) => {
                        if let Some(at) = backlog.iter().position(|seen| *seen == event) {
                            backlog.remove(at);
                            continue;
                        }
                        // Duplicates can only be the first events queued.
                        backlog.clear();
                        return Some((Item::Event(event), (receiver, group, backlog)));
                    }
                    Err(RecvError::Lagged(missed)) => {
                        return Some((Item::Lagged(missed), (receiver, group, backlog)));
                    }
                    Err(RecvError::Closed) => return None,
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::logged_path;

    #[test]
    fn calendar_paths_are_hidden_from_logs() {
        assert_eq!(logged_path("/ics/g1/s3cret.ics"), "/ics/<hidden>");
        assert_eq!(logged_path("/ics/g1/s3cret/u1.ics"), "/ics/<hidden>");
        assert_eq!(
            logged_path("/api/groups/g1/ledger"),
            "/api/groups/g1/ledger"
        );
    }
}
