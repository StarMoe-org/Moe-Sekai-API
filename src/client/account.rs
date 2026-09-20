use serde::{Deserialize, Deserializer, Serialize};

use crate::config::LoginProtocol;
use crate::error::AppError;

pub const DEFAULT_PROXY_ROLE: &str = "default";
pub const MYSEKAI_PROXY_ROLE: &str = "mysekai";

fn null_to_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

pub fn null_or_number_to_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Number(i64),
        Null,
    }
    match StringOrNumber::deserialize(deserializer)? {
        StringOrNumber::String(s) => Ok(s),
        StringOrNumber::Number(n) => Ok(n.to_string()),
        StringOrNumber::Null => Ok(String::new()),
    }
}

#[allow(dead_code)]
pub trait SekaiAccount: Send + Sync {
    fn user_id(&self) -> &str;
    fn set_user_id(&mut self, user_id: String);
    fn device_id(&self) -> &str;
    fn token(&self) -> &str;
    fn proxy_roles(&self) -> &[String];
    fn dump(&self, protocol: LoginProtocol) -> Result<Vec<u8>, AppError>;

    fn has_proxy_role(&self, role: &str) -> bool {
        normalized_proxy_roles(self.proxy_roles())
            .iter()
            .any(|configured| configured == &normalize_proxy_role(role))
    }
}

pub fn normalize_proxy_role(role: &str) -> String {
    role.trim().to_ascii_lowercase()
}

pub fn normalized_proxy_roles(roles: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = roles
        .iter()
        .map(|role| normalize_proxy_role(role))
        .filter(|role| !role.is_empty())
        .collect();
    if normalized.is_empty() {
        normalized.push(DEFAULT_PROXY_ROLE.to_string());
    }
    normalized
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SekaiAccountCP {
    #[serde(
        rename = "userId",
        default,
        deserialize_with = "null_or_number_to_string"
    )]
    pub user_id: String,
    #[serde(
        rename = "deviceId",
        default,
        deserialize_with = "null_to_empty_string"
    )]
    pub device_id: String,
    #[serde(default, deserialize_with = "null_to_empty_string")]
    pub credential: String,
    #[serde(rename = "proxyRoles", alias = "proxy_roles", default)]
    pub proxy_roles: Vec<String>,
}

impl SekaiAccount for SekaiAccountCP {
    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn set_user_id(&mut self, user_id: String) {
        self.user_id = user_id;
    }

    fn device_id(&self) -> &str {
        &self.device_id
    }

    fn token(&self) -> &str {
        &self.credential
    }

    fn proxy_roles(&self) -> &[String] {
        &self.proxy_roles
    }

    fn dump(&self, protocol: LoginProtocol) -> Result<Vec<u8>, AppError> {
        #[derive(Serialize)]
        struct LoginPayloadV1<'a> {
            #[serde(rename = "deviceId", skip_serializing_if = "Option::is_none")]
            device_id: Option<&'a str>,
            credential: &'a str,
            #[serde(rename = "authTriggerType")]
            auth_trigger_type: &'static str,
        }

        #[derive(Serialize)]
        struct LoginPayloadV2<'a> {
            #[serde(rename = "accessToken")]
            access_token: &'a str,
        }

        match protocol {
            LoginProtocol::V1 => {
                let payload = LoginPayloadV1 {
                    device_id: if self.device_id.is_empty() {
                        None
                    } else {
                        Some(&self.device_id)
                    },
                    credential: &self.credential,
                    auth_trigger_type: "normal",
                };
                rmp_serde::to_vec_named(&payload).map_err(|e| AppError::ParseError(e.to_string()))
            }
            LoginProtocol::V2 => {
                let payload = LoginPayloadV2 {
                    access_token: &self.credential,
                };
                rmp_serde::to_vec_named(&payload).map_err(|e| AppError::ParseError(e.to_string()))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SekaiAccountNuverse {
    #[serde(
        alias = "userId",
        alias = "userID",
        default,
        deserialize_with = "null_or_number_to_string"
    )]
    pub user_id: String,
    #[serde(
        rename = "deviceId",
        default,
        deserialize_with = "null_to_empty_string"
    )]
    pub device_id: String,
    #[serde(
        rename = "accessToken",
        default,
        deserialize_with = "null_to_empty_string"
    )]
    pub access_token: String,
    #[serde(rename = "proxyRoles", alias = "proxy_roles", default)]
    pub proxy_roles: Vec<String>,
}

impl SekaiAccount for SekaiAccountNuverse {
    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn set_user_id(&mut self, user_id: String) {
        self.user_id = user_id;
    }

    fn device_id(&self) -> &str {
        &self.device_id
    }

    fn token(&self) -> &str {
        &self.access_token
    }

    fn proxy_roles(&self) -> &[String] {
        &self.proxy_roles
    }

    fn dump(&self, protocol: LoginProtocol) -> Result<Vec<u8>, AppError> {
        #[derive(Serialize)]
        struct LoginPayloadV1<'a> {
            #[serde(rename = "deviceId", skip_serializing_if = "Option::is_none")]
            device_id: Option<&'a str>,
            #[serde(rename = "accessToken")]
            access_token: &'a str,
            #[serde(rename = "userID")]
            user_id: i64,
        }

        #[derive(Serialize)]
        struct LoginPayloadV2<'a> {
            #[serde(rename = "accessToken")]
            access_token: &'a str,
        }

        match protocol {
            LoginProtocol::V1 => {
                let user_id_num: i64 = self.user_id.parse().map_err(|_| {
                    AppError::ParseError(format!("Invalid user_id: {}", self.user_id))
                })?;

                let fallback_device_id = if self.device_id.is_empty() {
                    Some(self.user_id.as_str())
                } else {
                    Some(self.device_id.as_str())
                };

                let payload = LoginPayloadV1 {
                    device_id: fallback_device_id,
                    access_token: &self.access_token,
                    user_id: user_id_num,
                };
                rmp_serde::to_vec_named(&payload).map_err(|e| AppError::ParseError(e.to_string()))
            }
            // 6.4.0 sends only the access token; the server derives the account
            // and device from the GSDK JWT itself.
            LoginProtocol::V2 => {
                let payload = LoginPayloadV2 {
                    access_token: &self.access_token,
                };
                rmp_serde::to_vec_named(&payload).map_err(|e| AppError::ParseError(e.to_string()))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum AccountType {
    CP(SekaiAccountCP),
    Nuverse(SekaiAccountNuverse),
}

impl SekaiAccount for AccountType {
    fn user_id(&self) -> &str {
        match self {
            AccountType::CP(a) => a.user_id(),
            AccountType::Nuverse(a) => a.user_id(),
        }
    }

    fn set_user_id(&mut self, user_id: String) {
        match self {
            AccountType::CP(a) => a.set_user_id(user_id),
            AccountType::Nuverse(a) => a.set_user_id(user_id),
        }
    }

    fn device_id(&self) -> &str {
        match self {
            AccountType::CP(a) => a.device_id(),
            AccountType::Nuverse(a) => a.device_id(),
        }
    }

    fn token(&self) -> &str {
        match self {
            AccountType::CP(a) => a.token(),
            AccountType::Nuverse(a) => a.token(),
        }
    }

    fn proxy_roles(&self) -> &[String] {
        match self {
            AccountType::CP(a) => a.proxy_roles(),
            AccountType::Nuverse(a) => a.proxy_roles(),
        }
    }

    fn dump(&self, protocol: LoginProtocol) -> Result<Vec<u8>, AppError> {
        match self {
            AccountType::CP(a) => a.dump(protocol),
            AccountType::Nuverse(a) => a.dump(protocol),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_proxy_roles_default_to_normal_proxying() {
        assert_eq!(
            normalized_proxy_roles(&[]),
            vec![DEFAULT_PROXY_ROLE.to_string()]
        );
    }

    #[test]
    fn proxy_roles_are_trimmed_and_lowercased() {
        let roles = vec![" MySekai ".to_string(), "".to_string()];
        assert_eq!(
            normalized_proxy_roles(&roles),
            vec![MYSEKAI_PROXY_ROLE.to_string()]
        );
    }

    #[test]
    fn account_role_matching_does_not_fallback_for_special_roles() {
        let account = SekaiAccountCP {
            user_id: "1".to_string(),
            device_id: "device".to_string(),
            credential: "credential".to_string(),
            proxy_roles: vec![MYSEKAI_PROXY_ROLE.to_string()],
        };

        assert!(account.has_proxy_role(MYSEKAI_PROXY_ROLE));
        assert!(!account.has_proxy_role(DEFAULT_PROXY_ROLE));
    }

    /// Decodes a msgpack map's keys, so the tests assert on the wire shape
    /// rather than on a serialized byte string that shifts with field order.
    fn msgpack_keys(data: &[u8]) -> Vec<String> {
        let value = rmpv::decode::read_value(&mut std::io::Cursor::new(data)).unwrap();
        value
            .as_map()
            .expect("login body must be a map")
            .iter()
            .map(|(k, _)| k.as_str().expect("keys must be strings").to_string())
            .collect()
    }

    #[test]
    fn v2_login_body_carries_only_access_token() {
        let cp = SekaiAccountCP {
            user_id: "1".to_string(),
            device_id: "device".to_string(),
            credential: "cred".to_string(),
            proxy_roles: vec![],
        };
        assert_eq!(
            msgpack_keys(&cp.dump(LoginProtocol::V2).unwrap()),
            ["accessToken"]
        );

        let nuverse = SekaiAccountNuverse {
            user_id: "123".to_string(),
            device_id: "device".to_string(),
            access_token: "token".to_string(),
            proxy_roles: vec![],
        };
        assert_eq!(
            msgpack_keys(&nuverse.dump(LoginProtocol::V2).unwrap()),
            ["accessToken"]
        );
    }

    #[test]
    fn v1_login_body_keeps_legacy_fields() {
        let nuverse = SekaiAccountNuverse {
            user_id: "123".to_string(),
            device_id: "device".to_string(),
            access_token: "token".to_string(),
            proxy_roles: vec![],
        };
        let keys = msgpack_keys(&nuverse.dump(LoginProtocol::V1).unwrap());
        assert!(keys.contains(&"accessToken".to_string()));
        assert!(keys.contains(&"userID".to_string()));
        assert!(keys.contains(&"deviceId".to_string()));
    }

    #[test]
    fn v2_does_not_require_a_numeric_user_id() {
        // v1 parses user_id as i64; v2 must not, so accounts whose id is not a
        // plain integer still log in.
        let nuverse = SekaiAccountNuverse {
            user_id: "not-a-number".to_string(),
            device_id: String::new(),
            access_token: "token".to_string(),
            proxy_roles: vec![],
        };
        assert!(nuverse.dump(LoginProtocol::V1).is_err());
        assert!(nuverse.dump(LoginProtocol::V2).is_ok());
    }
}
