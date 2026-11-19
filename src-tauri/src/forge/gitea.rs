//! Gitea and Forgejo over the REST API v1 with an access token (scopes: read:user, write:repository, write:issue).

use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use super::http::{basic, diff_lines_from_patch, Api};
use super::{DiffLines, Forge, Pr, PrStatus, Remote};

pub struct Gitea {
    web: String,
    token: String,
    api: Api,
    me: OnceLock<String>,
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

impl Gitea {
    pub fn new(url: &str, token: &str) -> Gitea {
        let web = url.trim().trim_end_matches('/').to_string();
        Gitea {
            api: Api::new(
                &format!("{web}/api/v1"),
                "Authorization",
                &format!("token {token}"),
            ),
            web,
            token: token.to_string(),
            me: OnceLock::new(),
        }
    }

    fn pull_path(repo: &str, n: u64) -> String {
        format!("/repos/{repo}/pulls/{n}")
    }

    fn me(&self) -> Result<String> {
        if let Some(m) = self.me.get() {
            return Ok(m.clone());
        }
        let m = s(&self.api.get("/user")?, "login");
        if m.is_empty() {
            return Err(anyhow!("Gitea returned no login for this token"));
        }
        let _ = self.me.set(m.clone());
        Ok(m)
    }

    fn to_pr(repo: &str, d: &Value) -> Pr {
        let title = s(d, "title");
        Pr {
            repo: repo.into(),
            number: d["number"].as_u64().unwrap_or_default(),
            draft: d["draft"].as_bool().unwrap_or(false)
                || ["WIP:", "[WIP]"].iter().any(|p| title.starts_with(p)),
            title,
            author: d["user"]["login"].as_str().unwrap_or_default().into(),
            url: s(d, "html_url"),
            head: d["head"]["sha"].as_str().unwrap_or_default().into(),
            base: d["base"]["ref"].as_str().unwrap_or_default().into(),
            state: if d["merged"].as_bool() == Some(true) {
                "merged".into()
            } else {
                s(d, "state")
            },
            body: s(d, "body"),
        }
    }

    fn page_all(&self, path: &str) -> Result<Vec<Value>> {
        let sep = if path.contains('?') { '&' } else { '?' };
        let mut out = vec![];
        for page in 1..=20 {
            let v = self.api.get(&format!("{path}{sep}limit=50&page={page}"))?;
            let items = v.as_array().cloned().unwrap_or_default();
            let n = items.len();
            out.extend(items);
            if n < 50 {
                break;
            }
        }
        Ok(out)
    }
}

impl Forge for Gitea {
    fn current_user(&self) -> Result<String> {
        self.me()
    }

    fn fetch_pr(&self, repo: &str, number: u64) -> Result<Pr> {
        Ok(Self::to_pr(
            repo,
            &self.api.get(&Self::pull_path(repo, number))?,
        ))
    }

    fn requested_prs(&self, _teams: bool) -> Result<Vec<(String, u64)>> {
        let items =
            self.page_all("/repos/issues/search?type=pulls&state=open&review_requested=true")?;
        Ok(items
            .iter()
            .filter_map(|i| {
                Some((
                    i["repository"]["full_name"].as_str()?.to_string(),
                    i["number"].as_u64()?,
                ))
            })
            .collect())
    }

    fn ci_state(&self, pr: &Pr) -> Result<String> {
        let st = self
            .api
            .get(&format!("/repos/{}/commits/{}/status", pr.repo, pr.head))?;
        if st["total_count"].as_u64() == Some(0) {
            return Ok("none".into());
        }
        Ok(match st["state"].as_str() {
            Some("success") => "success",
            Some("pending") | Some("warning") => "pending",
            Some("failure") | Some("error") => "failure",
            _ => "none",
        }
        .into())
    }

    fn pr_status(&self, repo: &str, number: u64) -> Result<PrStatus> {
        let d = self.api.get(&Self::pull_path(repo, number))?;
        let pr = Self::to_pr(repo, &d);
        let ci = if pr.state == "open" {
            self.ci_state(&pr)?
        } else {
            String::new()
        };
        Ok(PrStatus {
            state: pr.state.clone(),
            draft: pr.draft,
            decision: String::new(),
            head: pr.head,
            ci,
            title: pr.title,
            url: pr.url,
            author: pr.author,
            additions: d["additions"].as_u64(),
            deletions: d["deletions"].as_u64(),
            changed_files: d["changed_files"].as_u64(),
            head_ref: d["head"]["ref"].as_str().unwrap_or_default().into(),
            base_ref: pr.base,
        })
    }

    fn discussion(&self, pr: &Pr) -> Result<String> {
        let mut parts = vec![];
        for c in self.page_all(&format!("/repos/{}/issues/{}/comments", pr.repo, pr.number))? {
            parts.push(format!(
                "[comment] {}: {}",
                c["user"]["login"].as_str().unwrap_or(""),
                crate::util::truncate(c["body"].as_str().unwrap_or(""), 600)
            ));
        }
        for r in self.page_all(&format!("{}/reviews", Self::pull_path(&pr.repo, pr.number)))? {
            if !r["body"].as_str().unwrap_or("").is_empty() {
                parts.push(format!(
                    "[review {}] {}: {}",
                    r["state"].as_str().unwrap_or(""),
                    r["user"]["login"].as_str().unwrap_or(""),
                    crate::util::truncate(r["body"].as_str().unwrap_or(""), 600)
                ));
            }
        }
        Ok(crate::util::keep_last(
            &parts.join("\n"),
            super::DISCUSSION_CHARS,
        ))
    }

    fn diff_lines(&self, pr: &Pr) -> Result<DiffLines> {
        let patch = self
            .api
            .get_text(&format!("{}.diff", Self::pull_path(&pr.repo, pr.number)))?;
        Ok(diff_lines_from_patch(&patch))
    }

    fn post_review(&self, pr: &Pr, review: &Value) -> Result<(String, u64)> {
        let event = match review["event"].as_str() {
            Some("APPROVE") => "APPROVED",
            _ => "COMMENT",
        };
        let comments: Vec<Value> = review["comments"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| json!({"path": c["path"], "body": c["body"], "new_position": c["line"]}))
            .collect();
        let r = self.api.send(
            "POST",
            &format!("{}/reviews", Self::pull_path(&pr.repo, pr.number)),
            &json!({"body": review["body"], "event": event, "commit_id": review["commit_id"], "comments": comments}),
        )?;
        let id = r["id"]
            .as_u64()
            .ok_or_else(|| anyhow!("Gitea did not return a review id"))?;
        let url = r["html_url"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| pr.url.clone());
        Ok((url, id))
    }

    fn list_reviews(&self, pr: &Pr) -> Result<Vec<(u64, String)>> {
        Ok(self
            .page_all(&format!("{}/reviews", Self::pull_path(&pr.repo, pr.number)))?
            .iter()
            .filter_map(|r| Some((r["id"].as_u64()?, s(r, "state"))))
            .collect())
    }

    fn dismiss_review(&self, pr: &Pr, id: u64, message: &str) -> Result<()> {
        self.api.send(
            "POST",
            &format!(
                "{}/reviews/{id}/dismissals",
                Self::pull_path(&pr.repo, pr.number)
            ),
            &json!({"message": message}),
        )?;
        Ok(())
    }

    fn remote(&self, repo: &str) -> Result<Remote> {
        Ok(Remote::Git {
            url: format!("{}/{repo}.git", self.web),
            header: Some(basic(&self.me()?, &self.token)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::http::fake;
    use super::*;

    fn server() -> fake::Fake {
        let pull = json!({
            "number": 3, "title": "Fix login", "user": {"login": "alice"}, "html_url": "https://git.example.com/o/r/pulls/3",
            "head": {"sha": "f00d", "ref": "fix"}, "base": {"ref": "main"}, "state": "open", "merged": false,
            "body": "", "additions": 4, "deletions": 1, "changed_files": 2
        });
        fake::serve(vec![
            ("GET /api/v1/user", 200, json!({"login": "me"}).to_string()),
            (
                "GET /api/v1/repos/issues/search",
                200,
                json!([{"number": 3, "repository": {"full_name": "o/r"}}]).to_string(),
            ),
            ("GET /api/v1/repos/o/r/pulls/3", 200, pull.to_string()),
            (
                "GET /api/v1/repos/o/r/commits/f00d/status",
                200,
                json!({"state": "failure", "total_count": 2}).to_string(),
            ),
            (
                "GET /api/v1/repos/o/r/pulls/3.diff",
                200,
                "diff --git a/a.go b/a.go\n--- a/a.go\n+++ b/a.go\n@@ -1 +1,2 @@\n x\n+y\n".into(),
            ),
            (
                "POST /api/v1/repos/o/r/pulls/3/reviews",
                200,
                json!({"id": 41, "html_url": "https://git.example.com/o/r/pulls/3#issuecomment-41"}).to_string(),
            ),
            (
                "GET /api/v1/repos/o/r/pulls/3/reviews",
                200,
                json!([{"id": 41, "state": "APPROVED", "body": "ok", "user": {"login": "me"}}]).to_string(),
            ),
            ("POST /api/v1/repos/o/r/pulls/3/reviews/41/dismissals", 200, "{}".into()),
        ])
    }

    #[test]
    fn reads_pull_requests() {
        let f = server();
        let g = Gitea::new(&f.url, "tok");
        assert_eq!(g.requested_prs(false).unwrap(), vec![("o/r".into(), 3)]);
        let pr = g.fetch_pr("o/r", 3).unwrap();
        assert_eq!((pr.head.as_str(), pr.state.as_str()), ("f00d", "open"));
        assert_eq!(g.ci_state(&pr).unwrap(), "failure");
        let st = g.pr_status("o/r", 3).unwrap();
        assert_eq!((st.additions, st.changed_files), (Some(4), Some(2)));
        assert_eq!(g.diff_lines(&pr).unwrap()["a.go"].len(), 2);
        assert!(f.seen.lock().unwrap().iter().all(|r| r.auth == "token tok"));
    }

    #[test]
    fn posts_and_dismisses_reviews() {
        let f = server();
        let g = Gitea::new(&f.url, "tok");
        let pr = g.fetch_pr("o/r", 3).unwrap();
        let review = json!({"commit_id": "f00d", "event": "APPROVE", "body": "LGTM",
                            "comments": [{"path": "a.go", "line": 2, "side": "RIGHT", "body": "nit"}]});
        assert_eq!(g.post_review(&pr, &review).unwrap().1, 41);
        assert_eq!(g.list_reviews(&pr).unwrap(), vec![(41, "APPROVED".into())]);
        g.dismiss_review(&pr, 41, "new commits").unwrap();
        match g.remote("o/r").unwrap() {
            Remote::Git { url, header } => {
                assert!(url.ends_with("/o/r.git"));
                assert!(header.unwrap().starts_with("Authorization: Basic "));
            }
        }
        let seen = f.seen.lock().unwrap();
        let post = seen
            .iter()
            .find(|r| r.method == "POST" && r.path.ends_with("/reviews"))
            .unwrap();
        assert!(
            post.body.contains("\"event\":\"APPROVED\"")
                && post.body.contains("\"new_position\":2")
        );
        assert!(seen
            .iter()
            .any(|r| r.path.ends_with("/reviews/41/dismissals")));
    }
}
