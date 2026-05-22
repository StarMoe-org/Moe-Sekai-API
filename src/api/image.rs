use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use regex::Regex;

use crate::config::ServerRegion;
use crate::AppState;

fn sniff_image_content_type(upstream: &str, bytes: &[u8]) -> String {
    if upstream.starts_with("image/") {
        return upstream.to_string();
    }
    if bytes.len() >= 3 && bytes[..3] == [0xff, 0xd8, 0xff] {
        return "image/jpeg".to_string();
    }
    if bytes.len() >= 8 && bytes[..8] == [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a] {
        return "image/png".to_string();
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "image/webp".to_string();
    }
    if !upstream.is_empty() {
        upstream.to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

pub async fn get_mysekai_image(
    State(state): State<std::sync::Arc<AppState>>,
    Path((server, param1, param2)): Path<(String, String, String)>,
) -> Response {
    let region: ServerRegion = match server.parse() {
        Ok(r) => r,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid server: {}", server),
            )
                .into_response();
        }
    };
    let Some(client) = state.clients.get(&region) else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Server not initialized").into_response();
    };
    static HEX64: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static DIGITS: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let hex64 = HEX64.get_or_init(|| Regex::new(r"^[a-f0-9]{64}$").unwrap());
    let digits = DIGITS.get_or_init(|| Regex::new(r"^\d+$").unwrap());
    let image_result: Result<(Vec<u8>, String), _> = if region.is_cp_server() {
        if !hex64.is_match(&param1) || !hex64.is_match(&param2) {
            return (
                StatusCode::BAD_REQUEST,
                "Invalid path format for colorful palette servers (expected 64-char hex)",
            )
                .into_response();
        }
        let combined = format!("{}/{}", param1, param2);
        client.get_cp_mysekai_image(&combined).await
    } else {
        if !digits.is_match(&param1) || !digits.is_match(&param2) {
            return (
                StatusCode::BAD_REQUEST,
                "Invalid path format for nuverse servers (expected numeric user_id and index)",
            )
                .into_response();
        }
        client
            .get_nuverse_mysekai_image(&param1, &param2)
            .await
            .map(|bytes| (bytes, "image/png".to_string()))
    };
    match image_result {
        Ok((bytes, content_type)) => {
            let ct = sniff_image_content_type(&content_type, &bytes);
            (StatusCode::OK, [("content-type", ct)], bytes).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("Fetch image failed: {}", e),
        )
            .into_response(),
    }
}

pub async fn get_mysekai_housing_thumbnail(
    State(state): State<std::sync::Arc<AppState>>,
    Path((server, hash1, hash2)): Path<(String, String, String)>,
) -> Response {
    let region: ServerRegion = match server.parse() {
        Ok(r) => r,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid server: {}", server),
            )
                .into_response();
        }
    };
    if !region.is_cp_server() {
        return (
            StatusCode::BAD_REQUEST,
            "mysekai housing thumbnail is only supported on colorful palette servers (jp/en)",
        )
            .into_response();
    }
    let Some(client) = state.clients.get(&region) else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Server not initialized").into_response();
    };
    static HEX64: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static UUID_LC: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let hex64 = HEX64.get_or_init(|| Regex::new(r"^[a-f0-9]{64}$").unwrap());
    let uuid_lc = UUID_LC.get_or_init(|| {
        Regex::new(r"^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$").unwrap()
    });
    if !hex64.is_match(&hash1) || !uuid_lc.is_match(&hash2) {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid path format (expected 64-char hex / lowercase uuid)",
        )
            .into_response();
    }
    let combined = format!("{}/{}", hash1, hash2);
    match client.get_cp_mysekai_housing_thumbnail(&combined).await {
        Ok((bytes, content_type)) => {
            let ct = sniff_image_content_type(&content_type, &bytes);
            (StatusCode::OK, [("content-type", ct)], bytes).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("Fetch image failed: {}", e),
        )
            .into_response(),
    }
}
