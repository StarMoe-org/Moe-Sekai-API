use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerRegion {
    Jp,
    En,
    Tw,
    Kr,
    Cn,
}

impl ServerRegion {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServerRegion::Jp => "jp",
            ServerRegion::En => "en",
            ServerRegion::Tw => "tw",
            ServerRegion::Kr => "kr",
            ServerRegion::Cn => "cn",
        }
    }

    pub fn is_cp_server(&self) -> bool {
        matches!(self, ServerRegion::Jp | ServerRegion::En)
    }
}

impl std::str::FromStr for ServerRegion {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "jp" => Ok(ServerRegion::Jp),
            "en" => Ok(ServerRegion::En),
            "tw" => Ok(ServerRegion::Tw),
            "kr" => Ok(ServerRegion::Kr),
            "cn" => Ok(ServerRegion::Cn),
            _ => Err(format!("Unknown server region: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RedisConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub password: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BackendConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub ssl: bool,
    #[serde(default)]
    pub ssl_cert: String,
    #[serde(default)]
    pub ssl_key: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default)]
    pub main_log_file: String,
    #[serde(default)]
    pub access_log: String,
    #[serde(default)]
    pub access_log_path: String,
    #[serde(default)]
    pub sekai_user_jwt_signing_key: String,
    #[serde(default)]
    pub enable_trust_proxy: bool,
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    #[serde(default)]
    pub proxy_header: String,
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    9999
}
fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DatabaseConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub driver: String,
    #[serde(default)]
    pub dsn: String,
    #[serde(default)]
    pub max_connections: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub password: String,
}

/// Login request body format spoken by the upstream game server.
///
/// - `V1`: legacy body, e.g. `{deviceId, accessToken, userID}` for Nuverse.
/// - `V2`: 6.4.0 body, which carries only `{accessToken}`.
///
/// The upstream protocol changes per game version, and not every region moves at
/// the same time, so this is configured per server instead of hardcoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoginProtocol {
    #[default]
    V1,
    V2,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub master_dir: String,
    #[serde(default)]
    pub version_path: String,
    #[serde(default)]
    pub account_dir: String,
    #[serde(default)]
    pub api_url: String,
    #[serde(default)]
    pub nuverse_master_data_url: String,
    #[serde(default)]
    pub nuverse_structure_file_path: String,
    #[serde(default)]
    pub require_cookies: bool,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub aes_key_hex: String,
    #[serde(default)]
    pub aes_iv_hex: String,
    #[serde(default)]
    pub enable_master_updater: bool,
    #[serde(default)]
    pub master_updater_cron: String,
    #[serde(default)]
    pub enable_app_hash_updater: bool,
    #[serde(default)]
    pub app_hash_updater_cron: String,
    #[serde(default)]
    pub remote_version_url: String,
    /// Per-server upstream HTTP(S) proxy. When omitted, the global `proxy`
    /// value is inherited; an explicit empty value disables proxying for this
    /// server.
    #[serde(default)]
    pub proxy: Option<String>,
    /// Which login request body format the upstream server expects. Defaults to
    /// `v1` so existing regions keep working; set `v2` for servers that have
    /// moved to the 6.4.0 protocol.
    #[serde(default)]
    pub login_protocol: LoginProtocol,
}

impl ServerConfig {
    pub fn resolve_proxy(&self, global_proxy: &str) -> Option<String> {
        resolve_proxy_value(self.proxy.as_deref(), global_proxy)
    }
}

fn resolve_proxy_value(server_proxy: Option<&str>, global_proxy: &str) -> Option<String> {
    match server_proxy {
        Some(proxy) if !proxy.trim().is_empty() => Some(proxy.to_string()),
        Some(_) => None,
        None if !global_proxy.trim().is_empty() => Some(global_proxy.to_string()),
        None => None,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppHashSource {
    #[serde(rename = "type")]
    pub source_type: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetUpdaterInfo {
    pub url: String,
    #[serde(default)]
    pub authorization: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub proxy: String,
    #[serde(default)]
    pub servers_dir: String,
    #[serde(default)]
    pub jp_sekai_cookie_url: String,
    #[serde(default)]
    pub git: GitConfig,
    #[serde(default)]
    pub redis: RedisConfig,
    pub backend: BackendConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub master_database: DatabaseConfig,
    #[serde(default)]
    pub apphash_sources: Vec<AppHashSource>,
    #[serde(default)]
    pub asset_updater_servers: Vec<AssetUpdaterInfo>,
    #[serde(default)]
    pub servers: HashMap<ServerRegion, ServerConfig>,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "localhost".to_string(),
            port: 6379,
            password: "".to_string(),
        }
    }
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            username: "".to_string(),
            email: "".to_string(),
            password: "".to_string(),
        }
    }
}

const ALL_REGIONS: [ServerRegion; 5] = [
    ServerRegion::Jp,
    ServerRegion::En,
    ServerRegion::Tw,
    ServerRegion::Kr,
    ServerRegion::Cn,
];

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let config_path =
            env::var("CONFIG_PATH").unwrap_or_else(|_| "moe-sekai-configs.yaml".to_string());
        Self::load_from_path_with_env(Path::new(&config_path), |key| env::var(key).ok())
    }

    pub fn load_from_path_with_env<F>(path: &Path, env_fn: F) -> anyhow::Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let file = File::open(path)
            .map_err(|e| anyhow::anyhow!("Failed to open config file '{}': {}", path.display(), e))?;
        let reader = BufReader::new(file);
        let mut root: serde_yaml::Value = serde_yaml::from_reader(reader)
            .map_err(|e| anyhow::anyhow!("Failed to parse config '{}': {}", path.display(), e))?;
        if root.is_null() {
            root = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }

        if let Some(servers_dir) = resolve_servers_dir(path, &root, &env_fn)? {
            merge_servers_dir(&mut root, &servers_dir)?;
        }

        let mut config: Config = serde_yaml::from_value(root)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize config: {}", e))?;

        apply_env_overrides(&mut config, &env_fn)?;
        Ok(config)
    }
}

fn resolve_servers_dir<F>(
    config_path: &Path,
    root: &serde_yaml::Value,
    env_fn: &F,
) -> anyhow::Result<Option<PathBuf>>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(env_dir) = env_fn("SERVERS_CONFIG_DIR") {
        let trimmed = env_dir.trim();
        if !trimmed.is_empty() {
            let dir = PathBuf::from(trimmed);
            if !dir.is_dir() {
                return Err(anyhow::anyhow!(
                    "SERVERS_CONFIG_DIR '{}' does not exist or is not a directory",
                    dir.display()
                ));
            }
            return Ok(Some(dir));
        }
    }

    if let Some(cfg_dir) = root
        .as_mapping()
        .and_then(|m| m.get(serde_yaml::Value::String("servers_dir".to_string())))
        .and_then(|v| v.as_str())
    {
        let trimmed = cfg_dir.trim();
        if !trimmed.is_empty() {
            let dir = resolve_relative_to_config(config_path, Path::new(trimmed));
            if !dir.is_dir() {
                return Err(anyhow::anyhow!(
                    "servers_dir '{}' does not exist or is not a directory",
                    dir.display()
                ));
            }
            return Ok(Some(dir));
        }
    }

    let auto_dir = resolve_relative_to_config(config_path, Path::new("servers.d"));
    if auto_dir.is_dir() {
        return Ok(Some(auto_dir));
    }

    Ok(None)
}

fn resolve_relative_to_config(config_path: &Path, candidate: &Path) -> PathBuf {
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }
    match config_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(candidate),
        _ => candidate.to_path_buf(),
    }
}

fn merge_servers_dir(root: &mut serde_yaml::Value, servers_dir: &Path) -> anyhow::Result<()> {
    let mut files_by_region: BTreeMap<ServerRegion, PathBuf> = BTreeMap::new();
    for entry in fs::read_dir(servers_dir).map_err(|e| {
        anyhow::anyhow!(
            "Failed to read servers_dir '{}': {}",
            servers_dir.display(),
            e
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());
        if !matches!(ext.as_deref(), Some("yaml") | Some("yml")) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(region) = stem.parse::<ServerRegion>() else {
            continue;
        };
        if let Some(prev) = files_by_region.insert(region, path.clone()) {
            return Err(anyhow::anyhow!(
                "Duplicate server config files for '{}': '{}' and '{}'",
                region.as_str(),
                prev.display(),
                path.display()
            ));
        }
    }

    if files_by_region.is_empty() {
        return Ok(());
    }

    let root_map = root
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("Root config must be a YAML mapping"))?;
    let servers_key = serde_yaml::Value::String("servers".to_string());
    if !root_map.contains_key(&servers_key) || root_map.get(&servers_key).is_some_and(|v| v.is_null())
    {
        root_map.insert(
            servers_key.clone(),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
    }
    let servers_map = root_map
        .get_mut(&servers_key)
        .and_then(|v| v.as_mapping_mut())
        .ok_or_else(|| anyhow::anyhow!("Config field 'servers' must be a YAML mapping"))?;

    for (region, file_path) in files_by_region {
        let file = File::open(&file_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to open server config '{}': {}",
                file_path.display(),
                e
            )
        })?;
        let mut overlay: serde_yaml::Value =
            serde_yaml::from_reader(BufReader::new(file)).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse server config '{}': {}",
                    file_path.display(),
                    e
                )
            })?;
        if overlay.is_null() {
            continue;
        }
        if !overlay.is_mapping() {
            return Err(anyhow::anyhow!(
                "Server config '{}' must be a YAML mapping",
                file_path.display()
            ));
        }
        let region_key = serde_yaml::Value::String(region.as_str().to_string());
        match servers_map.get_mut(&region_key) {
            Some(existing) => merge_yaml_value(existing, overlay),
            None => {
                // Ensure newly added region keeps any nested mappings cleanly.
                let mut empty = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
                merge_yaml_value(&mut empty, std::mem::replace(&mut overlay, serde_yaml::Value::Null));
                servers_map.insert(region_key, empty);
            }
        }
    }

    Ok(())
}

fn merge_yaml_value(base: &mut serde_yaml::Value, overlay: serde_yaml::Value) {
    match (base, overlay) {
        (serde_yaml::Value::Mapping(base_map), serde_yaml::Value::Mapping(overlay_map)) => {
            for (k, v) in overlay_map {
                match base_map.get_mut(&k) {
                    Some(existing) => merge_yaml_value(existing, v),
                    None => {
                        base_map.insert(k, v);
                    }
                }
            }
        }
        (base_slot, overlay_val) => {
            *base_slot = overlay_val;
        }
    }
}

fn apply_env_overrides<F>(config: &mut Config, env_fn: &F) -> anyhow::Result<()>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(port_str) = env_fn("PORT") {
        let trimmed = port_str.trim();
        if !trimmed.is_empty() {
            let port = trimmed
                .parse::<u16>()
                .map_err(|e| anyhow::anyhow!("Invalid PORT '{}': {}", port_str, e))?;
            config.backend.port = port;
        }
    }

    if let Some(proxy_str) = env_fn("PROXY") {
        config.proxy = proxy_str.trim().to_string();
    }

    if let Some(jwt_key) = env_fn("SEKAI_USER_JWT_SIGNING_KEY") {
        if !jwt_key.trim().is_empty() {
            config.backend.sekai_user_jwt_signing_key = jwt_key;
        }
    }

    if let Some(dsn) = env_fn("DATABASE_DSN") {
        if !dsn.trim().is_empty() {
            config.database.dsn = dsn;
        }
    }

    if let Some(redis_pw) = env_fn("REDIS_PASSWORD") {
        if !redis_pw.trim().is_empty() {
            config.redis.password = redis_pw;
        }
    }

    for region in ALL_REGIONS {
        let suffix = region.as_str().to_ascii_uppercase();

        let proxy_key = format!("PROXY_{}", suffix);
        if let Some(proxy_val) = env_fn(&proxy_key) {
            if let Some(server) = config.servers.get_mut(&region) {
                server.proxy = Some(proxy_val.trim().to_string());
            }
        }

        let proto_key = format!("LOGIN_PROTOCOL_{}", suffix);
        if let Some(proto_val) = env_fn(&proto_key) {
            let trimmed = proto_val.trim();
            if !trimmed.is_empty() {
                let parsed = match trimmed.to_ascii_lowercase().as_str() {
                    "v1" => LoginProtocol::V1,
                    "v2" => LoginProtocol::V2,
                    _ => {
                        return Err(anyhow::anyhow!(
                            "Invalid {} '{}': expected v1 or v2",
                            proto_key,
                            proto_val
                        ));
                    }
                };
                if let Some(server) = config.servers.get_mut(&region) {
                    server.login_protocol = parsed;
                }
            }
        }

        let enabled_key = format!("SERVER_ENABLED_{}", suffix);
        if let Some(enabled_val) = env_fn(&enabled_key) {
            let trimmed = enabled_val.trim();
            if !trimmed.is_empty() {
                let parsed = match trimmed.to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" => true,
                    "false" | "0" | "no" => false,
                    _ => {
                        return Err(anyhow::anyhow!(
                            "Invalid {} '{}': expected true or false",
                            enabled_key,
                            enabled_val
                        ));
                    }
                };
                if let Some(server) = config.servers.get_mut(&region) {
                    server.enabled = parsed;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{resolve_proxy_value, Config, LoginProtocol, ServerRegion};
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "moe_sekai_cfg_{}_{}_{}",
            name,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn omitted_server_proxy_inherits_global_proxy() {
        assert_eq!(
            resolve_proxy_value(None, "http://proxy.internal:7890"),
            Some("http://proxy.internal:7890".to_string())
        );
    }

    #[test]
    fn server_proxy_overrides_global_proxy() {
        assert_eq!(
            resolve_proxy_value(Some("http://tw-proxy.internal:7890"), "http://global:7890"),
            Some("http://tw-proxy.internal:7890".to_string())
        );
    }

    #[test]
    fn explicit_empty_server_proxy_disables_global_proxy() {
        assert_eq!(resolve_proxy_value(Some("  "), "http://global:7890"), None);
    }

    #[test]
    fn load_auto_discovers_servers_d_and_deep_merges_with_base() {
        let dir = make_temp_dir("auto_servers_d");
        let servers_dir = dir.join("servers.d");
        fs::create_dir_all(&servers_dir).unwrap();

        let base_path = dir.join("config.yaml");
        fs::write(
            &base_path,
            r#"
proxy: "http://global:8888"
backend:
  port: 9999
servers:
  cn:
    enabled: false
    api_url: "https://cn.example.com"
    headers:
      Host: "cn.example.com"
"#,
        )
        .unwrap();

        fs::write(
            servers_dir.join("cn.yaml"),
            r#"
enabled: true
login_protocol: v2
proxy: "http://vpngate-cn:8888"
headers:
  X-Platform: "Android"
"#,
        )
        .unwrap();

        fs::write(
            servers_dir.join("jp.yml"),
            r#"
enabled: true
api_url: "https://jp.example.com"
proxy: ""
"#,
        )
        .unwrap();

        let cfg = Config::load_from_path_with_env(&base_path, |_| None).unwrap();
        let cn = cfg.servers.get(&ServerRegion::Cn).unwrap();
        assert!(cn.enabled);
        assert_eq!(cn.login_protocol, LoginProtocol::V2);
        assert_eq!(cn.api_url, "https://cn.example.com");
        assert_eq!(cn.headers.get("Host").map(String::as_str), Some("cn.example.com"));
        assert_eq!(cn.headers.get("X-Platform").map(String::as_str), Some("Android"));
        assert_eq!(
            cn.resolve_proxy(&cfg.proxy).as_deref(),
            Some("http://vpngate-cn:8888")
        );

        let jp = cfg.servers.get(&ServerRegion::Jp).unwrap();
        assert!(jp.enabled);
        assert_eq!(jp.resolve_proxy(&cfg.proxy), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_applies_env_overrides_and_servers_config_dir() {
        let dir = make_temp_dir("env_overrides");
        let custom_dir = dir.join("custom-servers");
        fs::create_dir_all(&custom_dir).unwrap();

        let base_path = dir.join("config.yaml");
        fs::write(
            &base_path,
            r#"
proxy: "http://yaml-global:8888"
backend:
  port: 9999
  sekai_user_jwt_signing_key: "yaml-jwt"
database:
  enabled: true
  dsn: "yaml-dsn"
redis:
  enabled: true
  password: "yaml-redis-pw"
"#,
        )
        .unwrap();

        fs::write(
            custom_dir.join("kr.yaml"),
            r#"
enabled: false
api_url: "https://kr.example.com"
login_protocol: v1
"#,
        )
        .unwrap();

        let mut envs = HashMap::new();
        envs.insert(
            "SERVERS_CONFIG_DIR",
            custom_dir.to_string_lossy().into_owned(),
        );
        envs.insert("PORT", "8080".to_string());
        envs.insert("PROXY", "http://env-global:8888".to_string());
        envs.insert("PROXY_KR", "".to_string());
        envs.insert("LOGIN_PROTOCOL_KR", "V2".to_string());
        envs.insert("SERVER_ENABLED_KR", "true".to_string());
        envs.insert("SEKAI_USER_JWT_SIGNING_KEY", "env-jwt".to_string());
        envs.insert("DATABASE_DSN", "postgres://env-dsn".to_string());
        envs.insert("REDIS_PASSWORD", "env-redis-pw".to_string());

        let cfg = Config::load_from_path_with_env(&base_path, |k| envs.get(k).cloned()).unwrap();
        assert_eq!(cfg.backend.port, 8080);
        assert_eq!(cfg.proxy, "http://env-global:8888");
        assert_eq!(cfg.backend.sekai_user_jwt_signing_key, "env-jwt");
        assert_eq!(cfg.database.dsn, "postgres://env-dsn");
        assert_eq!(cfg.redis.password, "env-redis-pw");

        let kr = cfg.servers.get(&ServerRegion::Kr).unwrap();
        assert!(kr.enabled);
        assert_eq!(kr.login_protocol, LoginProtocol::V2);
        assert_eq!(kr.resolve_proxy(&cfg.proxy), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_region_files_in_servers_dir_return_error() {
        let dir = make_temp_dir("dup_region");
        let servers_dir = dir.join("servers.d");
        fs::create_dir_all(&servers_dir).unwrap();

        let base_path = dir.join("config.yaml");
        fs::write(&base_path, "backend:\n  port: 9999\n").unwrap();
        fs::write(servers_dir.join("cn.yaml"), "enabled: true\n").unwrap();
        fs::write(servers_dir.join("cn.yml"), "enabled: false\n").unwrap();

        let err = Config::load_from_path_with_env(&base_path, |_| None).unwrap_err();
        assert!(err.to_string().contains("Duplicate server config files"));

        let _ = fs::remove_dir_all(&dir);
    }
}
