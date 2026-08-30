//! Auth requirements derived from workflow login steps (OpenAPI-backed).

use crate::features::auth::AuthMode;
use crate::features::openapi_index::AuthRealmHint;
use crate::features::workflow::{detect_auth_realms, WorkflowScenario, WorkflowStep};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone)]
pub struct AuthInputField {
    pub key: String,
    pub label: String,
    pub required: bool,
    pub secret: bool,
    pub hint: String,
}

#[derive(Debug, Clone)]
pub struct RealmAuthSpec {
    pub realm: AuthRealmHint,
    pub title: String,
    pub login_summary: String,
    pub fields: Vec<AuthInputField>,
    /// Shown once per realm (auth mechanism notes).
    pub notes: Vec<String>,
}

/// Build per-realm auth field list from generated workflows (reflects OpenAPI login bodies).
pub fn build_realm_auth_specs(workflows: &[WorkflowScenario]) -> Vec<RealmAuthSpec> {
    let realms = detect_auth_realms(workflows);
    let mut specs = Vec::new();
    for realm in realms {
        if realm == AuthRealmHint::Public {
            continue;
        }
        if let Some(spec) = spec_for_realm(realm, workflows) {
            specs.push(spec);
        }
    }
    specs
}

fn spec_for_realm(realm: AuthRealmHint, workflows: &[WorkflowScenario]) -> Option<RealmAuthSpec> {
    match realm {
        AuthRealmHint::Admin => Some(admin_spec(workflows)),
        AuthRealmHint::User | AuthRealmHint::Annotator => login_realm_spec(realm, workflows),
        AuthRealmHint::Public => None,
    }
}

fn admin_spec(workflows: &[WorkflowScenario]) -> RealmAuthSpec {
    let header = workflows
        .iter()
        .filter(|w| workflow_realm_matches(w, AuthRealmHint::Admin))
        .flat_map(|w| w.steps.iter())
        .find_map(|s| match &s.auth_mode {
            AuthMode::ApiKeyHeader { header_name } => Some(header_name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "X-Admin-Secret".into());

    RealmAuthSpec {
        realm: AuthRealmHint::Admin,
        title: "Admin".into(),
        login_summary: format!("Header {header} on every /api/admin/* step (no email login)"),
        fields: vec![AuthInputField {
            key: "secret".into(),
            label: header.clone(),
            required: true,
            secret: true,
            hint: admin_secret_hint(&header),
        }],
        notes: vec![
            "Not your user password — this is the server admin key.".into(),
            "Env: TRACE_DIFF_ADMIN_SECRET or CONFUCIUS_ADMIN_KEY.".into(),
        ],
    }
}

fn login_realm_spec(realm: AuthRealmHint, workflows: &[WorkflowScenario]) -> Option<RealmAuthSpec> {
    let login = find_login_step(realm, workflows)?;
    let body_keys = login_body_field_keys(login);
    let mut fields = Vec::new();
    let mut seen = BTreeSet::new();

    for key in body_keys {
        if !seen.insert(key.clone()) {
            continue;
        }
        if key.eq_ignore_ascii_case("email") {
            fields.push(AuthInputField {
                key: key.clone(),
                label: "email".into(),
                required: true,
                secret: false,
                hint: email_hint(realm),
            });
        } else if key.eq_ignore_ascii_case("password") {
            fields.push(AuthInputField {
                key: key.clone(),
                label: "password".into(),
                required: true,
                secret: true,
                hint: "Account password for this realm.".into(),
            });
        } else if key.contains("captcha") {
            fields.push(AuthInputField {
                key: key.clone(),
                label: key.clone(),
                required: true,
                secret: false,
                hint: captcha_hint(),
            });
        } else {
            fields.push(AuthInputField {
                key: key.clone(),
                label: key.clone(),
                required: body_field_required(&key),
                secret: is_secret_field_name(&key),
                hint: generic_field_hint(&key),
            });
        }
    }

    if fields.is_empty() {
        fields.push(AuthInputField {
            key: "email".into(),
            label: "email".into(),
            required: true,
            secret: false,
            hint: email_hint(realm),
        });
        fields.push(AuthInputField {
            key: "password".into(),
            label: "password".into(),
            required: true,
            secret: true,
            hint: "Account password for this realm.".into(),
        });
    }

    fields.push(AuthInputField {
        key: "bearer_token".into(),
        label: "bearer token (optional)".into(),
        required: false,
        secret: true,
        hint: bearer_hint(),
    });

    let login_summary = format!(
        "{} {} — captures `{}`",
        login.method.to_ascii_uppercase(),
        login.path,
        login.capture_bearer.as_deref().unwrap_or("access_token")
    );

    let mut notes = vec![
        "Fill required fields OR paste a bearer token to skip the login step.".into(),
    ];
    if fields.iter().any(|f| f.key.contains("captcha")) {
        notes.push("This API requires captcha — email/password alone will not succeed.".into());
    }

    Some(RealmAuthSpec {
        realm,
        title: match realm {
            AuthRealmHint::User => "User".into(),
            AuthRealmHint::Annotator => "Annotator".into(),
            _ => realm.as_str().into(),
        },
        login_summary,
        fields,
        notes,
    })
}

fn find_login_step(realm: AuthRealmHint, workflows: &[WorkflowScenario]) -> Option<&WorkflowStep> {
    workflows
        .iter()
        .filter(|w| workflow_realm_matches(w, realm))
        .flat_map(|w| w.steps.iter())
        .find(|s| s.capture_bearer.is_some())
        .or_else(|| {
            workflows.iter().flat_map(|w| w.steps.iter()).find(|s| {
                s.capture_bearer.is_some() && step_realm(s).map(|r| r == realm).unwrap_or(false)
            })
        })
}

fn workflow_realm_matches(w: &WorkflowScenario, realm: AuthRealmHint) -> bool {
    w.auth_realm
        .as_deref()
        .and_then(AuthRealmHint::from_str)
        .map(|r| r == realm)
        .unwrap_or_else(|| match realm {
            AuthRealmHint::Admin => w.id.contains("admin"),
            AuthRealmHint::Annotator => w.id.contains("annotator"),
            AuthRealmHint::User => w.id == "auth_smoke" || w.id.contains("auth"),
            _ => false,
        })
}

fn step_realm(s: &WorkflowStep) -> Option<AuthRealmHint> {
    s.auth_realm.as_deref().and_then(AuthRealmHint::from_str)
}

fn login_body_field_keys(step: &WorkflowStep) -> Vec<String> {
    let Some(body) = step.body.as_ref().and_then(|b| b.as_object()) else {
        return Vec::new();
    };
    body.keys()
        .filter(|k| !k.starts_with('$'))
        .map(|k| k.to_string())
        .collect()
}

fn body_field_required(key: &str) -> bool {
    !matches!(
        key.to_ascii_lowercase().as_str(),
        "captcha_token" | "captcha" | "remember_me" | "device_id"
    )
}

fn is_secret_field_name(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("password") || k.contains("secret") || k.contains("token")
}

fn email_hint(realm: AuthRealmHint) -> String {
    match realm {
        AuthRealmHint::Annotator => {
            "Annotator account email (not the main user email). Env: TRACE_DIFF_ANNOTATOR_EMAIL."
                .into()
        }
        _ => "User account email. Env: TRACE_DIFF_EMAIL or CONFUCIUS_EMAIL.".into(),
    }
}

fn captcha_hint() -> String {
    "Complete captcha in the browser, then DevTools → Network → login request → copy \
     captcha_token from JSON body. Or paste a bearer token below to skip login. \
     Env: TRACE_DIFF_CAPTCHA_TOKEN."
        .into()
}

fn bearer_hint() -> String {
    "Alternative to login: browser DevTools → Network → POST login → Response → copy \
     access_token. Skips captcha. Env: TRACE_DIFF_BEARER_TOKEN."
        .into()
}

fn admin_secret_hint(header: &str) -> String {
    format!(
        "Value for `{header}` header. From server deploy config / .env (e.g. CONFUCIUS_ADMIN_SECRET)."
    )
}

fn generic_field_hint(key: &str) -> String {
    format!(
        "Required by OpenAPI login body field `{key}`. Check API docs or copy from a browser login request."
    )
}

/// Whether realm has minimum creds per spec (bearer OR all required login fields).
pub fn realm_spec_satisfied(
    spec: &RealmAuthSpec,
    values: &HashMap<(AuthRealmHint, String), String>,
) -> bool {
    if let Some(token) = values.get(&(spec.realm, "bearer_token".into())) {
        if !token.is_empty() {
            return true;
        }
    }
    if spec.realm == AuthRealmHint::Admin {
        return values
            .get(&(spec.realm, "secret".into()))
            .map(|s| !s.is_empty())
            .unwrap_or(false);
    }
    for field in &spec.fields {
        if field.key == "bearer_token" {
            continue;
        }
        if field.required {
            let v = values.get(&(spec.realm, field.key.clone()));
            if v.map(|s| s.is_empty()).unwrap_or(true) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::workflow::FlowKind;

    #[test]
    fn builds_user_spec_with_captcha_from_workflow_body() {
        let wfs = vec![WorkflowScenario {
            id: "auth_smoke".into(),
            label: "Auth".into(),
            description: String::new(),
            kind: FlowKind::Read,
            auth_realm: Some("user".into()),
            steps: vec![WorkflowStep {
                name: "login".into(),
                method: "POST".into(),
                path: "/api/auth/login".into(),
                body: Some(serde_json::json!({
                    "email": "${CONFUCIUS_EMAIL}",
                    "password": "${CONFUCIUS_PASSWORD}",
                    "captcha_token": "${TRACE_DIFF_CAPTCHA_TOKEN}"
                })),
                capture_bearer: Some("access_token".into()),
                expect_status: Some(200),
                ..Default::default()
            }],
        }];
        let specs = build_realm_auth_specs(&wfs);
        let user = specs.iter().find(|s| s.realm == AuthRealmHint::User).unwrap();
        assert!(user.fields.iter().any(|f| f.key == "captcha_token"));
        assert!(user.fields.iter().any(|f| f.key == "bearer_token"));
    }
}
