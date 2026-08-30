//! Named credentials so FLOW login steps can succeed (yellow → green).

use crate::features::openapi_index::AuthRealmHint;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RealmCredentials {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// Admin secret key (no email).
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub bearer_token: Option<String>,
    /// Extra login body fields (captcha_token, etc.).
    #[serde(default)]
    pub extras: HashMap<String, String>,
}

impl RealmCredentials {
    pub fn has_login(&self) -> bool {
        self.email.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
            && self.password.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
    }

    pub fn has_secret(&self) -> bool {
        self.secret.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
    }

    pub fn bearer(&self) -> Option<&str> {
        self.bearer_token.as_deref().filter(|s| !s.is_empty())
    }
}

/// Multi-profile auth container; also deserializes legacy single-profile JSON.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthProfiles {
    #[serde(default)]
    pub profiles: HashMap<String, RealmCredentials>,
    /// Legacy flat fields (user realm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default = "default_profile_name")]
    pub name: String,
}

fn default_profile_name() -> String {
    "default".into()
}

/// Backward-compatible alias used throughout the codebase.
pub type AuthProfile = AuthProfiles;

impl AuthProfiles {
    pub fn from_env(bearer_token: Option<String>) -> Self {
        let mut profiles = HashMap::new();

        let user = RealmCredentials {
            email: first_env(&["TRACE_DIFF_EMAIL", "CONFUCIUS_EMAIL"]),
            password: first_env(&["TRACE_DIFF_PASSWORD", "CONFUCIUS_PASSWORD"]),
            bearer_token: bearer_token
                .filter(|s| !s.is_empty())
                .or_else(|| first_env(&["TRACE_DIFF_BEARER_TOKEN"])),
            secret: None,
            extras: HashMap::new(),
        };
        profiles.insert("user".into(), user.clone());

        profiles.insert(
            "annotator".into(),
            RealmCredentials {
                email: first_env(&["TRACE_DIFF_ANNOTATOR_EMAIL", "CONFUCIUS_ANNOTATOR_EMAIL"]),
                password: first_env(&[
                    "TRACE_DIFF_ANNOTATOR_PASSWORD",
                    "CONFUCIUS_ANNOTATOR_PASSWORD",
                ]),
                bearer_token: None,
                secret: None,
                extras: HashMap::new(),
            },
        );

        profiles.insert(
            "admin".into(),
            RealmCredentials {
                email: None,
                password: None,
                bearer_token: None,
                secret: first_env(&["TRACE_DIFF_ADMIN_SECRET", "CONFUCIUS_ADMIN_KEY"]),
                extras: HashMap::new(),
            },
        );

        Self {
            profiles,
            email: user.email.clone(),
            password: user.password.clone(),
            bearer_token: user.bearer_token.clone(),
            secret: None,
            name: std::env::var("TRACE_DIFF_AUTH_PROFILE").unwrap_or_else(|_| "default".into()),
        }
    }

    pub fn merge_file(mut self, path: &Path) -> crate::error::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let file: AuthProfiles = serde_json::from_str(&text)?;

        if !file.profiles.is_empty() {
            for (k, v) in file.profiles {
                self.profiles.insert(k, v);
            }
        }

        // Legacy flat fields → user profile
        if file.email.is_some() || file.password.is_some() || file.bearer_token.is_some() {
            let user = self.profile_mut(AuthRealmHint::User);
            if user.email.is_none() {
                user.email = file.email.clone();
            }
            if user.password.is_none() {
                user.password = file.password.clone();
            }
            if user.bearer_token.is_none() {
                user.bearer_token = file.bearer_token.clone();
            }
        }
        if file.secret.is_some() {
            let admin = self.profile_mut(AuthRealmHint::Admin);
            if admin.secret.is_none() {
                admin.secret = file.secret.clone();
            }
        }
        if !file.name.is_empty() && file.name != "default" {
            self.name = file.name;
        }

        // Sync legacy top-level fields from user profile
        if let Some(u) = self.profiles.get("user") {
            if self.email.is_none() {
                self.email = u.email.clone();
            }
            if self.password.is_none() {
                self.password = u.password.clone();
            }
            if self.bearer_token.is_none() {
                self.bearer_token = u.bearer_token.clone();
            }
        }

        Ok(self)
    }

    pub fn with_cli(mut self, email: Option<String>, password: Option<String>) -> Self {
        if email.as_ref().map(|s| !s.is_empty()).unwrap_or(false) {
            self.profile_mut(AuthRealmHint::User).email = email.clone();
            self.email = email;
        }
        if password.as_ref().map(|s| !s.is_empty()).unwrap_or(false) {
            self.profile_mut(AuthRealmHint::User).password = password.clone();
            self.password = password;
        }
        self
    }

    pub fn with_realm_credentials(
        &mut self,
        realm: AuthRealmHint,
        email: Option<String>,
        password: Option<String>,
        secret: Option<String>,
    ) {
        let p = self.profile_mut(realm);
        if email.as_ref().map(|s| !s.is_empty()).unwrap_or(false) {
            p.email = email;
        }
        if password.as_ref().map(|s| !s.is_empty()).unwrap_or(false) {
            p.password = password;
        }
        if secret.as_ref().map(|s| !s.is_empty()).unwrap_or(false) {
            p.secret = secret;
        }
        if realm == AuthRealmHint::User {
            let u = self.profiles.get("user").cloned().unwrap_or_default();
            self.email = u.email;
            self.password = u.password;
        }
    }

    pub fn set_realm_field(&mut self, realm: AuthRealmHint, key: &str, value: Option<String>) {
        let val = value.filter(|s| !s.is_empty());
        let p = self.profile_mut(realm);
        match key.to_ascii_lowercase().as_str() {
            "email" => p.email = val,
            "password" => p.password = val,
            "secret" | "x-admin-secret" | "admin_secret" => p.secret = val,
            "bearer_token" | "access_token" => {
                p.bearer_token = val;
                if realm == AuthRealmHint::User {
                    self.bearer_token = p.bearer_token.clone();
                }
            }
            other => {
                if let Some(v) = val {
                    p.extras.insert(other.to_string(), v);
                }
            }
        }
        if realm == AuthRealmHint::User {
            if let Some(u) = self.profiles.get("user") {
                self.email = u.email.clone();
                self.password = u.password.clone();
            }
        }
    }

    pub fn field_value(&self, realm: AuthRealmHint, key: &str) -> Option<String> {
        let p = self.profile(realm);
        match key.to_ascii_lowercase().as_str() {
            "email" => p.email.clone().or_else(|| self.email.clone()),
            "password" => p.password.clone().or_else(|| self.password.clone()),
            "secret" => p.secret.clone(),
            "bearer_token" | "access_token" => {
                p.bearer_token
                    .clone()
                    .or_else(|| self.bearer_token.clone())
            }
            other => p.extras.get(other).cloned().or_else(|| self.lookup(other, realm)),
        }
    }

    pub fn profile(&self, realm: AuthRealmHint) -> &RealmCredentials {
        self.profiles.get(realm.as_str()).unwrap_or_else(|| {
            static EMPTY: OnceLock<RealmCredentials> = OnceLock::new();
            EMPTY.get_or_init(RealmCredentials::default)
        })
    }

    fn profile_mut(&mut self, realm: AuthRealmHint) -> &mut RealmCredentials {
        self.profiles
            .entry(realm.as_str().to_string())
            .or_default()
    }

    pub fn has_login(&self) -> bool {
        self.profile(AuthRealmHint::User).has_login()
    }

    pub fn has_realm_login(&self, realm: AuthRealmHint) -> bool {
        match realm {
            AuthRealmHint::Admin => self.profile(realm).has_secret(),
            AuthRealmHint::User | AuthRealmHint::Annotator => self.profile(realm).has_login(),
            AuthRealmHint::Public => true,
        }
    }

    pub fn realm_ready(&self, realm: AuthRealmHint) -> bool {
        match realm {
            AuthRealmHint::Public => true,
            AuthRealmHint::Admin => self.profile(realm).has_secret(),
            AuthRealmHint::User | AuthRealmHint::Annotator => {
                self.profile(realm).has_login() || self.profile(realm).bearer().is_some()
            }
        }
    }

    pub fn bearer(&self) -> Option<&str> {
        self.profile(AuthRealmHint::User)
            .bearer()
            .or(self.bearer_token.as_deref().filter(|s| !s.is_empty()))
    }

    pub fn bearer_for_realm(&self, realm: AuthRealmHint) -> Option<&str> {
        self.profile(realm).bearer()
    }

    /// Skip capture-login steps when we already have a token and no password login.
    pub fn skip_login_capture(&self, realm: AuthRealmHint) -> bool {
        self.profile(realm).bearer().is_some() && !self.profile(realm).has_login()
    }

    pub fn resolve_body(&self, value: &Value, realm: AuthRealmHint) -> Value {
        resolve_placeholders(value, self, realm)
    }

    pub fn auth_headers(
        &self,
        realm: AuthRealmHint,
        auth_mode: AuthMode,
        api_key_header: Option<&str>,
    ) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        match auth_mode {
            AuthMode::None => {}
            AuthMode::BearerCapture | AuthMode::BearerStatic => {
                if realm == AuthRealmHint::Admin {
                    if let Some(secret) = self.profile(realm).secret.as_ref().filter(|s| !s.is_empty())
                    {
                        headers.push(("Authorization".into(), format!("Bearer {secret}")));
                    }
                }
            }
            AuthMode::ApiKeyHeader { header_name } => {
                if let Some(secret) = self
                    .profile(AuthRealmHint::Admin)
                    .secret
                    .as_ref()
                    .filter(|s| !s.is_empty())
                {
                    headers.push((header_name.clone(), secret.clone()));
                } else if let Some(name) = api_key_header {
                    if let Some(secret) = self
                        .profile(AuthRealmHint::Admin)
                        .secret
                        .as_ref()
                        .filter(|s| !s.is_empty())
                    {
                        headers.push((name.to_string(), secret.clone()));
                    }
                }
            }
        }
        headers
    }

    pub fn detected_realms_needing_creds(&self, realms: &[AuthRealmHint]) -> Vec<AuthRealmHint> {
        realms
            .iter()
            .copied()
            .filter(|r| *r != AuthRealmHint::Public && !self.realm_ready(*r))
            .collect()
    }

    fn lookup(&self, key: &str, realm: AuthRealmHint) -> Option<String> {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                return Some(v);
            }
        }
        let p = self.profile(realm);
        match key {
            "TRACE_DIFF_EMAIL" | "CONFUCIUS_EMAIL" | "EMAIL" => {
                if realm == AuthRealmHint::Annotator {
                    self.profile(AuthRealmHint::Annotator).email.clone()
                } else {
                    p.email.clone().or_else(|| self.email.clone())
                }
            }
            "TRACE_DIFF_PASSWORD" | "CONFUCIUS_PASSWORD" | "PASSWORD" => {
                if realm == AuthRealmHint::Annotator {
                    self.profile(AuthRealmHint::Annotator).password.clone()
                } else {
                    p.password.clone().or_else(|| self.password.clone())
                }
            }
            "TRACE_DIFF_ANNOTATOR_EMAIL" | "CONFUCIUS_ANNOTATOR_EMAIL" | "ANNOTATOR_EMAIL" => {
                self.profile(AuthRealmHint::Annotator).email.clone()
            }
            "TRACE_DIFF_ANNOTATOR_PASSWORD" | "CONFUCIUS_ANNOTATOR_PASSWORD" | "ANNOTATOR_PASSWORD" => {
                self.profile(AuthRealmHint::Annotator).password.clone()
            }
            "TRACE_DIFF_ADMIN_SECRET" | "CONFUCIUS_ADMIN_KEY" | "ADMIN_SECRET" => {
                self.profile(AuthRealmHint::Admin).secret.clone()
            }
            "TRACE_DIFF_BEARER_TOKEN" => p.bearer_token.clone().or_else(|| self.bearer_token.clone()),
            "TRACE_DIFF_CAPTCHA_TOKEN" | "CAPTCHA_TOKEN" => p
                .extras
                .get("captcha_token")
                .or_else(|| p.extras.get("captcha"))
                .cloned(),
            "captcha_token" | "captcha" => p.extras.get(key).cloned(),
            other => p.extras.get(other).cloned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    #[default]
    None,
    BearerCapture,
    BearerStatic,
    #[serde(rename = "api_key_header")]
    ApiKeyHeader {
        #[serde(default = "default_api_key_header")]
        header_name: String,
    },
}

fn default_api_key_header() -> String {
    "X-Api-Key".into()
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::BearerCapture => "bearer_capture",
            Self::BearerStatic => "bearer_static",
            Self::ApiKeyHeader { .. } => "api_key_header",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "bearer_capture" => Self::BearerCapture,
            "bearer_static" => Self::BearerStatic,
            "api_key_header" => Self::ApiKeyHeader {
                header_name: "X-Api-Key".into(),
            },
            _ => Self::None,
        }
    }
}

fn first_env(keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|s| !s.is_empty())
}

fn resolve_placeholders(value: &Value, profile: &AuthProfiles, realm: AuthRealmHint) -> Value {
    match value {
        Value::String(s) => {
            if let Some(inner) = s.strip_prefix("${").and_then(|x| x.strip_suffix('}')) {
                if let Some(v) = profile.lookup(inner, realm) {
                    return Value::String(v);
                }
            }
            value.clone()
        }
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| resolve_placeholders(x, profile, realm))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), resolve_placeholders(v, profile, realm));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_confucius_placeholders_from_profile() {
        let mut profiles = AuthProfiles::default();
        profiles.profiles.insert(
            "user".into(),
            RealmCredentials {
                email: Some("a@b.com".into()),
                password: Some("secret".into()),
                secret: None,
                bearer_token: None,
                extras: HashMap::new(),
            },
        );
        let body = serde_json::json!({
            "email": "${CONFUCIUS_EMAIL}",
            "password": "${CONFUCIUS_PASSWORD}"
        });
        let resolved = profiles.resolve_body(&body, AuthRealmHint::User);
        assert_eq!(resolved["email"], "a@b.com");
        assert_eq!(resolved["password"], "secret");
    }

    #[test]
    fn annotator_and_admin_placeholders() {
        let mut profiles = AuthProfiles::default();
        profiles.profiles.insert(
            "annotator".into(),
            RealmCredentials {
                email: Some("ann@b.com".into()),
                password: Some("annpass".into()),
                secret: None,
                bearer_token: None,
                extras: HashMap::new(),
            },
        );
        profiles.profiles.insert(
            "admin".into(),
            RealmCredentials {
                email: None,
                password: None,
                secret: Some("admin-key".into()),
                bearer_token: None,
                extras: HashMap::new(),
            },
        );
        let ann_body = profiles.resolve_body(
            &serde_json::json!({ "email": "${ANNOTATOR_EMAIL}", "password": "${ANNOTATOR_PASSWORD}" }),
            AuthRealmHint::Annotator,
        );
        assert_eq!(ann_body["email"], "ann@b.com");
        assert_eq!(profiles.lookup("ADMIN_SECRET", AuthRealmHint::Admin), Some("admin-key".into()));
    }
}
