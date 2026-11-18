//! Loopback Authorization Code with PKCE, ID-token verification, and refresh.

use std::{
    collections::HashMap,
    io,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tiny_http::{Header, Method, Request, Response, Server};
use uuid::Uuid;

use crate::{API, ChatGpt, Error, checked, read_json};

const REGISTRATION_CLIENT: &str = "dynamic_agent_client";
const CALLBACK: &str = "/auth/callback";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct Endpoints {
    pub issuer: String,
    pub authorize: String,
    pub token: String,
    pub jwks: String,
}

impl Endpoints {
    pub fn openai() -> Self {
        let issuer = "https://auth.openai.com";
        Self {
            issuer: issuer.into(),
            authorize: format!("{issuer}/api/accounts/authorize"),
            token: format!("{issuer}/api/accounts/oauth/token"),
            jwks: format!("{issuer}/.well-known/jwks.json"),
        }
    }
}

/// Persist one opaque host identifier per installation and reuse it across accounts.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostId(Uuid);

impl Default for HostId {
    fn default() -> Self {
        Self(Uuid::new_v4())
    }
}

/// A verified account registration. Serialization includes OAuth secrets and is
/// intended for the host's protected credential store only.
#[derive(Serialize, Deserialize)]
pub struct Session {
    host: HostId,
    client_id: String,
    subject: String,
    email: Option<String>,
    id_token: String,
    access_token: String,
    refresh_token: String,
    scope: String,
    expires_at: u64,
}

impl Session {
    pub fn connection_id(&self) -> &str {
        &self.client_id
    }

    /// Stable, credential-free key for provider-native history scoped to this account registration.
    pub fn replay_scope(&self) -> String {
        let mut hash = Sha256::new();
        hash.update((self.client_id.len() as u64).to_be_bytes());
        hash.update(self.client_id.as_bytes());
        hash.update(self.subject.as_bytes());
        format!("{:x}", hash.finalize())
    }

    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    pub fn needs_refresh(&self) -> bool {
        self.expires_at <= now().saturating_add(60)
    }

    pub(crate) fn bearer(&self) -> Result<&str, Error> {
        if self.expires_at <= now()
            || !self
                .scope
                .split_ascii_whitespace()
                .any(|scope| scope == PLAN_SCOPE)
        {
            return Err(Error::Reconnect);
        }
        Ok(&self.access_token)
    }
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    id_token: Option<String>,
    token_type: String,
    expires_in: u64,
    scope: Option<String>,
}

impl Tokens {
    fn validate(&self, scope: &str) -> Result<(), Error> {
        if self.access_token.is_empty()
            || self.refresh_token.is_empty()
            || !self.token_type.eq_ignore_ascii_case("bearer")
            || self.expires_in == 0
        {
            return Err(Error::SignIn("OpenAI returned an incomplete token grant"));
        }
        if !scope
            .split_ascii_whitespace()
            .any(|scope| scope == PLAN_SCOPE)
        {
            return Err(Error::SignIn(
                "ChatGPT plan usage permission was not granted",
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct Identity {
    sub: String,
    email: Option<String>,
    nonce: Option<String>,
}

fn verify_identity(
    client: &ChatGpt,
    token: &str,
    client_id: &str,
    nonce: Option<&str>,
) -> Result<Identity, Error> {
    let invalid = || Error::SignIn("OpenAI ID token verification failed");
    let header = decode_header(token).map_err(|_| invalid())?;
    let kid = header.kid.as_deref().ok_or_else(invalid)?;
    if header.alg != Algorithm::RS256 {
        return Err(invalid());
    }
    let jwks: JwkSet = read_json(checked(
        client
            .http
            .get(&client.oauth.jwks)
            .timeout(HTTP_TIMEOUT)
            .send()
            .map_err(|_| Error::Transport)?,
    )?)?;
    let key = DecodingKey::from_jwk(jwks.find(kid).ok_or_else(invalid)?).map_err(|_| invalid())?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_required_spec_claims(&["exp", "sub", "iss", "aud"]);
    validation.set_issuer(&[&client.oauth.issuer]);
    validation.set_audience(&[client_id]);
    validation.validate_nbf = true;
    let identity = decode::<Identity>(token, &key, &validation)
        .map_err(|_| invalid())?
        .claims;
    if identity.sub.is_empty()
        || nonce.is_some_and(|nonce| identity.nonce.as_deref() != Some(nonce))
    {
        return Err(invalid());
    }
    Ok(identity)
}

pub(crate) fn sign_in(
    client: &ChatGpt,
    host: &HostId,
    returning: Option<&Session>,
    launch: impl FnOnce(&Url) -> io::Result<()>,
) -> Result<Session, Error> {
    if returning.is_some_and(|saved| &saved.host != host) {
        return Err(Error::SignIn(
            "the existing ChatGPT connection belongs to another host",
        ));
    }
    let server = Server::http("127.0.0.1:0")
        .map_err(|_| Error::SignIn("could not open the ChatGPT callback listener"))?;
    let redirect = format!("http://{}{CALLBACK}", server.server_addr());
    let verifier = random_value()?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_value()?;
    let nonce = random_value()?;
    let host_id = format!("urn:uuid:{}", host.0);
    let mut authorize = Url::parse(&client.oauth.authorize)
        .map_err(|_| Error::SignIn("invalid OpenAI authorization endpoint"))?;
    {
        let mut query = authorize.query_pairs_mut();
        if returning.is_none() {
            query.append_pair("agent_name_hint", "Syzygy");
        }
        query.extend_pairs([
            (
                "client_id",
                returning.map_or(REGISTRATION_CLIENT, Session::connection_id),
            ),
            ("ext_agent_host_id", &host_id),
            ("response_type", "code"),
            ("redirect_uri", &redirect),
            ("scope", SCOPES),
            ("resource", API),
            ("state", &state),
            ("nonce", &nonce),
            ("code_challenge_method", "S256"),
            ("code_challenge", &challenge),
        ]);
    }
    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
    launch(&authorize)
        .map_err(|_| Error::SignIn("could not open ChatGPT sign-in in the browser"))?;
    let (callback, query) = wait_callback(&server, &redirect, &state, deadline)?;
    let result = (|| {
        if query.contains_key("error") {
            return Err(Error::SignIn(
                "ChatGPT authorization was declined or failed",
            ));
        }
        let issued = query.get("client_id").map(String::as_str);
        let client_id = match (returning, issued) {
            (Some(saved), None) => saved.connection_id(),
            (Some(saved), Some(id)) if saved.connection_id() == id => id,
            (None, Some(id)) if !id.is_empty() && id != REGISTRATION_CLIENT => id,
            _ => {
                return Err(Error::SignIn(
                    "callback did not match the ChatGPT client registration",
                ));
            }
        };
        let code = query
            .get("code")
            .filter(|code| !code.is_empty())
            .ok_or(Error::SignIn(
                "ChatGPT callback did not contain an authorization code",
            ))?;
        let tokens = exchange(
            client,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", client_id),
                ("code", code),
                ("code_verifier", &verifier),
                ("redirect_uri", &redirect),
                ("resource", API),
            ],
        )?;
        let scope = tokens.scope.as_deref().unwrap_or("");
        tokens.validate(scope)?;
        let id_token = tokens
            .id_token
            .ok_or(Error::SignIn("OpenAI did not return an ID token"))?;
        let identity = verify_identity(client, &id_token, client_id, Some(&nonce))?;
        if returning.is_some_and(|saved| saved.subject != identity.sub) {
            return Err(Error::SignIn(
                "the returned ChatGPT identity did not match the selected account",
            ));
        }
        Ok(Session {
            host: host.clone(),
            client_id: client_id.to_owned(),
            subject: identity.sub,
            email: identity.email,
            id_token,
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            scope: scope.to_owned(),
            expires_at: now().saturating_add(tokens.expires_in),
        })
    })();
    if result.is_ok() {
        reply(
            callback,
            200,
            "Connected to Syzygy. You can close this tab.",
        );
    } else {
        reply(
            callback,
            400,
            "Syzygy could not connect this ChatGPT account. Return to Syzygy to continue.",
        );
    }
    result
}

pub(crate) fn refresh(client: &ChatGpt, session: &mut Session) -> Result<(), Error> {
    let tokens = exchange(
        client,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &session.client_id),
            ("refresh_token", &session.refresh_token),
            ("resource", API),
        ],
    )?;
    let scope = tokens.scope.as_deref().unwrap_or(&session.scope);
    tokens.validate(scope)?;
    if let Some(id_token) = tokens.id_token {
        let identity = verify_identity(client, &id_token, &session.client_id, None)?;
        if identity.sub != session.subject {
            return Err(Error::SignIn(
                "the renewed ChatGPT identity did not match this account",
            ));
        }
        session.id_token = id_token;
        session.email = identity.email;
    }
    session.scope = scope.to_owned();
    session.access_token = tokens.access_token;
    session.refresh_token = tokens.refresh_token;
    session.expires_at = now().saturating_add(tokens.expires_in);
    Ok(())
}

fn exchange(client: &ChatGpt, form: &[(&str, &str)]) -> Result<Tokens, Error> {
    let response = client
        .http
        .post(&client.oauth.token)
        .timeout(HTTP_TIMEOUT)
        .form(form)
        .send()
        .map_err(|_| Error::Transport)?;
    match checked(response) {
        Ok(response) => read_json(response),
        Err(Error::Http {
            code: Some(code), ..
        }) if code == "invalid_grant" => Err(Error::Reconnect),
        Err(error) => Err(error),
    }
}

fn wait_callback(
    server: &Server,
    redirect: &str,
    state: &str,
    deadline: Instant,
) -> Result<(Request, HashMap<String, String>), Error> {
    let base = Url::parse(redirect).map_err(|_| Error::SignIn("invalid loopback redirect"))?;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Some(request) = server
            .recv_timeout(remaining)
            .map_err(|_| Error::SignIn("ChatGPT callback listener failed"))?
        else {
            break;
        };
        let target = request.url();
        if request.method() != &Method::Get || target.split('?').next() != Some(CALLBACK) {
            reply(request, 404, "This is the Syzygy sign-in callback.");
            continue;
        }
        if target.len() <= 16384
            && let Ok(url) = base.join(target)
        {
            let pairs: Vec<_> = url.query_pairs().into_owned().collect();
            let count = pairs.len();
            let query: HashMap<_, _> = pairs.into_iter().collect();
            if query.len() == count && query.get("state").is_some_and(|value| value == state) {
                return Ok((request, query));
            }
        }
        reply(
            request,
            400,
            "This callback did not match the pending sign-in.",
        );
    }
    Err(Error::SignIn("ChatGPT sign-in timed out"))
}

fn reply(request: Request, status: u16, message: &str) {
    let mut response = Response::from_string(message).with_status_code(status);
    response.add_header(Header::from_bytes("Cache-Control", "no-store").expect("valid header"));
    response
        .add_header(Header::from_bytes("Referrer-Policy", "no-referrer").expect("valid header"));
    let _ = request.respond(response);
}

fn random_value() -> Result<String, Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| Error::SignIn("could not generate sign-in randomness"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
