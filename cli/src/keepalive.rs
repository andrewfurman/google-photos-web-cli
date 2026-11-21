//! `auth keepalive`: rotate the short-lived Google sign-in cookies so a session used only by this
//! CLI stays valid. See docs/SESSION_REFRESH.md for sources and caveats.
use crate::{auth::Session, protocol::SessionExpired};
use anyhow::{anyhow, bail, Result};
use reqwest::{blocking::Client, header, redirect::Policy, StatusCode};
use reqwest_cookie_store::CookieStore;
use std::{collections::BTreeMap, time::Duration};

/// The only request this CLI sends directly to accounts.google.com.
pub const ROTATE_COOKIES_URL: &str = "https://accounts.google.com/RotateCookies";
/// Fixed body Google's own sign-in pages send (as used by gemini-webapi's rotate_1psidts).
pub const ROTATE_COOKIES_BODY: &str = r#"[000,"-0000000000000000000"]"#;
/// The short-lived cookie that must be rotated regularly.
pub const PSIDTS: &str = "__Secure-1PSIDTS";

/// What changed in the cookie store. Holds cookie names only, never values.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Cookies whose value changed or that were newly added.
    pub changed: Vec<String>,
    /// Cookies Google asked us to delete (expired Set-Cookie).
    pub removed: Vec<String>,
    /// Set-Cookie headers rejected as malformed or for a domain outside the request host.
    pub rejected: usize,
}
impl MergeReport {
    pub fn rotated_psidts(&self) -> bool {
        self.changed.iter().any(|name| name == PSIDTS)
    }
}

fn snapshot(store: &CookieStore) -> BTreeMap<(String, String, String), String> {
    store
        .iter_unexpired()
        .map(|c| {
            (
                (
                    c.domain().unwrap_or_default().to_owned(),
                    c.path().unwrap_or_default().to_owned(),
                    c.name().to_owned(),
                ),
                c.value().to_owned(),
            )
        })
        .collect()
}

/// Merge `Set-Cookie` header values received from `url` into `store` following RFC 6265
/// (domain/path scoping, replacement, and deletion via past expiry). Values are never logged.
pub fn merge_set_cookies<'a>(
    store: &mut CookieStore,
    url: &url::Url,
    set_cookies: impl IntoIterator<Item = &'a str>,
) -> MergeReport {
    let before = snapshot(store);
    let mut rejected = 0;
    for raw in set_cookies {
        if store.parse(raw, url).is_err() {
            rejected += 1;
        }
    }
    let after = snapshot(store);
    let mut changed: Vec<String> = after
        .iter()
        .filter(|(key, value)| before.get(*key) != Some(*value))
        .map(|((_, _, name), _)| name.clone())
        .collect();
    let mut removed: Vec<String> = before
        .keys()
        .filter(|key| !after.contains_key(*key))
        .map(|(_, _, name)| name.clone())
        .collect();
    changed.sort();
    changed.dedup();
    removed.sort();
    removed.dedup();
    MergeReport {
        changed,
        removed,
        rejected,
    }
}

fn rotate_url() -> url::Url {
    url::Url::parse(ROTATE_COOKIES_URL).expect("static URL")
}

/// POST RotateCookies with the session's cookies and return the session holding the merged
/// cookies. The caller saves it. Errors never include cookie values or response bodies.
pub fn rotate(session: &Session) -> Result<(Session, MergeReport)> {
    if session.is_environment_only() {
        bail!("auth keepalive needs a session file: a session supplied in GPHOTOS_SESSION cannot be updated, so rotating it would leave that copy stale. Use --session-file PATH or the default session file");
    }
    let mut store = session.store()?;
    if store.iter_unexpired().next().is_none() {
        return Err(SessionExpired.into());
    }
    let url = rotate_url();
    let cookie_header = store
        .get_request_values(&url)
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ");
    if !cookie_header.contains(&format!("{PSIDTS}=")) && !cookie_header.contains("SID=") {
        bail!("The saved session has no Google sign-in cookies (SID / __Secure-1PSIDTS). Import a fresh session");
    }
    // No cookie provider and no redirects: exactly one request to accounts.google.com, whose
    // Set-Cookie headers are merged explicitly below.
    let http = Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(20))
        .redirect(Policy::none())
        .user_agent(concat!("google-photos-cli/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut cookie_value = header::HeaderValue::from_str(&cookie_header)
        .map_err(|_| anyhow!("The saved session contains an unsupported cookie encoding"))?;
    cookie_value.set_sensitive(true);
    let response = http
        .post(url.clone())
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, "https://accounts.google.com")
        .header(header::COOKIE, cookie_value)
        .body(ROTATE_COOKIES_BODY)
        .send()
        .map_err(|e| {
            if e.is_timeout() {
                anyhow!("Google RotateCookies request timed out")
            } else {
                anyhow!("Google RotateCookies network request failed")
            }
        })?;
    let status = response.status();
    let set_cookies: Vec<String> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_owned))
        .collect();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(anyhow!(SessionExpired).context(format!(
            "Google rejected the cookie rotation (HTTP {status}); the session was signed out or revoked"
        )));
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        bail!("Google rate-limited the cookie rotation (HTTP 429). Run keepalive less often (every ~10 minutes)");
    }
    if !status.is_success() {
        bail!("Google cookie rotation returned HTTP {status}; the session was not changed");
    }
    let report = merge_set_cookies(&mut store, &url, set_cookies.iter().map(String::as_str));
    Ok((session.with_store(&store)?, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> url::Url {
        rotate_url()
    }
    fn store_with(cookies: &[&str]) -> CookieStore {
        let mut store = CookieStore::default();
        let origin = url::Url::parse("https://google.com/").unwrap();
        for raw in cookies {
            store.parse(raw, &origin).unwrap();
        }
        store
    }
    fn value(store: &CookieStore, name: &str) -> Option<String> {
        store
            .iter_unexpired()
            .find(|c| c.name() == name)
            .map(|c| c.value().to_owned())
    }

    #[test]
    fn rotation_replaces_psidts_and_keeps_other_cookies() {
        let mut store = store_with(&[
            "SID=keep; Domain=.google.com; Path=/; Secure",
            "__Secure-1PSIDTS=old; Domain=.google.com; Path=/; Secure; HttpOnly",
            "SIDCC=oldcc; Domain=.google.com; Path=/",
        ]);
        let report = merge_set_cookies(
            &mut store,
            &url(),
            [
                "__Secure-1PSIDTS=new; Domain=.google.com; Path=/; Secure; HttpOnly; Expires=Fri, 01 Jan 2100 00:00:00 GMT",
                "SIDCC=newcc; Domain=.google.com; Path=/; Expires=Fri, 01 Jan 2100 00:00:00 GMT",
            ],
        );
        assert_eq!(report.changed, vec!["SIDCC", PSIDTS]);
        assert!(report.rotated_psidts());
        assert!(report.removed.is_empty());
        assert_eq!(report.rejected, 0);
        assert_eq!(value(&store, PSIDTS).as_deref(), Some("new"));
        assert_eq!(value(&store, "SID").as_deref(), Some("keep"));
        assert_eq!(store.iter_unexpired().count(), 3, "no duplicate cookies");
        // The rotated cookie is sent to Photos afterwards.
        let photos = url::Url::parse("https://photos.google.com/").unwrap();
        assert!(store
            .get_request_values(&photos)
            .any(|(n, v)| n == PSIDTS && v == "new"));
    }

    #[test]
    fn same_value_is_not_reported_as_rotated() {
        let mut store = store_with(&["__Secure-1PSIDTS=same; Domain=.google.com; Path=/; Secure"]);
        let report = merge_set_cookies(
            &mut store,
            &url(),
            ["__Secure-1PSIDTS=same; Domain=.google.com; Path=/; Secure"],
        );
        assert!(report.changed.is_empty());
        assert!(!report.rotated_psidts());
    }

    #[test]
    fn expired_set_cookie_deletes_and_foreign_domains_are_rejected() {
        let mut store = store_with(&[
            "SID=keep; Domain=.google.com; Path=/",
            "OLD=gone; Domain=.google.com; Path=/",
        ]);
        let report = merge_set_cookies(
            &mut store,
            &url(),
            [
                "OLD=; Domain=.google.com; Path=/; Expires=Thu, 01 Jan 1970 00:00:01 GMT",
                "EVIL=x; Domain=.evil.test; Path=/",
                "not a cookie",
            ],
        );
        assert_eq!(report.removed, vec!["OLD"]);
        assert!(report.changed.is_empty());
        assert_eq!(report.rejected, 2);
        assert!(value(&store, "OLD").is_none());
        assert!(store.iter_any().all(|c| c.name() != "EVIL"));
    }

    #[test]
    fn host_only_accounts_cookies_stay_on_accounts_host() {
        let mut store = store_with(&["SID=keep; Domain=.google.com; Path=/"]);
        let report = merge_set_cookies(&mut store, &url(), ["__Host-GAPS=a; Path=/; Secure"]);
        assert_eq!(report.changed, vec!["__Host-GAPS"]);
        let photos = url::Url::parse("https://photos.google.com/").unwrap();
        assert!(store
            .get_request_values(&photos)
            .all(|(n, _)| n != "__Host-GAPS"));
        assert!(store
            .get_request_values(&url())
            .any(|(n, _)| n == "__Host-GAPS"));
    }

    #[test]
    fn merged_store_roundtrips_through_the_session_file_format() {
        let store = store_with(&["__Secure-1PSIDTS=old; Domain=.google.com; Path=/; Secure"]);
        let session = Session::new(&store, 2, None).unwrap();
        let mut merged = session.store().unwrap();
        merge_set_cookies(
            &mut merged,
            &url(),
            ["__Secure-1PSIDTS=new; Domain=.google.com; Path=/; Secure; Expires=Fri, 01 Jan 2100 00:00:00 GMT"],
        );
        let updated = session.with_store(&merged).unwrap();
        assert_eq!(updated.account_index, 2);
        let reloaded = Session::from_token(&updated.token().unwrap()).unwrap();
        assert_eq!(
            value(&reloaded.store().unwrap(), PSIDTS).as_deref(),
            Some("new")
        );
    }

    #[test]
    fn rotation_targets_only_accounts_google_com_over_https() {
        let url = url();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("accounts.google.com"));
        assert_eq!(url.path(), "/RotateCookies");
        assert_eq!(ROTATE_COOKIES_BODY, "[000,\"-0000000000000000000\"]");
    }

    #[test]
    fn environment_sessions_are_refused_before_any_request() {
        let store = store_with(&["SID=x; Domain=.google.com; Path=/"]);
        let token = Session::new(&store, 0, None).unwrap().token().unwrap();
        // from_token sessions are file-less but not environment-only; emulate auth::load's env path.
        std::env::set_var("GPHOTOS_SESSION", &token);
        let session = crate::auth::load().unwrap();
        std::env::remove_var("GPHOTOS_SESSION");
        let err = rotate(&session).err().unwrap().to_string();
        assert!(err.contains("GPHOTOS_SESSION"), "{err}");
        assert!(!err.contains(&token));
    }
}
