#![forbid(unsafe_code)]
//! Replaceable Reddit adapter boundary.
//!
//! This crate is the only place that understands Reddit credentials, HTTP
//! endpoints, fullnames, response envelopes, or Markdown projection details.

mod bridge;
mod export;

pub use bridge::{
    BigStickAction, BridgeError, BridgeService, MemoryRedditCredentialStore,
    PlatformRedditCredentialStore, ProjectionAction, QueueComment, QueuePost, RedditCredential,
    RedditCredentialStore, RedditLinkService, ResolveDuplicatesAction, WithdrawalAction,
};
pub use export::{ExportError, ExportItem, ExportItemKind, ExportPreview, preview_export};

/// Resolves a portable `nostr:` URI through an optional user-selected HTTPS
/// gateway while keeping the complete Nostr identifier inside the URL.
///
/// # Errors
///
/// Returns an error for a malformed URI or unsafe/incomplete gateway template.
pub fn resolve_portable_link(
    portable: &str,
    gateway_template: Option<&str>,
) -> Result<String, RedditError> {
    let Some(identifier) = portable.strip_prefix("nostr:") else {
        return Err(RedditError::Invalid(
            "portable record link is not a nostr: URI".to_owned(),
        ));
    };
    if identifier.is_empty() || !identifier.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(RedditError::Invalid(
            "portable Nostr identifier is malformed".to_owned(),
        ));
    }
    let Some(template) = gateway_template else {
        return Ok(portable.to_owned());
    };
    if !template.starts_with("https://")
        || !template.contains("{identifier}")
        || template.chars().any(char::is_whitespace)
    {
        return Err(RedditError::Invalid(
            "gateway must be an HTTPS template containing {identifier}".to_owned(),
        ));
    }
    Ok(template.replace("{identifier}", identifier))
}

use std::{collections::BTreeSet, fmt::Write as _, io::Read as _, time::Duration};

use hydra_domain::{ContentBody, DomainError};
use reqwest::{StatusCode, blocking::Client};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

const AUTHORIZE_URL: &str = "https://www.reddit.com/api/v1/authorize.compact";
const TOKEN_URL: &str = "https://www.reddit.com/api/v1/access_token";
const API_ROOT: &str = "https://oauth.reddit.com";
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_LISTING_ITEMS: usize = 5_000;
const MAX_THREAD_ITEMS: usize = 5_000;
const MAX_THREAD_DEPTH: usize = 64;

#[derive(Debug, Error)]
pub enum RedditError {
    #[error("Reddit adapter input is invalid: {0}")]
    Invalid(String),
    #[error("Reddit authorization was rejected or expired")]
    AuthorizationRejected,
    #[error("Reddit requires authentication")]
    Unauthorized,
    #[error("Reddit rejected the operation: {0}")]
    Rejected(String),
    #[error("Reddit rate limited the operation")]
    RateLimited,
    #[error("Reddit transport failed: {0}")]
    Transport(String),
    #[error("Reddit response could not be understood: {0}")]
    Response(String),
    #[error("the Reddit object is unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthRequest {
    pub authorization_url: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_in: u64,
    #[serde(default)]
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedditIdentity {
    pub username: String,
    pub account_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RedditFullname(String);

impl RedditFullname {
    /// Parses one Reddit post (`t3_`) or comment (`t1_`) fullname.
    ///
    /// # Errors
    ///
    /// Returns an error for any other object type or malformed base36 ID.
    pub fn parse(value: impl Into<String>) -> Result<Self, RedditError> {
        let value = value.into();
        let Some((prefix, id)) = value.split_once('_') else {
            return Err(RedditError::Invalid("missing fullname prefix".to_owned()));
        };
        if !matches!(prefix, "t1" | "t3")
            || id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase())
        {
            return Err(RedditError::Invalid(format!(
                "unsupported Reddit fullname {value}"
            )));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_comment(&self) -> bool {
        self.0.starts_with("t1_")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedditThing {
    pub fullname: RedditFullname,
    pub author: Option<String>,
    pub subreddit: String,
    pub title: Option<String>,
    pub body: String,
    pub permalink: String,
    pub parent: Option<RedditFullname>,
    pub locked: bool,
    pub removed: bool,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub media_urls: Vec<String>,
    pub edited_at: Option<u64>,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPage {
    pub things: Vec<RedditThing>,
    pub after: Option<String>,
}

/// One centrally imposed subreddit rule shown only in Hydra's `/r/` chamber.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedditRule {
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitPost {
    pub subreddit: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitComment {
    pub parent: RedditFullname,
    pub body: String,
}

pub trait RedditAdapter {
    /// Returns the currently authenticated Reddit identity.
    ///
    /// # Errors
    /// Returns an error when Reddit cannot authenticate or respond.
    fn identity(&self) -> Result<RedditIdentity, RedditError>;
    /// Creates one self/text post and returns its exact Reddit identity.
    ///
    /// # Errors
    /// Returns an error when validation or submission fails.
    fn submit_post(&self, request: &SubmitPost) -> Result<RedditThing, RedditError>;
    /// Creates one comment under the exact supplied parent.
    ///
    /// # Errors
    /// Returns an error when validation or submission fails.
    fn submit_comment(&self, request: &SubmitComment) -> Result<RedditThing, RedditError>;
    /// Replaces text authored by the authenticated account.
    ///
    /// # Errors
    /// Returns an error when validation or editing fails.
    fn edit(&self, target: &RedditFullname, body: &str) -> Result<RedditThing, RedditError>;
    /// Deletes one item authored by the authenticated account.
    ///
    /// # Errors
    /// Returns an error when deletion is rejected or unavailable.
    fn delete(&self, target: &RedditFullname) -> Result<(), RedditError>;
    /// Fetches one exact live Reddit object.
    ///
    /// # Errors
    /// Returns an error when the object is unavailable or malformed.
    fn fetch(&self, target: &RedditFullname) -> Result<RedditThing, RedditError>;
    /// Fetches one bounded page of an account's own history.
    ///
    /// # Errors
    /// Returns an error when the account history cannot be fetched.
    fn history(&self, username: &str, after: Option<&str>) -> Result<HistoryPage, RedditError>;

    /// Fetches one bounded page from a subreddit without making it durable.
    ///
    /// # Errors
    /// Returns an error when browsing is unsupported or unavailable.
    fn community(
        &self,
        _subreddit: &str,
        _sort: &str,
        _after: Option<&str>,
    ) -> Result<HistoryPage, RedditError> {
        Err(RedditError::Invalid(
            "Reddit community browsing is unavailable".to_owned(),
        ))
    }

    /// Fetches the subreddit operator's current rules as external context.
    ///
    /// # Errors
    /// Returns an error when rules are unsupported or unavailable.
    fn community_rules(&self, _subreddit: &str) -> Result<Vec<RedditRule>, RedditError> {
        Err(RedditError::Invalid(
            "Reddit community rules are unavailable".to_owned(),
        ))
    }

    /// Fetches one post and its currently returned comment tree without making
    /// any item durable.
    ///
    /// # Errors
    /// Returns an error when browsing is unsupported or unavailable.
    fn thread(&self, _post: &RedditFullname) -> Result<Vec<RedditThing>, RedditError> {
        Err(RedditError::Invalid(
            "Reddit thread browsing is unavailable".to_owned(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct RedditDataApi {
    client: Client,
    client_id: String,
    redirect_uri: String,
    user_agent: String,
    access_token: String,
}

impl RedditDataApi {
    /// Builds the system-browser OAuth request with a fresh CSRF state.
    ///
    /// # Errors
    ///
    /// Returns an error for missing configuration or URL encoding failure.
    pub fn authorization_request(
        client_id: &str,
        redirect_uri: &str,
    ) -> Result<OAuthRequest, RedditError> {
        if client_id.trim().is_empty()
            || client_id.len() > 256
            || client_id.chars().any(char::is_control)
            || validate_oauth_redirect(redirect_uri).is_err()
        {
            return Err(RedditError::Invalid(
                "OAuth client ID and redirect URI are required".to_owned(),
            ));
        }
        let state = uuid::Uuid::new_v4().to_string();
        let mut url = Url::parse(AUTHORIZE_URL).map_err(response_error)?;
        url.query_pairs_mut()
            .append_pair("client_id", client_id)
            .append_pair("response_type", "code")
            .append_pair("state", &state)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("duration", "permanent")
            .append_pair("scope", "identity read history submit edit");
        Ok(OAuthRequest {
            authorization_url: url.to_string(),
            state,
        })
    }

    /// Exchanges a browser callback code for OAuth tokens.
    ///
    /// # Errors
    ///
    /// Returns an error for state mismatch, rejection, transport, or malformed
    /// token response.
    pub fn exchange_code(
        client_id: &str,
        redirect_uri: &str,
        expected_state: &str,
        callback_state: &str,
        code: &str,
        user_agent: &str,
    ) -> Result<OAuthTokens, RedditError> {
        if expected_state != callback_state
            || expected_state.len() > 256
            || expected_state.chars().any(char::is_control)
            || code.trim().is_empty()
            || code.len() > 4_096
            || code.chars().any(char::is_control)
            || client_id.trim().is_empty()
            || client_id.len() > 256
            || client_id.chars().any(char::is_control)
            || validate_oauth_redirect(redirect_uri).is_err()
        {
            return Err(RedditError::AuthorizationRejected);
        }
        let client = http_client(user_agent)?;
        token_request(
            &client,
            client_id,
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
            ],
        )
    }

    /// Refreshes an installed-app OAuth token.
    ///
    /// # Errors
    ///
    /// Returns an error for rejection, transport, or malformed token response.
    pub fn refresh(
        client_id: &str,
        refresh_token: &str,
        user_agent: &str,
    ) -> Result<OAuthTokens, RedditError> {
        if client_id.trim().is_empty()
            || client_id.len() > 256
            || client_id.chars().any(char::is_control)
            || refresh_token.trim().is_empty()
            || refresh_token.len() > 8_192
            || refresh_token.chars().any(char::is_control)
        {
            return Err(RedditError::AuthorizationRejected);
        }
        let client = http_client(user_agent)?;
        token_request(
            &client,
            client_id,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
        )
    }

    /// Creates an authenticated adapter. Tokens remain caller-owned and are
    /// never persisted by this crate.
    ///
    /// # Errors
    ///
    /// Returns an error for missing fields or HTTP client construction.
    pub fn new(
        client_id: String,
        redirect_uri: String,
        user_agent: String,
        access_token: String,
    ) -> Result<Self, RedditError> {
        if client_id.trim().is_empty()
            || client_id.len() > 256
            || validate_oauth_redirect(&redirect_uri).is_err()
            || access_token.trim().is_empty()
            || access_token.len() > 8_192
            || [
                client_id.as_str(),
                redirect_uri.as_str(),
                access_token.as_str(),
            ]
            .iter()
            .any(|value| value.chars().any(char::is_control))
        {
            return Err(RedditError::Invalid(
                "Reddit OAuth configuration is incomplete".to_owned(),
            ));
        }
        let client = http_client(&user_agent)?;
        Ok(Self {
            client,
            client_id,
            redirect_uri,
            user_agent,
            access_token,
        })
    }

    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    #[must_use]
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<serde_json::Value, RedditError> {
        let response = self
            .client
            .get(format!("{API_ROOT}{path}"))
            .bearer_auth(&self.access_token)
            .query(query)
            .send()
            .map_err(transport_error)?;
        checked_json(response)
    }

    fn post(&self, path: &str, form: &[(&str, &str)]) -> Result<serde_json::Value, RedditError> {
        let response = self
            .client
            .post(format!("{API_ROOT}{path}"))
            .bearer_auth(&self.access_token)
            .form(form)
            .send()
            .map_err(transport_error)?;
        checked_json(response)
    }
}

impl RedditAdapter for RedditDataApi {
    fn identity(&self) -> Result<RedditIdentity, RedditError> {
        let value = self.get("/api/v1/me", &[("raw_json", "1")])?;
        let username = required_string(&value, "name")?;
        validate_username(&username)?;
        let account_id = required_string(&value, "id")?;
        if account_id.len() > 128
            || !account_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(RedditError::Response(
                "Reddit account ID is malformed".to_owned(),
            ));
        }
        Ok(RedditIdentity {
            username,
            account_id,
        })
    }

    fn submit_post(&self, request: &SubmitPost) -> Result<RedditThing, RedditError> {
        validate_submission(&request.title, &request.body, &request.subreddit)?;
        let value = self.post(
            "/api/submit",
            &[
                ("api_type", "json"),
                ("kind", "self"),
                ("sr", &request.subreddit),
                ("title", &request.title),
                ("text", &request.body),
                ("resubmit", "true"),
                ("raw_json", "1"),
            ],
        )?;
        self.fetch(&submission_fullname(&value)?)
    }

    fn submit_comment(&self, request: &SubmitComment) -> Result<RedditThing, RedditError> {
        ContentBody::parse(request.body.clone()).map_err(|error| domain_error(&error))?;
        let value = self.post(
            "/api/comment",
            &[
                ("api_type", "json"),
                ("thing_id", request.parent.as_str()),
                ("text", &request.body),
                ("raw_json", "1"),
            ],
        )?;
        self.fetch(&submission_fullname(&value)?)
    }

    fn edit(&self, target: &RedditFullname, body: &str) -> Result<RedditThing, RedditError> {
        ContentBody::parse(body.to_owned()).map_err(|error| domain_error(&error))?;
        self.post(
            "/api/editusertext",
            &[
                ("api_type", "json"),
                ("thing_id", target.as_str()),
                ("text", body),
                ("raw_json", "1"),
            ],
        )?;
        self.fetch(target)
    }

    fn delete(&self, target: &RedditFullname) -> Result<(), RedditError> {
        self.post("/api/del", &[("id", target.as_str())])?;
        Ok(())
    }

    fn fetch(&self, target: &RedditFullname) -> Result<RedditThing, RedditError> {
        let value = self.get("/api/info", &[("id", target.as_str()), ("raw_json", "1")])?;
        listing_things(&value)?
            .into_iter()
            .next()
            .ok_or(RedditError::Unavailable)
    }

    fn history(&self, username: &str, after: Option<&str>) -> Result<HistoryPage, RedditError> {
        validate_username(username)?;
        validate_listing_cursor(after)?;
        let mut query = vec![("limit", "100"), ("raw_json", "1")];
        if let Some(after) = after {
            query.push(("after", after));
        }
        let value = self.get(&format!("/user/{username}/overview"), &query)?;
        Ok(HistoryPage {
            things: listing_things(&value)?,
            after: value
                .pointer("/data/after")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        })
    }

    fn community(
        &self,
        subreddit: &str,
        sort: &str,
        after: Option<&str>,
    ) -> Result<HistoryPage, RedditError> {
        validate_subreddit(subreddit)?;
        validate_listing_cursor(after)?;
        if !matches!(sort, "hot" | "new" | "top" | "controversial") {
            return Err(RedditError::Invalid("unsupported Reddit sort".to_owned()));
        }
        let mut query = vec![("limit", "100"), ("raw_json", "1")];
        if let Some(after) = after {
            query.push(("after", after));
        }
        let value = self.get(&format!("/r/{subreddit}/{sort}"), &query)?;
        Ok(HistoryPage {
            things: listing_things(&value)?,
            after: value
                .pointer("/data/after")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        })
    }

    fn community_rules(&self, subreddit: &str) -> Result<Vec<RedditRule>, RedditError> {
        validate_subreddit(subreddit)?;
        let value = self.get(&format!("/r/{subreddit}/about/rules"), &[("raw_json", "1")])?;
        parse_community_rules(&value)
    }

    fn thread(&self, post: &RedditFullname) -> Result<Vec<RedditThing>, RedditError> {
        if post.is_comment() {
            return Err(RedditError::Invalid(
                "a Reddit thread requires a t3 post fullname".to_owned(),
            ));
        }
        let id = post
            .as_str()
            .strip_prefix("t3_")
            .ok_or_else(|| RedditError::Invalid("invalid Reddit post fullname".to_owned()))?;
        let value = self.get(&format!("/comments/{id}"), &[("raw_json", "1")])?;
        let listings = value.as_array().ok_or_else(|| {
            RedditError::Response("thread response is not a listing pair".to_owned())
        })?;
        let mut things = Vec::new();
        for listing in listings {
            collect_listing_things(listing, &mut things)?;
        }
        Ok(things)
    }
}

fn parse_community_rules(value: &serde_json::Value) -> Result<Vec<RedditRule>, RedditError> {
    let rules = value
        .get("rules")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| RedditError::Response("community rules are not an array".to_owned()))?;
    rules
        .iter()
        .map(|rule| {
            let title = rule
                .get("short_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Untitled rule")
                .trim()
                .to_owned();
            let description = rule
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned();
            Ok(RedditRule { title, description })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution<'a> {
    None,
    PostedFromHydra(&'a str),
    BigStick(&'a str),
    /// An already-rendered marker retained by an existing projection.
    Literal(&'a str),
}

impl Attribution<'_> {
    fn suffix(self) -> Result<Option<String>, RedditError> {
        match self {
            Self::None => Ok(None),
            Self::PostedFromHydra(link) => {
                validate_link(link)?;
                Ok(Some(format!("[Posted from Hydra]({link})")))
            }
            Self::BigStick(link) => {
                validate_link(link)?;
                Ok(Some(format!("↳ [Uncensorable record]({link})")))
            }
            Self::Literal(value) => Ok(Some(value.to_owned())),
        }
    }
}

/// Renders canonical Hydra text into the deliberately small Reddit Markdown
/// subset used by the bridge and reports every transformation.
///
/// # Errors
///
/// Returns an error for empty/oversized content or an invalid portable link.
pub fn render_markdown(
    body: &str,
    attribution: Attribution<'_>,
) -> Result<(String, Vec<String>), RedditError> {
    let body = ContentBody::parse(body.to_owned()).map_err(|error| domain_error(&error))?;
    let mut rendered = body.as_str().replace("\r\n", "\n").replace('\r', "\n");
    let mut losses = BTreeSet::new();
    if rendered.contains("<details") || rendered.contains("</details>") {
        rendered = rendered.replace("<details>", "").replace("</details>", "");
        losses.insert("removed unsupported HTML details tags".to_owned());
    }
    if let Some(suffix) = attribution.suffix()? {
        write!(rendered, "\n\n{suffix}").expect("writing to a String cannot fail");
    }
    Ok((rendered, losses.into_iter().collect()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithdrawalMarker<'a> {
    Reddacted(&'a str),
    Withdrawn(&'a str),
    Continues(&'a str),
    Elsewhere(&'a str),
    CustomLinked { label: &'a str, link: &'a str },
    Custom(&'a str),
}

/// Renders one terminal Reddacted replacement marker.
///
/// # Errors
///
/// Returns an error for an invalid portable link or empty custom marker.
pub fn withdrawal_marker(marker: WithdrawalMarker<'_>) -> Result<String, RedditError> {
    let (label, link) = match marker {
        WithdrawalMarker::Reddacted(link) => ("Reddacted — view in Hydra", link),
        WithdrawalMarker::Withdrawn(link) => ("Withdrawn from Reddit — view in Hydra", link),
        WithdrawalMarker::Continues(link) => ("The discussion continues on Hydra", link),
        WithdrawalMarker::Elsewhere(link) => {
            ("Redacted. The discussion continues elsewhere.", link)
        }
        WithdrawalMarker::CustomLinked { label, link } => {
            let label = label.trim();
            if label.is_empty() || label.len() > 300 || has_unsafe_marker_text(label) {
                return Err(RedditError::Invalid(
                    "custom withdrawal marker cannot be empty".to_owned(),
                ));
            }
            validate_link(link)?;
            return Ok(format!("[{}]({link})", label.replace(['[', ']'], "")));
        }
        WithdrawalMarker::Custom(value) => {
            if ContentBody::parse(value.to_owned()).is_err() {
                return Err(RedditError::Invalid(
                    "custom withdrawal marker cannot be empty".to_owned(),
                ));
            }
            return Ok(value.to_owned());
        }
    };
    validate_link(link)?;
    Ok(format!("[{label}]({link})"))
}

fn validate_oauth_redirect(value: &str) -> Result<(), RedditError> {
    if value.len() > 2_048 || value.chars().any(char::is_control) {
        return Err(RedditError::Invalid(
            "OAuth redirect URI is malformed".to_owned(),
        ));
    }
    let url = Url::parse(value)
        .map_err(|_| RedditError::Invalid("OAuth redirect URI is malformed".to_owned()))?;
    let loopback = url
        .host()
        .and_then(|host| match host {
            url::Host::Ipv4(address) => Some(address.is_loopback()),
            url::Host::Ipv6(address) => Some(address.is_loopback()),
            url::Host::Domain(_) => None,
        })
        .unwrap_or(false);
    if url.scheme() != "http"
        || !loopback
        || url.username() != ""
        || url.password().is_some()
        || url.port().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/oauth/reddit"
    {
        return Err(RedditError::Invalid(
            "OAuth redirect must be Hydra's loopback callback".to_owned(),
        ));
    }
    Ok(())
}

fn validate_listing_cursor(after: Option<&str>) -> Result<(), RedditError> {
    if let Some(after) = after {
        RedditFullname::parse(after).map(|_| ())?;
    }
    Ok(())
}

fn has_unsafe_marker_text(value: &str) -> bool {
    value.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
    })
}

fn http_client(user_agent: &str) -> Result<Client, RedditError> {
    if user_agent.trim().is_empty()
        || user_agent.len() > 512
        || user_agent.chars().any(char::is_control)
    {
        return Err(RedditError::Invalid("User-Agent is required".to_owned()));
    }
    Client::builder()
        .user_agent(user_agent)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(transport_error)
}

fn token_request(
    client: &Client,
    client_id: &str,
    form: &[(&str, &str)],
) -> Result<OAuthTokens, RedditError> {
    let response = client
        .post(TOKEN_URL)
        .basic_auth(client_id, Some(""))
        .form(form)
        .send()
        .map_err(transport_error)?;
    let value = checked_json(response)?;
    if value.get("error").is_some() {
        return Err(RedditError::AuthorizationRejected);
    }
    let tokens: OAuthTokens = serde_json::from_value(value).map_err(response_error)?;
    if tokens.access_token.is_empty()
        || tokens.access_token.len() > 8_192
        || tokens
            .refresh_token
            .as_ref()
            .is_some_and(|token| token.len() > 8_192)
        || tokens.scope.len() > 1_024
    {
        return Err(RedditError::Response(
            "Reddit OAuth token response exceeds Hydra's safety limits".to_owned(),
        ));
    }
    Ok(tokens)
}

fn checked_json(response: reqwest::blocking::Response) -> Result<serde_json::Value, RedditError> {
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(RedditError::Unauthorized);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(RedditError::RateLimited);
    }
    if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
        return Err(RedditError::Unavailable);
    }
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(MAX_RESPONSE_BYTES).unwrap_or(u64::MAX))
    {
        return Err(RedditError::Response(
            "Reddit response exceeds the 8 MiB safety limit".to_owned(),
        ));
    }
    let mut bytes = Vec::new();
    response
        .take(u64::try_from(MAX_RESPONSE_BYTES + 1).expect("fixed response limit fits u64"))
        .read_to_end(&mut bytes)
        .map_err(transport_error)?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(RedditError::Response(
            "Reddit response exceeds the 8 MiB safety limit".to_owned(),
        ));
    }
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(response_error)?;
    if !status.is_success() {
        return Err(RedditError::Rejected(reddit_error_text(&value)));
    }
    if let Some(errors) = value
        .pointer("/json/errors")
        .and_then(serde_json::Value::as_array)
        && !errors.is_empty()
    {
        return Err(RedditError::Rejected(reddit_error_text(&value)));
    }
    Ok(value)
}

fn listing_things(value: &serde_json::Value) -> Result<Vec<RedditThing>, RedditError> {
    let children = value
        .pointer("/data/children")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| RedditError::Response("listing has no children".to_owned()))?;
    if children.len() > MAX_LISTING_ITEMS {
        return Err(RedditError::Response(
            "Reddit listing exceeds the item safety limit".to_owned(),
        ));
    }
    children
        .iter()
        .map(|child| parse_thing(&child["data"]))
        .collect()
}

fn collect_listing_things(
    value: &serde_json::Value,
    things: &mut Vec<RedditThing>,
) -> Result<(), RedditError> {
    let children = listing_children(value)?;
    let mut pending = children
        .iter()
        .rev()
        .map(|child| (child, 0_usize))
        .collect::<Vec<_>>();
    while let Some((child, depth)) = pending.pop() {
        if depth > MAX_THREAD_DEPTH {
            return Err(RedditError::Response(
                "Reddit thread exceeds the nesting safety limit".to_owned(),
            ));
        }
        let data = &child["data"];
        let kind = child.get("kind").and_then(serde_json::Value::as_str);
        if matches!(kind, Some("t1" | "t3")) {
            if things.len() >= MAX_THREAD_ITEMS {
                return Err(RedditError::Response(
                    "Reddit thread exceeds the item safety limit".to_owned(),
                ));
            }
            things.push(parse_thing(data)?);
        }
        if let Some(replies) = data.get("replies").filter(|replies| replies.is_object()) {
            pending.extend(
                listing_children(replies)?
                    .iter()
                    .rev()
                    .map(|reply| (reply, depth + 1)),
            );
        }
    }
    Ok(())
}

fn listing_children(value: &serde_json::Value) -> Result<&[serde_json::Value], RedditError> {
    value
        .pointer("/data/children")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| RedditError::Response("listing has no children".to_owned()))
}

fn parse_thing(data: &serde_json::Value) -> Result<RedditThing, RedditError> {
    let fullname = RedditFullname::parse(required_string(data, "name")?)?;
    let body = if fullname.is_comment() {
        data.get("body")
    } else {
        data.get("selftext")
    }
    .and_then(serde_json::Value::as_str)
    .unwrap_or_default()
    .to_owned();
    if body.len() > ContentBody::MAX_LEN {
        return Err(RedditError::Response(
            "Reddit body exceeds Hydra's safety limit".to_owned(),
        ));
    }
    let parent = data
        .get("parent_id")
        .and_then(serde_json::Value::as_str)
        .map(RedditFullname::parse)
        .transpose()?;
    let author = data
        .get("author")
        .and_then(serde_json::Value::as_str)
        .filter(|author| *author != "[deleted]")
        .map(str::to_owned);
    if let Some(author) = &author {
        validate_username(author)?;
    }
    let subreddit = required_string(data, "subreddit")?;
    validate_subreddit(&subreddit)?;
    let title = data
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if title
        .as_ref()
        .is_some_and(|title| title.len() > 300 || title.chars().any(char::is_control))
    {
        return Err(RedditError::Response(
            "Reddit title exceeds Hydra's safety limit".to_owned(),
        ));
    }
    let permalink = required_string(data, "permalink")?;
    if permalink.len() > 4_096
        || !permalink.starts_with('/')
        || permalink.chars().any(char::is_control)
    {
        return Err(RedditError::Response(
            "Reddit permalink is malformed".to_owned(),
        ));
    }
    let removed = body == "[removed]";
    let deleted = body == "[deleted]";
    let media_urls = reddit_media_urls(data);
    Ok(RedditThing {
        fullname,
        author,
        subreddit,
        title,
        body,
        permalink,
        parent,
        locked: data
            .get("locked")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        removed,
        deleted,
        media_urls,
        edited_at: numeric_timestamp(data.get("edited")),
        created_at: numeric_timestamp(data.get("created_utc")).unwrap_or_default(),
    })
}

fn reddit_media_urls(data: &serde_json::Value) -> Vec<String> {
    let candidates = [
        data.get("url_overridden_by_dest")
            .and_then(serde_json::Value::as_str),
        data.pointer("/secure_media/reddit_video/fallback_url")
            .and_then(serde_json::Value::as_str),
        data.pointer("/preview/reddit_video_preview/fallback_url")
            .and_then(serde_json::Value::as_str),
    ];
    let mut urls = BTreeSet::new();
    for candidate in candidates.into_iter().flatten() {
        let candidate = candidate.replace("&amp;", "&");
        if candidate.len() <= 4_096
            && Url::parse(&candidate).is_ok_and(|url| {
                matches!(url.scheme(), "https" | "http") && url.host_str().is_some()
            })
        {
            urls.insert(candidate);
        }
    }
    urls.into_iter().take(32).collect()
}

fn numeric_timestamp(value: Option<&serde_json::Value>) -> Option<u64> {
    value.and_then(|value| {
        value.as_u64().or_else(|| {
            value
                .as_f64()
                .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
                .map(|duration| duration.as_secs())
        })
    })
}

fn submission_fullname(value: &serde_json::Value) -> Result<RedditFullname, RedditError> {
    value
        .pointer("/json/data/name")
        .or_else(|| value.pointer("/json/data/things/0/data/name"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| RedditError::Response("submission response has no fullname".to_owned()))
        .and_then(RedditFullname::parse)
}

fn required_string(value: &serde_json::Value, key: &str) -> Result<String, RedditError> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            !value.trim().is_empty() && value.len() <= 4_096 && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(|| RedditError::Response(format!("missing {key}")))
}

fn validate_username(username: &str) -> Result<(), RedditError> {
    if username.is_empty()
        || username.len() > 64
        || !username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(RedditError::Invalid(
            "Reddit username is malformed".to_owned(),
        ));
    }
    Ok(())
}

fn validate_submission(title: &str, body: &str, subreddit: &str) -> Result<(), RedditError> {
    if title.trim().is_empty() || title.len() > 300 || title.chars().any(char::is_control) {
        return Err(RedditError::Invalid(
            "post title and subreddit are required".to_owned(),
        ));
    }
    validate_subreddit(subreddit)?;
    ContentBody::parse(body.to_owned()).map_err(|error| domain_error(&error))?;
    Ok(())
}

fn validate_subreddit(subreddit: &str) -> Result<(), RedditError> {
    if subreddit.is_empty()
        || subreddit.len() > 21
        || !subreddit
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(RedditError::Invalid(
            "subreddit must be a bare Reddit community name".to_owned(),
        ));
    }
    Ok(())
}

fn validate_link(link: &str) -> Result<(), RedditError> {
    if link.starts_with("nostr:nevent1") || link.starts_with("hydra:") {
        return Ok(());
    }
    let url = Url::parse(link).map_err(response_error)?;
    if url.scheme() == "https" {
        Ok(())
    } else {
        Err(RedditError::Invalid(
            "portable link must be nostr:, hydra:, or HTTPS".to_owned(),
        ))
    }
}

fn reddit_error_text(value: &serde_json::Value) -> String {
    let mut result = value
        .pointer("/json/errors")
        .map(ToString::to_string)
        .or_else(|| value.get("message").map(ToString::to_string))
        .unwrap_or_else(|| "unknown Reddit error".to_owned());
    result.truncate(2_048);
    result
}

fn domain_error(error: &DomainError) -> RedditError {
    RedditError::Invalid(error.to_string())
}

fn transport_error(error: impl std::fmt::Display) -> RedditError {
    RedditError::Transport(error.to_string())
}

fn response_error(error: impl std::fmt::Display) -> RedditError {
    RedditError::Response(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_request_has_state_and_only_settled_scopes() {
        let request = RedditDataApi::authorization_request(
            "client-id",
            "http://127.0.0.1:43117/oauth/reddit",
        )
        .unwrap();
        let url = Url::parse(&request.authorization_url).unwrap();
        let query = url
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();

        assert_eq!(query.get("state").unwrap(), &request.state);
        assert_eq!(query.get("duration").unwrap(), "permanent");
        assert_eq!(
            query.get("scope").unwrap(),
            "identity read history submit edit"
        );
        assert!(
            RedditDataApi::authorization_request(
                "client-id",
                "https://attacker.example/oauth/reddit"
            )
            .is_err()
        );
        assert!(
            RedditDataApi::authorization_request(
                "client-id",
                "http://localhost:43117/oauth/reddit"
            )
            .is_err()
        );
        assert!(
            RedditDataApi::authorization_request(
                "client-id",
                "http://127.0.0.1:43117/oauth/reddit?leak=1"
            )
            .is_err()
        );
    }

    #[test]
    fn fullname_parser_rejects_non_post_comment_objects() {
        assert!(RedditFullname::parse("t1_abc123").unwrap().is_comment());
        assert!(!RedditFullname::parse("t3_abc123").unwrap().is_comment());
        assert!(RedditFullname::parse("t2_account").is_err());
        assert!(RedditFullname::parse("../comment").is_err());
    }

    fn comment_child(index: usize, replies: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "kind": "t1",
            "data": {
                "name": format!("t1_{index:x}"),
                "body": "bounded",
                "author": "qa",
                "subreddit": "science",
                "permalink": format!("/r/science/comments/root/thread/{index:x}/"),
                "parent_id": "t3_root",
                "created_utc": 1,
                "replies": replies
            }
        })
    }

    #[test]
    fn hostile_thread_depth_is_rejected_without_recursive_descent() {
        let mut replies = serde_json::json!({"data":{"children":[]}});
        for index in 0..=MAX_THREAD_DEPTH + 1 {
            replies = serde_json::json!({"data":{"children":[comment_child(index, &replies)]}});
        }
        let error = collect_listing_things(&replies, &mut Vec::new()).unwrap_err();
        assert!(error.to_string().contains("nesting safety limit"));
    }

    #[test]
    fn hostile_thread_width_is_bounded() {
        let children = (0..=MAX_THREAD_ITEMS)
            .map(|index| comment_child(index, &serde_json::Value::String(String::new())))
            .collect::<Vec<_>>();
        let listing = serde_json::json!({"data":{"children":children}});
        let error = collect_listing_things(&listing, &mut Vec::new()).unwrap_err();
        assert!(error.to_string().contains("item safety limit"));
    }

    #[test]
    fn hostile_flat_listing_width_and_cursor_are_bounded() {
        let children = (0..=MAX_LISTING_ITEMS)
            .map(|index| comment_child(index, &serde_json::Value::String(String::new())))
            .collect::<Vec<_>>();
        let listing = serde_json::json!({"data":{"children":children}});
        assert!(
            listing_things(&listing)
                .unwrap_err()
                .to_string()
                .contains("item safety limit")
        );
        assert!(validate_listing_cursor(Some("t3_valid")).is_ok());
        assert!(validate_listing_cursor(Some("../escape")).is_err());
        assert!(validate_listing_cursor(Some("t3_valid?limit=999")).is_err());
    }

    #[test]
    fn bridge_rendering_is_opt_in_and_reports_loss() {
        let (plain, losses) = render_markdown("Hello", Attribution::None).unwrap();
        assert_eq!(plain, "Hello");
        assert!(losses.is_empty());

        let (marked, losses) = render_markdown(
            "<details>Context</details>",
            Attribution::BigStick("nostr:nevent1example"),
        )
        .unwrap();
        assert!(marked.contains("Uncensorable record"));
        assert_eq!(losses, vec!["removed unsupported HTML details tags"]);
    }

    #[test]
    fn withdrawal_presets_are_explicit_and_portable() {
        assert_eq!(
            withdrawal_marker(WithdrawalMarker::Withdrawn("nostr:nevent1example")).unwrap(),
            "[Withdrawn from Reddit — view in Hydra](nostr:nevent1example)"
        );
        assert_eq!(
            withdrawal_marker(WithdrawalMarker::Elsewhere("nostr:nevent1example")).unwrap(),
            "[Redacted. The discussion continues elsewhere.](nostr:nevent1example)"
        );
        assert_eq!(
            withdrawal_marker(WithdrawalMarker::CustomLinked {
                label: "Moved with care",
                link: "nostr:nevent1example",
            })
            .unwrap(),
            "[Moved with care](nostr:nevent1example)"
        );
        assert!(withdrawal_marker(WithdrawalMarker::Custom(" ")).is_err());
        assert!(
            withdrawal_marker(WithdrawalMarker::CustomLinked {
                label: "Hydra \u{202e} spoof",
                link: "nostr:nevent1example",
            })
            .is_err()
        );
        assert_eq!(
            resolve_portable_link(
                "nostr:nevent1example",
                Some("https://reader.example/{identifier}")
            )
            .unwrap(),
            "https://reader.example/nevent1example"
        );
        assert!(
            resolve_portable_link("nostr:nevent1example", Some("http://unsafe/{identifier}"))
                .is_err()
        );
    }

    #[test]
    fn reddit_listing_parser_preserves_live_status_and_topology() {
        let value = serde_json::json!({
            "data": {"children": [{"data": {
                "name": "t1_reply",
                "author": "alice",
                "subreddit": "science",
                "body": "Evidence",
                "permalink": "/r/science/comments/post/thread/reply/",
                "parent_id": "t3_post",
                "locked": true,
                "edited": 42
            }}]}
        });
        let thing = listing_things(&value).unwrap().remove(0);
        assert_eq!(thing.parent.unwrap().as_str(), "t3_post");
        assert!(thing.locked);
        assert!(!thing.removed);
        assert_eq!(thing.edited_at, Some(42));
    }

    #[test]
    fn reddit_parser_accepts_empty_link_posts_and_keeps_deleted_author_distinct() {
        let value = serde_json::json!({
            "data": {"children": [{"data": {
                "name": "t3_link",
                "author": "[deleted]",
                "subreddit": "science",
                "title": "External paper",
                "selftext": "",
                "url_overridden_by_dest": "https://example.org/paper.pdf",
                "permalink": "/r/science/comments/link/external_paper/"
            }}]}
        });
        let thing = listing_things(&value).unwrap().remove(0);
        assert_eq!(thing.body, "");
        assert_eq!(thing.author, None);
        assert!(!thing.removed);
        assert!(!thing.deleted);
        assert_eq!(thing.media_urls, vec!["https://example.org/paper.pdf"]);
    }

    #[test]
    fn reddit_parser_distinguishes_removed_from_deleted_source_text() {
        for (body, removed, deleted) in [("[removed]", true, false), ("[deleted]", false, true)] {
            let value = serde_json::json!({
                "data": {"children": [{"data": {
                    "name": "t1_reply",
                    "author": "[deleted]",
                    "subreddit": "science",
                    "body": body,
                    "permalink": "/r/science/comments/post/thread/reply/",
                    "parent_id": "t3_post"
                }}]}
            });
            let thing = listing_things(&value).unwrap().remove(0);
            assert_eq!(thing.removed, removed);
            assert_eq!(thing.deleted, deleted);
        }
    }

    #[test]
    fn nested_thread_parser_preserves_every_loaded_reply_once() {
        let value = serde_json::json!({"data":{"children":[{
            "kind":"t1",
            "data":{
                "name":"t1_parent",
                "author":"alice",
                "subreddit":"science",
                "body":"Parent",
                "permalink":"/r/science/comments/post/title/parent/",
                "parent_id":"t3_post",
                "created_utc":1,
                "replies":{"data":{"children":[{
                    "kind":"t1",
                    "data":{
                        "name":"t1_child",
                        "author":"bob",
                        "subreddit":"science",
                        "body":"Child",
                        "permalink":"/r/science/comments/post/title/child/",
                        "parent_id":"t1_parent",
                        "created_utc":2,
                        "replies":""
                    }
                }]}}
            }
        }]}});
        let mut things = Vec::new();
        collect_listing_things(&value, &mut things).unwrap();
        assert_eq!(things.len(), 2);
        assert_eq!(things[0].fullname.as_str(), "t1_parent");
        assert_eq!(things[1].parent.as_ref().unwrap().as_str(), "t1_parent");
    }

    #[test]
    fn subreddit_validation_accepts_coordinates_not_paths() {
        assert!(validate_subreddit("Ask_Science").is_ok());
        assert!(validate_subreddit("r/science").is_err());
        assert!(validate_subreddit("science?sort=new").is_err());
    }

    #[test]
    fn subreddit_rules_remain_external_centralized_context() {
        let rules = parse_community_rules(&serde_json::json!({"rules": [
            {"short_name": "Cite primary sources", "description": "Link the paper."},
            {"short_name": "Be civil", "description": ""}
        ]}))
        .unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].title, "Cite primary sources");
        assert_eq!(rules[0].description, "Link the paper.");
    }
}
