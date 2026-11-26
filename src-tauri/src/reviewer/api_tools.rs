//! Read-only tools for the OpenAI-compatible API engine, and the diff split into per-file pieces.
//!
//! The model calls `read_file`, `list_dir`, `search` and `get_diff` through the chat completions `tools` field; Nod
//! runs them here, inside the PR's checkout only: no commands, no network, no writes. Paths that leave the checkout
//! (`..`, absolute paths, drive prefixes, links resolved outside it) and `.git` are refused with an error text the
//! model reads, never served.
//!
//! Each `get_diff` call records which bytes of the file's diff it returned ([`Tools::last_range`]); the engine counts a
//! file as seen when the diffs still in the conversation cover all of it ([`covered`]). Reading the current file is not
//! seeing the change.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

/// At most this much text goes back to the model from one tool call, with a large budget ([`result_max`]).
pub const RESULT_MAX: usize = 16 * 1024;
const READ_LINES: usize = 400;
const LIST_MAX: usize = 300;
const SEARCH_MAX: usize = 100;
const SEARCH_FILES_MAX: usize = 20_000;
const SEARCH_FILE_BYTES: u64 = 1024 * 1024;

/// One file's part of `git diff`.
#[derive(Debug, Clone, PartialEq)]
pub struct FileDiff {
    pub path: String,
    pub text: String,
}

/// Changes that rarely need a reviewer's eyes and are often huge: lock files, minified bundles, source maps,
/// snapshots. They go last into the message; not seeing them does not hold an approval (it is listed).
pub fn is_low_value(path: &str) -> bool {
    let p = path.replace('\\', "/").to_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    const LOCKS: [&str; 14] = [
        "cargo.lock",
        "package-lock.json",
        "npm-shrinkwrap.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "bun.lock",
        "bun.lockb",
        "poetry.lock",
        "pipfile.lock",
        "uv.lock",
        "gemfile.lock",
        "composer.lock",
        "go.sum",
        "packages.lock.json",
    ];
    LOCKS.contains(&name)
        || name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || name.ends_with(".map")
        || name.ends_with(".snap")
        || p.split('/').any(|seg| seg == "__snapshots__")
}

/// Whether git printed only that the file differs ("Binary files … differ", or a binary patch) instead of its
/// changes. The PR decides that itself: a `-diff` line in its `.gitattributes`, or one NUL byte in the file.
pub fn opaque_section(text: &str) -> bool {
    text.lines().any(|l| {
        (l.starts_with("Binary files ") && l.ends_with(" differ")) || l == "GIT binary patch"
    })
}

/// Extensions of files that are binary by nature: their changes cannot be reviewed as text by any engine.
const BINARY_EXT: [&str; 44] = [
    "png", "jpg", "jpeg", "gif", "webp", "ico", "bmp", "tif", "tiff", "avif", "heic", "psd", "pdf",
    "zip", "gz", "tgz", "bz2", "xz", "7z", "rar", "jar", "war", "woff", "woff2", "ttf", "otf",
    "eot", "mp3", "mp4", "mov", "avi", "wav", "ogg", "webm", "flac", "exe", "dll", "so", "dylib",
    "class", "pyc", "wasm", "sqlite", "db",
];

/// A path as git prints it: `"a/\344\270\255.rs"` (quoted, octal escapes) or plain.
fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) else {
        return s.to_string();
    };
    let mut bytes = vec![];
    let mut it = inner.bytes().peekable();
    while let Some(b) = it.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match it.next() {
            Some(d @ b'0'..=b'7') => {
                let mut v = (d - b'0') as u32;
                for _ in 0..2 {
                    match it.peek() {
                        Some(&n @ b'0'..=b'7') => {
                            v = v * 8 + (n - b'0') as u32;
                            it.next();
                        }
                        _ => break,
                    }
                }
                bytes.push(v as u8);
            }
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(c) => bytes.push(c),
            None => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The path a `diff --git` section is about: the new name, or the old one for a deleted file.
fn section_path(section: &str) -> String {
    let mut old = None;
    for l in section.lines().take(12) {
        if let Some(p) = l.strip_prefix("+++ ") {
            if p != "/dev/null" {
                let p = unquote(p.trim_end());
                return p.strip_prefix("b/").unwrap_or(&p).to_string();
            }
        } else if let Some(p) = l.strip_prefix("--- ") {
            if p != "/dev/null" {
                let p = unquote(p.trim_end());
                old = Some(p.strip_prefix("a/").unwrap_or(&p).to_string());
            }
        } else if let Some(p) = l.strip_prefix("rename to ") {
            return unquote(p.trim_end());
        }
    }
    if let Some(o) = old {
        return o;
    }
    // binary or mode-only change: "diff --git a/x b/x"
    let head = section.lines().next().unwrap_or("");
    let rest = head.strip_prefix("diff --git ").unwrap_or(head);
    if rest.starts_with('"') {
        // "a/…" "b/…": the second quoted string
        if let Some(i) = rest.rfind(" \"") {
            let p = unquote(&rest[i + 1..]);
            return p.strip_prefix("b/").unwrap_or(&p).to_string();
        }
    }
    match rest.rfind(" b/") {
        Some(i) => rest[i + 3..].to_string(),
        None => rest.to_string(),
    }
}

/// `git diff` output, one piece per file, in git's order. A path git prints twice (a symlink that became a regular
/// file, or the other way round: a deletion and an addition) is one piece, so it counts as seen only when all of it
/// was.
pub fn split_diff(patch: &str) -> Vec<FileDiff> {
    let mut starts: Vec<usize> = vec![];
    let mut pos = 0;
    for l in patch.split_inclusive('\n') {
        if l.starts_with("diff --git ") {
            starts.push(pos);
        }
        pos += l.len();
    }
    let mut out = vec![];
    for (i, &s) in starts.iter().enumerate() {
        let e = starts.get(i + 1).copied().unwrap_or(patch.len());
        let text = &patch[s..e];
        let path = section_path(text);
        match out.iter_mut().find(|f: &&mut FileDiff| f.path == path) {
            Some(f) => f.text.push_str(text),
            None => out.push(FileDiff {
                path,
                text: text.to_string(),
            }),
        }
    }
    out
}

/// The diffs that fit into `budget` bytes, whole files only: ordinary files first in git's order, low-value ones
/// after them; a file that does not fit is skipped and smaller ones after it may still go in.
pub struct Packed {
    pub text: String,
    pub included: Vec<String>,
    /// (path, size in bytes) of the files left out, in the same order.
    pub left_out: Vec<(String, usize)>,
}

pub fn pack(files: &[FileDiff], budget: usize) -> Packed {
    let (low, normal): (Vec<&FileDiff>, Vec<&FileDiff>) =
        files.iter().partition(|f| is_low_value(&f.path));
    let mut p = Packed {
        text: String::new(),
        included: vec![],
        left_out: vec![],
    };
    for f in normal.into_iter().chain(low) {
        if p.text.len() + f.text.len() <= budget {
            p.text.push_str(&f.text);
            p.included.push(f.path.clone());
        } else {
            p.left_out.push((f.path.clone(), f.text.len()));
        }
    }
    p
}

/// How much one tool call may return under a conversation budget of `budget` bytes: a fifth of it, between 2 and
/// 16 KiB, so that even a small local model can make a few calls.
pub fn result_max(budget: usize) -> usize {
    (budget / 5).clamp(2 * 1024, RESULT_MAX)
}

/// The changed files whose whole diff is covered by `ranges` ((path, start, end) in bytes, from `get_diff`).
pub fn covered(files: &[FileDiff], ranges: &[(String, usize, usize)]) -> BTreeSet<String> {
    let mut by: HashMap<&str, Vec<(usize, usize)>> = HashMap::new();
    for (p, s, e) in ranges {
        by.entry(p.as_str()).or_default().push((*s, *e));
    }
    let mut out = BTreeSet::new();
    for f in files {
        let Some(r) = by.get_mut(f.path.as_str()) else {
            continue;
        };
        r.sort();
        let mut reach = 0;
        for &(s, e) in r.iter() {
            if s > reach {
                break;
            }
            reach = reach.max(e);
        }
        if reach >= f.text.len() {
            out.insert(f.path.clone());
        }
    }
    out
}

/// Cut `s` to at most `max` bytes at a character boundary, saying so.
fn cap(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max.saturating_sub(80);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push_str("\n[output cut: too long. Ask for a narrower range.]");
    s
}

/// The pages of one diff, each at most `size` bytes: cut at line ends, and a longer line is cut into pieces of its
/// own (at character boundaries), so no page is ever shortened on the way to the model.
fn pages(text: &str, size: usize) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let mut end = 0;
    for l in text.split_inclusive('\n') {
        if end > start && end - start + l.len() > size {
            out.push(&text[start..end]);
            start = end;
        }
        end += l.len();
        while end - start > size {
            let mut cut = start + size;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            out.push(&text[start..cut]);
            start = cut;
        }
    }
    if end > start || out.is_empty() {
        out.push(&text[start..end]);
    }
    out
}

fn binary(head: &[u8]) -> bool {
    head.iter().take(8000).any(|&b| b == 0)
}

pub struct Tools {
    root: PathBuf,
    files: Vec<FileDiff>,
    /// What the last `get_diff` call returned: (path, start, end) in bytes of that file's diff.
    pub last_range: Option<(String, usize, usize)>,
    /// How many tool calls ran.
    pub calls: usize,
    /// At most this much text goes back from one call (other than get_diff).
    max: usize,
    /// get_diff pages fit into this; it stays the same while a review runs, so page numbers keep their meaning.
    page_max: usize,
}

impl Tools {
    /// `max` = see [`result_max`].
    pub fn new(wt: &Path, files: Vec<FileDiff>, max: usize) -> Tools {
        Tools {
            root: wt.canonicalize().unwrap_or_else(|_| wt.to_path_buf()),
            files,
            last_range: None,
            calls: 0,
            max,
            page_max: max,
        }
    }

    /// The limit for the next results other than get_diff pages (the room left in the conversation).
    pub fn set_max(&mut self, max: usize) {
        self.max = max;
    }

    /// The most one get_diff page returns.
    pub fn page_max(&self) -> usize {
        self.page_max
    }

    /// A new page size for get_diff, after the model's context turned out smaller; the model is told the new page
    /// count with each page.
    pub fn set_page_max(&mut self, max: usize) {
        self.page_max = max;
        self.max = max;
    }

    /// The size of one `get_diff` page: what is left of `max` after the page header.
    fn page_size(&self) -> usize {
        self.page_max.saturating_sub(100).max(512)
    }

    /// For a file git showed no changes of ([`opaque_section`]): whether it is really binary (a binary extension, and
    /// binary content or gone from the checkout), so not seeing it is fine; anything else may be source code the PR
    /// hid from the diff.
    pub fn really_binary(&self, path: &str) -> bool {
        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
        if !path.contains('.') || !BINARY_EXT.contains(&ext.as_str()) {
            return false;
        }
        match self.inside(&self.root.join(path)) {
            Some(full) => fs::read(full).map_or(true, |b| binary(&b)),
            // deleted (or not a plain path): judged by the extension alone
            None => !self.root.join(path).exists(),
        }
    }

    /// The diff, one piece per changed file.
    pub fn files(&self) -> &[FileDiff] {
        &self.files
    }

    /// The `tools` field of the request.
    pub fn defs() -> Value {
        let f = |name: &str, desc: &str, props: Value, req: &[&str]| {
            json!({"type": "function", "function": {
                "name": name,
                "description": desc,
                "parameters": {"type": "object", "properties": props, "required": req},
            }})
        };
        json!([
            f(
                "get_diff",
                "The PR's diff for one changed file (paths as in diffstat.txt). Long diffs come in pages: ask for page 2, 3, … until the last one.",
                json!({"path": {"type": "string"}, "page": {"type": "integer", "description": "1-based, default 1"}}),
                &["path"],
            ),
            f(
                "read_file",
                "Read a file of the checkout (the PR's head commit), with line numbers. Path relative to the repository root.",
                json!({
                    "path": {"type": "string"},
                    "start_line": {"type": "integer", "description": "1-based, default 1"},
                    "max_lines": {"type": "integer", "description": "default and at most 400"}
                }),
                &["path"],
            ),
            f(
                "list_dir",
                "List a directory of the checkout. Path relative to the repository root; \".\" is the root.",
                json!({"path": {"type": "string"}}),
                &[],
            ),
            f(
                "search",
                "Search the checkout's text files for a string (or a regular expression with regex=true). Returns path:line: text, at most 100 matches.",
                json!({
                    "pattern": {"type": "string"},
                    "path": {"type": "string", "description": "directory or file to search in, default the root"},
                    "regex": {"type": "boolean"},
                    "ignore_case": {"type": "boolean"}
                }),
                &["pattern"],
            ),
        ])
    }

    /// Run one tool call. Errors are returned as text for the model, never raised.
    pub fn call(&mut self, name: &str, args: &str) -> String {
        self.calls += 1;
        self.last_range = None;
        let a: Value = if args.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str(args) {
                Ok(v @ Value::Object(_)) => v,
                _ => return "error: the arguments are not a JSON object".into(),
            }
        };
        let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
        let n = |k: &str, d: u64| a[k].as_u64().unwrap_or(d);
        let out = match name {
            "get_diff" => self.get_diff(&s("path"), n("page", 1) as usize),
            "read_file" => self.read_file(
                &s("path"),
                n("start_line", 1).max(1) as usize,
                (n("max_lines", READ_LINES as u64) as usize).clamp(1, READ_LINES),
            ),
            "list_dir" => self.list_dir(&s("path")),
            "search" => self.search(
                &s("pattern"),
                &s("path"),
                a["regex"].as_bool().unwrap_or(false),
                a["ignore_case"].as_bool().unwrap_or(false),
            ),
            other => Err(format!(
                "unknown tool {other:?}; the tools are get_diff, read_file, list_dir and search"
            )),
        };
        let max = if name == "get_diff" {
            self.page_max
        } else {
            self.max
        };
        cap(out.unwrap_or_else(|e| format!("error: {e}")), max)
    }

    /// A path of the checkout, refused when it is absolute, climbs out, names `.git`, uses a Windows stream (`a:b`)
    /// or resolves outside the root or into `.git` (a link, a junction, an 8.3 name such as `GIT~1`).
    fn resolve(&self, p: &str) -> Result<PathBuf, String> {
        let p = p.trim();
        let rel = Path::new(if p.is_empty() { "." } else { p });
        for c in rel.components() {
            match c {
                Component::CurDir => {}
                Component::Normal(x) => {
                    let x = x.to_string_lossy();
                    if x.eq_ignore_ascii_case(".git") {
                        return Err(".git is not readable".into());
                    }
                    if x.contains(':') {
                        return Err(format!("{p:?} is not a plain path"));
                    }
                }
                _ => {
                    return Err(format!(
                        "{p:?} is outside the checkout; use a path relative to the repository root"
                    ))
                }
            }
        }
        if !self.root.join(rel).exists() {
            return Err(format!("{p:?} does not exist"));
        }
        self.inside(&self.root.join(rel))
            .ok_or_else(|| format!("{p:?} is outside the checkout"))
    }

    /// `p` resolved, when it is in the checkout and not in its `.git`.
    fn inside(&self, p: &Path) -> Option<PathBuf> {
        let full = p.canonicalize().ok()?;
        let rel = full.strip_prefix(&self.root).ok()?;
        let git = rel.components().any(|c| match c {
            Component::Normal(x) => {
                let x = x.to_string_lossy();
                x.eq_ignore_ascii_case(".git") || x.contains(':')
            }
            _ => true,
        });
        (!git).then_some(full)
    }

    fn rel(&self, full: &Path) -> String {
        full.strip_prefix(&self.root)
            .unwrap_or(full)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn get_diff(&mut self, path: &str, page: usize) -> Result<String, String> {
        let want = path.trim().trim_start_matches("./").replace('\\', "/");
        let f = self.files.iter().find(|f| f.path == want).ok_or_else(|| {
            format!("{path:?} is not a changed file of this PR (see diffstat.txt)")
        })?;
        let ps = pages(&f.text, self.page_size());
        let page = page.max(1);
        let text = ps
            .get(page - 1)
            .ok_or_else(|| format!("{path:?} has {} page(s)", ps.len()))?;
        let start = text.as_ptr() as usize - f.text.as_ptr() as usize;
        self.last_range = Some((f.path.clone(), start, start + text.len()));
        let more = if page < ps.len() {
            format!(" Ask for page {} next.", page + 1)
        } else {
            String::new()
        };
        Ok(format!("[page {page}/{}.{more}]\n{text}", ps.len()))
    }

    fn read_file(&self, path: &str, start: usize, max: usize) -> Result<String, String> {
        let full = self.resolve(path)?;
        if full.is_dir() {
            return Err(format!("{path:?} is a directory; use list_dir"));
        }
        let bytes = fs::read(&full).map_err(|e| format!("cannot read {path:?}: {e}"))?;
        if binary(&bytes) {
            return Err(format!("{path:?} is a binary file"));
        }
        let text = String::from_utf8_lossy(&bytes);
        let total = text.lines().count();
        let mut out = String::new();
        let mut last = start.saturating_sub(1);
        let room = self.max.saturating_sub(200);
        for (i, l) in text.lines().enumerate().skip(start - 1).take(max) {
            let mut line = format!("{}\t{l}\n", i + 1);
            if out.len() + line.len() > room {
                if !out.is_empty() {
                    break;
                }
                // a single line longer than a whole result: its beginning, marked, so the reader moves on
                let mut end = room.saturating_sub(60);
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                line.truncate(end);
                line.push_str(&format!(" [line cut: {} bytes]\n", l.len()));
            }
            out.push_str(&line);
            last = i + 1;
        }
        if start > total.max(1) {
            return Err(format!("{path:?} has {total} lines"));
        }
        if last < total {
            out.push_str(&format!(
                "[lines {start}-{last} of {total}; ask with start_line={} for more]\n",
                last + 1
            ));
        }
        Ok(out)
    }

    fn list_dir(&self, path: &str) -> Result<String, String> {
        let full = self.resolve(path)?;
        if !full.is_dir() {
            return Err(format!("{path:?} is not a directory"));
        }
        let mut names: Vec<String> = fs::read_dir(&full)
            .map_err(|e| format!("cannot list {path:?}: {e}"))?
            .flatten()
            .filter(|e| self.inside(&e.path()).is_some())
            .map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                if e.path().is_dir() {
                    n + "/"
                } else {
                    n
                }
            })
            .collect();
        names.sort();
        let total = names.len();
        names.truncate(LIST_MAX);
        let mut out = names.join("\n");
        if total > LIST_MAX {
            out.push_str(&format!("\n[{LIST_MAX} of {total} entries shown]"));
        }
        if out.is_empty() {
            out = "(empty)".into();
        }
        Ok(out)
    }

    fn search(
        &self,
        pattern: &str,
        path: &str,
        regex: bool,
        ignore_case: bool,
    ) -> Result<String, String> {
        if pattern.is_empty() {
            return Err("empty pattern".into());
        }
        let src = if regex {
            pattern.to_string()
        } else {
            regex_lite::escape(pattern)
        };
        let src = if ignore_case {
            format!("(?i){src}")
        } else {
            src
        };
        let re =
            regex_lite::Regex::new(&src).map_err(|e| format!("bad regular expression: {e}"))?;
        let start = self.resolve(path)?;
        let mut stack = vec![start];
        let (mut hits, mut scanned) = (vec![], 0usize);
        let mut more = false;
        'walk: while let Some(p) = stack.pop() {
            if p.is_dir() {
                let Ok(rd) = fs::read_dir(&p) else { continue };
                // each entry checked again: a link or junction inside must not lead out of the checkout
                let mut kids: Vec<PathBuf> = rd
                    .flatten()
                    .filter_map(|e| self.inside(&e.path()).map(|_| e.path()))
                    .collect();
                kids.sort();
                kids.reverse();
                stack.extend(kids);
                continue;
            }
            scanned += 1;
            if scanned > SEARCH_FILES_MAX {
                more = true;
                break;
            }
            if fs::metadata(&p).map_or(true, |m| m.len() > SEARCH_FILE_BYTES) {
                continue;
            }
            let Ok(bytes) = fs::read(&p) else { continue };
            if binary(&bytes) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (i, l) in text.lines().enumerate() {
                if re.is_match(l) {
                    if hits.len() == SEARCH_MAX {
                        more = true;
                        break 'walk;
                    }
                    let l = l.trim();
                    let mut end = l.len().min(200);
                    while !l.is_char_boundary(end) {
                        end -= 1;
                    }
                    hits.push(format!("{}:{}: {}", self.rel(&p), i + 1, &l[..end]));
                }
            }
        }
        if hits.is_empty() {
            return Ok("no matches".into());
        }
        let mut out = hits.join("\n");
        if more {
            out.push_str("\n[more matches not shown; narrow the pattern or the path]");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n\
diff --git a/Cargo.lock b/Cargo.lock\n--- a/Cargo.lock\n+++ b/Cargo.lock\n@@ -1 +1 @@\n-x\n+y\n\
diff --git a/gone.rs b/gone.rs\ndeleted file mode 100644\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n\
diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n\
diff --git \"a/\\344\\270\\255.rs\" \"b/\\344\\270\\255.rs\"\n--- \"a/\\344\\270\\255.rs\"\n+++ \"b/\\344\\270\\255.rs\"\n@@ -1 +1 @@\n-a\n+b\n";

    #[test]
    fn the_diff_splits_into_files_with_their_paths() {
        let f = split_diff(PATCH);
        let paths: Vec<&str> = f.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            ["src/a.rs", "Cargo.lock", "gone.rs", "img.png", "中.rs"]
        );
        assert_eq!(f.iter().map(|f| f.text.len()).sum::<usize>(), PATCH.len());
        assert!(f[0].text.ends_with("+new\n"));
    }

    /// Regression (redteam-2): a symlink replaced by a large regular file comes out as two sections for one path; the
    /// small one fit into the message, the file counted as seen, and its new content was never shown.
    #[test]
    fn two_sections_for_one_path_are_one_file() {
        let patch = format!(
            "diff --git a/link b/link\ndeleted file mode 120000\n--- a/link\n+++ /dev/null\n@@ -1 +0,0 @@\n-target\n\\ No newline at end of file\n\
             diff --git a/link b/link\nnew file mode 100644\n--- /dev/null\n+++ b/link\n@@ -0,0 +1 @@\n+{}\n\
             diff --git a/z.rs b/z.rs\n--- a/z.rs\n+++ b/z.rs\n@@ -1 +1 @@\n-a\n+b\n",
            "x".repeat(5000)
        );
        let f = split_diff(&patch);
        let paths: Vec<&str> = f.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["link", "z.rs"]);
        assert!(f[0].text.contains("-target") && f[0].text.contains(&"x".repeat(5000)));
        let p = pack(&f, 1000);
        assert_eq!(p.included, ["z.rs"]);
        assert_eq!(p.left_out[0].0, "link");
    }

    #[test]
    fn whole_files_only_and_low_value_ones_last() {
        let files = vec![
            FileDiff {
                path: "big.rs".into(),
                text: "x".repeat(90),
            },
            FileDiff {
                path: "Cargo.lock".into(),
                text: "l".repeat(20),
            },
            FileDiff {
                path: "small.rs".into(),
                text: "s".repeat(30),
            },
            FileDiff {
                path: "ui/__snapshots__/a.snap".into(),
                text: "n".repeat(10),
            },
        ];
        let p = pack(&files, 70);
        // big.rs does not fit; small.rs after it still does; low-value ones fill what is left, in order
        assert_eq!(
            p.included,
            ["small.rs", "Cargo.lock", "ui/__snapshots__/a.snap"]
        );
        assert_eq!(p.left_out, [("big.rs".to_string(), 90)]);
        assert_eq!(
            p.text,
            format!("{}{}{}", "s".repeat(30), "l".repeat(20), "n".repeat(10))
        );
        assert!(is_low_value("web/dist/app.min.js") && is_low_value("go.sum"));
        assert!(!is_low_value("src/lock.rs") && !is_low_value("dist/app.js"));
    }

    fn checkout() -> (tempfile::TempDir, Tools) {
        let d = tempfile::tempdir().unwrap();
        let wt = d.path().join("wt");
        fs::create_dir_all(wt.join("src")).unwrap();
        fs::create_dir_all(wt.join(".git")).unwrap();
        fs::write(wt.join(".git/config"), "secret").unwrap();
        fs::write(d.path().join("outside.txt"), "outside").unwrap();
        let lines: String = (1..=1000).map(|i| format!("line {i}\n")).collect();
        fs::write(wt.join("src/a.rs"), &lines).unwrap();
        fs::write(wt.join("src/b.rs"), "fn needle() {}\nlet Needle = 1;\n").unwrap();
        fs::write(wt.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
        let big = (0..3000)
            .map(|i| format!("+added line {i}\n"))
            .collect::<String>();
        let files = vec![
            FileDiff {
                path: "src/a.rs".into(),
                text: format!("diff --git a/src/a.rs b/src/a.rs\n{big}"),
            },
            FileDiff {
                path: "src/b.rs".into(),
                text: "diff --git a/src/b.rs b/src/b.rs\n+x\n".into(),
            },
        ];
        let t = Tools::new(&wt, files, RESULT_MAX);
        (d, t)
    }

    #[test]
    fn paths_stay_inside_the_checkout() {
        let (d, mut t) = checkout();
        let outside = d.path().join("outside.txt").to_string_lossy().to_string();
        for p in [
            "../outside.txt",
            "src/../../outside.txt",
            outside.as_str(),
            "/etc/passwd",
            "C:\\Windows\\win.ini",
            ".git/config",
            ".GIT/config",
            "src/../.git/config",
        ] {
            let r = t.call("read_file", &json!({"path": p}).to_string());
            assert!(r.starts_with("error:"), "{p}: {r}");
            assert!(
                !r.contains("outside\n") && !r.contains("secret"),
                "{p}: {r}"
            );
        }
        let r = t.call("list_dir", "{}");
        assert!(r.contains("src/") && !r.contains(".git"), "{r}");
        let r = t.call("list_dir", &json!({"path": ".."}).to_string());
        assert!(r.starts_with("error:"), "{r}");
        let r = t.call("search", &json!({"pattern": "secret"}).to_string());
        assert_eq!(r, "no matches");
    }

    /// Regression (redteam-1): on Windows the 8.3 name `GIT~1` and the stream syntax `.git::$INDEX_ALLOCATION` read
    /// `.git`, and `search` followed a junction out of the checkout.
    #[cfg(windows)]
    #[test]
    fn windows_aliases_and_junctions_do_not_lead_out() {
        let (d, mut t) = checkout();
        for p in [
            "GIT~1/config",
            ".git::$INDEX_ALLOCATION/config",
            "src/b.rs:stream",
        ] {
            let r = t.call("read_file", &json!({"path": p}).to_string());
            assert!(r.starts_with("error:") && !r.contains("secret"), "{p}: {r}");
        }
        let r = t.call("list_dir", &json!({"path": "GIT~1"}).to_string());
        assert!(r.starts_with("error:"), "{r}");
        let r = t.call(
            "search",
            &json!({"pattern": "secret", "path": "GIT~1"}).to_string(),
        );
        assert!(r.starts_with("error:"), "{r}");
        let out = d.path().join("outdir");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("s.txt"), "TOPSECRET").unwrap();
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(d.path().join("wt").join("ext"))
            .arg(&out)
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "could not create a junction");
        assert_eq!(
            t.call("search", &json!({"pattern": "TOPSECRET"}).to_string()),
            "no matches"
        );
        assert!(t
            .call("read_file", &json!({"path": "ext/s.txt"}).to_string())
            .starts_with("error:"));
        assert!(!t.call("list_dir", "{}").contains("ext"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_checkout_is_refused() {
        let (d, mut t) = checkout();
        std::os::unix::fs::symlink(d.path().join("outside.txt"), d.path().join("wt/link")).unwrap();
        let r = t.call("read_file", &json!({"path": "link"}).to_string());
        assert!(
            r.starts_with("error:") && r.contains("outside the checkout"),
            "{r}"
        );
    }

    #[test]
    fn files_are_read_in_numbered_windows() {
        let (_d, mut t) = checkout();
        let r = t.call("read_file", &json!({"path": "src/a.rs"}).to_string());
        assert!(
            r.starts_with("1\tline 1\n") && r.contains("400\tline 400\n") && !r.contains("401\t")
        );
        assert!(r.contains("ask with start_line=401"), "{r}");
        let r = t.call(
            "read_file",
            &json!({"path": "./src/a.rs", "start_line": 999}).to_string(),
        );
        assert_eq!(r, "999\tline 999\n1000\tline 1000\n");
        assert!(t
            .call("read_file", &json!({"path": "blob.bin"}).to_string())
            .contains("binary"));
        assert!(t
            .call("read_file", &json!({"path": "src"}).to_string())
            .contains("directory"));
        assert!(t.call("read_file", "not json").starts_with("error:"));
        // a line longer than a whole result: its beginning, marked, and the next call moves on
        fs::write(
            t.root.join("long.txt"),
            format!(
                "a
{}
c
",
                "x".repeat(20_000)
            ),
        )
        .unwrap();
        let r = t.call(
            "read_file",
            &json!({"path": "long.txt", "start_line": 2}).to_string(),
        );
        assert!(
            r.starts_with("2	xxx")
                && r.contains("[line cut: 20000 bytes]")
                && r.len() <= RESULT_MAX,
            "{}",
            &r[r.len() - 200..]
        );
        assert!(
            r.ends_with(
                "3	c
"
            ),
            "{}",
            &r[r.len() - 200..]
        );
        assert!(t.call("rm", "{}").contains("unknown tool"));
    }

    #[test]
    fn search_finds_text_and_regexes() {
        let (_d, mut t) = checkout();
        let r = t.call("search", &json!({"pattern": "needle"}).to_string());
        assert_eq!(r, "src/b.rs:1: fn needle() {}");
        let r = t.call(
            "search",
            &json!({"pattern": "needle", "ignore_case": true, "path": "src"}).to_string(),
        );
        assert_eq!(r.lines().count(), 2);
        let r = t.call(
            "search",
            &json!({"pattern": "^line 99\\d$", "regex": true}).to_string(),
        );
        assert_eq!(r.lines().count(), 10, "{r}");
        let r = t.call("search", &json!({"pattern": "line"}).to_string());
        assert!(
            r.lines().count() == SEARCH_MAX + 1 && r.contains("more matches"),
            "{r}"
        );
        assert!(t
            .call(
                "search",
                &json!({"pattern": "(", "regex": true}).to_string()
            )
            .starts_with("error:"));
    }

    /// Regression (redteam-1): a diff line longer than a tool result was its own page, cut on the way to the model,
    /// and the file still counted as seen. Now long lines are paged too, and no page is ever cut.
    #[test]
    fn long_lines_are_paged_not_cut() {
        let d = tempfile::tempdir().unwrap();
        let line = format!(
            "+{}END_MARKER
",
            "é".repeat(20_000)
        );
        let text = format!(
            "diff --git a/src/data.rs b/src/data.rs
@@ -0,0 +1 @@
{line}"
        );
        for max in [2048, 4096, RESULT_MAX] {
            let mut t = Tools::new(
                d.path(),
                vec![FileDiff {
                    path: "src/data.rs".into(),
                    text: text.clone(),
                }],
                max,
            );
            let mut ranges = vec![];
            let mut got = String::new();
            let mut page = 1;
            loop {
                let r = t.call(
                    "get_diff",
                    &json!({"path": "src/data.rs", "page": page}).to_string(),
                );
                ranges.extend(t.last_range.clone());
                assert!(
                    r.len() <= max && !r.contains("output cut"),
                    "{max}: page {page} is {} bytes",
                    r.len()
                );
                let (head, body) = r
                    .split_once(
                        "]
",
                    )
                    .unwrap();
                got.push_str(body);
                if !head.contains("next") {
                    break;
                }
                page += 1;
            }
            assert_eq!(got, text, "{max}: the pages make up the whole diff");
            assert!(covered(t.files(), &ranges).contains("src/data.rs"));
        }
    }

    #[test]
    fn a_diff_counts_as_seen_once_every_page_was_fetched() {
        let (_d, mut t) = checkout();
        let mut ranges = vec![];
        let r = t.call("get_diff", &json!({"path": "src/a.rs"}).to_string());
        ranges.extend(t.last_range.clone());
        let n: usize = r[r.find('/').unwrap() + 1..r.find('.').unwrap()]
            .parse()
            .unwrap();
        assert!(n >= 3 && r.len() <= RESULT_MAX, "{n} pages");
        for page in 2..n {
            t.call(
                "get_diff",
                &json!({"path": "src/a.rs", "page": page}).to_string(),
            );
            ranges.extend(t.last_range.clone());
        }
        // the same page twice does not make up for a missing one
        ranges.push(ranges[0].clone());
        assert!(
            !covered(t.files(), &ranges).contains("src/a.rs"),
            "the last page is still missing"
        );
        let r = t.call(
            "get_diff",
            &json!({"path": "src/a.rs", "page": n}).to_string(),
        );
        assert!(!r.contains("next"));
        ranges.extend(t.last_range.clone());
        assert!(covered(t.files(), &ranges).contains("src/a.rs"));
        // a list_dir in between leaves no range behind
        t.call("list_dir", "{}");
        assert!(t.last_range.is_none());
        assert!(t
            .call("get_diff", &json!({"path": "nope.rs"}).to_string())
            .contains("not a changed file"));
        assert!(t
            .call(
                "get_diff",
                &json!({"path": "src/a.rs", "page": 99}).to_string()
            )
            .starts_with("error:"));
    }
}
