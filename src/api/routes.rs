use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::State,
    middleware,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use tower_http::trace::TraceLayer;

use crate::client::sekai_client::RegionHealth;
use crate::AppState as MainAppState;

use super::apis;
use super::image;
use super::middleware::auth_middleware;

static START_TIME: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub uptime_secs: u64,
    pub regions: BTreeMap<String, RegionHealth>,
}

#[derive(Debug, Serialize)]
pub struct RegionsHealthResponse {
    pub status: &'static str,
    pub regions: BTreeMap<String, RegionHealth>,
}

fn build_regions_health(state: &MainAppState) -> BTreeMap<String, RegionHealth> {
    let mut regions = BTreeMap::new();
    for (region, server_cfg) in &state.config.servers {
        let snapshot = if let Some(client) = state.clients.get(region) {
            client.health_snapshot()
        } else {
            RegionHealth {
                enabled: server_cfg.enabled,
                active_sessions: 0,
                proxy_configured: server_cfg.resolve_proxy(&state.config.proxy).is_some(),
                last_login_ok_at: None,
                last_login_error: if server_cfg.enabled {
                    Some("client not initialized".to_string())
                } else {
                    None
                },
                last_login_error_at: None,
            }
        };
        regions.insert(region.as_str().to_string(), snapshot);
    }
    regions
}

pub async fn health_check(State(state): State<Arc<MainAppState>>) -> Json<HealthResponse> {
    let start = START_TIME.get_or_init(Instant::now);
    let uptime = start.elapsed().as_secs();
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: uptime,
        regions: build_regions_health(&state),
    })
}

pub async fn health_regions(
    State(state): State<Arc<MainAppState>>,
) -> Json<RegionsHealthResponse> {
    Json(RegionsHealthResponse {
        status: "ok",
        regions: build_regions_health(&state),
    })
}

pub fn create_router(state: Arc<MainAppState>) -> Router {
    START_TIME.get_or_init(Instant::now);

    let public_routes = Router::new()
        .route("/health", get(health_check))
        .route("/health/regions", get(health_regions))
        .route(
            "/image/{server}/mysekai/{param1}/{param2}",
            get(image::get_mysekai_image),
        )
        .route(
            "/image/{server}/mysekai-housing/{hash1}/{hash2}",
            get(image::get_mysekai_housing_thumbnail),
        );

    let api_routes = Router::new()
        .route("/{server}/{user_id}/profile", get(apis::get_user_profile))
        .route("/{server}/system", get(apis::get_system))
        .route("/{server}/information", get(apis::get_information))
        .route(
            "/{server}/reload-accounts",
            post(apis::post_reload_accounts),
        )
        .route(
            "/{server}/user/mysekai/{target_user_id}/room",
            post(apis::post_mysekai_room),
        )
        .route(
            "/{server}/user/mysekai/housing-competition/{housing_id}/list",
            get(apis::get_mysekai_housing_competition_list),
        )
        .route(
            "/{server}/user/mysekai/housing-competition/{housing_id}/mysekai-owner/{owner_user_id}/entry",
            post(apis::post_mysekai_housing_competition_entry),
        )
        .route(
            "/{server}/event/{event_id}/ranking-top100",
            get(apis::get_event_ranking_top100),
        )
        .route(
            "/{server}/event/{event_id}/ranking-border",
            get(apis::get_event_ranking_border),
        )
        .route(
            "/{server}/custom-music-score/published/search/{score_id}",
            get(apis::get_custom_music_score_published_search_id),
        )
        .route(
            "/{server}/custom-music-score/{score_id}/full",
            get(apis::get_custom_music_score_full),
        )
        .route(
            "/{server}/custom-music-score/{score_id}/preview",
            get(apis::get_custom_music_score_preview),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .merge(public_routes)
        .nest("/api", api_routes)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::build_regions_health;
    use crate::config::{
        BackendConfig, Config, DatabaseConfig, GitConfig, LoginProtocol, RedisConfig, ServerConfig,
        ServerRegion,
    };
    use std::collections::HashMap;

    fn dummy_server(enabled: bool, proxy: Option<&str>) -> ServerConfig {
        ServerConfig {
            enabled,
            master_dir: String::new(),
            version_path: String::new(),
            account_dir: String::new(),
            api_url: String::new(),
            nuverse_master_data_url: String::new(),
            nuverse_structure_file_path: String::new(),
            require_cookies: false,
            headers: HashMap::new(),
            aes_key_hex: String::new(),
            aes_iv_hex: String::new(),
            enable_master_updater: false,
            master_updater_cron: String::new(),
            enable_app_hash_updater: false,
            app_hash_updater_cron: String::new(),
            remote_version_url: String::new(),
            proxy: proxy.map(str::to_string),
            login_protocol: LoginProtocol::V1,
        }
    }

    #[test]
    fn build_regions_health_reports_disabled_and_uninitialized_regions() {
        let mut servers = HashMap::new();
        servers.insert(ServerRegion::Jp, dummy_server(false, None));
        servers.insert(
            ServerRegion::Cn,
            dummy_server(true, Some("http://vpngate-cn:8888")),
        );

        let cfg = Config {
            proxy: String::new(),
            servers_dir: String::new(),
            jp_sekai_cookie_url: String::new(),
            git: GitConfig::default(),
            redis: RedisConfig::default(),
            backend: BackendConfig {
                host: "0.0.0.0".to_string(),
                port: 9999,
                ssl: false,
                ssl_cert: String::new(),
                ssl_key: String::new(),
                log_level: "info".to_string(),
                main_log_file: String::new(),
                access_log: String::new(),
                access_log_path: String::new(),
                sekai_user_jwt_signing_key: String::new(),
                enable_trust_proxy: false,
                trusted_proxies: Vec::new(),
                proxy_header: String::new(),
            },
            database: DatabaseConfig::default(),
            master_database: DatabaseConfig::default(),
            apphash_sources: Vec::new(),
            asset_updater_servers: Vec::new(),
            servers,
        };

        let state = crate::AppState {
            config: cfg,
            clients: HashMap::new(),
            db: None,
            master_db: None,
            redis: None,
            jwt_secret: None,
        };

        let health = build_regions_health(&state);
        assert!(!health["jp"].enabled);
        assert_eq!(health["jp"].active_sessions, 0);
        assert_eq!(health["jp"].last_login_error, None);

        assert!(health["cn"].enabled);
        assert!(health["cn"].proxy_configured);
        assert_eq!(health["cn"].active_sessions, 0);
        assert_eq!(
            health["cn"].last_login_error.as_deref(),
            Some("client not initialized")
        );
    }
}
