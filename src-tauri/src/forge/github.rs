//! GitHub (and GitHub Enterprise): shells out to the `gh` CLI, so it uses whatever account `gh auth login` set up.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use super::{commentable_lines, DiffLines, Forge, Pr, PrStatus, Remote};
use crate::util::{run, RunOpts};

pub struct Gh {
    /// Empty for github.com; otherwise a GitHub Enterprise host such as `github.example.com`.
    host: String,
}

impl Gh {
    pub fn new(url: &str) -> Gh {
        let host = url
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string();
        Gh {
            host: if host == "github.com" {
                String::new()
            } else {
                host
            },
        }
    }
}

thread_local! {
    /// The Enterprise host for `gh` calls made by this thread (each account's daemon runs on its own threads).
    static HOST: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn host_env(host: &str) -> Option<HashMap<String, String>> {
    (!host.is_empty()).then(|| {
        let mut env: HashMap<String, String> = std::env::vars().collect();
        env.insert("GH_HOST".into(), host.into());
        env
    })
}

fn gh(args: &[&str]) -> Result<String> {
    let mut v = vec!["gh"];
    v.extend_from_slice(args);
    let env = HOST.with(|h| host_env(&h.borrow()));
    run(
        &v,
        RunOpts {
            timeout: Duration::from_secs(120),
            env: env.as_ref(),
            ..Default::default()
        },
    )
}

fn gh_json(args: &[&str]) -> Result<Value> {
    let out = gh(args)?;
    serde_json::from_str(if out.trim().is_empty() { "null" } else { &out })
        .context("gh returned invalid JSON")
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

impl Gh {
    fn on_host(&self) {
        HOST.with(|h| *h.borrow_mut() = self.host.clone());
    }
}

impl Forge for Gh {
    fn current_user(&self) -> Result<String> {
        self.on_host();
        Ok(gh(&["api", "user", "--jq", ".login"])?.trim().to_string())
    }

    fn fetch_pr(&self, repo: &str, number: u64) -> Result<Pr> {
        self.on_host();
        let d = gh_json(&["api", &format!("repos/{repo}/pulls/{number}")])?;
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
        self.on_host();
        // `user-review-requested` = you personally; `review-requested` also matches teams you belong to.
        let who = if teams {
            "review-requested"
        } else {
            "user-review-requested"
        };
        let q = format!("q=is:pr is:open {who}:@me archived:false");
        // all pages: someone asked to review 60 PRs must not see the last ten ignored
        let out = gh(&[
            "api",
            "--paginate",
            "-X",
            "GET",
            "search/issues",
            "-f",
            &q,
            "-f",
            "per_page=100",
            "--jq",
            ".items[] | [.repository_url, .number] | @tsv",
        ])?;
        Ok(out
            .lines()
            .filter_map(|l| {
                let (url, n) = l.split_once('\t')?;
                let parts: Vec<&str> = url.rsplitn(3, '/').collect();
                Some((
                    format!("{}/{}", parts.get(1)?, parts.first()?),
                    n.trim().parse().ok()?,
                ))
            })
            .collect())
    }

    fn ci_state(&self, pr: &Pr) -> Result<String> {
        self.on_host();
        let runs = gh(&[
            "api",
            &format!(
                "repos/{}/commits/{}/check-runs?per_page=100",
                pr.repo, pr.head
            ),
            "--paginate",
            "--jq",
            ".check_runs[] | [.status, (.conclusion // \"\")] | @tsv",
        ])?;
        let status = gh_json(&[
            "api",
            &format!("repos/{}/commits/{}/status", pr.repo, pr.head),
        ])?;
        let mut states = vec![];
        for l in runs.lines() {
            let mut it = l.split('\t');
            let st = it.next().unwrap_or("");
            let concl = it.next().unwrap_or("");
            states.push(if st != "completed" {
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
        self.on_host();
        let d = gh_json(&[
            "pr", "view", &number.to_string(), "-R", repo, "--json",
            "state,isDraft,reviewDecision,headRefOid,title,url,author,additions,deletions,changedFiles,headRefName,baseRefName,mergedAt",
        ])?;
        let state = if d["mergedAt"].as_str().is_some_and(|x| !x.is_empty()) {
            "merged".to_string()
        } else {
            s(&d, "state").to_lowercase()
        };
        let mut st = PrStatus {
            state: state.clone(),
            draft: d["isDraft"].as_bool().unwrap_or(false),
            decision: s(&d, "reviewDecision"),
            head: s(&d, "headRefOid"),
            ci: String::new(),
            title: s(&d, "title"),
            url: s(&d, "url"),
            author: d["author"]["login"].as_str().unwrap_or_default().into(),
            additions: d["additions"].as_u64(),
            deletions: d["deletions"].as_u64(),
            changed_files: d["changedFiles"].as_u64(),
            head_ref: s(&d, "headRefName"),
            base_ref: s(&d, "baseRefName"),
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
        self.on_host();
        let mut parts = vec![];
        let base = format!("repos/{}", pr.repo);
        let list = |path: String| -> Result<Vec<Value>> {
            Ok(gh_json(&["api", "-X", "GET", &path, "-f", "per_page=100"])?
                .as_array()
                .cloned()
                .unwrap_or_default())
        };
        let cut = |v: &Value, n: usize| crate::util::truncate(v["body"].as_str().unwrap_or(""), n);
        for c in list(format!("{base}/issues/{}/comments", pr.number))? {
            parts.push(format!(
                "[comment] {}: {}",
                c["user"]["login"].as_str().unwrap_or(""),
                cut(&c, 600)
            ));
        }
        for r in list(format!("{base}/pulls/{}/reviews", pr.number))? {
            if !r["body"].as_str().unwrap_or("").is_empty() {
                parts.push(format!(
                    "[review {}] {}: {}",
                    r["state"].as_str().unwrap_or(""),
                    r["user"]["login"].as_str().unwrap_or(""),
                    cut(&r, 600)
                ));
            }
        }
        for c in list(format!("{base}/pulls/{}/comments", pr.number))? {
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
        self.on_host();
        let pages = gh_json(&[
            "api",
            &format!("repos/{}/pulls/{}/files", pr.repo, pr.number),
            "--paginate",
            "--slurp",
        ])?;
        let mut out = DiffLines::new();
        for page in pages.as_array().into_iter().flatten() {
            for f in page.as_array().into_iter().flatten() {
                out.insert(
                    s(f, "filename"),
                    commentable_lines(f["patch"].as_str().unwrap_or("")),
                );
            }
        }
        Ok(out)
    }

    fn post_review(&self, pr: &Pr, review: &Value) -> Result<(String, u64)> {
        self.on_host();
        let out = run(
            &[
                "gh",
                "api",
                "-X",
                "POST",
                &format!("repos/{}/pulls/{}/reviews", pr.repo, pr.number),
                "--input",
                "-",
                "--jq",
                "[.html_url, .id] | @tsv",
            ],
            RunOpts {
                stdin: Some(&review.to_string()),
                timeout: Duration::from_secs(120),
                env: host_env(&self.host).as_ref(),
                ..Default::default()
            },
        )?;
        let (url, id) = out
            .trim()
            .split_once('\t')
            .ok_or_else(|| anyhow!("unexpected gh output: {out}"))?;
        Ok((url.into(), id.parse()?))
    }

    fn list_reviews(&self, pr: &Pr) -> Result<Vec<(u64, String)>> {
        self.on_host();
        // all pages: on a busy PR our own review can be past the first hundred
        let out = gh(&[
            "api",
            "--paginate",
            &format!("repos/{}/pulls/{}/reviews?per_page=100", pr.repo, pr.number),
            "--jq",
            ".[] | [.id, .state] | @tsv",
        ])?;
        Ok(out
            .lines()
            .filter_map(|l| {
                let (id, state) = l.split_once('\t')?;
                Some((id.trim().parse().ok()?, state.trim().to_string()))
            })
            .collect())
    }

    fn dismiss_review(&self, pr: &Pr, id: u64, message: &str) -> Result<()> {
        self.on_host();
        let body = json!({"message": message, "event": "DISMISS"}).to_string();
        run(
            &[
                "gh",
                "api",
                "-X",
                "PUT",
                &format!(
                    "repos/{}/pulls/{}/reviews/{id}/dismissals",
                    pr.repo, pr.number
                ),
                "--input",
                "-",
            ],
            RunOpts {
                stdin: Some(&body),
                timeout: Duration::from_secs(60),
                env: host_env(&self.host).as_ref(),
                ..Default::default()
            },
        )?;
        Ok(())
    }

    fn remote(&self, _repo: &str) -> Result<Remote> {
        Ok(Remote::Gh {
            host: self.host.clone(),
        })
    }
}
