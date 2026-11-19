//! A small JSON-over-HTTPS client for the REST forges (Gitea, Gitee), plus a fake server for their tests.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;

/// Where the token goes.
enum Auth {
    /// A request header (name, value), e.g. Gitea's `Authorization: token …`.
    Header(String, String),
    /// A query parameter (name, value), e.g. Gitee's `access_token`.
    Query(String, String),
}

pub struct Api {
    /// e.g. `https://gitea.example.com/api/v1`
    pub base: String,
    auth: Auth,
}

impl Api {
    pub fn new(base: &str, header: &str, value: &str) -> Api {
        Api {
            base: base.trim_end_matches('/').to_string(),
            auth: Auth::Header(header.to_string(), value.to_string()),
        }
    }

    /// The token travels as a query parameter (`?name=value`) on every request.
    pub fn with_query(base: &str, name: &str, value: &str) -> Api {
        Api {
            base: base.trim_end_matches('/').to_string(),
            auth: Auth::Query(name.to_string(), value.to_string()),
        }
    }

    fn req(&self, method: &str, path: &str) -> ureq::Request {
        let mut url = if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{path}", self.base)
        };
        if let Auth::Query(k, v) = &self.auth {
            let sep = if url.contains('?') { '&' } else { '?' };
            url = format!("{url}{sep}{k}={}", enc(v));
        }
        let r = ureq::request(method, &url)
            .timeout(Duration::from_secs(60))
            .set("Accept", "application/json")
            .set("User-Agent", concat!("nod/", env!("CARGO_PKG_VERSION")));
        match &self.auth {
            Auth::Header(k, v) => r.set(k, v),
            Auth::Query(..) => r,
        }
    }

    fn finish(
        &self,
        method: &str,
        path: &str,
        r: Result<ureq::Response, ureq::Error>,
    ) -> Result<ureq::Response> {
        match r {
            Ok(resp) => Ok(resp),
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                Err(anyhow!(
                    "{method} {path}: HTTP {code}: {}",
                    crate::util::truncate(body.trim(), 300)
                ))
            }
            // transport errors quote the URL, which carries a query-parameter token: never show it
            Err(e) => Err(anyhow!("{method} {path}: {}", self.redact(&e.to_string()))),
        }
    }

    fn redact(&self, text: &str) -> String {
        match &self.auth {
            Auth::Query(_, v) if !v.is_empty() => {
                text.replace(&enc(v), "***").replace(v.as_str(), "***")
            }
            _ => text.to_string(),
        }
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        let r = self.finish("GET", path, self.req("GET", path).call())?;
        Ok(r.into_json()?)
    }

    pub fn get_text(&self, path: &str) -> Result<String> {
        let r = self.finish("GET", path, self.req("GET", path).call())?;
        Ok(r.into_string()?)
    }

    pub fn send(&self, method: &str, path: &str, body: &Value) -> Result<Value> {
        let r = self.finish(method, path, self.req(method, path).send_json(body.clone()))?;
        let text = r.into_string()?;
        Ok(if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text)?
        })
    }
}

/// Percent-encode a query or path value (`owner/repo` → `owner%2Frepo`).
pub fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `Authorization: Basic base64(user:password)` for git over HTTPS.
pub fn basic(user: &str, password: &str) -> String {
    use base64::Engine;
    format!(
        "Authorization: Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

/// Commentable right-side lines per file from a unified multi-file diff (`git diff` / `.diff` output).
pub fn diff_lines_from_patch(patch: &str) -> super::DiffLines {
    let mut out = super::DiffLines::new();
    let mut file: Option<String> = None;
    let mut buf = String::new();
    let flush = |file: &Option<String>, buf: &mut String, out: &mut super::DiffLines| {
        if let Some(f) = file {
            out.insert(f.clone(), super::commentable_lines(buf));
        }
        buf.clear();
    };
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            flush(&file, &mut buf, &mut out);
            file = None;
            continue;
        }
        if let Some(p) = line.strip_prefix("+++ ") {
            file = (p != "/dev/null").then(|| p.trim_start_matches("b/").to_string());
            continue;
        }
        if line.starts_with("--- ") && buf.is_empty() {
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
    }
    flush(&file, &mut buf, &mut out);
    out
}

#[cfg(test)]
pub mod fake {
    //! A tiny HTTP server answering canned JSON per "METHOD /path" (query ignored unless the route has one), and
    //! recording every request (method, path with query, auth header, body).

    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    pub struct Seen {
        pub method: String,
        pub path: String,
        pub auth: String,
        pub body: String,
    }

    pub struct Fake {
        pub url: String,
        pub seen: Arc<Mutex<Vec<Seen>>>,
    }

    /// routes: "GET /api/v4/user" → (status, body)
    pub fn serve(routes: Vec<(&str, u16, String)>) -> Fake {
        serve_with(
            routes
                .into_iter()
                .map(|(k, s, b)| (k, s, b, vec![]))
                .collect(),
        )
    }

    /// (route, status, body, extra response headers)
    pub type Route<'a> = (&'a str, u16, String, Vec<(&'a str, &'a str)>);

    /// Like `serve`, with extra response headers per route (e.g. `Link` for pagination). In a header value,
    /// `{base}` is replaced by the server's own address.
    pub fn serve_with(routes: Vec<Route>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let routes: HashMap<String, (u16, String, String)> = routes
            .into_iter()
            .map(|(k, s, b, h)| {
                let extra: String = h
                    .iter()
                    .map(|(n, v)| format!("{n}: {}\r\n", v.replace("{base}", &url)))
                    .collect();
                (k.to_string(), (s, b, extra))
            })
            .collect();
        let seen = Arc::new(Mutex::new(vec![]));
        let rec = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() || first.is_empty() {
                    continue;
                }
                let mut parts = first.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let full = parts.next().unwrap_or("").to_string();
                let (mut len, mut auth) = (0usize, String::new());
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    let (k, v) = h.split_once(':').unwrap_or(("", ""));
                    match k.trim().to_lowercase().as_str() {
                        "content-length" => len = v.trim().parse().unwrap_or(0),
                        "authorization" | "private-token" => auth = v.trim().to_string(),
                        _ => {}
                    }
                }
                let mut body = vec![0; len];
                let _ = reader.read_exact(&mut body);
                rec.lock().unwrap().push(Seen {
                    method: method.clone(),
                    path: full.clone(),
                    auth,
                    body: String::from_utf8_lossy(&body).into(),
                });
                let path = full.split('?').next().unwrap_or("");
                let (status, resp, extra) = routes
                    .get(&format!("{method} {full}"))
                    .or_else(|| routes.get(&format!("{method} {path}")))
                    .cloned()
                    .unwrap_or((404, "{\"message\":\"not found\"}".into(), String::new()));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                    resp.len()
                );
            }
        });
        Fake { url, seen }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(enc("grp/sub/my proj"), "grp%2Fsub%2Fmy%20proj");
        assert!(basic("oauth2", "t").ends_with("b2F1dGgyOnQ="));
        let d = diff_lines_from_patch(
            "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1,2 +1,3 @@\n a\n+b\n c\n\
             diff --git a/gone.rs b/gone.rs\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n",
        );
        assert_eq!(d["x.rs"], std::collections::HashSet::from([1, 2, 3]));
        assert!(!d.contains_key("gone.rs"));
    }
}
