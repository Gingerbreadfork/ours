//! The admin token: generation, checking and the middleware that
//! enforces it.

use crate::routes::{ApiError, AppState};
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, Uri};
use axum::middleware::Next;
use axum::response::{AppendHeaders, IntoResponse, Redirect, Response};

pub(crate) const COOKIE_NAME: &str = "ours_admin";
const COOKIE_MAX_AGE_SECONDS: u32 = 30 * 24 * 60 * 60;
const TOKEN_BYTES: usize = 32;
const MAX_TOKEN_LEN: usize = 128;
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// 32 random bytes as unpadded base64url (43 characters).
pub(crate) fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::fill(&mut bytes);
    base64url(&bytes)
}

/// A token is safe to put in a URL and a cookie without any encoding.
pub(crate) fn is_valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_TOKEN_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
}

fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

/// Compares without stopping at the first differing byte. An empty
/// expected token never matches.
fn tokens_match(given: &str, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let (given, expected) = (given.as_bytes(), expected.as_bytes());
    let mut diff = u8::from(given.len() != expected.len());
    for (i, b) in expected.iter().enumerate() {
        diff |= b ^ given.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(token.trim())
}

fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(COOKIE_NAME)?.strip_prefix('='))
}

fn query_token(uri: &Uri) -> Option<&str> {
    uri.query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
}

/// The request's own path and query with the `token` parameter removed.
fn without_token(uri: &Uri) -> String {
    let rest: Vec<&str> = uri
        .query()
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty() && !pair.starts_with("token="))
        .collect();
    if rest.is_empty() {
        uri.path().to_owned()
    } else {
        format!("{}?{}", uri.path(), rest.join("&"))
    }
}

pub(crate) async fn require_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    if bearer(headers).is_some_and(|t| tokens_match(t, &state.token)) {
        return next.run(request).await;
    }
    if query_token(request.uri()).is_some_and(|t| tokens_match(t, &state.token)) {
        return sign_in(&state.token, &without_token(request.uri()));
    }
    if cookie(headers).is_some_and(|t| tokens_match(t, &state.token)) {
        return next.run(request).await;
    }
    ApiError::Unauthorized.into_response()
}

/// Sets the cookie and sends the browser to the same page without the
/// token in its address.
fn sign_in(token: &str, target: &str) -> Response {
    let cookie = format!(
        "{COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={COOKIE_MAX_AGE_SECONDS}"
    );
    match HeaderValue::from_str(&cookie) {
        Ok(value) => (AppendHeaders([(SET_COOKIE, value)]), Redirect::to(target)).into_response(),
        Err(_) => Redirect::to(target).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_matches_known_vectors() {
        assert_eq!(base64url(b"Man"), "TWFu");
        assert_eq!(base64url(b"Ma"), "TWE");
        assert_eq!(base64url(b"M"), "TQ");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn generated_tokens_are_long_unique_and_valid() {
        let (a, b) = (generate_token(), generate_token());
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert!(is_valid_token(&a));
    }

    #[test]
    fn token_validity() {
        assert!(is_valid_token("abc-DEF_123.~"));
        assert!(!is_valid_token(""));
        assert!(!is_valid_token("has space"));
        assert!(!is_valid_token("semi;colon"));
        assert!(!is_valid_token(&"a".repeat(129)));
    }

    #[test]
    fn matching_rejects_wrong_short_long_and_empty() {
        assert!(tokens_match("secret", "secret"));
        assert!(!tokens_match("secrex", "secret"));
        assert!(!tokens_match("secre", "secret"));
        assert!(!tokens_match("secrets", "secret"));
        assert!(!tokens_match("", ""));
    }

    #[test]
    fn token_is_stripped_from_the_redirect_target() -> anyhow::Result<()> {
        assert_eq!(without_token(&"/?token=t".parse()?), "/");
        assert_eq!(
            without_token(&"/api/groups/g/events?token=t&limit=5".parse()?),
            "/api/groups/g/events?limit=5"
        );
        Ok(())
    }
}
