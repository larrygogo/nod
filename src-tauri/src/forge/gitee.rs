//! Gitee (gitee.com) over the OpenAPI v5 with a personal access token (私人令牌), sent as `access_token`.
//!
//! Differences from the other hosts, all following the v5 spec (https://gitee.com/api/v5/doc_json):
//! - There is no cross-repository search for review requests. Review requests are PRs whose `assignees` (审查人员)
//!   include you, so Nod lists the repositories you can access (cached for ten minutes, filtered by [scope])
//!   and asks each for open PRs with `assignee=<you>`.
//! - "Approve" is `POST …/pulls/{n}/review` (审查通过). Gitee has no way for a reviewer to withdraw it (only an
//!   administrator can reset all review states), so a later failing commit cannot dismiss Nod's approval; the
//!   notification then says the old approval still stands.
//! - Inline comments take `path` + `position`; when Gitee refuses a position, the finding is posted as a regular
//!   comment that names the file and line instead of being lost.
//! - CI state comes from check runs.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use super::http::{basic, Api};
use super::{commentable_lines, DiffLines, Forge, Pr, PrStatus, Remote};

/// Stands for "our 审查通过 on this PR" in `list_reviews` / `dismiss_review`.
pub const APPROVAL_ID: u64 = 1;
const REPO_CACHE: Duration = Duration::from_secs(600);
const MAX_REPOS: usize = 500;
/// Gitee has no "review requested from me" search, so finding requests costs one call per repository. Scanning on
/// every poll (30 s) is ~120 calls per repository an hour and runs into the API's rate limit; a full scan at most
/// every five minutes keeps it at 12. PRs already known are still refreshed on every poll, through other calls.
const SCAN_EVERY: Duration = Duration::from_secs(300);

/// Review requests found by the last full scan: (repository, PR number).
type Found = Vec<(String, u64)>;

pub struct Gitee {
    web: String,
    token: String,
    api: Api,
    me: OnceLock<String>,
    repos: Mutex<Option<(Instant, Vec<String>)>>,
    /// The last full scan for review requests and when it ran.
    scan: Mutex<Option<(Instant, Found)>>,
    /// The [scope] patterns (include, exclude): repositories outside are not even scanned.
    scope: (Vec<String>, Vec<String>),
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

impl Gitee {
    pub fn new(url: &str, token: &str) -> Gitee {
        let web = if url.trim().is_empty() {
            "https://gitee.com".to_string()
        } else {
            url.trim().trim_end_matches('/').to_string()
        };
        Gitee {
            api: Api::with_query(&format!("{web}/api/v5"), "access_token", token),
            web,
            token: token.to_string(),
            me: OnceLock::new(),
            repos: Mutex::new(None),
            scan: Mutex::new(None),
            scope: (vec![], vec![]),
        }
    }

    fn me(&self) -> Result<String> {
        if let Some(m) = self.me.get() {
            return Ok(m.clone());
        }
        let m = s(&self.api.get("/user")?, "login");
        if m.is_empty() {
            bail!("Gitee returned no login for this token");
        }
        let _ = self.me.set(m.clone());
        Ok(m)
    }

    fn pull(repo: &str, n: u64) -> String {
        format!("/repos/{repo}/pulls/{n}")
    }

    /// `full_name` of every repository the token can see (owned, collaborator, organisation member).
    fn repositories(&self) -> Result<Vec<String>> {
        if let Some((at, list)) = self.repos.lock().unwrap().as_ref() {
            if at.elapsed() < REPO_CACHE {
                return Ok(list.clone());
            }
        }
        let mut out = vec![];
        for page in 1..=(MAX_REPOS / 100) {
            let v = self.api.get(&format!(
                "/user/repos?affiliation=owner,collaborator,organization_member&per_page=100&page={page}"
            ))?;
            let items = v.as_array().cloned().unwrap_or_default();
            let n = items.len();
            out.extend(
                items
                    .iter()
                    .filter_map(|r| r["full_name"].as_str().map(str::to_string)),
            );
            if n < 100 {
                break;
            }
        }
        if out.len() >= MAX_REPOS {
            crate::warn!(
                "gitee: you can access more than {MAX_REPOS} repositories; only the first {MAX_REPOS} are checked. \
                 Limit [scope] repos to the ones to review."
            );
        }
        *self.repos.lock().unwrap() = Some((Instant::now(), out.clone()));
        Ok(out)
    }

    /// Head commit: `head.sha` when the PR has it, otherwise the last of its commits.
    fn head_sha(&self, repo: &str, n: u64, d: &Value) -> Result<String> {
        if let Some(sha) = d["head"]["sha"].as_str().filter(|x| !x.is_empty()) {
            return Ok(sha.to_string());
        }
        let commits = self.api.get(&format!("{}/commits", Self::pull(repo, n)))?;
        commits
            .as_array()
            .and_then(|c| c.last())
            .and_then(|c| c["sha"].as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Gitee returned no commits for {repo}#{n}"))
    }

    fn to_pr(&self, repo: &str, d: &Value) -> Result<Pr> {
        let n = d["number"].as_u64().unwrap_or_default();
        let base = d["base"]["ref"]
            .as_str()
            .or_else(|| d["base"].as_str())
            .unwrap_or_default()
            .to_string();
        Ok(Pr {
            repo: repo.into(),
            number: n,
            title: s(d, "title"),
            author: d["user"]["login"].as_str().unwrap_or_default().into(),
            url: s(d, "html_url"),
            head: self.head_sha(repo, n, d)?,
            base,
            draft: d["draft"].as_bool().unwrap_or(false),
            state: match d["state"].as_str() {
                Some("open") => "open",
                Some("merged") => "merged",
                _ => "closed",
            }
            .into(),
            body: s(d, "body"),
        })
    }

    fn checks(&self, repo: &str, sha: &str) -> Result<String> {
        let v = self
            .api
            .get(&format!("/repos/{repo}/commits/{sha}/check-runs"))?;
        let runs = v["check_runs"]
            .as_array()
            .or_else(|| v.as_array())
            .cloned()
            .unwrap_or_default();
        if runs.is_empty() {
            return Ok("none".into());
        }
        let mut pending = false;
        for r in &runs {
            if r["status"].as_str() != Some("completed") {
                pending = true;
                continue;
            }
            match r["conclusion"].as_str() {
                Some("success") | Some("neutral") | Some("skipped") => {}
                _ => return Ok("failure".into()),
            }
        }
        Ok(if pending { "pending" } else { "success" }.into())
    }

    /// Inline comments and the summary comment; returns the summary's address and id.
    fn post_comments(&self, pr: &Pr, review: &Value, path: &str) -> Result<(String, u64)> {
        // Gitee places inline comments by their position in the file's diff, not by line number.
        let patches = self.patches(pr)?;
        // A retry after a failure partway through finds the comments it already posted for this commit and skips
        // them. Every comment carries a hidden marker with the commit, so the same finding on a new commit is posted.
        let mark = format!(
            "\n\n<!-- Nod {} -->",
            review["commit_id"].as_str().unwrap_or("")
        );
        let posted = self.my_comments(path)?;
        let mut unplaced = vec![];
        for c in review["comments"].as_array().into_iter().flatten() {
            let file = c["path"].as_str().unwrap_or("");
            let line = c["line"].as_u64().unwrap_or(0);
            let text = format!("`{file}:{line}`\n\n{}", c["body"].as_str().unwrap_or(""));
            let Some(position) = patches.get(file).and_then(|p| diff_position(p, line)) else {
                unplaced.push(text);
                continue;
            };
            let body = format!("{}{mark}", c["body"].as_str().unwrap_or(""));
            if posted.iter().any(|(p, b, _, _)| p == file && *b == body) {
                continue;
            }
            let inline = json!({"body": body, "commit_id": review["commit_id"], "path": file, "position": position});
            match self.api.send("POST", path, &inline) {
                Ok(_) => {}
                // refused by Gitee (4xx): keep the finding in the summary rather than lose it
                Err(e) if format!("{e:#}").contains("HTTP 4") => unplaced.push(text),
                // network or server trouble: let the engine retry the whole post
                Err(e) => return Err(e),
            }
        }
        let mut body = review["body"].as_str().unwrap_or("").to_string();
        for u in &unplaced {
            body.push_str("\n\n---\n");
            body.push_str(u);
        }
        body.push_str(&mark);
        if let Some((_, _, url, id)) = posted
            .iter()
            .find(|(p, b, _, _)| p.is_empty() && *b == body)
        {
            return Ok((
                if url.is_empty() {
                    pr.url.clone()
                } else {
                    url.clone()
                },
                *id,
            ));
        }
        let summary = self.api.send("POST", path, &json!({"body": body}))?;
        let url = summary["html_url"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| pr.url.clone());
        Ok((url, summary["id"].as_u64().unwrap_or_default()))
    }

    /// Comments this account already left on the pull request: (path or "" for a plain comment, body, url, id).
    fn my_comments(&self, path: &str) -> Result<Vec<(String, String, String, u64)>> {
        let me = self.me()?;
        let mut out = vec![];
        // every page: on a busy PR the comments of the failed attempt can be past the first hundred
        for page in 1..=20 {
            let v = self.api.get(&format!("{path}?per_page=100&page={page}"))?;
            let items = v.as_array().cloned().unwrap_or_default();
            out.extend(
                items
                    .iter()
                    .filter(|c| c["user"]["login"].as_str() == Some(me.as_str()))
                    .map(|c| {
                        (
                            s(c, "path"),
                            s(c, "body"),
                            s(c, "html_url"),
                            c["id"].as_u64().unwrap_or_default(),
                        )
                    }),
            );
            if items.len() < 100 {
                break;
            }
        }
        Ok(out)
    }

    /// Each changed file's diff (files that were removed are left out).
    fn patches(&self, pr: &Pr) -> Result<std::collections::HashMap<String, String>> {
        let v = self
            .api
            .get(&format!("{}/files", Self::pull(&pr.repo, pr.number)))?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter(|f| f["status"].as_str() != Some("removed"))
            .map(|f| {
                // the spec says a string; the live API has returned an object with the diff inside
                let patch = f["patch"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| f["patch"]["diff"].as_str().map(str::to_string))
                    .unwrap_or_default();
                (s(f, "filename"), patch)
            })
            .collect())
    }

    /// One pass over the repositories in scope for open pull requests that list this account as a reviewer.
    fn scan_requests(&self) -> Result<Vec<(String, u64)>> {
        let me = self.me()?;
        let mut out = vec![];
        for repo in self.repositories()? {
            let (inc, exc) = &self.scope;
            if !crate::config::selected(inc, exc, |p| crate::config::repo_matches(p, &repo, &me)) {
                continue;
            }
            // one repository failing (pull requests turned off, access revoked) must not stop the others
            let prs = match self.api.get(&format!(
                "/repos/{repo}/pulls?state=open&assignee={}&per_page=100",
                super::http::enc(&me)
            )) {
                Ok(v) => v,
                Err(e) => {
                    crate::warn!("gitee: skipping {repo}: {e:#}");
                    continue;
                }
            };
            for p in prs.as_array().into_iter().flatten() {
                // the filter matches assignees; double-check
                let mine = p["assignees"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|a| a["login"].as_str() == Some(me.as_str()));
                if let (true, Some(n)) = (mine, p["number"].as_u64()) {
                    out.push((repo.clone(), n));
                }
            }
        }
        Ok(out)
    }

    /// Restrict the repository scan to the review scope (saves one request per out-of-scope repository).
    pub fn with_scope(mut self, include: &[String], exclude: &[String]) -> Gitee {
        self.scope = (include.to_vec(), exclude.to_vec());
        self
    }
}

impl Forge for Gitee {
    fn current_user(&self) -> Result<String> {
        self.me()
    }

    fn fetch_pr(&self, repo: &str, number: u64) -> Result<Pr> {
        let d = self.api.get(&Self::pull(repo, number))?;
        self.to_pr(repo, &d)
    }

    fn requested_prs(&self, _teams: bool) -> Result<Vec<(String, u64)>> {
        if let Some((at, found)) = &*self.scan.lock().unwrap() {
            if at.elapsed() < SCAN_EVERY {
                return Ok(found.clone());
            }
        }
        let found = self.scan_requests()?;
        *self.scan.lock().unwrap() = Some((Instant::now(), found.clone()));
        Ok(found)
    }

    fn ci_state(&self, pr: &Pr) -> Result<String> {
        self.checks(&pr.repo, &pr.head)
    }

    fn pr_status(&self, repo: &str, number: u64) -> Result<PrStatus> {
        let d = self.api.get(&Self::pull(repo, number))?;
        let pr = self.to_pr(repo, &d)?;
        let approved = d["assignees"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|a| a["accept"].as_bool() == Some(true));
        Ok(PrStatus {
            ci: if pr.state == "open" {
                self.checks(repo, &pr.head)?
            } else {
                String::new()
            },
            state: pr.state,
            draft: pr.draft,
            decision: if approved {
                "APPROVED".into()
            } else {
                String::new()
            },
            head: pr.head,
            title: pr.title,
            url: pr.url,
            author: pr.author,
            additions: None,
            deletions: None,
            changed_files: None,
            head_ref: d["head"]["ref"].as_str().unwrap_or_default().into(),
            base_ref: pr.base,
        })
    }

    fn discussion(&self, pr: &Pr) -> Result<String> {
        let v = self.api.get(&format!(
            "{}/comments?per_page=100&direction=asc",
            Self::pull(&pr.repo, pr.number)
        ))?;
        let mut parts = vec![];
        for c in v.as_array().into_iter().flatten() {
            let who = c["user"]["login"].as_str().unwrap_or("");
            let body = crate::util::truncate(c["body"].as_str().unwrap_or(""), 600);
            match c["path"].as_str().filter(|p| !p.is_empty()) {
                Some(p) => parts.push(format!(
                    "[inline] {who} {p}:{}: {body}",
                    c["new_line"].as_str().unwrap_or("")
                )),
                None => parts.push(format!("[comment] {who}: {body}")),
            }
        }
        Ok(crate::util::keep_last(
            &parts.join("\n"),
            super::DISCUSSION_CHARS,
        ))
    }

    fn diff_lines(&self, pr: &Pr) -> Result<DiffLines> {
        Ok(self
            .patches(pr)?
            .iter()
            .map(|(f, p)| (f.clone(), commentable_lines(p)))
            .collect())
    }

    fn post_review(&self, pr: &Pr, review: &Value) -> Result<(String, u64)> {
        let pull = Self::pull(&pr.repo, pr.number);
        let path = format!("{pull}/comments");
        // Approve first: if it fails nothing has been posted yet, so the engine's retry does not duplicate comments.
        let approve = review["event"] == "APPROVE";
        // A retry after a later step failed finds the approval already given and does not send it again.
        if approve && self.list_reviews(pr)?.is_empty() {
            self.api
                .send("POST", &format!("{pull}/review"), &json!({}))?;
        }
        let posted = self.post_comments(pr, review, &path);
        if approve {
            // the approval stands even if the comments never get through: say so instead of a plain failure
            let (url, _) = posted.map_err(|e| {
                e.context(
                    "the pull request is already approved on Gitee (this cannot be withdrawn), but posting the review comments failed",
                )
            })?;
            return Ok((url, APPROVAL_ID));
        }
        posted
    }

    fn list_reviews(&self, pr: &Pr) -> Result<Vec<(u64, String)>> {
        let me = self.me()?;
        let d = self.api.get(&Self::pull(&pr.repo, pr.number))?;
        let mine = d["assignees"].as_array().into_iter().flatten().any(|a| {
            a["login"].as_str() == Some(me.as_str()) && a["accept"].as_bool() == Some(true)
        });
        Ok(if mine {
            vec![(APPROVAL_ID, "APPROVED".into())]
        } else {
            vec![]
        })
    }

    fn dismiss_review(&self, _pr: &Pr, id: u64, _message: &str) -> Result<()> {
        if id != APPROVAL_ID {
            return Ok(());
        }
        bail!("Gitee does not let a reviewer withdraw an approval (only an administrator can reset reviews)")
    }

    fn remote(&self, repo: &str) -> Result<Remote> {
        Ok(Remote::Git {
            url: format!("{}/{repo}.git", self.web),
            header: Some(basic(&self.me()?, &self.token)),
        })
    }
}

/// The position of new-file line `line` in a file's diff, as Gitee (like GitHub's original review API) counts it:
/// 1 is the line below the first `@@` header, and every following line — later `@@` headers included — adds one.
/// `None` when the line is not in the diff (a comment there would land on the wrong line).
pub fn diff_position(patch: &str, line: u64) -> Option<u64> {
    let mut pos = 0u64;
    let mut new_line: Option<u64> = None;
    let mut started = false;
    for raw in patch.lines() {
        if let Some(rest) = raw.strip_prefix("@@ -") {
            if started {
                pos += 1;
            }
            started = true;
            new_line = rest
                .split_once(" +")
                .and_then(|(_, r)| r.split([',', ' ']).next())
                .and_then(|n| n.parse().ok());
            continue;
        }
        if !started {
            continue;
        }
        pos += 1;
        if raw.starts_with('-') || raw.starts_with('\\') {
            continue;
        }
        let n = new_line?;
        if n == line {
            return Some(pos);
        }
        new_line = Some(n + 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::http::fake;
    use super::*;

    fn server(review_ok: bool) -> fake::Fake {
        let pull = json!({
            "number": 5, "title": "修复登录", "user": {"login": "alice"}, "html_url": "https://gitee.com/o/r/pulls/5",
            "head": {"ref": "fix", "sha": "c0ffee"}, "base": {"ref": "master"}, "state": "open", "draft": false,
            "body": "", "assignees": [{"login": "me", "accept": false}, {"login": "bob", "accept": true}]
        });
        fake::serve(vec![
            ("GET /api/v5/user", 200, json!({"login": "me"}).to_string()),
            ("GET /api/v5/user/repos", 200, json!([{"full_name": "o/r"}, {"full_name": "o/other"}]).to_string()),
            ("GET /api/v5/repos/o/r/pulls", 200, json!([{"number": 5, "assignees": [{"login": "me"}]}]).to_string()),
            ("GET /api/v5/repos/o/other/pulls", 200, "[]".into()),
            ("GET /api/v5/repos/o/r/pulls/5", 200, pull.to_string()),
            (
                "GET /api/v5/repos/o/r/commits/c0ffee/check-runs",
                200,
                json!({"total_count": 1, "check_runs": [{"status": "completed", "conclusion": "success"}]}).to_string(),
            ),
            (
                "GET /api/v5/repos/o/r/pulls/5/files",
                200,
                json!([{"filename": "a.py", "status": "modified", "patch": {"diff": "@@ -1 +1,2 @@\n x\n+y\n"}}]).to_string(),
            ),
            (
                "POST /api/v5/repos/o/r/pulls/5/comments",
                if review_ok { 201 } else { 400 },
                json!({"id": 77, "html_url": "https://gitee.com/o/r/pulls/5#note_77"}).to_string(),
            ),
            ("POST /api/v5/repos/o/r/pulls/5/review", 201, "".into()),
            ("GET /api/v5/repos/o/r/pulls/5/comments", 200, "[]".into()),
        ])
    }

    #[test]
    fn finds_requests_and_reads_prs() {
        let f = server(true);
        let g = Gitee::new(&f.url, "tok");
        assert_eq!(g.requested_prs(false).unwrap(), vec![("o/r".into(), 5)]);
        let pr = g.fetch_pr("o/r", 5).unwrap();
        assert_eq!(
            (pr.head.as_str(), pr.base.as_str(), pr.author.as_str()),
            ("c0ffee", "master", "alice")
        );
        assert_eq!(g.ci_state(&pr).unwrap(), "success");
        assert_eq!(g.diff_lines(&pr).unwrap()["a.py"].len(), 2);
        let seen = f.seen.lock().unwrap();
        assert!(
            seen.iter().all(|r| r.path.contains("access_token=tok")),
            "token as access_token"
        );
        assert!(seen.iter().any(|r| r.path.contains("assignee=me")));
    }

    #[test]
    fn scope_limits_the_repository_scan() {
        let f = server(true);
        let g = Gitee::new(&f.url, "tok").with_scope(&["o/other".into()], &[]);
        assert!(g.requested_prs(false).unwrap().is_empty());
        assert!(!f
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.path.starts_with("/api/v5/repos/o/r/pulls")));
    }

    #[test]
    fn posts_a_review_and_cannot_withdraw_it() {
        let f = server(true);
        let g = Gitee::new(&f.url, "tok");
        let pr = g.fetch_pr("o/r", 5).unwrap();
        let review = json!({"commit_id": "c0ffee", "event": "APPROVE", "body": "LGTM",
                            "comments": [{"path": "a.py", "line": 2, "side": "RIGHT", "body": "nit"}]});
        assert_eq!(g.post_review(&pr, &review).unwrap().1, APPROVAL_ID);
        assert!(g.dismiss_review(&pr, APPROVAL_ID, "x").is_err());
        let seen = f.seen.lock().unwrap();
        let posts: Vec<&fake::Seen> = seen.iter().filter(|r| r.method == "POST").collect();
        assert!(posts[0].path.contains("/pulls/5/review"), "approve first");
        // line 2 of a.py is the second line of its diff
        assert!(
            posts[1].body.contains("\"position\":2") && posts[1].body.contains("\"path\":\"a.py\"")
        );
        assert!(posts[2].body.contains("LGTM"));
    }

    #[test]
    fn refused_inline_comments_become_regular_comments() {
        let f = server(false);
        let g = Gitee::new(&f.url, "tok");
        let pr = g.fetch_pr("o/r", 5).unwrap();
        let review = json!({"commit_id": "c0ffee", "event": "COMMENT", "body": "see notes",
                            "comments": [{"path": "a.py", "line": 2, "body": "nit"},
                                         {"path": "a.py", "line": 99, "body": "outside the diff"}]});
        // every comment POST is refused (400) here, so the summary fails too and the error reaches the engine
        assert!(g.post_review(&pr, &review).is_err());
        let seen = f.seen.lock().unwrap();
        let summary = seen.iter().rev().find(|r| r.method == "POST").unwrap();
        assert!(
            summary.body.contains("`a.py:2`") && summary.body.contains("`a.py:99`"),
            "refused and unplaceable findings end up in the summary: {}",
            summary.body
        );
    }

    #[test]
    fn a_failure_after_approving_says_the_approval_stands() {
        let f = server(false);
        let g = Gitee::new(&f.url, "tok");
        let pr = g.fetch_pr("o/r", 5).unwrap();
        let review =
            json!({"commit_id": "c0ffee", "event": "APPROVE", "body": "LGTM", "comments": []});
        let err = format!("{:#}", g.post_review(&pr, &review).unwrap_err());
        assert!(err.contains("already approved on Gitee"), "{err}");
    }

    /// Regression (code review): a retry after a failure partway through posted every inline comment again.
    #[test]
    fn a_retry_does_not_post_the_same_comments_twice() {
        let pull = json!({"number": 5, "user": {"login": "alice"}, "head": {"sha": "c0ffee"}, "base": {"ref": "master"},
                          "state": "open", "assignees": [{"login": "me", "accept": false}]});
        let f = fake::serve(vec![
            ("GET /api/v5/user", 200, json!({"login": "me"}).to_string()),
            ("GET /api/v5/repos/o/r/pulls/5", 200, pull.to_string()),
            (
                "GET /api/v5/repos/o/r/pulls/5/files",
                200,
                json!([{"filename": "a.py", "status": "modified", "patch": "@@ -1 +1,2 @@
 x
+y
"}])
                .to_string(),
            ),
            // the first attempt got as far as the inline comment; someone else commented too; an older commit's
            // review left the same summary, which must not count
            (
                "GET /api/v5/repos/o/r/pulls/5/comments",
                200,
                json!([{"id": 3, "user": {"login": "me"}, "path": "a.py", "body": "nit\n\n<!-- Nod c0ffee -->"},
                       {"id": 2, "user": {"login": "me"}, "body": "see notes\n\n<!-- Nod oldsha -->"},
                       {"id": 4, "user": {"login": "bob"}, "body": "see notes\n\n<!-- Nod c0ffee -->"}])
                .to_string(),
            ),
            (
                "POST /api/v5/repos/o/r/pulls/5/comments",
                201,
                json!({"id": 9}).to_string(),
            ),
        ]);
        let g = Gitee::new(&f.url, "tok");
        let pr = g.fetch_pr("o/r", 5).unwrap();
        let review = json!({"commit_id": "c0ffee", "event": "COMMENT", "body": "see notes",
                            "comments": [{"path": "a.py", "line": 2, "body": "nit"}]});
        assert_eq!(g.post_review(&pr, &review).unwrap().1, 9);
        let seen = f.seen.lock().unwrap();
        let posts: Vec<&fake::Seen> = seen.iter().filter(|r| r.method == "POST").collect();
        assert_eq!(posts.len(), 1, "only the summary is posted");
        assert!(!posts[0].body.contains("\"path\""));
        assert!(posts[0].body.contains("Nod c0ffee"), "tied to this commit");
    }

    /// The full scan for review requests runs at most every few minutes (one call per repository).
    #[test]
    fn the_repository_scan_is_not_repeated_on_every_poll() {
        let f = server(true);
        let g = Gitee::new(&f.url, "tok");
        g.requested_prs(false).unwrap();
        g.requested_prs(false).unwrap();
        let scans = f
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path.starts_with("/api/v5/repos/o/r/pulls?"))
            .count();
        assert_eq!(scans, 1);
    }

    #[test]
    fn one_failing_repository_does_not_stop_the_scan() {
        let f = fake::serve(vec![
            ("GET /api/v5/user", 200, json!({"login": "me"}).to_string()),
            (
                "GET /api/v5/user/repos",
                200,
                json!([{"full_name": "o/gone"}, {"full_name": "o/r"}]).to_string(),
            ),
            // o/gone has no route: 404
            (
                "GET /api/v5/repos/o/r/pulls",
                200,
                json!([{"number": 5, "assignees": [{"login": "me"}]}]).to_string(),
            ),
        ]);
        let g = Gitee::new(&f.url, "tok");
        assert_eq!(g.requested_prs(false).unwrap(), vec![("o/r".into(), 5)]);
    }

    #[test]
    fn positions_count_diff_lines_not_file_lines() {
        let p = "@@ -10,3 +10,4 @@ fn a\n ctx10\n-old11\n+new11\n+new12\n ctx13\n@@ -40,2 +41,2 @@\n ctx41\n+new42\n";
        assert_eq!(diff_position(p, 10), Some(1));
        assert_eq!(diff_position(p, 11), Some(3)); // after the removed line
        assert_eq!(diff_position(p, 13), Some(5));
        assert_eq!(diff_position(p, 41), Some(7)); // the second @@ header counts as a line
        assert_eq!(diff_position(p, 42), Some(8));
        assert_eq!(diff_position(p, 20), None);
    }

    #[test]
    fn transport_errors_do_not_reveal_the_token() {
        let g = Gitee::new("http://127.0.0.1:9", "secret-token");
        let err = format!("{:#}", g.current_user().unwrap_err());
        assert!(!err.contains("secret-token"), "{err}");
    }
}
