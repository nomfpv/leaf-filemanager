//! What a job takes and what it leaves out: include and exclude globs, hidden items, `.gitignore` files. The same filter is used
//! on both sides, and a path left out on EITHER side is left out of the plan altogether (never copied, never removed): a file one
//! side ignores is never "missing" there.

use std::path::Path;

/// A glob: `*` is any run of characters but `/`, `**` any run including `/`, `?` one character. Matched without regard to case.
/// A pattern without `/` is matched against the name alone, one with `/` against the path from the job's root.
#[derive(Debug, Clone)]
pub struct Glob {
    pat: Vec<char>,
    whole_path: bool,
}

impl Glob {
    pub fn new(p: &str) -> Option<Glob> {
        let p = p.trim().trim_start_matches('/').trim_end_matches('/');
        if p.is_empty() {
            return None;
        }
        Some(Glob { pat: p.to_lowercase().chars().collect(), whole_path: p.contains('/') })
    }

    pub fn matches(&self, rel: &str, name: &str) -> bool {
        let s: Vec<char> = if self.whole_path { rel.to_lowercase().chars().collect() } else { name.to_lowercase().chars().collect() };
        glob_match(&self.pat, &s)
    }
}

fn glob_match(p: &[char], s: &[char]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some('*') if p.get(1) == Some(&'*') => {
            // `**/` also matches nothing at all ("docs/**/x" takes "docs/x")
            let rest = if p.get(2) == Some(&'/') { &p[3..] } else { &p[2..] };
            if p.get(2) == Some(&'/') && glob_match(rest, s) {
                return true;
            }
            (0..=s.len()).any(|i| glob_match(rest, &s[i..]))
        }
        Some('*') => {
            let rest = &p[1..];
            for i in 0..=s.len() {
                if glob_match(rest, &s[i..]) {
                    return true;
                }
                if i < s.len() && s[i] == '/' {
                    break;
                }
            }
            false
        }
        Some('?') => s.first().is_some_and(|c| *c != '/') && glob_match(&p[1..], &s[1..]),
        Some(c) => s.first() == Some(c) && glob_match(&p[1..], &s[1..]),
    }
}

/// The filter of one job.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    include: Vec<Glob>,
    exclude: Vec<Glob>,
    skip_hidden: bool,
    pub gitignore: bool,
}

impl Filter {
    pub fn of(job: &crate::Job) -> Filter {
        Filter {
            include: job.include.iter().filter_map(|g| Glob::new(g)).collect(),
            exclude: job.exclude.iter().filter_map(|g| Glob::new(g)).collect(),
            skip_hidden: job.skip_hidden,
            gitignore: job.gitignore,
        }
    }

    /// Left out by the globs or as hidden? `rel` is the path from the root, `/` between the parts. Include globs only choose
    /// files: folders are always looked into (unless an exclude names them).
    pub fn leaves_out(&self, rel: &str, name: &str, is_dir: bool, hidden: bool) -> bool {
        if self.skip_hidden && hidden {
            return true;
        }
        if self.gitignore && is_dir && name == ".git" {
            return true;
        }
        if self.exclude.iter().any(|g| g.matches(rel, name)) {
            return true;
        }
        !is_dir && !self.include.is_empty() && !self.include.iter().any(|g| g.matches(rel, name))
    }
}

/// The `.gitignore` files met on the way down, deepest last.
#[derive(Default)]
pub struct GitStack {
    layers: Vec<(String, ignore::gitignore::Gitignore)>,
}

impl GitStack {
    /// Adds the `.gitignore` of the folder `dir_rel` (`""` for the root).
    pub fn push(&mut self, dir_rel: &str, text: &str) {
        let mut b = ignore::gitignore::GitignoreBuilder::new(Path::new("/"));
        for line in text.lines() {
            let _ = b.add_line(None, line);
        }
        if let Ok(g) = b.build() {
            self.layers.push((dir_rel.to_string(), g));
        }
    }

    /// Forgets the layers that are not above `dir_rel` any more.
    pub fn leave_to(&mut self, dir_rel: &str) {
        self.layers.retain(|(d, _)| d.is_empty() || dir_rel == d || dir_rel.starts_with(&format!("{d}/")));
    }

    pub fn ignores(&self, rel: &str, is_dir: bool) -> bool {
        for (d, g) in self.layers.iter().rev() {
            let sub = if d.is_empty() { rel } else { rel.strip_prefix(d.as_str()).and_then(|r| r.strip_prefix('/')).unwrap_or(rel) };
            let m = g.matched(Path::new(sub), is_dir);
            if m.is_ignore() {
                return true;
            }
            if m.is_whitelist() {
                return false;
            }
        }
        false
    }
}
