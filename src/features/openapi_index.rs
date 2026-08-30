//! Parsed OpenAPI operation index for workflow detection.

use serde_json::Value;
use std::collections::{HashMap, HashSet};

const HTTP_METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options"];
const MAX_REF_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecuritySchemeKind {
    BearerJwt,
    ApiKeyHeader,
    ApiKeyQuery,
    Other,
}

#[derive(Debug, Clone)]
pub struct SecurityScheme {
    pub name: String,
    pub kind: SecuritySchemeKind,
    /// Header or query param name for apiKey schemes.
    pub param_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ParamSpec {
    pub name: String,
    pub location: String,
    pub required: bool,
    pub example: Option<Value>,
    pub schema_type: Option<String>,
    pub format: Option<String>,
}

impl ParamSpec {
    pub fn probe_value(&self) -> Value {
        if let Some(ex) = &self.example {
            return ex.clone();
        }
        if let Some(fmt) = &self.format {
            if fmt.eq_ignore_ascii_case("uuid") {
                return Value::String("00000000-0000-0000-0000-000000000000".into());
            }
        }
        match self.schema_type.as_deref() {
            Some("integer") | Some("number") => Value::Number(1.into()),
            Some("boolean") => Value::Bool(false),
            Some("array") => Value::Array(vec![]),
            Some("object") => Value::Object(serde_json::Map::new()),
            _ => Value::String("probe".into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BodySpec {
    pub content_type: String,
    pub required: bool,
    pub example: Option<Value>,
    pub required_properties: Vec<String>,
    pub property_names: Vec<String>,
}

impl BodySpec {
    pub fn minimal_json(&self) -> Value {
        if let Some(ex) = &self.example {
            return ex.clone();
        }
        let mut map = serde_json::Map::new();
        for prop in &self.required_properties {
            map.insert(prop.clone(), Value::String("probe".into()));
        }
        if map.is_empty() && !self.property_names.is_empty() {
            for prop in &self.property_names {
                map.insert(prop.clone(), Value::String("probe".into()));
            }
        }
        Value::Object(map)
    }

    pub fn is_credential_login(&self) -> bool {
        let names: HashSet<_> = self.property_names.iter().map(|s| s.as_str()).collect();
        names.contains("email") && names.contains("password")
    }

    pub fn is_secret_login(&self) -> bool {
        let names: HashSet<_> = self.property_names.iter().map(|s| s.as_str()).collect();
        (names.contains("secret") || names.contains("api_key")) && !names.contains("email")
    }
}

#[derive(Debug, Clone)]
pub struct EndpointOp {
    pub path: String,
    pub method: String,
    pub tags: Vec<String>,
    pub summary: String,
    pub operation_id: String,
    /// True when operation (or spec default) requires authentication.
    pub requires_auth: bool,
    /// Explicitly public (operation.security = []).
    pub explicitly_public: bool,
    pub path_params: Vec<ParamSpec>,
    pub has_request_body: bool,
    pub security_schemes: Vec<String>,
    pub query_params: Vec<ParamSpec>,
    pub header_params: Vec<ParamSpec>,
    pub request_body: Option<BodySpec>,
    pub success_statuses: Vec<u16>,
    pub response_token_fields: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct OpenApiIndex {
    pub ops: Vec<EndpointOp>,
    pub security_schemes: HashMap<String, SecurityScheme>,
    by_key: HashMap<(String, String), usize>,
}

impl OpenApiIndex {
    pub fn from_json(openapi_json: &str) -> Self {
        let v: Value = match serde_json::from_str(openapi_json) {
            Ok(v) => v,
            Err(_) => {
                return Self {
                    ops: Vec::new(),
                    security_schemes: HashMap::new(),
                    by_key: HashMap::new(),
                }
            }
        };

        let security_schemes = parse_security_schemes(&v);
        let global_security = v
            .get("security")
            .and_then(|s| s.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);

        let mut ops = Vec::new();
        let Some(paths) = v.get("paths").and_then(|p| p.as_object()) else {
            return Self {
                ops,
                security_schemes,
                by_key: HashMap::new(),
            };
        };

        for (path, item) in paths {
            let Some(item_obj) = item.as_object() else {
                continue;
            };
            for method in HTTP_METHODS {
                let Some(detail) = item_obj.get(*method) else {
                    continue;
                };
                let method_upper = method.to_ascii_uppercase();
                let tags = detail
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let summary = detail
                    .get("summary")
                    .or_else(|| detail.get("operationId"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let operation_id = detail
                    .get("operationId")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();

                let (requires_auth, explicitly_public, security_schemes_op) =
                    operation_security(detail, global_security);
                let path_params = extract_params(detail, path, "path", &v);
                let query_params = extract_params(detail, path, "query", &v);
                let header_params = extract_params(detail, path, "header", &v);
                let request_body = parse_request_body(detail, &v);
                let has_request_body = request_body.is_some();
                let success_statuses = parse_success_statuses(detail);
                let response_token_fields = parse_response_token_fields(detail, &v);

                ops.push(EndpointOp {
                    path: path.clone(),
                    method: method_upper,
                    tags,
                    summary,
                    operation_id,
                    requires_auth,
                    explicitly_public,
                    path_params,
                    has_request_body,
                    security_schemes: security_schemes_op,
                    query_params,
                    header_params,
                    request_body,
                    success_statuses,
                    response_token_fields,
                });
            }
        }

        let mut by_key = HashMap::new();
        for (i, op) in ops.iter().enumerate() {
            by_key.insert((op.path.clone(), op.method.clone()), i);
        }

        Self {
            ops,
            security_schemes,
            by_key,
        }
    }

    pub fn find(&self, path: &str, method: &str) -> Option<&EndpointOp> {
        self.by_key
            .get(&(path.to_string(), method.to_ascii_uppercase()))
            .map(|i| &self.ops[*i])
    }

    pub fn login_candidates(&self) -> Vec<&EndpointOp> {
        self.ops
            .iter()
            .filter(|op| op.method == "POST" && op.path.to_lowercase().contains("login"))
            .collect()
    }

    pub fn login_for_realm(&self, realm: AuthRealmHint) -> Option<&EndpointOp> {
        let mut candidates = self.login_candidates();
        candidates.sort_by_key(|op| std::cmp::Reverse(login_rank_for_realm(op, realm)));
        candidates.into_iter().next()
    }

    pub fn by_tag(&self, tag: &str) -> Vec<&EndpointOp> {
        self.ops
            .iter()
            .filter(|op| op.primary_tag().eq_ignore_ascii_case(tag))
            .collect()
    }

    pub fn all_tags(&self) -> Vec<String> {
        let mut tags = std::collections::BTreeSet::new();
        for op in &self.ops {
            if !op.primary_tag().is_empty() {
                tags.insert(op.primary_tag());
            }
        }
        tags.into_iter().collect()
    }

    pub fn scheme(&self, name: &str) -> Option<&SecurityScheme> {
        self.security_schemes.get(name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AuthRealmHint {
    User,
    Annotator,
    Admin,
    Public,
}

impl AuthRealmHint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Annotator => "annotator",
            Self::Admin => "admin",
            Self::Public => "public",
        }
    }

    pub fn uses_login_capture(self) -> bool {
        matches!(self, Self::User | Self::Annotator)
    }
}

impl std::str::FromStr for AuthRealmHint {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "user" => Ok(Self::User),
            "annotator" => Ok(Self::Annotator),
            "admin" => Ok(Self::Admin),
            "public" => Ok(Self::Public),
            _ => Err(()),
        }
    }
}

pub fn infer_realm(op: &EndpointOp) -> AuthRealmHint {
    let path = op.path.to_lowercase();
    let tag = op.primary_tag().to_lowercase();

    if path.contains("/api/admin/") || path.starts_with("/admin/") || tag == "admin" {
        return AuthRealmHint::Admin;
    }
    if path.contains("/api/annotator/")
        || path.contains("/api/annotators/")
        || path.contains("/annotator/")
        || tag == "annotators"
        || tag == "annotator"
    {
        return AuthRealmHint::Annotator;
    }
    if tag == "auth" || path.contains("/api/auth/") {
        return AuthRealmHint::User;
    }
    if op.requires_auth || looks_protected_path(&path) {
        return AuthRealmHint::User;
    }
    AuthRealmHint::Public
}

pub fn infer_realm_from_tag(tag: &str) -> AuthRealmHint {
    let t = tag.to_lowercase();
    if t == "admin" {
        AuthRealmHint::Admin
    } else if t == "annotators" || t == "annotator" {
        AuthRealmHint::Annotator
    } else {
        AuthRealmHint::User
    }
}

fn looks_protected_path(path: &str) -> bool {
    path.contains("/admin/")
        || path.contains("/auth/")
        || path.contains("/user")
        || path.contains("/account")
        || path.contains("/billing")
        || path.contains("/annotator")
}

impl EndpointOp {
    pub fn primary_tag(&self) -> String {
        self.tags
            .first()
            .cloned()
            .unwrap_or_else(|| "api".to_string())
    }

    pub fn has_path_params(&self) -> bool {
        !self.path_params.is_empty() || self.path.contains('{')
    }

    pub fn probe_path(&self) -> String {
        templated_path_for_op(self)
    }

    pub fn is_get(&self) -> bool {
        self.method == "GET"
    }

    pub fn is_write_method(&self) -> bool {
        matches!(self.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE")
    }

    pub fn default_expect_status(&self) -> Option<u16> {
        self.success_statuses.first().copied()
    }

    pub fn has_bearer_scheme(&self, index: &OpenApiIndex) -> bool {
        self.security_schemes.iter().any(|name| {
            index
                .scheme(name)
                .map(|s| s.kind == SecuritySchemeKind::BearerJwt)
                .unwrap_or_else(|| name.to_ascii_lowercase().contains("bearer"))
        })
    }

    pub fn primary_api_key_header(&self, index: &OpenApiIndex) -> Option<(String, String)> {
        for name in &self.security_schemes {
            if let Some(scheme) = index.scheme(name) {
                if scheme.kind == SecuritySchemeKind::ApiKeyHeader {
                    if let Some(param) = &scheme.param_name {
                        return Some((name.clone(), param.clone()));
                    }
                }
            }
        }
        None
    }

    pub fn login_token_field(&self) -> Option<String> {
        for field in &["access_token", "token", "jwt"] {
            if self.response_token_fields.iter().any(|f| f == *field) {
                return Some((*field).into());
            }
        }
        self.response_token_fields.first().cloned()
    }
}

pub fn templated_path_for_op(op: &EndpointOp) -> String {
    if op.path_params.is_empty() {
        return templated_path_for_probe(&op.path);
    }
    let mut out = String::new();
    let mut chars = op.path.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut param = String::new();
            for ch in chars.by_ref() {
                if ch == '}' {
                    break;
                }
                param.push(ch);
            }
            let placeholder = op
                .path_params
                .iter()
                .find(|p| p.name == param)
                .map(|p| p.probe_value())
                .unwrap_or_else(|| {
                    if param.to_ascii_lowercase().contains("uuid") {
                        Value::String("00000000-0000-0000-0000-000000000000".into())
                    } else {
                        Value::Number(1.into())
                    }
                });
            match placeholder {
                Value::String(s) => out.push_str(&s),
                Value::Number(n) => out.push_str(&n.to_string()),
                _ => out.push('1'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Substitute `{id}` placeholders for probe requests (fallback).
pub fn templated_path_for_probe(path: &str) -> String {
    let mut out = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut param = String::new();
            for ch in chars.by_ref() {
                if ch == '}' {
                    break;
                }
                param.push(ch);
            }
            let placeholder = if param.to_ascii_lowercase().contains("uuid") {
                "00000000-0000-0000-0000-000000000000"
            } else {
                "1"
            };
            out.push_str(placeholder);
        } else {
            out.push(c);
        }
    }
    out
}

pub fn build_query_string(op: &EndpointOp) -> Option<String> {
    let required: Vec<_> = op.query_params.iter().filter(|p| p.required).collect();
    if required.is_empty() {
        return None;
    }
    let pairs: Vec<String> = required
        .iter()
        .map(|p| {
            let val = p.probe_value();
            let s = match val {
                Value::String(v) => v,
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => "probe".into(),
            };
            format!("{}={}", urlencoding_light(&p.name), urlencoding_light(&s))
        })
        .collect();
    Some(pairs.join("&"))
}

fn urlencoding_light(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "%20".into(),
            '&' => "%26".into(),
            '=' => "%3D".into(),
            _ if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' => {
                c.to_string()
            }
            _ => format!("%{:02X}", c as u8),
        })
        .collect()
}

fn parse_security_schemes(spec: &Value) -> HashMap<String, SecurityScheme> {
    let mut out = HashMap::new();
    let Some(schemes) = spec
        .get("components")
        .and_then(|c| c.get("securitySchemes"))
        .and_then(|s| s.as_object())
    else {
        return out;
    };
    for (name, detail) in schemes {
        let kind = detail
            .get("type")
            .and_then(|t| t.as_str())
            .map(|t| match t {
                "http" => {
                    let scheme = detail.get("scheme").and_then(|s| s.as_str()).unwrap_or("");
                    if scheme.eq_ignore_ascii_case("bearer") {
                        SecuritySchemeKind::BearerJwt
                    } else {
                        SecuritySchemeKind::Other
                    }
                }
                "apiKey" => match detail.get("in").and_then(|i| i.as_str()) {
                    Some("header") => SecuritySchemeKind::ApiKeyHeader,
                    Some("query") => SecuritySchemeKind::ApiKeyQuery,
                    _ => SecuritySchemeKind::Other,
                },
                _ => SecuritySchemeKind::Other,
            })
            .unwrap_or(SecuritySchemeKind::Other);
        let param_name = detail
            .get("name")
            .and_then(|n| n.as_str())
            .map(str::to_string);
        out.insert(
            name.clone(),
            SecurityScheme {
                name: name.clone(),
                kind,
                param_name,
            },
        );
    }
    out
}

fn operation_security(detail: &Value, global_security: bool) -> (bool, bool, Vec<String>) {
    if let Some(sec) = detail.get("security") {
        if let Some(arr) = sec.as_array() {
            if arr.is_empty() {
                return (false, true, Vec::new());
            }
            let schemes = extract_scheme_names(arr);
            return (true, false, schemes);
        }
    }
    (global_security, false, Vec::new())
}

fn extract_scheme_names(security: &[Value]) -> Vec<String> {
    let mut names = Vec::new();
    for item in security {
        if let Some(obj) = item.as_object() {
            names.extend(obj.keys().cloned());
        }
    }
    names
}

fn extract_params(detail: &Value, path: &str, loc: &str, spec: &Value) -> Vec<ParamSpec> {
    let mut params = Vec::new();
    if let Some(arr) = detail.get("parameters").and_then(|p| p.as_array()) {
        for p in arr {
            let resolved = resolve_ref_value(p, spec, 0);
            if resolved.get("in").and_then(|x| x.as_str()) == Some(loc) {
                if let Some(spec_p) = param_from_value(&resolved) {
                    params.push(spec_p);
                }
            }
        }
    }
    if loc == "path" && params.is_empty() {
        for segment in path.split('/') {
            if segment.starts_with('{') && segment.ends_with('}') {
                let name = segment.trim_matches(['{', '}']).to_string();
                params.push(ParamSpec {
                    name,
                    location: "path".into(),
                    required: true,
                    example: None,
                    schema_type: None,
                    format: None,
                });
            }
        }
    }
    params
}

fn param_from_value(v: &Value) -> Option<ParamSpec> {
    let name = v.get("name")?.as_str()?.to_string();
    let location = v.get("in")?.as_str()?.to_string();
    let required = v.get("required").and_then(|r| r.as_bool()).unwrap_or(false);
    let schema = v.get("schema").cloned().unwrap_or(Value::Null);
    let example = v
        .get("example")
        .cloned()
        .or_else(|| schema.get("example").cloned());
    let schema_type = schema
        .get("type")
        .and_then(|t| t.as_str())
        .map(str::to_string);
    let format = schema
        .get("format")
        .and_then(|f| f.as_str())
        .map(str::to_string);
    Some(ParamSpec {
        name,
        location,
        required,
        example,
        schema_type,
        format,
    })
}

fn parse_request_body(detail: &Value, spec: &Value) -> Option<BodySpec> {
    let rb = detail.get("requestBody")?;
    let resolved = resolve_ref_value(rb, spec, 0);
    let required = resolved
        .get("required")
        .and_then(|r| r.as_bool())
        .unwrap_or(false);
    let content = resolved.get("content")?.as_object()?;
    let (content_type, media) = content
        .iter()
        .find(|(k, _)| k.contains("json"))
        .or_else(|| content.iter().next())?;
    let schema_val = media.get("schema")?;
    let schema = resolve_ref_value(schema_val, spec, 0);
    let example = media
        .get("example")
        .cloned()
        .or_else(|| schema.get("example").cloned());
    let (required_properties, property_names) = schema_properties(&schema, spec);
    Some(BodySpec {
        content_type: content_type.clone(),
        required,
        example,
        required_properties,
        property_names,
    })
}

fn schema_properties(schema: &Value, spec: &Value) -> (Vec<String>, Vec<String>) {
    let resolved = resolve_ref_value(schema, spec, 0);
    let props = resolved.get("properties").and_then(|p| p.as_object());
    let required: HashSet<_> = resolved
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let mut property_names = Vec::new();
    let mut required_properties = Vec::new();
    if let Some(props) = props {
        for (name, _) in props {
            property_names.push(name.clone());
            if required.contains(name.as_str()) {
                required_properties.push(name.clone());
            }
        }
    }
    (required_properties, property_names)
}

fn parse_success_statuses(detail: &Value) -> Vec<u16> {
    let mut codes = Vec::new();
    if let Some(responses) = detail.get("responses").and_then(|r| r.as_object()) {
        for key in responses.keys() {
            if let Ok(code) = key.parse::<u16>() {
                if (200..300).contains(&code) {
                    codes.push(code);
                }
            }
        }
    }
    codes.sort();
    codes
}

fn parse_response_token_fields(detail: &Value, spec: &Value) -> Vec<String> {
    let mut fields = Vec::new();
    let responses = detail.get("responses").and_then(|r| r.as_object());
    let Some(responses) = responses else {
        return fields;
    };
    for (code, resp) in responses {
        if code == "default" {
            continue;
        }
        if let Ok(c) = code.parse::<u16>() {
            if !(200..300).contains(&c) {
                continue;
            }
        } else if code != "2XX" && code != "2xx" {
            continue;
        }
        let resolved = resolve_ref_value(resp, spec, 0);
        if let Some(content) = resolved.get("content").and_then(|c| c.as_object()) {
            for (_, media) in content {
                if let Some(schema) = media.get("schema") {
                    collect_token_fields(&resolve_ref_value(schema, spec, 0), spec, &mut fields);
                }
            }
        }
    }
    fields.sort();
    fields.dedup();
    fields
}

fn collect_token_fields(schema: &Value, spec: &Value, out: &mut Vec<String>) {
    let resolved = resolve_ref_value(schema, spec, 0);
    if let Some(props) = resolved.get("properties").and_then(|p| p.as_object()) {
        for name in props.keys() {
            let lower = name.to_ascii_lowercase();
            if lower.contains("token") || lower == "jwt" || lower == "access_token" {
                out.push(name.clone());
            }
        }
    }
}

fn resolve_ref_value(value: &Value, spec: &Value, depth: usize) -> Value {
    if depth >= MAX_REF_DEPTH {
        return value.clone();
    }
    if let Some(ref_path) = value.get("$ref").and_then(|r| r.as_str()) {
        if let Some(resolved) = resolve_ref(spec, ref_path) {
            return resolve_ref_value(&resolved, spec, depth + 1);
        }
    }
    value.clone()
}

fn resolve_ref(spec: &Value, ref_path: &str) -> Option<Value> {
    let parts: Vec<&str> = ref_path.trim_start_matches("#/").split('/').collect();
    let mut current = spec;
    for part in parts {
        current = current.get(part)?;
    }
    Some(current.clone())
}

fn login_rank_for_realm(op: &EndpointOp, realm: AuthRealmHint) -> i32 {
    let path = op.path.to_lowercase();
    let mut score = 0;
    match realm {
        AuthRealmHint::User => {
            if path == "/api/auth/login" {
                score += 100;
            } else if path.ends_with("/auth/login") {
                score += 80;
            } else if op.primary_tag().eq_ignore_ascii_case("auth") {
                score += 50;
            }
            if path.contains("annotator") {
                score -= 50;
            }
        }
        AuthRealmHint::Annotator => {
            if path.contains("annotator") && path.contains("login") {
                score += 100;
            } else if op.primary_tag().eq_ignore_ascii_case("annotators") {
                score += 80;
            }
            if path == "/api/auth/login" {
                score -= 40;
            }
        }
        AuthRealmHint::Admin => score -= 100,
        AuthRealmHint::Public => {}
    }
    score -= path.len() as i32 / 10;
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_security_and_tags() {
        let spec = r#"{
            "security": [{"Bearer": []}],
            "components": {
                "securitySchemes": {
                    "Bearer": { "type": "http", "scheme": "bearer" }
                }
            },
            "paths": {
                "/api/health": {
                    "get": {
                        "tags": ["health"],
                        "summary": "Health check",
                        "security": []
                    }
                },
                "/api/admin/users": {
                    "get": { "tags": ["admin"], "summary": "List users" }
                }
            }
        }"#;
        let idx = OpenApiIndex::from_json(spec);
        let health = idx.find("/api/health", "GET").unwrap();
        assert!(!health.requires_auth);
        assert!(health.explicitly_public);
        let admin = idx.find("/api/admin/users", "GET").unwrap();
        assert!(admin.requires_auth);
    }

    #[test]
    fn parses_request_body_with_ref() {
        let spec = r##"{
            "components": {
                "schemas": {
                    "LoginRequest": {
                        "type": "object",
                        "required": ["email", "password"],
                        "properties": {
                            "email": { "type": "string" },
                            "password": { "type": "string" }
                        }
                    }
                }
            },
            "paths": {
                "/api/auth/login": {
                    "post": {
                        "requestBody": {
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/LoginRequest" }
                                }
                            }
                        },
                        "responses": {
                            "200": {
                                "content": {
                                    "application/json": {
                                        "schema": {
                                            "type": "object",
                                            "properties": {
                                                "access_token": { "type": "string" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }"##;
        let idx = OpenApiIndex::from_json(spec);
        let login = idx.find("/api/auth/login", "POST").unwrap();
        let body = login.request_body.as_ref().unwrap();
        assert!(body.is_credential_login());
        assert_eq!(login.response_token_fields, vec!["access_token"]);
    }

    #[test]
    fn infer_realm_admin_and_annotator() {
        let spec = r#"{
            "paths": {
                "/api/admin/users": { "get": { "tags": ["admin"] } },
                "/api/annotators/me": { "get": { "tags": ["annotators"], "security": [] } }
            }
        }"#;
        let idx = OpenApiIndex::from_json(spec);
        let admin = idx.find("/api/admin/users", "GET").unwrap();
        assert_eq!(infer_realm(admin), AuthRealmHint::Admin);
        let ann = idx.find("/api/annotators/me", "GET").unwrap();
        assert_eq!(infer_realm(ann), AuthRealmHint::Annotator);
    }

    #[test]
    fn query_params_build_string() {
        let op = EndpointOp {
            path: "/api/items".into(),
            method: "GET".into(),
            tags: vec![],
            summary: String::new(),
            operation_id: String::new(),
            requires_auth: false,
            explicitly_public: true,
            path_params: vec![],
            has_request_body: false,
            security_schemes: vec![],
            query_params: vec![ParamSpec {
                name: "limit".into(),
                location: "query".into(),
                required: true,
                example: Some(Value::Number(10.into())),
                schema_type: Some("integer".into()),
                format: None,
            }],
            header_params: vec![],
            request_body: None,
            success_statuses: vec![200],
            response_token_fields: vec![],
        };
        assert_eq!(build_query_string(&op), Some("limit=10".into()));
    }
}
