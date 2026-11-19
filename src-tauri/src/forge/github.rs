//! GitHub (and GitHub Enterprise) over its REST and GraphQL APIs, signed in through Nod's GitHub App
//! (see `github_auth`). Acts as the signed-in user; reaches the repositories where the App is installed.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use super::github_auth::{Hosts, Session, SIGNIN_REQUIRED};
use super::http::{basic, enc};
use super::{commentable_lines, DiffLines, Forge, Pr, PrStatus, Remote};

pub struct Gh {
    pub session: Session,
}

/// The App's client ID for an account: its own `client_id` setting (GitHub Enterprise), else Nod's App.
pub fn client_id(acc: &crate::config::ForgeAccount) -> String {
    acc.settings
        .get("client_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(super::github_auth::DEFAULT_CLIENT_ID)
        .to_string()
}

impl Gh {
    pub fn new(acc: &crate::config::ForgeAccount) -> Gh {
        Gh {
            session: Session::new(&acc.id, Hosts::new(&acc.url), &client_id(acc)),
        }
    }

    fn hosts(&self) -> &Hosts {
        &self.session.hosts
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}/{}", self.hosts().api, path.trim_start_matches('/'))
        }
    }

    /// One request with the current token; a 401 re-reads the keychain once (the window may have signed in again).
    fn call(&self, method: &str, url: &str, body: Option<&Value>) -> Result<ureq::Response> {
        for attempt in 0..2 {
            let token = self.session.token()?;
            let req = ureq::request(method, url)
                .timeout(Duration::from_secs(120))
                .set("Accept", "application/vnd.github+json")
                .set("X-GitHub-Api-Version", "2022-11-28")
                .set("User-Agent", concat!("nod/", env!("CARGO_PKG_VERSION")))
                .set("Authorization", &format!("Bearer {token}"));
            let r = match body {
                Some(b) => req.send_json(b.clone()),
                None => req.call(),
            };
            match r {
                Ok(resp) => return Ok(resp),
                Err(ureq::Error::Status(401, _)) if attempt == 0 => self.session.invalidate(),
                Err(ureq::Error::Status(401, _)) => {
                    bail!("{SIGNIN_REQUIRED}: GitHub no longer accepts the sign-in; sign in again in Nod")
                }
                Err(ureq::Error::Status(code, resp)) => {
                    let text = resp.into_string().unwrap_or_default();
                    let hint = if matches!(code, 403 | 404) && url.contains("/repos/") {
                        " (if this repository's owner has not installed the Nod GitHub App, Nod cannot see it)"
                    } else {
                        ""
                    };
                    bail!(
                        "{method} {}: HTTP {code}: {}{hint}",
                        path_of(url),
                        crate::util::truncate(text.trim(), 300)
                    )
                }
                Err(e) => bail!("{method} {}: {e}", path_of(url)),
            }
        }
        unreachable!()
    }

    fn json(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let text = self.call(method, &self.url(path), body)?.into_string()?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).context("GitHub returned invalid JSON")
    }

    fn get(&self, path: &str) -> Result<Value> {
        self.json("GET", path, None)
    }

    /// Every page of a list endpoint, following `Link: <…>; rel="next"`. `key` picks the list out of an object
    /// response (`items` for search, `check_runs` for checks); `None` for endpoints that return an array.
    fn get_all(&self, path: &str, key: Option<&str>) -> Result<Vec<Value>> {
        let mut out = vec![];
        let mut next = Some(self.url(path));
        let mut pages = 0;
        while let Some(url) = next.take() {
            pages += 1;
            if pages > 50 {
                break; // 5000 items: far beyond anything a review needs; never loop forever on a bad Link
            }
            let resp = self.call("GET", &url, None)?;
            next = resp.header("Link").and_then(next_link);
            let v: Value = resp.into_json().context("GitHub returned invalid JSON")?;
            let list = match key {
                Some(k) => v[k].clone(),
                None => v,
            };
            out.extend(list.as_array().cloned().unwrap_or_default());
        }
        Ok(out)
    }

    /// Accounts and organizations where the signed-in user can use the App (empty = it sees no repository).
    pub fn installations(&self) -> Result<Vec<String>> {
        Ok(self
            .get_all("user/installations?per_page=100", Some("installations"))?
            .iter()
            .filter_map(|i| i["account"]["login"].as_str().map(str::to_string))
            .collect())
    }

    fn graphql(&self, query: &str, vars: Value) -> Result<Value> {
        let resp = self.call(
            "POST",
            &self.hosts().graphql,
            Some(&json!({"query": query, "variables": vars})),
        )?;
        let v: Value = resp.into_json().context("GitHub returned invalid JSON")?;
        if let Some(errs) = v["errors"].as_array().filter(|e| !e.is_empty()) {
            let msg: Vec<&str> = errs.iter().filter_map(|e| e["message"].as_str()).collect();
            bail!("GitHub GraphQL: {}", msg.join("; "));
        }
        Ok(v["data"].clone())
    }
}

/// The path of an API URL, for error messages.
fn path_of(url: &str) -> &str {
    url.split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|i| &rest[i..]))
        .unwrap_or(url)
}

/// The `rel="next"` target of a `Link` header.
fn next_link(h: &str) -> Option<String> {
    h.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        rel.contains("rel=\"next\"").then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
    })
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

const PR_STATUS_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      state isDraft reviewDecision headRefOid title url author { login }
      additions deletions changedFiles headRefName baseRefName merged
    }
  }
}";

impl Forge for Gh {
    fn current_user(&self) -> Result<String> {
        Ok(s(&self.get("user")?, "login"))
    }

    fn fetch_pr(&self, repo: &str, number: u64) -> Result<Pr> {
        let d = self.get(&format!("repos/{repo}/pulls/{number}"))?;
        let state = if d["merged"].as_bool() == Some(true) {
            "merged".into()
        } else {
            s(&d, "state")
        };
        Ok(Pr {
            repo: repo.into(),
            number,
            title: s(&d, "title"),
            author: d["user"]["login"].as_str().unwrap_or_default().into(),
            url: s(&d, "html_url"),
            head: d["head"]["sha"].as_str().unwrap_or_default().into(),
            base: d["base"]["ref"].as_str().unwrap_or_default().into(),
            draft: d["draft"].as_bool().unwrap_or(false),
            state,
            body: s(&d, "body"),
        })
    }

    fn requested_prs(&self, teams: bool) -> Result<Vec<(String, u64)>> {
        // `user-review-requested` = you personally; `review-requested` also matches teams you belong to.
        let who = if teams {
            "review-requested"
        } else {
            "user-review-requested"
        };
        let q = format!("is:pr is:open {who}:@me archived:false");
        // all pages: someone asked to review 60 PRs must not see the last ten ignored
        let items = self.get_all(
            &format!("search/issues?q={}&per_page=100", enc(&q)),
            Some("items"),
        )?;
        Ok(items
            .iter()
            .filter_map(|it| {
                let url = it["repository_url"].as_str()?;
                let parts: Vec<&str> = url.rsplitn(3, '/').collect();
                Some((
                    format!("{}/{}", parts.get(1)?, parts.first()?),
                    it["number"].as_u64()?,
                ))
            })
            .collect())
    }

    fn ci_state(&self, pr: &Pr) -> Result<String> {
        let runs = self.get_all(
            &format!(
                "repos/{}/commits/{}/check-runs?per_page=100",
                pr.repo, pr.head
            ),
            Some("check_runs"),
        )?;
        let status = self.get(&format!("repos/{}/commits/{}/status", pr.repo, pr.head))?;
        let mut states = vec![];
        for r in &runs {
            let concl = r["conclusion"].as_str().unwrap_or("");
            states.push(if r["status"].as_str() != Some("completed") {
                "pending"
            } else if ["success", "neutral", "skipped"].contains(&concl) {
                "success"
            } else {
                "failure"
            });
        }
        for st in status["statuses"].as_array().into_iter().flatten() {
            states.push(match st["state"].as_str() {
                Some("success") => "success",
                Some("pending") => "pending",
                _ => "failure",
            });
        }
        Ok(if states.is_empty() {
            "none"
        } else if states.contains(&"failure") {
            "failure"
        } else if states.contains(&"pending") {
            "pending"
        } else {
            "success"
        }
        .into())
    }

    fn pr_status(&self, repo: &str, number: u64) -> Result<PrStatus> {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| anyhow!("bad repository {repo:?}"))?;
        // "Could not resolve to a Repository" reads like a DNS failure to the network classifier: say what it means
        let not_found = || {
            anyhow!(
                "{repo}#{number} not found (is the Nod GitHub App installed for this repository?)"
            )
        };
        let data = self
            .graphql(
                PR_STATUS_QUERY,
                json!({"owner": owner, "name": name, "number": number}),
            )
            .map_err(|e| {
                if e.to_string().contains("Could not resolve to a") {
                    not_found()
                } else {
                    e
                }
            })?;
        let d = &data["repository"]["pullRequest"];
        if d.is_null() {
            return Err(not_found());
        }
        let state = if d["merged"].as_bool() == Some(true) {
            "merged".to_string()
        } else {
            s(d, "state").to_lowercase()
        };
        let mut st = PrStatus {
            state: state.clone(),
            draft: d["isDraft"].as_bool().unwrap_or(false),
            decision: s(d, "reviewDecision"),
            head: s(d, "headRefOid"),
            ci: String::new(),
            title: s(d, "title"),
            url: s(d, "url"),
            author: d["author"]["login"].as_str().unwrap_or_default().into(),
            additions: d["additions"].as_u64(),
            deletions: d["deletions"].as_u64(),
            changed_files: d["changedFiles"].as_u64(),
            head_ref: s(d, "headRefName"),
            base_ref: s(d, "baseRefName"),
        };
        if state == "open" {
            let pr = Pr {
                repo: repo.into(),
                number,
                title: st.title.clone(),
                author: st.author.clone(),
                url: st.url.clone(),
                head: st.head.clone(),
                base: st.base_ref.clone(),
                draft: st.draft,
                state,
                body: String::new(),
            };
            st.ci = self.ci_state(&pr)?;
        }
        Ok(st)
    }

    fn discussion(&self, pr: &Pr) -> Result<String> {
        let mut parts = vec![];
        let base = format!("repos/{}", pr.repo);
        let cut = |v: &Value, n: usize| crate::util::truncate(v["body"].as_str().unwrap_or(""), n);
        for c in self.get_all(
            &format!("{base}/issues/{}/comments?per_page=100", pr.number),
            None,
        )? {
            parts.push(format!(
                "[comment] {}: {}",
                c["user"]["login"].as_str().unwrap_or(""),
                cut(&c, 600)
            ));
        }
        for r in self.get_all(
            &format!("{base}/pulls/{}/reviews?per_page=100", pr.number),
            None,
        )? {
            if !r["body"].as_str().unwrap_or("").is_empty() {
                parts.push(format!(
                    "[review {}] {}: {}",
                    r["state"].as_str().unwrap_or(""),
                    r["user"]["login"].as_str().unwrap_or(""),
                    cut(&r, 600)
                ));
            }
        }
        for c in self.get_all(
            &format!("{base}/pulls/{}/comments?per_page=100", pr.number),
            None,
        )? {
            parts.push(format!(
                "[inline] {} {}:{}: {}",
                c["user"]["login"].as_str().unwrap_or(""),
                c["path"].as_str().unwrap_or(""),
                c["line"],
                cut(&c, 300)
            ));
        }
        Ok(crate::util::keep_last(
            &parts.join("\n"),
            super::DISCUSSION_CHARS,
        ))
    }

    fn diff_lines(&self, pr: &Pr) -> Result<DiffLines> {
        let mut out = DiffLines::new();
        for f in self.get_all(
            &format!("repos/{}/pulls/{}/files?per_page=100", pr.repo, pr.number),
            None,
        )? {
            out.insert(
                s(&f, "filename"),
                commentable_lines(f["patch"].as_str().unwrap_or("")),
            );
        }
        Ok(out)
    }

    fn post_review(&self, pr: &Pr, review: &Value) -> Result<(String, u64)> {
        let v = self.json(
            "POST",
            &format!("repos/{}/pulls/{}/reviews", pr.repo, pr.number),
            Some(review),
        )?;
        let id = v["id"]
            .as_u64()
            .ok_or_else(|| anyhow!("GitHub did not return a review id"))?;
        Ok((s(&v, "html_url"), id))
    }

    fn list_reviews(&self, pr: &Pr) -> Result<Vec<(u64, String)>> {
        // all pages: on a busy PR our own review can be past the first hundred
        Ok(self
            .get_all(
                &format!("repos/{}/pulls/{}/reviews?per_page=100", pr.repo, pr.number),
                None,
            )?
            .iter()
            .filter_map(|r| Some((r["id"].as_u64()?, s(r, "state"))))
            .collect())
    }

    fn dismiss_review(&self, pr: &Pr, id: u64, message: &str) -> Result<()> {
        self.json(
            "PUT",
            &format!(
                "repos/{}/pulls/{}/reviews/{id}/dismissals",
                pr.repo, pr.number
            ),
            Some(&json!({"message": message, "event": "DISMISS"})),
        )?;
        Ok(())
    }

    fn remote(&self, repo: &str) -> Result<Remote> {
        // git over HTTPS accepts a user access token as the password of `x-access-token`
        Ok(Remote::Git {
            url: format!("{}/{repo}.git", self.hosts().web),
            header: Some(basic("x-access-token", &self.session.token()?)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::github_auth::{save, Tokens};
    use super::super::http::fake;
    use super::*;
    use crate::util::now;

    fn gh(f: &fake::Fake, account: &str) -> Gh {
        crate::secrets::use_fake();
        save(
            account,
            &Tokens {
                access: "ghu_tok".into(),
                expires_at: now() + 3600.0,
                ..Default::default()
            },
        )
        .unwrap();
        Gh {
            session: Session::new(account, Hosts::at(&f.url), "cid"),
        }
    }

    fn pr() -> Pr {
        Pr {
            repo: "o/r".into(),
            number: 3,
            title: "t".into(),
            author: "alice".into(),
            url: "u".into(),
            head: "f00d".into(),
            base: "main".into(),
            draft: false,
            state: "open".into(),
            body: String::new(),
        }
    }

    #[test]
    fn next_link_header() {
        let h = r#"<https://api.github.com/x?page=2>; rel="next", <https://api.github.com/x?page=5>; rel="last""#;
        assert_eq!(
            next_link(h).as_deref(),
            Some("https://api.github.com/x?page=2")
        );
        assert_eq!(next_link(r#"<https://a/x?page=1>; rel="prev""#), None);
        assert_eq!(
            path_of("https://api.github.com/repos/o/r?x=1"),
            "/repos/o/r?x=1"
        );
    }

    #[test]
    fn requested_prs_follow_every_page() {
        let q = enc("is:pr is:open user-review-requested:@me archived:false");
        let first = format!("GET /search/issues?q={q}&per_page=100");
        let f = fake::serve_with(vec![
            (
                first.as_str(),
                200,
                json!({"items": [{"repository_url": "https://api.github.com/repos/o/r", "number": 3}]}).to_string(),
                vec![("Link", "<{base}/search/issues?page=2>; rel=\"next\"")],
            ),
            (
                "GET /search/issues?page=2",
                200,
                json!({"items": [{"repository_url": "https://api.github.com/repos/team/app", "number": 9}]}).to_string(),
                vec![],
            ),
        ]);
        let g = gh(&f, "t1");
        assert_eq!(
            g.requested_prs(false).unwrap(),
            vec![("o/r".into(), 3), ("team/app".into(), 9)]
        );
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|r| r.auth == "Bearer ghu_tok"));
    }

    #[test]
    fn pr_and_ci() {
        let f = fake::serve(vec![
            (
                "GET /repos/o/r/pulls/3",
                200,
                json!({"title": "Fix", "user": {"login": "alice"}, "html_url": "h", "head": {"sha": "f00d"}, "base": {"ref": "main"}, "draft": false, "state": "closed", "merged": true, "body": "b"}).to_string(),
            ),
            (
                "GET /repos/o/r/commits/f00d/check-runs",
                200,
                json!({"check_runs": [{"status": "completed", "conclusion": "success"}, {"status": "in_progress", "conclusion": null}]}).to_string(),
            ),
            ("GET /repos/o/r/commits/f00d/status", 200, json!({"statuses": [{"state": "success"}]}).to_string()),
            (
                "POST /graphql",
                200,
                json!({"data": {"repository": {"pullRequest": {"state": "OPEN", "isDraft": false, "reviewDecision": "APPROVED", "headRefOid": "f00d", "title": "Fix", "url": "h", "author": {"login": "alice"}, "additions": 4, "deletions": 1, "changedFiles": 2, "headRefName": "fix", "baseRefName": "main", "merged": false}}}}).to_string(),
            ),
        ]);
        let g = gh(&f, "t2");
        let p = g.fetch_pr("o/r", 3).unwrap();
        assert_eq!(
            (p.state.as_str(), p.head.as_str(), p.author.as_str()),
            ("merged", "f00d", "alice")
        );
        assert_eq!(g.ci_state(&pr()).unwrap(), "pending");
        let st = g.pr_status("o/r", 3).unwrap();
        assert_eq!(
            (st.state.as_str(), st.decision.as_str(), st.ci.as_str()),
            ("open", "APPROVED", "pending")
        );
        assert_eq!(
            (st.additions, st.changed_files, st.base_ref.as_str()),
            (Some(4), Some(2), "main")
        );
        let seen = f.seen.lock().unwrap();
        let gq = seen.iter().find(|r| r.path == "/graphql").unwrap();
        assert!(gq.body.contains("\"owner\":\"o\"") && gq.body.contains("\"number\":3"));
    }

    #[test]
    fn merged_status_and_missing_pr() {
        let f = fake::serve(vec![(
            "POST /graphql",
            200,
            json!({"data": {"repository": {"pullRequest": {"state": "MERGED", "merged": true, "reviewDecision": null}}}}).to_string(),
        )]);
        let g = gh(&f, "t3");
        let st = g.pr_status("o/r", 3).unwrap();
        assert_eq!((st.state.as_str(), st.ci.as_str()), ("merged", ""));
        let f2 = fake::serve(vec![("POST /graphql", 200, json!({"data": {"repository": null}, "errors": [{"message": "Could not resolve to a Repository"}]}).to_string())]);
        let e = gh(&f2, "t3b").pr_status("o/r", 3).unwrap_err().to_string();
        assert!(
            e.contains("not found") && !crate::supervisor::is_network(&e),
            "{e}"
        );
    }

    #[test]
    fn posts_lists_and_dismisses_reviews() {
        let f = fake::serve(vec![
            (
                "POST /repos/o/r/pulls/3/reviews",
                200,
                json!({"id": 41, "html_url": "https://github.com/o/r/pull/3#pullrequestreview-41"})
                    .to_string(),
            ),
            (
                "GET /repos/o/r/pulls/3/reviews",
                200,
                json!([{"id": 41, "state": "APPROVED"}, {"id": 7, "state": "COMMENTED"}])
                    .to_string(),
            ),
            (
                "PUT /repos/o/r/pulls/3/reviews/41/dismissals",
                200,
                "{}".into(),
            ),
            (
                "GET /repos/o/r/pulls/3/files",
                200,
                json!([{"filename": "a.rs", "patch": "@@ -1 +1,2 @@\n x\n+y"}]).to_string(),
            ),
        ]);
        let g = gh(&f, "t4");
        let (url, id) = g
            .post_review(&pr(), &json!({"event": "APPROVE", "body": "ok"}))
            .unwrap();
        assert_eq!(
            (url.as_str(), id),
            ("https://github.com/o/r/pull/3#pullrequestreview-41", 41)
        );
        assert_eq!(
            g.list_reviews(&pr()).unwrap(),
            vec![(41, "APPROVED".into()), (7, "COMMENTED".into())]
        );
        g.dismiss_review(&pr(), 41, "new commits").unwrap();
        assert_eq!(g.diff_lines(&pr()).unwrap()["a.rs"].len(), 2);
        let seen = f.seen.lock().unwrap();
        assert!(seen[0].body.contains("\"event\":\"APPROVE\""));
        let d = seen.iter().find(|r| r.method == "PUT").unwrap();
        assert!(d.body.contains("DISMISS") && d.body.contains("new commits"));
    }

    #[test]
    fn errors_name_the_cause_and_never_the_token() {
        let f = fake::serve(vec![
            (
                "GET /repos/o/r/pulls/3",
                404,
                json!({"message": "Not Found"}).to_string(),
            ),
            (
                "GET /user",
                401,
                json!({"message": "Bad credentials"}).to_string(),
            ),
        ]);
        let g = gh(&f, "t5");
        let e = g.fetch_pr("o/r", 3).unwrap_err().to_string();
        assert!(
            e.contains("HTTP 404") && e.contains("Nod GitHub App"),
            "{e}"
        );
        let e = g.current_user().unwrap_err().to_string();
        assert!(e.contains(SIGNIN_REQUIRED), "{e}");
        assert!(!e.contains("ghu_tok"));
        // the 401 was retried once with a token re-read from the keychain
        assert_eq!(
            f.seen
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.path == "/user")
                .count(),
            2
        );
    }

    #[test]
    fn installations() {
        let f = fake::serve(vec![(
            "GET /user/installations",
            200,
            json!({"total_count": 2, "installations": [{"account": {"login": "me"}}, {"account": {"login": "ReverseGame"}}]}).to_string(),
        )]);
        assert_eq!(
            gh(&f, "t7").installations().unwrap(),
            vec!["me", "ReverseGame"]
        );
    }

    #[test]
    fn clones_with_a_header_not_a_url_token() {
        let f = fake::serve(vec![]);
        let g = gh(&f, "t6");
        match g.remote("o/r").unwrap() {
            Remote::Git { url, header } => {
                assert_eq!(url, format!("{}/o/r.git", f.url));
                assert!(!url.contains("ghu_tok"));
                assert!(header.unwrap().starts_with("Authorization: Basic "));
            }
        }
    }

    #[test]
    fn client_id_setting_overrides_the_default() {
        let mut acc = crate::config::ForgeAccount {
            id: "ghe".into(),
            kind: "github".into(),
            ..Default::default()
        };
        assert_eq!(
            client_id(&acc),
            super::super::github_auth::DEFAULT_CLIENT_ID
        );
        acc.settings
            .insert("client_id".into(), toml::Value::String(" Iv1.corp ".into()));
        assert_eq!(client_id(&acc), "Iv1.corp");
    }
}
