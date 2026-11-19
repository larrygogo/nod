//! Code hosts ("forges"): GitHub, Gitee, Gitea/Forgejo and external plugins, behind one trait so the engine does
//! not care where a pull request lives (and can be tested without the network).

pub mod gitea;
pub mod gitee;
pub mod github;
pub mod github_auth;
pub mod http;

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Pr {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub url: String,
    pub head: String,
    pub base: String,
    pub draft: bool,
    /// open | closed | merged
    pub state: String,
    pub body: String,
}

impl Pr {
    pub fn key(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }
}

/// What a notification shows about the PR right now.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PrStatus {
    pub state: String,
    pub draft: bool,
    pub decision: String,
    pub head: String,
    pub ci: String,
    pub title: String,
    pub url: String,
    pub author: String,
    #[serde(default)]
    pub additions: Option<u64>,
    #[serde(default)]
    pub deletions: Option<u64>,
    #[serde(default)]
    pub changed_files: Option<u64>,
    #[serde(default)]
    pub head_ref: String,
    #[serde(default)]
    pub base_ref: String,
}

pub type DiffLines = HashMap<String, HashSet<u64>>;

/// How much of a PR's discussion is given to the reviewer (the newest part).
pub const DISCUSSION_CHARS: usize = 12000;

pub trait Forge: Send + Sync {
    fn current_user(&self) -> Result<String>;
    fn fetch_pr(&self, repo: &str, number: u64) -> Result<Pr>;
    /// Open PRs where you are a requested reviewer; `teams` also includes requests to teams you belong to.
    fn requested_prs(&self, teams: bool) -> Result<Vec<(String, u64)>>;
    /// success | failure | pending | none
    fn ci_state(&self, pr: &Pr) -> Result<String>;
    fn pr_status(&self, repo: &str, number: u64) -> Result<PrStatus>;
    fn discussion(&self, pr: &Pr) -> Result<String>;
    fn diff_lines(&self, pr: &Pr) -> Result<DiffLines>;
    /// Returns (html_url, review id).
    fn post_review(&self, pr: &Pr, review: &Value) -> Result<(String, u64)>;
    /// (id, state) of every review on the PR.
    fn list_reviews(&self, pr: &Pr) -> Result<Vec<(u64, String)>>;
    fn dismiss_review(&self, pr: &Pr, id: u64, message: &str) -> Result<()>;
    /// Where to fetch the repository from, and how to authenticate.
    fn remote(&self, repo: &str) -> Result<Remote>;
    /// The ref that holds the head of pull request `number` (fetched into the local mirror).
    fn head_ref(&self, number: u64) -> String {
        format!("refs/pull/{number}/head")
    }
}

/// How to get a repository's code.
#[derive(Debug, Clone, PartialEq)]
pub enum Remote {
    /// Plain git over HTTPS; `header` (e.g. `Authorization: Bearer …`) is passed to git through the environment, never
    /// on the command line or in the repository's config.
    Git { url: String, header: Option<String> },
}

/// `owner/repo#12` (any number of path segments) as stored in state files.
pub fn split_key(key: &str) -> Result<(String, u64)> {
    let (repo, n) = key
        .rsplit_once('#')
        .ok_or_else(|| anyhow!("bad key {key:?}"))?;
    let n: u64 = n.parse().map_err(|_| anyhow!("bad PR number in {key:?}"))?;
    if repo.split('/').count() < 2 || repo.split('/').any(str::is_empty) {
        return Err(anyhow!("bad key {key:?}"));
    }
    Ok((repo.to_string(), n))
}

/// Pasted text cut down to the PR reference in it: surrounding spaces, quotes, brackets and punctuation go; a link
/// inside a sentence ("please look at https://… thanks") is picked out; `github.com/o/r/pull/1` gets its scheme.
pub fn clean_ref(text: &str) -> String {
    let trim = |s: &str| {
        s.trim_matches(|c: char| {
            c.is_whitespace()
                || "\"'`<>()[]{}“”‘’「」『』（）【】《》,，.。;；:：!！?？、".contains(c)
        })
        .to_string()
    };
    let t = text.trim();
    // ASCII lowercasing keeps byte offsets, so the index found in `low` is valid in `t`
    let low = t.to_ascii_lowercase();
    if let Some(i) = low.find("https://").or_else(|| low.find("http://")) {
        let url: String = t[i..]
            .chars()
            .take_while(|c| {
                !c.is_whitespace() && !"\"'`<>“”‘’「」（）【】《》，。；！？、".contains(*c)
            })
            .collect();
        return trim(&url);
    }
    let t = trim(t);
    // a link without its scheme: host/owner/repo/pull/12
    let first = t.split('/').next().unwrap_or("");
    if first.contains('.') && t.split('/').count() >= 5 {
        return format!("https://{t}");
    }
    // owner/repo#12 with stray spaces ("owner/repo #12")
    t.split_whitespace().collect()
}

/// A pull request link on any host: `(host, repo path, number)`. GitHub `…/owner/repo/pull/12`, Gitee and Gitea
/// `…/owner/repo/pulls/12`; anything after the number (`/files`, `/changes`, `/commits/<sha>`), a query or a
/// fragment is ignored.
pub fn parse_url(s: &str) -> Option<(String, String, u64)> {
    let s = clean_ref(s);
    let rest = s
        .split_once("://")
        .map(|(scheme, r)| (scheme.to_ascii_lowercase(), r));
    let rest = match rest {
        Some((sc, r)) if sc == "https" || sc == "http" => r,
        _ => return None,
    };
    let rest = rest.split(['?', '#']).next()?.trim_end_matches('/');
    let (host, path) = rest.split_once('/')?;
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    // the last "pull"/"pulls" segment followed by a number; owner/repo (or a GitLab-style group path) before it
    let at = (0..parts.len().saturating_sub(1))
        .rev()
        .find(|&i| matches!(parts[i], "pull" | "pulls") && parts[i + 1].parse::<u64>().is_ok())?;
    let n: u64 = parts[at + 1].parse().ok()?;
    let repo = parts[..at].join("/");
    (repo.split('/').count() >= 2).then(|| (host.to_lowercase(), repo, n))
}
#[cfg(test)]
pub fn parse_key(s: &str) -> Result<(String, u64)> {
    let s = s.trim().trim_end_matches('/');
    let (repo, num) = if let Some(rest) = s.strip_prefix("https://github.com/") {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() == 4 && parts[2] == "pull" {
            (format!("{}/{}", parts[0], parts[1]), parts[3])
        } else {
            return Err(anyhow!(
                "expected owner/repo#123 or a pull request URL, got {s:?}"
            ));
        }
    } else if let Some((r, n)) = s.split_once('#') {
        (r.to_string(), n)
    } else {
        return Err(anyhow!(
            "expected owner/repo#123 or a pull request URL, got {s:?}"
        ));
    };
    let ok = repo.split('/').count() == 2
        && repo
            .split('/')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c)));
    let n: u64 = num.parse().map_err(|_| anyhow!("bad PR number in {s:?}"))?;
    if !ok {
        return Err(anyhow!(
            "expected owner/repo#123 or a pull request URL, got {s:?}"
        ));
    }
    Ok((repo, n))
}

/// Right-side line numbers inside diff hunks (added or context lines) — the only lines GitHub accepts comments on.
pub fn commentable_lines(patch: &str) -> HashSet<u64> {
    let mut lines = HashSet::new();
    let mut cur: Option<u64> = None;
    for raw in patch.lines() {
        if let Some(rest) = raw.strip_prefix("@@ -") {
            // @@ -a,b +c,d @@
            cur = rest
                .split_once(" +")
                .and_then(|(_, r)| r.split([',', ' ']).next())
                .and_then(|n| n.parse().ok());
            continue;
        }
        let Some(c) = cur else { continue };
        if raw.starts_with('\\') || raw.starts_with('-') {
            continue;
        }
        lines.insert(c);
        cur = Some(c + 1);
    }
    lines
}

/// Which account a PR typed by hand belongs to: a link picks the account on that host; `repo#123` means the first
/// enabled account. Returns (account, repo, number).
pub fn resolve(
    cfg: &crate::config::Config,
    text: &str,
) -> Result<(crate::config::ForgeAccount, String, u64)> {
    let accounts: Vec<_> = cfg.accounts().into_iter().filter(|a| a.enabled).collect();
    let host_of = |a: &crate::config::ForgeAccount| {
        let u = a.url.trim();
        if u.is_empty() {
            match a.kind.as_str() {
                "github" => "github.com".to_string(),
                "gitee" => "gitee.com".to_string(),
                _ => String::new(),
            }
        } else {
            u.trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_end_matches('/')
                .split('/')
                .next()
                .unwrap_or("")
                .to_string()
        }
    };
    if let Some((host, repo, n)) = parse_url(text) {
        let acc = accounts
            .iter()
            .find(|a| host_of(a).eq_ignore_ascii_case(&host))
            .ok_or_else(|| anyhow!("no enabled code host account for {host}"))?;
        return Ok((acc.clone(), repo, n));
    }
    let key = clean_ref(text);
    let (repo, n) = split_key(&key).map_err(|_| {
        anyhow!(
            "expected owner/repo#123 or a pull request link, got {:?}",
            text.trim()
        )
    })?;
    let acc = accounts
        .first()
        .ok_or_else(|| anyhow!("no enabled code host account"))?;
    Ok((acc.clone(), repo, n))
}

/// The accounts configured in `[[forges]]`, as forge implementations.
pub fn make(
    acc: &crate::config::ForgeAccount,
    cfg: &crate::config::Config,
) -> anyhow::Result<std::sync::Arc<dyn Forge>> {
    use std::sync::Arc;
    Ok(match acc.kind.as_str() {
        "github" => Arc::new(github::Gh::new(acc)),
        "gitee" => Arc::new(
            gitee::Gitee::new(&acc.url, &crate::secrets::forge_token(acc))
                .with_scope(&cfg.scope.repos, &cfg.scope.exclude_repos),
        ),
        "gitea" => Arc::new(gitea::Gitea::new(
            &acc.url,
            &crate::secrets::forge_token(acc),
        )),
        k if k.starts_with("ext:") => Arc::new(crate::plugins::ExternalForge::new(acc)?),
        k => return Err(anyhow!("unknown code host kind {k:?}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunks() {
        assert_eq!(
            commentable_lines("@@ -1,2 +1,3 @@\n a\n-b\n+c\n+d\n"),
            HashSet::from([1, 2, 3])
        );
        assert_eq!(
            commentable_lines("@@ -10 +20 @@ fn x\n+a\n"),
            HashSet::from([20])
        );
    }

    #[test]
    fn urls_and_keys() {
        assert_eq!(
            parse_url("https://github.com/acme/app/pull/12"),
            Some(("github.com".into(), "acme/app".into(), 12))
        );
        assert_eq!(
            parse_url("https://gitee.com/oschina/app/pulls/7"),
            Some(("gitee.com".into(), "oschina/app".into(), 7))
        );
        assert_eq!(
            parse_url("https://git.example.com/o/r/pulls/3/"),
            Some(("git.example.com".into(), "o/r".into(), 3))
        );
        assert_eq!(parse_url("https://x.com/o/r/issues/3"), None);
        // pasted the way people actually paste them
        let want = Some((
            "github.com".to_string(),
            "ReverseGame/reqgenie".to_string(),
            253,
        ));
        for s in [
            "  https://github.com/ReverseGame/reqgenie/pull/253/changes  ",
            "https://github.com/ReverseGame/reqgenie/pull/253/files#diff-abc",
            "https://github.com/ReverseGame/reqgenie/pull/253/commits/0a1b2c",
            "https://github.com/ReverseGame/reqgenie/pull/253?notification_referrer_id=x",
            "https://github.com/ReverseGame/reqgenie/pull/253#issuecomment-1",
            "<https://github.com/ReverseGame/reqgenie/pull/253>",
            "帮忙看下这个 https://github.com/ReverseGame/reqgenie/pull/253 ，谢谢",
            "github.com/ReverseGame/reqgenie/pull/253",
            "HTTPS://GitHub.com/ReverseGame/reqgenie/pull/253",
        ] {
            assert_eq!(parse_url(s), want, "{s}");
        }
        assert_eq!(parse_url("https://github.com/o/r/pull/new"), None);
        assert_eq!(
            clean_ref("  ReverseGame/reqgenie #253 "),
            "ReverseGame/reqgenie#253"
        );
        assert_eq!(clean_ref("`o/r#7`"), "o/r#7");
        assert_eq!(split_key("a/b/c#1").unwrap(), ("a/b/c".into(), 1));
        assert!(split_key("a#1").is_err());
    }

    #[test]
    fn keys() {
        assert_eq!(parse_key("acme/app#12").unwrap(), ("acme/app".into(), 12));
        assert_eq!(
            parse_key("https://github.com/acme/app/pull/12").unwrap(),
            ("acme/app".into(), 12)
        );
        assert!(parse_key("app 12").is_err());
        assert!(parse_key("a/b/c#1").is_err());
    }
}
