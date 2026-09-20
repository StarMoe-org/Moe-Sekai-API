use std::sync::Arc;

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

use crate::client::account::MYSEKAI_PROXY_ROLE;
use crate::config::ServerRegion;
use crate::error::AppError;
use crate::AppState;

pub struct ApiResponse {
    status: StatusCode,
    body: JsonValue,
}

impl IntoResponse for ApiResponse {
    fn into_response(self) -> Response {
        let json = sonic_rs::to_string(&self.body).unwrap_or_else(|_| "{}".to_string());
        (self.status, [("content-type", "application/json")], json).into_response()
    }
}

fn get_client(state: &AppState, server: &str) -> Result<Arc<crate::client::SekaiClient>, AppError> {
    let region: ServerRegion = server
        .parse()
        .map_err(|_| AppError::InvalidServerRegion(server.to_string()))?;

    state
        .clients
        .get(&region)
        .cloned()
        .ok_or(AppError::NoClientAvailable)
}

#[derive(Debug, Deserialize)]
pub struct MySekaiRoomQuery {
    #[serde(rename = "mysekaiVisitType", default)]
    pub mysekai_visit_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MySekaiHousingCompetitionListQuery {
    #[serde(rename = "isLottery", default)]
    pub is_lottery: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MySekaiHousingCompetitionEntryQuery {
    #[serde(rename = "isBackNumber", default)]
    pub is_back_number: Option<String>,
    #[serde(rename = "mysekaiOwnerUserSubmittedAt")]
    pub mysekai_owner_user_submitted_at: i64,
}

fn get_jp_client(
    state: &AppState,
    server: &str,
) -> Result<Arc<crate::client::SekaiClient>, AppError> {
    let region: ServerRegion = server
        .parse()
        .map_err(|_| AppError::InvalidServerRegion(server.to_string()))?;
    if region != ServerRegion::Jp {
        return Err(AppError::BadRequest(
            "custom music score endpoints are only supported for jp".to_string(),
        ));
    }

    state
        .clients
        .get(&region)
        .cloned()
        .ok_or(AppError::NoClientAvailable)
}

async fn proxy_game_api(
    state: &AppState,
    server: &str,
    path: &str,
) -> Result<ApiResponse, AppError> {
    let client = get_client(state, server)?;
    let (data, status) = client.get_game_api(path, None).await?;

    Ok(ApiResponse {
        status: StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        body: data,
    })
}

async fn post_game_api_with_role(
    state: &AppState,
    server: &str,
    path: &str,
    params: Option<&HashMap<String, String>>,
    role: &str,
) -> Result<ApiResponse, AppError> {
    let client = get_client(state, server)?;
    let (data, status) = client.post_game_api_with_role(path, params, role).await?;

    Ok(ApiResponse {
        status: StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        body: data,
    })
}

async fn post_game_api_body_with_role(
    state: &AppState,
    server: &str,
    path: &str,
    body: &JsonValue,
    params: Option<&HashMap<String, String>>,
    role: &str,
) -> Result<ApiResponse, AppError> {
    let client = get_client(state, server)?;
    let (data, status) = client
        .post_game_api_with_body_and_role(path, body, params, role)
        .await?;

    Ok(ApiResponse {
        status: StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        body: data,
    })
}

async fn get_game_api_with_role(
    state: &AppState,
    server: &str,
    path: &str,
    params: Option<&HashMap<String, String>>,
    role: &str,
) -> Result<ApiResponse, AppError> {
    let client = get_client(state, server)?;
    let (data, status) = client.get_game_api_with_role(path, params, role).await?;

    Ok(ApiResponse {
        status: StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        body: data,
    })
}

pub async fn get_user_profile(
    State(state): State<Arc<AppState>>,
    axum::Extension(auth_user): axum::Extension<Option<crate::api::middleware::AuthUser>>,
    Path((server, user_id)): Path<(String, String)>,
) -> Result<ApiResponse, AppError> {
    if !user_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::ParseError("user_id must be numeric".to_string()));
    }
    if let Some(user) = auth_user {
        tracing::debug!("User {} requesting profile for {}", user.id, user_id);
    }
    let path = format!("/user/{{userId}}/{}/profile", user_id);
    proxy_game_api(&state, &server, &path).await
}

pub async fn get_system(
    State(state): State<Arc<AppState>>,
    Path(server): Path<String>,
) -> Result<ApiResponse, AppError> {
    proxy_game_api(&state, &server, "/system").await
}

pub async fn get_information(
    State(state): State<Arc<AppState>>,
    Path(server): Path<String>,
) -> Result<ApiResponse, AppError> {
    proxy_game_api(&state, &server, "/information").await
}

pub async fn post_mysekai_room(
    State(state): State<Arc<AppState>>,
    Path((server, target_user_id)): Path<(String, String)>,
    Query(query): Query<MySekaiRoomQuery>,
) -> Result<ApiResponse, AppError> {
    if !target_user_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::ParseError(
            "target_user_id must be numeric".to_string(),
        ));
    }

    let visit_type = query
        .mysekai_visit_type
        .unwrap_or_else(|| "id_search".to_string());
    if visit_type != "id_search" {
        return Err(AppError::BadRequest(
            "mysekaiVisitType must be id_search".to_string(),
        ));
    }

    let path = format!("/user/{{userId}}/mysekai/{}/room/entry", target_user_id);
    let mut params = HashMap::new();
    params.insert("mysekaiVisitType".to_string(), visit_type);
    post_game_api_with_role(&state, &server, &path, Some(&params), MYSEKAI_PROXY_ROLE).await
}

pub async fn get_mysekai_housing_competition_list(
    State(state): State<Arc<AppState>>,
    Path((server, housing_id)): Path<(String, String)>,
    Query(query): Query<MySekaiHousingCompetitionListQuery>,
) -> Result<ApiResponse, AppError> {
    if !housing_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::ParseError(
            "housing_id must be numeric".to_string(),
        ));
    }

    let is_lottery = match query.is_lottery.as_deref() {
        None | Some("") => false,
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "true" => true,
            "false" => false,
            _ => {
                return Err(AppError::BadRequest(
                    "isLottery must be true or false".to_string(),
                ));
            }
        },
    };

    let path = format!(
        "/user/{{userId}}/mysekai/housing-competition/{}/list",
        housing_id
    );
    let mut params = HashMap::new();
    params.insert(
        "isLottery".to_string(),
        if is_lottery { "True" } else { "False" }.to_string(),
    );
    get_game_api_with_role(&state, &server, &path, Some(&params), MYSEKAI_PROXY_ROLE).await
}

pub async fn post_mysekai_housing_competition_entry(
    State(state): State<Arc<AppState>>,
    Path((server, housing_id, owner_user_id)): Path<(String, String, String)>,
    Query(query): Query<MySekaiHousingCompetitionEntryQuery>,
) -> Result<ApiResponse, AppError> {
    if !housing_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::ParseError(
            "housing_id must be numeric".to_string(),
        ));
    }
    if !owner_user_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::ParseError(
            "owner_user_id must be numeric".to_string(),
        ));
    }

    let is_back_number = match query.is_back_number.as_deref() {
        None | Some("") => false,
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "true" => true,
            "false" => false,
            _ => {
                return Err(AppError::BadRequest(
                    "isBackNumber must be true or false".to_string(),
                ));
            }
        },
    };
    let body = json!({
        "isBackNumber": is_back_number,
        "mysekaiOwnerUserSubmittedAt": query.mysekai_owner_user_submitted_at,
    });

    let path = format!(
        "/user/{{userId}}/mysekai/housing-competition/{}/mysekai-owner/{}/entry",
        housing_id, owner_user_id
    );
    post_game_api_body_with_role(&state, &server, &path, &body, None, MYSEKAI_PROXY_ROLE).await
}

pub async fn get_event_ranking_top100(
    State(state): State<Arc<AppState>>,
    Path((server, event_id)): Path<(String, String)>,
) -> Result<ApiResponse, AppError> {
    if !event_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::ParseError("event_id must be numeric".to_string()));
    }
    let path = format!(
        "/user/{{userId}}/event/{}/ranking?rankingViewType=top100",
        event_id
    );
    let mut resp = proxy_game_api(&state, &server, &path).await?;

    // Nuverse servers (TW/KR/CN) return userCard as a flat array; restore to keyed dict
    let region: ServerRegion = server
        .parse()
        .map_err(|_| AppError::InvalidServerRegion(server.to_string()))?;
    if !region.is_cp_server() {
        crate::client::nuverse::restore_ranking_user_cards(&mut resp.body);
    }

    Ok(resp)
}

pub async fn get_custom_music_score_published_search_id(
    State(state): State<Arc<AppState>>,
    Path((server, score_id)): Path<(String, String)>,
) -> Result<ApiResponse, AppError> {
    get_jp_client(&state, &server)?;
    let path = format!(
        "/user/{{userId}}/custom-music-score/published/search/{}",
        score_id
    );
    proxy_game_api(&state, &server, &path).await
}

pub async fn get_custom_music_score_full(
    State(state): State<Arc<AppState>>,
    Path((server, score_id)): Path<(String, String)>,
) -> Result<ApiResponse, AppError> {
    get_custom_music_score_resource(&state, &server, &score_id, "full").await
}

pub async fn get_custom_music_score_preview(
    State(state): State<Arc<AppState>>,
    Path((server, score_id)): Path<(String, String)>,
) -> Result<ApiResponse, AppError> {
    get_custom_music_score_resource(&state, &server, &score_id, "preview").await
}

async fn get_custom_music_score_resource(
    state: &AppState,
    server: &str,
    score_id: &str,
    kind: &str,
) -> Result<ApiResponse, AppError> {
    let client = get_jp_client(state, server)?;
    let detail_path = format!(
        "/user/{{userId}}/custom-music-score/published/search/{}",
        score_id
    );
    let (detail, _) = client.get_game_api(&detail_path, None).await?;

    if detail
        .get("customMusicScoreOfficialCreatorPublishedResponseJson")
        .is_some()
    {
        return Err(AppError::BadRequest(
            "official creator custom music score does not expose userCustomMusicScorePath"
                .to_string(),
        ));
    }

    let score_path = detail
        .pointer(
            "/userCustomMusicScoreInfoJson/userCustomMusicScoreInfoJson/userCustomMusicScorePath",
        )
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::ParseError("missing userCustomMusicScorePath".to_string()))?;

    let blob = client
        .get_jp_custom_music_score_blob_text(kind, score_path)
        .await?;
    let body = crate::client::SekaiClient::decode_custom_music_score_blob_text(&blob)?;

    Ok(ApiResponse {
        status: StatusCode::OK,
        body,
    })
}

pub async fn get_event_ranking_border(
    State(state): State<Arc<AppState>>,
    Path((server, event_id)): Path<(String, String)>,
) -> Result<ApiResponse, AppError> {
    if !event_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::ParseError("event_id must be numeric".to_string()));
    }
    let path = format!("/user/{{userId}}/event/{}/ranking-border", event_id);
    let mut resp = proxy_game_api(&state, &server, &path).await?;

    // Nuverse servers (TW/KR/CN) return userCard as a flat array; restore to keyed dict
    let region: ServerRegion = server
        .parse()
        .map_err(|_| AppError::InvalidServerRegion(server.to_string()))?;
    if !region.is_cp_server() {
        crate::client::nuverse::restore_ranking_user_cards(&mut resp.body);
    }

    Ok(resp)
}
