use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use parking_lot::{Mutex, RwLock};
use reqwest::{Client, Response};
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::config::{ServerConfig, ServerRegion};
use crate::crypto::SekaiCryptor;
use crate::error::{AppError, SekaiHttpStatus};

use super::account::{
    AccountType, SekaiAccount, SekaiAccountCP, SekaiAccountNuverse, DEFAULT_PROXY_ROLE,
    MYSEKAI_PROXY_ROLE,
};
use super::helper::{CookieHelper, VersionHelper, VersionInfo};
use super::session::AccountSession;
use super::token_utils;

pub struct SekaiClient {
    pub region: ServerRegion,
    pub config: ServerConfig,
    pub cookie_helper: Option<Arc<CookieHelper>>,
    pub version_helper: Arc<VersionHelper>,
    pub proxy: Option<String>,
    pub cryptor: SekaiCryptor,
    pub headers: Arc<Mutex<HashMap<String, String>>>,
    pub http_client: Client,

    sessions: Arc<RwLock<Vec<Arc<AccountSession>>>>,
    session_index: AtomicUsize,
    reload_lock: Arc<tokio::sync::Mutex<()>>,
    recovery_started: Arc<AtomicBool>,
}

struct RecoveryFlagGuard {
    flag: Arc<AtomicBool>,
}

impl Drop for RecoveryFlagGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SessionPoolReplacement {
    EmptyCandidate { active: usize },
    Replaced { active: usize },
}

struct ParsedAccounts {
    accounts: Vec<AccountType>,
    complete: bool,
}

const INITIAL_RECOVERY_DELAY: Duration = Duration::from_secs(5);
const MAX_RECOVERY_DELAY: Duration = Duration::from_secs(300);

fn next_recovery_delay(current: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_RECOVERY_DELAY)
}

fn try_claim_recovery(flag: &AtomicBool) -> bool {
    flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

fn should_wait_for_complete_parse(parse_complete: bool) -> bool {
    !parse_complete
}

fn is_account_change_event(event: &Result<notify::Event, notify::Error>, region: &str) -> bool {
    use notify::EventKind;

    match event {
        Ok(event) => matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        ),
        Err(e) => {
            error!("{} File watcher error: {}", region, e);
            false
        }
    }
}

fn replace_session_pool(
    sessions: &RwLock<Vec<Arc<AccountSession>>>,
    session_index: &AtomicUsize,
    new_sessions: Vec<Arc<AccountSession>>,
) -> SessionPoolReplacement {
    if new_sessions.is_empty() {
        return SessionPoolReplacement::EmptyCandidate {
            active: sessions.read().len(),
        };
    }

    let mut active_sessions = sessions.write();
    *active_sessions = new_sessions;
    session_index.store(0, Ordering::SeqCst);
    SessionPoolReplacement::Replaced {
        active: active_sessions.len(),
    }
}

impl SekaiClient {
    pub async fn new(
        region: ServerRegion,
        config: ServerConfig,
        proxy: Option<String>,
        jp_cookie_url: Option<String>,
    ) -> Result<Self, AppError> {
        let cryptor = SekaiCryptor::from_hex(&config.aes_key_hex, &config.aes_iv_hex)?;
        let mut headers = HashMap::new();
        for (k, v) in &config.headers {
            headers.insert(k.clone(), v.clone());
        }
        let mut client_builder = Client::builder()
            .timeout(Duration::from_secs(45))
            .pool_max_idle_per_host(20)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60));
        if let Some(ref proxy_url) = proxy {
            if !proxy_url.is_empty() {
                client_builder =
                    client_builder
                        .proxy(reqwest::Proxy::all(proxy_url).map_err(|e| {
                            AppError::NetworkError(format!("Invalid proxy: {}", e))
                        })?);
            }
        }
        let http_client = client_builder
            .build()
            .map_err(|e| AppError::NetworkError(e.to_string()))?;
        let version_helper = Arc::new(VersionHelper::new(&config.version_path));
        let cookie_helper = if region == ServerRegion::Jp && config.require_cookies {
            jp_cookie_url
                .filter(|url| !url.is_empty())
                .map(|url| Arc::new(CookieHelper::new(&url)))
        } else {
            None
        };
        let client = Self {
            region,
            config,
            cookie_helper,
            version_helper,
            proxy,
            cryptor,
            headers: Arc::new(Mutex::new(headers)),
            http_client,
            sessions: Arc::new(RwLock::new(Vec::new())),
            session_index: AtomicUsize::new(0),
            reload_lock: Arc::new(tokio::sync::Mutex::new(())),
            recovery_started: Arc::new(AtomicBool::new(false)),
        };
        Ok(client)
    }

    pub async fn init(&self) -> Result<(), AppError> {
        info!(
            "{} Initializing client...",
            self.region.as_str().to_uppercase()
        );
        if let Some(ref helper) = self.cookie_helper {
            let cookie = helper.get_cookies(self.proxy.as_deref()).await?;
            self.headers.lock().insert("Cookie".to_string(), cookie);
        }
        let version = self.version_helper.load().await?;
        self.update_version_headers(&version);
        self.reload_accounts().await?;
        info!(
            "{} Client initialized with {} sessions",
            self.region.as_str().to_uppercase(),
            self.sessions.read().len()
        );
        Ok(())
    }

    async fn build_session_pool(&self, accounts: Vec<AccountType>) -> Vec<Arc<AccountSession>> {
        let mut sessions = Vec::new();
        let mut upgrade_refreshed = false;
        for account in accounts {
            if self.region.is_cp_server() && account.user_id().is_empty() {
                warn!(
                    "{} Skipping account with empty user_id",
                    self.region.as_str().to_uppercase()
                );
                continue;
            }
            let session = Arc::new(AccountSession::new(account));
            match self.login(&session).await {
                Ok(_) => {
                    sessions.push(session);
                }
                Err(AppError::UpgradeRequired) if !upgrade_refreshed => {
                    upgrade_refreshed = true;
                    warn!(
                        "{} Login returned 426 during account reload, refreshing version...",
                        self.region.as_str().to_uppercase()
                    );
                    if let Err(e) = self.refresh_version_from_remote().await {
                        error!(
                            "{} Failed to refresh version: {}",
                            self.region.as_str().to_uppercase(),
                            e
                        );
                        continue;
                    }
                    match self.login(&session).await {
                        Ok(login_resp) => {
                            self.update_version_headers_from_login(&login_resp);
                            sessions.push(session);
                        }
                        Err(AppError::UpgradeRequired) => {
                            warn!(
                                "{} Still 426 after version refresh, waiting for app version update...",
                                self.region.as_str().to_uppercase()
                            );
                            tokio::time::sleep(Duration::from_secs(10)).await;
                            if let Err(e) = self.refresh_version_from_remote().await {
                                error!(
                                    "{} Failed to refresh version after wait: {}",
                                    self.region.as_str().to_uppercase(),
                                    e
                                );
                                continue;
                            }
                            match self.login(&session).await {
                                Ok(login_resp) => {
                                    self.update_version_headers_from_login(&login_resp);
                                    sessions.push(session);
                                }
                                Err(e) => {
                                    error!(
                                        "{} Login failed after waiting for app update: {}",
                                        self.region.as_str().to_uppercase(),
                                        e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            error!(
                                "{} Re-login after version refresh failed: {}",
                                self.region.as_str().to_uppercase(),
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "{} Failed to login account: {}",
                        self.region.as_str().to_uppercase(),
                        e
                    );
                }
            }
        }
        sessions
    }

    fn update_version_headers(&self, version: &VersionInfo) {
        let mut headers = self.headers.lock();
        headers.insert("X-App-Version".to_string(), version.app_version.clone());
        headers.insert("X-Data-Version".to_string(), version.data_version.clone());
        headers.insert("X-Asset-Version".to_string(), version.asset_version.clone());
        headers.insert("X-App-Hash".to_string(), version.app_hash.clone());
    }

    fn update_version_headers_from_login(&self, login: &LoginResponse) {
        let mut headers = self.headers.lock();
        if !login.data_version.is_empty() {
            headers.insert("X-Data-Version".to_string(), login.data_version.clone());
        }
        if !login.asset_version.is_empty() {
            headers.insert("X-Asset-Version".to_string(), login.asset_version.clone());
        }
        info!(
            "{} Updated version headers from login: dataVersion={}, assetVersion={}",
            self.region.as_str().to_uppercase(),
            login.data_version,
            login.asset_version
        );
    }

    pub async fn refresh_version(&self) -> Result<(), AppError> {
        let version = self.version_helper.load().await?;
        self.update_version_headers(&version);
        Ok(())
    }

    pub async fn refresh_version_from_remote(&self) -> Result<(), AppError> {
        let url = if !self.config.remote_version_url.is_empty() {
            self.config.remote_version_url.clone()
        } else {
            Self::default_remote_version_url(self.region).to_string()
        };
        if url.is_empty() {
            return self.refresh_version().await;
        }
        info!(
            "{} Fetching remote version from {}",
            self.region.as_str().to_uppercase(),
            url
        );
        match self
            .version_helper
            .fetch_and_update_from_remote(&url, self.proxy.as_deref())
            .await
        {
            Ok(version) => {
                info!(
                    "{} Remote version fetched: appVersion={}, appHash={}",
                    self.region.as_str().to_uppercase(),
                    version.app_version,
                    &version.app_hash[..version.app_hash.len().min(16)]
                );
                self.update_version_headers(&version);
                Ok(())
            }
            Err(e) => {
                warn!(
                    "{} Failed to fetch remote version: {}, falling back to local",
                    self.region.as_str().to_uppercase(),
                    e
                );
                self.refresh_version().await
            }
        }
    }

    fn default_remote_version_url(region: crate::config::ServerRegion) -> &'static str {
        use crate::config::ServerRegion;
        match region {
            ServerRegion::Jp => "https://raw.githubusercontent.com/Team-Haruki/haruki-sekai-master/main/versions/current_version.json",
            ServerRegion::En => "https://raw.githubusercontent.com/Team-Haruki/haruki-sekai-en-master/main/versions/current_version.json",
            ServerRegion::Tw => "https://raw.githubusercontent.com/Team-Haruki/haruki-sekai-tc-master/main/versions/current_version.json",
            ServerRegion::Kr => "https://raw.githubusercontent.com/Team-Haruki/haruki-sekai-kr-master/main/versions/current_version.json",
            ServerRegion::Cn => "https://raw.githubusercontent.com/Team-Haruki/haruki-sekai-sc-master/main/versions/current_version.json",
        }
    }

    pub async fn refresh_cookies(&self) -> Result<(), AppError> {
        if let Some(ref helper) = self.cookie_helper {
            let cookie = helper.get_cookies(self.proxy.as_deref()).await?;
            self.headers.lock().insert("Cookie".to_string(), cookie);
        }
        Ok(())
    }

    pub async fn reload_accounts(&self) -> Result<(), AppError> {
        let _reload_guard = self.reload_lock.lock().await;
        self.reload_accounts_locked().await
    }

    async fn reload_accounts_locked(&self) -> Result<(), AppError> {
        info!(
            "{} Reloading accounts...",
            self.region.as_str().to_uppercase()
        );

        let parsed = match self.parse_accounts() {
            Ok(parsed) => parsed,
            Err(e) => {
                warn!(
                    "{} Failed to parse accounts, keeping {} active sessions: {}",
                    self.region.as_str().to_uppercase(),
                    self.session_count(),
                    e
                );
                return Ok(());
            }
        };
        if parsed.accounts.is_empty() {
            warn!(
                "{} No accounts found in {}, keeping {} active sessions",
                self.region.as_str().to_uppercase(),
                self.config.account_dir,
                self.session_count()
            );
            return Ok(());
        }
        if should_wait_for_complete_parse(parsed.complete) {
            warn!(
                "{} Account files were only partially parsed, keeping {} active sessions",
                self.region.as_str().to_uppercase(),
                self.session_count()
            );
            return Ok(());
        }

        let new_sessions = self.build_session_pool(parsed.accounts).await;
        match replace_session_pool(&self.sessions, &self.session_index, new_sessions) {
            SessionPoolReplacement::EmptyCandidate { active } => {
                warn!(
                    "{} No accounts logged in successfully, keeping {} active sessions",
                    self.region.as_str().to_uppercase(),
                    active
                );
            }
            SessionPoolReplacement::Replaced { active } => {
                info!(
                    "{} Accounts reloaded, {} sessions active",
                    self.region.as_str().to_uppercase(),
                    active
                );
            }
        }
        Ok(())
    }

    pub fn session_count(&self) -> usize {
        self.sessions.read().len()
    }

    pub fn start_empty_pool_recovery(self: &Arc<Self>) {
        if self.session_count() > 0 || !try_claim_recovery(&self.recovery_started) {
            return;
        }

        let client = self.clone();
        let flag_guard = RecoveryFlagGuard {
            flag: self.recovery_started.clone(),
        };
        tokio::spawn(async move {
            let _flag_guard = flag_guard;
            let mut delay = INITIAL_RECOVERY_DELAY;
            loop {
                tokio::time::sleep(delay).await;
                if client.session_count() > 0 {
                    return;
                }

                warn!(
                    "{} Session pool is empty, retrying account login",
                    client.region.as_str().to_uppercase()
                );
                {
                    let _reload_guard = client.reload_lock.lock().await;
                    if client.session_count() > 0 {
                        return;
                    }
                    if let Err(e) = client.reload_accounts_locked().await {
                        error!(
                            "{} Empty session pool recovery failed: {}",
                            client.region.as_str().to_uppercase(),
                            e
                        );
                    }
                }
                if client.session_count() > 0 {
                    info!(
                        "{} Session pool recovered successfully",
                        client.region.as_str().to_uppercase()
                    );
                    return;
                }
                delay = next_recovery_delay(delay);
            }
        });
    }

    pub fn start_file_watcher(self: Arc<Self>) -> Result<(), AppError> {
        use notify::{Config, PollWatcher, RecursiveMode, Watcher};
        use std::sync::mpsc::channel;

        let account_dir = self.config.account_dir.clone();
        if account_dir.is_empty() || !Path::new(&account_dir).exists() {
            warn!(
                "{} Account directory not found: {}, skipping file watcher",
                self.region.as_str().to_uppercase(),
                account_dir
            );
            return Ok(());
        }
        let (tx, rx) = channel();
        let config = Config::default().with_poll_interval(Duration::from_secs(5));
        let mut watcher = PollWatcher::new(tx, config)
            .map_err(|e| AppError::Internal(format!("Failed to create file watcher: {}", e)))?;
        watcher
            .watch(Path::new(&account_dir), RecursiveMode::NonRecursive)
            .map_err(|e| AppError::Internal(format!("Failed to watch directory: {}", e)))?;
        let client = self.clone();
        let region_str = self.region.as_str().to_uppercase();
        std::thread::spawn(move || {
            let _watcher = watcher;
            info!(
                "{} File watcher started for {} (polling mode, 5s interval)",
                region_str, account_dir
            );
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime for file watcher");
            // Wait longer than one poll interval so a file write spanning polls is stable.
            let settle_duration = Duration::from_secs(6);
            while let Ok(res) = rx.recv() {
                if !is_account_change_event(&res, &region_str) {
                    continue;
                }

                let mut changed_paths = match res {
                    Ok(event) => event.paths,
                    Err(_) => Vec::new(),
                };
                let mut quiet_deadline = std::time::Instant::now() + settle_duration;
                loop {
                    let remaining =
                        quiet_deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(remaining) {
                        Ok(next) => {
                            if is_account_change_event(&next, &region_str) {
                                if let Ok(event) = next {
                                    changed_paths.extend(event.paths);
                                    quiet_deadline = std::time::Instant::now() + settle_duration;
                                }
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                        | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }

                info!(
                    "{} Account files settled after changes: {:?}",
                    region_str, changed_paths
                );
                let client_clone = client.clone();
                rt.block_on(async {
                    if let Err(e) = client_clone.reload_accounts().await {
                        error!("{} Failed to reload accounts: {}", region_str, e);
                    }
                });
            }
        });
        Ok(())
    }

    fn parse_accounts(&self) -> Result<ParsedAccounts, AppError> {
        let mut accounts = Vec::new();
        let mut complete = true;
        let account_dir = Path::new(&self.config.account_dir);
        if !account_dir.exists() {
            return Ok(ParsedAccounts { accounts, complete });
        }
        let entries = fs::read_dir(account_dir)
            .map_err(|e| AppError::ParseError(format!("Failed to read account dir: {}", e)))?;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    warn!("Failed to read account directory entry: {}", e);
                    complete = false;
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let data = match fs::read(&path) {
                Ok(d) => d,
                Err(e) => {
                    warn!("Failed to read {}: {}", path.display(), e);
                    complete = false;
                    continue;
                }
            };
            match self.parse_account_file(&path, &data) {
                Ok((mut parsed, file_complete)) => {
                    accounts.append(&mut parsed);
                    complete &= file_complete;
                }
                Err(e) => {
                    warn!("Failed to parse {}: {}", path.display(), e);
                    complete = false;
                }
            }
        }
        Ok(ParsedAccounts { accounts, complete })
    }

    fn parse_account_file(
        &self,
        path: &Path,
        data: &[u8],
    ) -> Result<(Vec<AccountType>, bool), AppError> {
        let value: serde_json::Value = sonic_rs::from_slice(data)
            .map_err(|e| AppError::ParseError(format!("JSON parse error: {}", e)))?;
        let mut accounts = Vec::new();
        let mut complete = true;
        match value {
            serde_json::Value::Array(arr) => {
                for (idx, item) in arr.into_iter().enumerate() {
                    if let Some(acc) = self.parse_account_value(item, path, Some(idx)) {
                        accounts.push(acc);
                    } else {
                        complete = false;
                    }
                }
            }
            serde_json::Value::Object(_) => {
                if let Some(acc) = self.parse_account_value(value, path, None) {
                    accounts.push(acc);
                } else {
                    complete = false;
                }
            }
            _ => complete = false,
        }
        Ok((accounts, complete))
    }

    fn parse_account_value(
        &self,
        value: serde_json::Value,
        path: &Path,
        idx: Option<usize>,
    ) -> Option<AccountType> {
        let log_prefix = if let Some(i) = idx {
            format!("[{}][{}]", path.display(), i)
        } else {
            format!("[{}]", path.display())
        };

        if self.region.is_cp_server() {
            let json_str = serde_json::to_string(&value).ok()?;
            match sonic_rs::from_str::<SekaiAccountCP>(&json_str) {
                Ok(mut acc) => {
                    if let Ok(user_id) = token_utils::extract_user_id_from_jwt(&acc.credential) {
                        debug!("{} Extracted user_id from JWT: {}", log_prefix, user_id);
                        acc.user_id = user_id;
                    } else if acc.user_id.is_empty() {
                        warn!(
                            "{} Failed to extract user_id from JWT and no fallback",
                            log_prefix
                        );
                    }
                    Some(AccountType::CP(acc))
                }
                Err(e) => {
                    warn!("{} CP unmarshal error: {}", log_prefix, e);
                    None
                }
            }
        } else {
            let json_str = serde_json::to_string(&value).ok()?;
            match sonic_rs::from_str::<SekaiAccountNuverse>(&json_str) {
                Ok(mut acc) => {
                    if let Ok(user_id) =
                        token_utils::extract_user_id_from_nuverse_token(&acc.access_token)
                    {
                        debug!(
                            "{} Extracted user_id from Nuverse token: {}",
                            log_prefix, user_id
                        );
                        acc.user_id = user_id;
                    } else if acc.user_id.is_empty() || acc.user_id == "0" {
                        warn!(
                            "{} Failed to extract user_id from Nuverse token and no fallback",
                            log_prefix
                        );
                    }
                    Some(AccountType::Nuverse(acc))
                }
                Err(e) => {
                    warn!("{} Nuverse unmarshal error: {}", log_prefix, e);
                    None
                }
            }
        }
    }

    #[must_use]
    pub fn get_session(&self) -> Option<Arc<AccountSession>> {
        self.get_session_for_role(DEFAULT_PROXY_ROLE).ok()
    }

    pub fn get_session_for_role(&self, role: &str) -> Result<Arc<AccountSession>, AppError> {
        let sessions = self.sessions.read();
        if sessions.is_empty() {
            return Err(AppError::NoClientAvailable);
        }
        let start = self.session_index.fetch_add(1, Ordering::SeqCst);
        for offset in 0..sessions.len() {
            let idx = (start + offset) % sessions.len();
            let session = sessions[idx].clone();
            if session.has_proxy_role(role) {
                return Ok(session);
            }
        }
        Err(AppError::NoProxyAccountForRole(role.to_string()))
    }

    fn prepare_request(
        &self,
        session: &AccountSession,
        method: reqwest::Method,
        url: &str,
    ) -> reqwest::RequestBuilder {
        let mut req = self.http_client.request(method, url);
        let headers = self.headers.lock();
        for (k, v) in headers.iter() {
            if k.to_lowercase() != "x-request-id" {
                req = req.header(k, v);
            }
        }
        if let Some(ref token) = session.get_session_token() {
            req = req.header("X-Session-Token", token);
        }
        req = req.header("X-Request-Id", Uuid::new_v4().to_string());
        req
    }

    fn update_session_token(&self, session: &AccountSession, resp: &Response) {
        if let Some(token) = resp.headers().get("x-session-token") {
            if let Ok(token_str) = token.to_str() {
                let old_token = session.get_session_token();
                session.set_session_token(Some(token_str.to_string()));
                debug!(
                    "Account #{} session token updated (old: {:?}, new: {}...)",
                    session.user_id(),
                    old_token.as_deref().map(|s| &s[..s.len().min(40)]),
                    &token_str[..token_str.len().min(40)]
                );
            }
        }
    }

    pub async fn call_api<T: serde::Serialize>(
        &self,
        session: &AccountSession,
        method: &str,
        path: &str,
        data: Option<&T>,
        params: Option<&HashMap<String, String>>,
    ) -> Result<Response, AppError> {
        let _lock = session.lock_api().await;
        let user_id = session.user_id().to_string();
        let url = format!("{}/api{}", self.config.api_url, path).replace("{userId}", &user_id);
        info!("Account #{} {} {}", user_id, method.to_uppercase(), path);
        let max_retries = 4;
        let mut last_error = None;
        for attempt in 1..=max_retries {
            let method_enum = match method.to_uppercase().as_str() {
                "GET" => reqwest::Method::GET,
                "POST" => reqwest::Method::POST,
                "PUT" => reqwest::Method::PUT,
                "DELETE" => reqwest::Method::DELETE,
                "PATCH" => reqwest::Method::PATCH,
                _ => reqwest::Method::GET,
            };
            let mut req = self.prepare_request(session, method_enum, &url);
            if path.contains("/mysekai/") {
                req = req.header("X-User-Execute-Location", "mysekai");
            }
            if let Some(p) = params {
                req = req.query(p);
            }
            if let Some(body_data) = data {
                let packed = self.cryptor.pack(body_data)?;
                req = req.body(packed);
            }
            match req.send().await {
                Ok(resp) => {
                    self.update_session_token(session, &resp);
                    return Ok(resp);
                }
                Err(e) => {
                    if e.is_timeout() {
                        warn!(
                            "Account #{} request timed out (attempt {}), retrying...",
                            session.user_id(),
                            attempt
                        );
                    } else {
                        error!(
                            "request error (attempt {}): server={}, err={}",
                            attempt,
                            self.region.as_str().to_uppercase(),
                            e
                        );
                    }
                    last_error = Some(AppError::NetworkError(e.to_string()));
                }
            }
            if attempt < max_retries {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        Err(last_error.unwrap_or(AppError::NetworkError(
            "Request failed after retries".to_string(),
        )))
    }

    pub async fn get(
        &self,
        session: &AccountSession,
        path: &str,
        params: Option<&HashMap<String, String>>,
    ) -> Result<Response, AppError> {
        self.call_api::<()>(session, "GET", path, None, params)
            .await
    }

    pub async fn call_api_with_raw_body(
        &self,
        session: &AccountSession,
        method: &str,
        path: &str,
        body: Vec<u8>,
        params: Option<&HashMap<String, String>>,
    ) -> Result<Response, AppError> {
        let _lock = session.lock_api().await;
        let user_id = session.user_id().to_string();
        let url = format!("{}/api{}", self.config.api_url, path).replace("{userId}", &user_id);
        info!("Account #{} {} {}", user_id, method.to_uppercase(), path);
        let max_retries = 4;
        let mut last_error = None;
        for attempt in 1..=max_retries {
            let method_enum = match method.to_uppercase().as_str() {
                "GET" => reqwest::Method::GET,
                "POST" => reqwest::Method::POST,
                "PUT" => reqwest::Method::PUT,
                "DELETE" => reqwest::Method::DELETE,
                "PATCH" => reqwest::Method::PATCH,
                _ => reqwest::Method::GET,
            };
            let mut req = self.prepare_request(session, method_enum, &url);
            if path.contains("/mysekai/") {
                req = req.header("X-User-Execute-Location", "mysekai");
            }
            if let Some(p) = params {
                req = req.query(p);
            }
            req = req.body(body.clone());
            match req.send().await {
                Ok(resp) => {
                    self.update_session_token(session, &resp);
                    return Ok(resp);
                }
                Err(e) => {
                    if e.is_timeout() {
                        warn!(
                            "Account #{} raw request timed out (attempt {}), retrying...",
                            session.user_id(),
                            attempt
                        );
                    } else {
                        error!(
                            "raw request error (attempt {}): server={}, err={}",
                            attempt,
                            self.region.as_str().to_uppercase(),
                            e
                        );
                    }
                    last_error = Some(AppError::NetworkError(e.to_string()));
                }
            }
            if attempt < max_retries {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        Err(last_error.unwrap_or(AppError::NetworkError(
            "Request failed after retries".to_string(),
        )))
    }

    pub async fn post<T: serde::Serialize>(
        &self,
        session: &AccountSession,
        path: &str,
        data: Option<&T>,
        params: Option<&HashMap<String, String>>,
    ) -> Result<Response, AppError> {
        self.call_api(session, "POST", path, data, params).await
    }

    pub async fn post_empty_body(
        &self,
        session: &AccountSession,
        path: &str,
        params: Option<&HashMap<String, String>>,
    ) -> Result<Response, AppError> {
        let encrypted = self.cryptor.pack_bytes_allow_empty(&[])?;
        self.call_api_with_raw_body(session, "POST", path, encrypted, params)
            .await
    }

    pub async fn handle_response<T: DeserializeOwned>(
        &self,
        resp: Response,
    ) -> Result<T, AppError> {
        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        let body = resp
            .bytes()
            .await
            .map_err(|e| AppError::NetworkError(e.to_string()))?;

        if content_type.contains("octet-stream") || content_type.contains("binary") {
            let sekai_status = SekaiHttpStatus::from_code(status)?;
            match sekai_status {
                SekaiHttpStatus::Ok
                | SekaiHttpStatus::ClientError
                | SekaiHttpStatus::NotFound
                | SekaiHttpStatus::Conflict => self.cryptor.unpack(&body),
                SekaiHttpStatus::SessionError => Err(AppError::SessionError),
                SekaiHttpStatus::GameUpgrade => Err(AppError::UpgradeRequired),
                SekaiHttpStatus::UnderMaintenance => Err(AppError::UnderMaintenance),
                _ => Err(AppError::Unknown {
                    status,
                    body: String::from_utf8_lossy(&body).to_string(),
                }),
            }
        } else {
            let sekai_status = SekaiHttpStatus::from_code(status)?;
            match sekai_status {
                SekaiHttpStatus::UnderMaintenance => Err(AppError::UnderMaintenance),
                SekaiHttpStatus::ServerError => Err(AppError::Unknown {
                    status,
                    body: String::from_utf8_lossy(&body).to_string(),
                }),
                SekaiHttpStatus::SessionError if content_type.contains("xml") => {
                    Err(AppError::CookieExpired)
                }
                _ => Err(AppError::Unknown {
                    status,
                    body: String::from_utf8_lossy(&body).to_string(),
                }),
            }
        }
    }

    pub async fn handle_response_ordered(
        &self,
        resp: reqwest::Response,
    ) -> Result<(IndexMap<String, JsonValue>, u16), AppError> {
        let (value, status) = self.handle_response_value(resp).await?;
        match value {
            JsonValue::Object(map) => Ok((map.into_iter().collect(), status)),
            _ => Err(AppError::CryptoError(
                "Expected object at top level".to_string(),
            )),
        }
    }

    pub async fn handle_response_value(
        &self,
        resp: reqwest::Response,
    ) -> Result<(JsonValue, u16), AppError> {
        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp
            .bytes()
            .await
            .map_err(|e| AppError::NetworkError(e.to_string()))?;
        let sekai_status = SekaiHttpStatus::from_code(status)?;

        if content_type.contains("octet-stream") || content_type.contains("binary") {
            return match sekai_status {
                SekaiHttpStatus::Ok
                | SekaiHttpStatus::ClientError
                | SekaiHttpStatus::NotFound
                | SekaiHttpStatus::Conflict
                | SekaiHttpStatus::ServerError => self
                    .cryptor
                    .unpack_value(&body)
                    .map(|data| (data, status))
                    .map_err(|e| {
                        tracing::warn!(
                            status,
                            error = %e,
                            "failed to decode encrypted upstream response"
                        );
                        e
                    }),
                SekaiHttpStatus::SessionError => Err(AppError::SessionError),
                SekaiHttpStatus::GameUpgrade => Err(AppError::UpgradeRequired),
                SekaiHttpStatus::UnderMaintenance => Err(AppError::UnderMaintenance),
            };
        }

        let body_text = String::from_utf8_lossy(&body).trim().to_string();
        if content_type.contains("json") {
            let value = if body_text.is_empty() {
                JsonValue::Null
            } else {
                sonic_rs::from_str(&body_text)?
            };
            return Ok((value, status));
        }

        match sekai_status {
            SekaiHttpStatus::ClientError => Err(AppError::BadRequest(if body_text.is_empty() {
                "Upstream bad request".to_string()
            } else {
                body_text.clone()
            })),
            SekaiHttpStatus::NotFound => Err(AppError::NotFound(if body_text.is_empty() {
                "Upstream resource not found".to_string()
            } else {
                body_text.clone()
            })),
            SekaiHttpStatus::Conflict => Err(AppError::Internal(if body_text.is_empty() {
                "Upstream conflict".to_string()
            } else {
                body_text.clone()
            })),
            SekaiHttpStatus::UnderMaintenance => Err(AppError::UnderMaintenance),
            SekaiHttpStatus::SessionError if content_type.contains("xml") => {
                Err(AppError::CookieExpired)
            }
            SekaiHttpStatus::ServerError => Err(AppError::Unknown {
                status,
                body: body_text,
            }),
            _ => Err(AppError::Unknown {
                status,
                body: body_text,
            }),
        }
    }

    pub async fn login(&self, session: &AccountSession) -> Result<LoginResponse, AppError> {
        let payload = session.dump_account()?;
        let encrypted = self.cryptor.pack_bytes(&payload)?;
        let (url, method) = if self.region.is_cp_server() {
            let url = format!(
                "{}/api/user/{}/auth?refreshUpdatedResources=False",
                self.config.api_url,
                session.user_id()
            );
            (url, reqwest::Method::PUT)
        } else {
            let url = format!("{}/api/user/auth", self.config.api_url);
            (url, reqwest::Method::POST)
        };
        let mut req = self.prepare_request(session, method, &url);
        req = req
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .header(reqwest::header::ACCEPT, "application/octet-stream");
        req = req.body(encrypted);
        info!("Account #{} logging in...", session.user_id());
        let resp = req
            .send()
            .await
            .map_err(|e| AppError::NetworkError(e.to_string()))?;
        self.update_session_token(session, &resp);
        let login_resp: LoginResponse = self.handle_response(resp).await?;
        if !login_resp.session_token.is_empty() {
            session.set_session_token(Some(login_resp.session_token.clone()));
        }
        if !self.region.is_cp_server() {
            if let Some(ref user_reg) = login_resp.user_registration {
                if !user_reg.user_id.is_empty() && user_reg.user_id != "0" {
                    let old_uid = session.user_id();
                    session.set_user_id(user_reg.user_id.clone());
                    info!(
                        "Account #{} -> {} (from login response)",
                        old_uid, user_reg.user_id
                    );
                }
            }
        }
        info!("Account #{} logged in successfully", session.user_id());
        Ok(login_resp)
    }

    #[tracing::instrument(skip(self, params), fields(region = ?self.region))]
    pub async fn get_game_api(
        &self,
        path: &str,
        params: Option<&HashMap<String, String>>,
    ) -> Result<(JsonValue, u16), AppError> {
        self.call_game_api_with_role("GET", path, params, DEFAULT_PROXY_ROLE)
            .await
    }

    #[tracing::instrument(skip(self, params), fields(region = ?self.region, proxy_role = %role))]
    pub async fn get_game_api_with_role(
        &self,
        path: &str,
        params: Option<&HashMap<String, String>>,
        role: &str,
    ) -> Result<(JsonValue, u16), AppError> {
        self.call_game_api_with_role("GET", path, params, role)
            .await
    }

    #[tracing::instrument(skip(self, params), fields(region = ?self.region, proxy_role = %role))]
    pub async fn post_game_api_with_role(
        &self,
        path: &str,
        params: Option<&HashMap<String, String>>,
        role: &str,
    ) -> Result<(JsonValue, u16), AppError> {
        self.call_game_api_with_role("POST", path, params, role)
            .await
    }

    #[tracing::instrument(skip(self, body, params), fields(region = ?self.region, proxy_role = %role))]
    pub async fn post_game_api_with_body_and_role<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
        params: Option<&HashMap<String, String>>,
        role: &str,
    ) -> Result<(JsonValue, u16), AppError> {
        self.call_game_api_with_body_and_role("POST", path, body, params, role)
            .await
    }

    async fn call_game_api_with_role(
        &self,
        method: &str,
        path: &str,
        params: Option<&HashMap<String, String>>,
        role: &str,
    ) -> Result<(JsonValue, u16), AppError> {
        self.call_game_api_with_optional_body_and_role::<()>(method, path, None, params, role)
            .await
    }

    async fn call_game_api_with_body_and_role<T: serde::Serialize>(
        &self,
        method: &str,
        path: &str,
        body: &T,
        params: Option<&HashMap<String, String>>,
        role: &str,
    ) -> Result<(JsonValue, u16), AppError> {
        self.call_game_api_with_optional_body_and_role(method, path, Some(body), params, role)
            .await
    }

    async fn call_game_api_with_optional_body_and_role<T: serde::Serialize>(
        &self,
        method: &str,
        path: &str,
        body: Option<&T>,
        params: Option<&HashMap<String, String>>,
        role: &str,
    ) -> Result<(JsonValue, u16), AppError> {
        let session = self.get_session_for_role(role)?;
        let max_retries = 4;
        let mut retry_count = 0;
        while retry_count < max_retries {
            let resp = if method.eq_ignore_ascii_case("POST") {
                if let Some(body) = body {
                    self.post(&session, path, Some(body), params).await?
                } else {
                    self.post_empty_body(&session, path, params).await?
                }
            } else {
                self.call_api::<()>(&session, method, path, None, params)
                    .await?
            };
            match self.handle_response_value(resp).await {
                Ok((json_value, upstream_status)) => {
                    return Ok((json_value, upstream_status));
                }
                Err(AppError::SessionError) => {
                    warn!(
                        "{} Session expired, re-logging in...",
                        self.region.as_str().to_uppercase()
                    );
                    if let Err(e) = self.login(&session).await {
                        error!(
                            "{} Re-login failed: {}",
                            self.region.as_str().to_uppercase(),
                            e
                        );
                        return Err(AppError::SessionError);
                    }
                    retry_count += 1;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(AppError::CookieExpired) => {
                    if self.config.require_cookies {
                        warn!(
                            "{} Cookies expired, refreshing...",
                            self.region.as_str().to_uppercase()
                        );
                        self.refresh_cookies().await?;
                        retry_count += 1;
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    } else {
                        return Err(AppError::CookieExpired);
                    }
                }
                Err(AppError::UpgradeRequired) => {
                    warn!(
                        "{} Server upgrade required, refreshing version and re-logging in...",
                        self.region.as_str().to_uppercase()
                    );
                    // First attempt: refresh version from remote and try login
                    self.refresh_version_from_remote().await?;
                    match self.login(&session).await {
                        Ok(login_resp) => {
                            self.update_version_headers_from_login(&login_resp);
                        }
                        Err(AppError::UpgradeRequired) => {
                            warn!(
                                "{} Login returned 426, waiting for app version update...",
                                self.region.as_str().to_uppercase()
                            );
                            tokio::time::sleep(Duration::from_secs(10)).await;
                            self.refresh_version_from_remote().await?;
                            match self.login(&session).await {
                                Ok(login_resp) => {
                                    self.update_version_headers_from_login(&login_resp);
                                }
                                Err(e) => {
                                    error!(
                                        "{} Re-login after waiting for app update failed: {}",
                                        self.region.as_str().to_uppercase(),
                                        e
                                    );
                                    return Err(AppError::UpgradeRequired);
                                }
                            }
                        }
                        Err(e) => {
                            error!(
                                "{} Re-login after version refresh failed: {}",
                                self.region.as_str().to_uppercase(),
                                e
                            );
                            return Err(AppError::UpgradeRequired);
                        }
                    }
                    retry_count += 1;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(AppError::UnderMaintenance) => {
                    return Err(AppError::UnderMaintenance);
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }
        Err(AppError::NetworkError(
            "Max retry attempts reached".to_string(),
        ))
    }

    pub async fn get_cp_mysekai_image(&self, path: &str) -> Result<(Vec<u8>, String), AppError> {
        let session = self.get_session_for_role(MYSEKAI_PROXY_ROLE)?;
        let path_clean = path.trim_start_matches('/');
        let image_url = format!("{}/image/mysekai-photo/{}", self.config.api_url, path_clean);
        let req = self.prepare_request(&session, reqwest::Method::GET, &image_url);
        let resp = req
            .send()
            .await
            .map_err(|e| AppError::NetworkError(e.to_string()))?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(AppError::Unknown {
                status,
                body: format!("Failed to fetch image from {}", image_url),
            });
        }
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AppError::NetworkError(e.to_string()))?;
        Ok((bytes.to_vec(), content_type))
    }

    pub async fn get_cp_mysekai_housing_thumbnail(
        &self,
        path: &str,
    ) -> Result<(Vec<u8>, String), AppError> {
        let session = self.get_session_for_role(MYSEKAI_PROXY_ROLE)?;
        let path_clean = path.trim_start_matches('/');
        let image_url = format!(
            "{}/image/mysekai-housing-competition/thumbnail/{}",
            self.config.api_url, path_clean
        );
        let req = self.prepare_request(&session, reqwest::Method::GET, &image_url);
        let resp = req
            .send()
            .await
            .map_err(|e| AppError::NetworkError(e.to_string()))?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(AppError::Unknown {
                status,
                body: format!("Failed to fetch image from {}", image_url),
            });
        }
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AppError::NetworkError(e.to_string()))?;
        Ok((bytes.to_vec(), content_type))
    }

    pub async fn get_jp_custom_music_score_blob_text(
        &self,
        kind: &str,
        path: &str,
    ) -> Result<String, AppError> {
        if self.region != ServerRegion::Jp {
            return Err(AppError::BadRequest(
                "custom music score blob is only supported for jp".to_string(),
            ));
        }
        if kind != "full" && kind != "preview" {
            return Err(AppError::BadRequest(
                "custom music score blob kind must be full or preview".to_string(),
            ));
        }

        let session = self.get_session().ok_or(AppError::NoClientAvailable)?;
        let path_clean = path.trim_start_matches('/');
        let url = format!(
            "{}/blob/custom-music-score/{}/{}",
            self.config.api_url, kind, path_clean
        );
        let req = self.prepare_request(&session, reqwest::Method::GET, &url);
        let resp = req
            .send()
            .await
            .map_err(|e| AppError::NetworkError(e.to_string()))?;
        self.update_session_token(&session, &resp);

        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| AppError::NetworkError(e.to_string()))?;
        if status != 200 {
            return Err(AppError::Unknown { status, body });
        }

        Ok(body)
    }

    pub fn decode_custom_music_score_blob_text(blob_text: &str) -> Result<JsonValue, AppError> {
        use base64::Engine as _;

        let compressed = base64::engine::general_purpose::STANDARD
            .decode(blob_text.trim())
            .map_err(|e| AppError::ParseError(format!("base64 decode failed: {}", e)))?;
        let mut decoder = flate2::read::GzDecoder::new(compressed.as_slice());
        let mut decoded = Vec::new();
        decoder
            .read_to_end(&mut decoded)
            .map_err(|e| AppError::ParseError(format!("gzip decompress failed: {}", e)))?;
        sonic_rs::from_slice(&decoded).map_err(AppError::from)
    }

    pub async fn get_nuverse_mysekai_image(
        &self,
        user_id: &str,
        index: &str,
    ) -> Result<Vec<u8>, AppError> {
        let session = self.get_session_for_role(MYSEKAI_PROXY_ROLE)?;
        let path = format!("/user/{}/mysekai/photo/{}", user_id, index);
        let resp = self.get(&session, &path, None).await?;
        let data: std::collections::HashMap<String, serde_json::Value> =
            self.handle_response(resp).await?;
        let thumbnail = data
            .get("thumbnail")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::ParseError("missing thumbnail in response".to_string()))?;
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(thumbnail)
            .map_err(|e| AppError::ParseError(format!("failed to decode base64: {}", e)))?;
        Ok(bytes)
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LoginResponse {
    #[serde(rename = "sessionToken", default)]
    pub session_token: String,
    #[serde(rename = "dataVersion", default)]
    pub data_version: String,
    #[serde(rename = "assetVersion", default)]
    pub asset_version: String,
    #[serde(rename = "assetHash", default)]
    pub asset_hash: String,
    #[serde(rename = "suiteMasterSplitPath", default)]
    pub suite_master_split_path: Vec<String>,
    #[serde(rename = "cdnVersion", default)]
    pub cdn_version: i32,
    #[serde(rename = "userRegistration", default)]
    pub user_registration: Option<UserRegistration>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct UserRegistration {
    #[serde(
        alias = "userId",
        alias = "userID",
        default,
        deserialize_with = "super::account::null_or_number_to_string"
    )]
    pub user_id: String,
}

#[cfg(test)]
mod tests {
    use super::{
        next_recovery_delay, replace_session_pool, should_wait_for_complete_parse,
        try_claim_recovery, RecoveryFlagGuard, SekaiClient, SessionPoolReplacement,
        INITIAL_RECOVERY_DELAY, MAX_RECOVERY_DELAY,
    };
    use crate::client::account::{AccountType, SekaiAccountCP, DEFAULT_PROXY_ROLE};
    use crate::client::session::AccountSession;
    use base64::Engine as _;
    use flate2::{write::GzEncoder, Compression};
    use parking_lot::RwLock;
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    fn session(user_id: &str, roles: &[&str]) -> Arc<AccountSession> {
        Arc::new(AccountSession::new(AccountType::CP(SekaiAccountCP {
            user_id: user_id.to_string(),
            device_id: "device".to_string(),
            credential: "credential".to_string(),
            proxy_roles: roles.iter().map(|role| (*role).to_string()).collect(),
        })))
    }

    #[test]
    fn decode_custom_music_score_blob_text_decodes_base64_gzip_json() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(br#"{"MusicId":121,"NoteList":[{"id":1}]}"#)
            .unwrap();
        let compressed = encoder.finish().unwrap();
        let blob = base64::engine::general_purpose::STANDARD.encode(compressed);

        let decoded = SekaiClient::decode_custom_music_score_blob_text(&blob).unwrap();

        assert_eq!(decoded["MusicId"], 121);
        assert_eq!(decoded["NoteList"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn empty_candidate_keeps_active_session_pool() {
        let sessions = RwLock::new(vec![session("old", &[DEFAULT_PROXY_ROLE])]);
        let index = AtomicUsize::new(7);

        let result = replace_session_pool(&sessions, &index, Vec::new());

        assert_eq!(result, SessionPoolReplacement::EmptyCandidate { active: 1 });
        assert_eq!(sessions.read()[0].user_id(), "old");
        assert_eq!(index.load(Ordering::SeqCst), 7);
    }

    #[test]
    fn replacing_session_pool_resets_round_robin_index() {
        let sessions = RwLock::new(vec![session("old", &[DEFAULT_PROXY_ROLE])]);
        let index = AtomicUsize::new(7);

        let result = replace_session_pool(
            &sessions,
            &index,
            vec![session("new", &[DEFAULT_PROXY_ROLE])],
        );

        assert_eq!(result, SessionPoolReplacement::Replaced { active: 1 });
        assert_eq!(sessions.read()[0].user_id(), "new");
        assert_eq!(index.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn partial_success_candidate_replaces_old_session_pool() {
        let sessions = RwLock::new(vec![
            session("old-1", &[DEFAULT_PROXY_ROLE]),
            session("old-2", &[DEFAULT_PROXY_ROLE]),
        ]);
        let index = AtomicUsize::new(7);

        let result = replace_session_pool(
            &sessions,
            &index,
            vec![session("new", &[DEFAULT_PROXY_ROLE])],
        );

        assert_eq!(result, SessionPoolReplacement::Replaced { active: 1 });
        assert_eq!(sessions.read()[0].user_id(), "new");
        assert_eq!(index.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn incomplete_parse_never_replaces_session_pool() {
        assert!(should_wait_for_complete_parse(false));
        assert!(!should_wait_for_complete_parse(true));
    }

    #[test]
    fn recovery_backoff_caps_at_maximum_delay() {
        assert_eq!(
            next_recovery_delay(INITIAL_RECOVERY_DELAY),
            INITIAL_RECOVERY_DELAY * 2
        );
        assert_eq!(next_recovery_delay(MAX_RECOVERY_DELAY), MAX_RECOVERY_DELAY);
    }

    #[test]
    fn recovery_flag_guard_resets_flag_when_dropped() {
        let flag = Arc::new(AtomicBool::new(true));

        drop(RecoveryFlagGuard { flag: flag.clone() });

        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn recovery_claim_allows_only_one_worker() {
        let flag = AtomicBool::new(false);

        assert!(try_claim_recovery(&flag));
        assert!(!try_claim_recovery(&flag));
    }
}
