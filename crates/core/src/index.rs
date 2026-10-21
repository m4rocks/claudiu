//! A local index of a repository for the `claudiu` MCP tools: `repo_map`, `outline`, `find_symbol`, `related_tests`.
//!
//! `ignore` walks the files (respecting .gitignore), `tree-sitter` reads their symbols and identifiers. References
//! are by name, not by type resolution: a name that appears in a file counts as a use. That is wrong now and then
//! and cheap always, which is the trade the model wants for navigation.
//!
//! A refresh stats every file but parses only the ones whose modification time or size changed.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use ignore::WalkBuilder;
use serde_json::Value;
use tree_sitter::{Language, Node, Parser};

const MAX_FILE_BYTES: u64 = 1_000_000;
/// Only definitions this near the top of the tree are looked at; deeper is expression code.
const MAX_LEVEL: usize = 14;
const MAP_TOKENS: usize = 1_500;
const NAMES_PER_FILE: usize = 8;
const REF_FILES: usize = 15;
const REF_LINES: usize = 6;
/// A name defined in more files than this (`new`, `main`, ...) says nothing about which file matters.
const COMMON_DEFS: u32 = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
}

impl Lang {
    fn of(path: &Path) -> Option<Lang> {
        Some(match path.extension()?.to_str()? {
            "rs" => Lang::Rust,
            "py" => Lang::Python,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
            _ => return None,
        })
    }

    fn grammar(self) -> Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        }
    }
}

struct Symbol {
    name: String,
    kind: &'static str,
    line: u32,
    end: u32,
    depth: u8,
}

struct FileInfo {
    stamp: (Option<SystemTime>, u64),
    symbols: Vec<Symbol>,
    idents: HashSet<String>,
    is_test: bool,
}

/// What a node defines, and whether the definitions inside it belong to it (methods in a class).
fn classify(lang: Lang, n: Node) -> Option<(&'static str, bool)> {
    Some(match (lang, n.kind()) {
        (Lang::Rust, "function_item" | "function_signature_item") => ("fn", false),
        (Lang::Rust, "struct_item") => ("struct", false),
        (Lang::Rust, "enum_item") => ("enum", false),
        (Lang::Rust, "trait_item") => ("trait", true),
        (Lang::Rust, "impl_item") => ("impl", true),
        (Lang::Rust, "mod_item") => ("mod", true),
        (Lang::Rust, "const_item") => ("const", false),
        (Lang::Rust, "static_item") => ("static", false),
        (Lang::Rust, "type_item") => ("type", false),
        (Lang::Rust, "macro_definition") => ("macro", false),
        (Lang::Python, "function_definition") => ("def", false),
        (Lang::Python, "class_definition") => ("class", true),
        (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, kind) => match kind {
            "function_declaration" | "generator_function_declaration" => ("function", false),
            "class_declaration" | "abstract_class_declaration" => ("class", true),
            "method_definition" => ("method", false),
            "interface_declaration" => ("interface", false),
            "type_alias_declaration" => ("type", false),
            "enum_declaration" => ("enum", false),
            "variable_declarator" if n.child_by_field_name("value").is_some_and(|v| matches!(v.kind(), "arrow_function" | "function_expression" | "function")) => ("function", false),
            _ => return None,
        },
        _ => return None,
    })
}

fn name_of(lang: Lang, n: Node, src: &[u8]) -> Option<String> {
    let field = if lang == Lang::Rust && n.kind() == "impl_item" { "type" } else { "name" };
    let text = n.child_by_field_name(field)?.utf8_text(src).ok()?;
    let text = text.split('<').next().unwrap_or(text);
    let name = text.rsplit("::").next().unwrap_or(text).trim();
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')).then(|| name.to_string())
}

fn symbols(lang: Lang, n: Node, src: &[u8], depth: u8, level: usize, out: &mut Vec<Symbol>) {
    if level > MAX_LEVEL {
        return;
    }
    let mut cursor = n.walk();
    for child in n.children(&mut cursor) {
        match classify(lang, child) {
            Some((kind, container)) => {
                let named = name_of(lang, child, src);
                if let Some(name) = &named {
                    out.push(Symbol { name: name.clone(), kind, line: child.start_position().row as u32 + 1, end: child.end_position().row as u32 + 1, depth });
                }
                if container {
                    symbols(lang, child, src, depth + u8::from(named.is_some()), level + 1, out);
                }
            }
            None => symbols(lang, child, src, depth, level + 1, out),
        }
    }
}

fn identifiers(root: Node, src: &[u8]) -> HashSet<String> {
    let mut found = HashSet::new();
    let mut cursor = root.walk();
    loop {
        let n = cursor.node();
        if n.child_count() == 0
            && n.kind().ends_with("identifier")
            && let Ok(text) = n.utf8_text(src)
        {
            found.insert(text.to_string());
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return found;
            }
        }
    }
}

fn is_test_path(rel: &str) -> bool {
    let mut parts = rel.split('/').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_some() {
            if matches!(part, "test" | "tests" | "__tests__" | "spec" | "specs") {
                return true;
            }
        } else {
            let stem = part.split('.').next().unwrap_or(part);
            return stem.starts_with("test_") || stem.ends_with("_test") || stem == "tests" || part.contains(".test.") || part.contains(".spec.");
        }
    }
    false
}

fn parse(lang: Lang, rel: &str, bytes: &[u8], stamp: (Option<SystemTime>, u64)) -> Option<FileInfo> {
    let mut parser = Parser::new();
    parser.set_language(&lang.grammar()).ok()?;
    let tree = parser.parse(bytes, None)?;
    let mut syms = Vec::new();
    symbols(lang, tree.root_node(), bytes, 0, 0, &mut syms);
    Some(FileInfo { stamp, symbols: syms, idents: identifiers(tree.root_node(), bytes), is_test: is_test_path(rel) })
}

fn slash(path: &Path) -> String {
    path.components().filter_map(|c| if let Component::Normal(s) = c { s.to_str() } else { None }).collect::<Vec<_>>().join("/")
}

/// Whether `word` occurs in `line` as a whole identifier.
fn has_word(line: &str, word: &str) -> bool {
    let is_id = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    line.match_indices(word).any(|(at, _)| {
        !line[..at].chars().next_back().is_some_and(is_id) && !line[at + word.len()..].chars().next().is_some_and(is_id)
    })
}

pub struct Index {
    root: PathBuf,
    files: BTreeMap<String, FileInfo>,
    parses: u64,
    /// For each name: how many files mention it, and how many define it. Rebuilt after any change.
    stats: Option<(HashMap<String, u32>, HashMap<String, u32>)>,
}

impl Index {
    pub fn new(root: PathBuf) -> Index {
        Index { root, files: BTreeMap::new(), parses: 0, stats: None }
    }

    pub fn parses(&self) -> u64 {
        self.parses
    }

    /// Brings the index up to date: stat everything, parse what changed, forget what is gone.
    pub fn refresh(&mut self) {
        let mut seen = HashSet::new();
        let mut todo = Vec::new();
        let walker = WalkBuilder::new(&self.root)
            .require_git(false)
            .filter_entry(|e| {
                let name = e.file_name().to_str().unwrap_or("");
                // Build output without a .gitignore entry would swamp the index.
                !(name == "node_modules" || (name == "target" && e.path().parent().is_some_and(|p| p.join("Cargo.toml").exists())))
            })
            .build();
        for entry in walker.flatten() {
            let path = entry.path();
            let Some(lang) = Lang::of(path).filter(|_| entry.file_type().is_some_and(|t| t.is_file())) else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let Ok(rel) = path.strip_prefix(&self.root) else { continue };
            let rel = slash(rel);
            let stamp = (meta.modified().ok(), meta.len());
            if self.files.get(&rel).is_none_or(|f| f.stamp != stamp) {
                todo.push((rel.clone(), path.to_path_buf(), lang, stamp));
            }
            seen.insert(rel);
        }
        let before = self.files.len();
        self.files.retain(|rel, _| seen.contains(rel));
        let changed = todo.len() + (before - self.files.len());

        let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).min(todo.len().max(1));
        let size = todo.len().div_ceil(threads).max(1);
        let parsed: Vec<(String, FileInfo)> = std::thread::scope(|s| {
            let jobs: Vec<_> = todo
                .chunks(size)
                .map(|chunk| {
                    s.spawn(move || {
                        chunk
                            .iter()
                            .filter_map(|(rel, path, lang, stamp)| {
                                let bytes = std::fs::read(path).ok()?;
                                Some((rel.clone(), parse(*lang, rel, &bytes, *stamp)?))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            jobs.into_iter().flat_map(|j| j.join().unwrap_or_default()).collect()
        });
        self.parses += parsed.len() as u64;
        for (rel, info) in parsed {
            self.files.insert(rel, info);
        }
        if changed > 0 {
            self.stats = None;
        }
    }

    fn stats(&mut self) -> &(HashMap<String, u32>, HashMap<String, u32>) {
        let files = &self.files;
        self.stats.get_or_insert_with(|| {
            let (mut mentions, mut defs) = (HashMap::new(), HashMap::new());
            for f in files.values() {
                for id in &f.idents {
                    *mentions.entry(id.clone()).or_insert(0) += 1;
                }
                for name in f.symbols.iter().map(|s| s.name.as_str()).collect::<HashSet<_>>() {
                    *defs.entry(name.to_string()).or_insert(0) += 1;
                }
            }
            (mentions, defs)
        })
    }

    /// The path as the index knows it: relative to the root, slashes, nothing that climbs out.
    fn key(&self, path: &str) -> Option<String> {
        let path = Path::new(path);
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        if rel.components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_) | Component::RootDir)) {
            return None;
        }
        Some(slash(rel))
    }

    fn repo_map(&mut self, prefix: &str, tokens: usize) -> String {
        let budget = tokens.max(200) * 4;
        let (mentions, defs) = self.stats().clone();
        let mut rows: Vec<(u32, &str, String)> = Vec::new();
        for (path, f) in &self.files {
            let under = prefix.is_empty() || path == prefix || path.strip_prefix(prefix).is_some_and(|r| r.starts_with('/'));
            if !under || f.symbols.is_empty() {
                continue;
            }
            let mut seen = HashSet::new();
            let mut names: Vec<(u32, &str)> = f
                .symbols
                .iter()
                .filter(|s| seen.insert(s.name.as_str()))
                .map(|s| {
                    let uses = mentions.get(&s.name).copied().unwrap_or(1).saturating_sub(1);
                    (if defs.get(&s.name).copied().unwrap_or(0) > COMMON_DEFS { 0 } else { uses.min(20) }, s.name.as_str())
                })
                .collect();
            let score = names.iter().map(|n| n.0).sum();
            names.sort_by_key(|n| std::cmp::Reverse(n.0));
            let more = names.len().saturating_sub(NAMES_PER_FILE);
            let mut list = names.iter().take(NAMES_PER_FILE).map(|n| n.1).collect::<Vec<_>>().join(", ");
            if more > 0 {
                list.push_str(&format!(" +{more}"));
            }
            rows.push((score, path.as_str(), list));
        }
        if rows.is_empty() {
            return "No indexed source files here (Rust, Python, TypeScript and JavaScript are read).".into();
        }
        rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        let mut out = String::new();
        let mut shown = 0;
        for (_, path, list) in &rows {
            let line = format!("{path}  {list}\n");
            if out.len() + line.len() > budget.saturating_sub(40) {
                break;
            }
            out.push_str(&line);
            shown += 1;
        }
        if shown < rows.len() {
            out.push_str(&format!("... {} more files; narrow with path.", rows.len() - shown));
        }
        out
    }

    fn outline(&self, path: &str) -> String {
        let Some((key, file)) = self.key(path).and_then(|k| self.files.get(&k).map(|f| (k, f))) else {
            return format!("{path} is not in the index (unknown, ignored, or not a Rust, Python, TypeScript or JavaScript file).");
        };
        let mut out = format!("{key}\n");
        for s in &file.symbols {
            let span = if s.end > s.line { format!("{}-{}", s.line, s.end) } else { s.line.to_string() };
            out.push_str(&format!("{}{} {} {span}\n", "  ".repeat(s.depth as usize + 1), s.kind, s.name));
        }
        out
    }

    fn source_line(&self, path: &str, line: u32) -> String {
        let text = std::fs::read_to_string(self.root.join(path)).unwrap_or_default();
        let found = text.lines().nth(line as usize - 1).unwrap_or("").trim();
        found.chars().take(110).collect()
    }

    fn find_symbol(&self, name: &str) -> String {
        let mut out = String::new();
        let mut def_lines: HashSet<(&str, u32)> = HashSet::new();
        let mut defs = 0;
        for (path, f) in &self.files {
            for s in f.symbols.iter().filter(|s| s.name == name) {
                def_lines.insert((path.as_str(), s.line));
                if defs < 10 {
                    out.push_str(&format!("{path}:{}  {}\n", s.line, self.source_line(path, s.line)));
                }
                defs += 1;
            }
        }
        if defs > 10 {
            out.push_str(&format!("... {} more definitions\n", defs - 10));
        }

        let users: Vec<&str> = self.files.iter().filter(|(_, f)| f.idents.contains(name)).map(|(p, _)| p.as_str()).collect();
        let mut lines = String::new();
        let mut files_shown = 0;
        for path in &users {
            let text = std::fs::read_to_string(self.root.join(path)).unwrap_or_default();
            let at: Vec<u32> = text
                .lines()
                .enumerate()
                .filter(|(i, l)| has_word(l, name) && !def_lines.contains(&(*path, *i as u32 + 1)))
                .map(|(i, _)| i as u32 + 1)
                .collect();
            if at.is_empty() {
                continue;
            }
            if files_shown == REF_FILES {
                files_shown += 1;
                break;
            }
            files_shown += 1;
            let more = at.len().saturating_sub(REF_LINES);
            let shown = at.iter().take(REF_LINES).map(u32::to_string).collect::<Vec<_>>().join(",");
            lines.push_str(&format!("{path}:{shown}{}\n", if more > 0 { format!(" +{more}") } else { String::new() }));
        }
        if defs == 0 && files_shown == 0 {
            return format!("No symbol named {name} in the index.");
        }
        if files_shown > 0 {
            if files_shown > REF_FILES {
                lines.push_str(&format!("... and more files ({} mention it in all)\n", users.len()));
            }
            out.push_str(if defs == 0 { "No definition found; mentioned in:\n" } else { "used in:\n" });
            out.push_str(&lines);
        }
        out
    }

    fn related_tests(&mut self, path: &str) -> String {
        let Some(key) = self.key(path).filter(|k| self.files.contains_key(k)) else {
            return format!("{path} is not in the index.");
        };
        let (_, defs) = self.stats().clone();
        let own = &self.files[&key];
        let names: HashSet<&str> = own.symbols.iter().map(|s| s.name.as_str()).filter(|n| n.len() >= 3 && defs.get(*n).copied().unwrap_or(0) <= COMMON_DEFS).collect();
        let stem = key.rsplit('/').next().unwrap_or(&key).split('.').next().unwrap_or("").to_string();
        let mut hits: Vec<(usize, &str, Vec<&str>)> = Vec::new();
        for (p, f) in &self.files {
            if !f.is_test || *p == key {
                continue;
            }
            let mut shared: Vec<&str> = names.iter().copied().filter(|n| f.idents.contains(*n)).collect();
            shared.sort_unstable();
            if shared.is_empty() {
                continue;
            }
            let bonus = if !stem.is_empty() && p.contains(&stem) { 100 } else { 0 };
            hits.push((shared.len() + bonus, p.as_str(), shared));
        }
        if hits.is_empty() {
            return format!("No tests found that use the symbols of {key}.");
        }
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        let mut out = String::new();
        for (_, p, shared) in hits.iter().take(10) {
            out.push_str(&format!("{p}  uses {}\n", shared.iter().take(4).copied().collect::<Vec<_>>().join(", ")));
        }
        out
    }
}

/// One index per project folder, shared by every session in it.
#[derive(Default)]
pub struct Indexes(Mutex<HashMap<PathBuf, Arc<Mutex<Index>>>>);

impl Indexes {
    fn of(&self, root: &Path) -> Arc<Mutex<Index>> {
        self.0.lock().unwrap().entry(root.to_path_buf()).or_insert_with(|| Arc::new(Mutex::new(Index::new(root.to_path_buf())))).clone()
    }

    pub fn parses(&self, root: &Path) -> u64 {
        self.0.lock().unwrap().get(root).map_or(0, |i| i.lock().unwrap().parses())
    }

    /// Runs one of the four tools against the (refreshed) index of `root`; `None` if `tool` is not one of them.
    pub fn call(&self, root: &Path, tool: &str, args: &Value) -> Option<String> {
        if !matches!(tool, "repo_map" | "outline" | "find_symbol" | "related_tests") {
            return None;
        }
        let index = self.of(root);
        let mut index = index.lock().unwrap();
        index.refresh();
        let text = |key: &str| args[key].as_str().unwrap_or("").to_string();
        Some(match tool {
            "repo_map" => {
                let path = index.key(&text("path")).unwrap_or_default();
                index.repo_map(&path, args["tokens"].as_u64().map_or(MAP_TOKENS, |t| t as usize))
            }
            "outline" => index.outline(&text("path")),
            "find_symbol" => index.find_symbol(&text("name")),
            _ => index.related_tests(&text("path")),
        })
    }
}

pub fn tools() -> Value {
    serde_json::json!([
        {
            "name": "repo_map",
            "description": "Files of the repo with their main symbols, most used first. path narrows it; tokens sets the size.",
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}, "tokens": {"type": "integer"}}}
        },
        {
            "name": "outline",
            "description": "Symbols of one file with line ranges. Use it before reading a big file.",
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}
        },
        {
            "name": "find_symbol",
            "description": "Where a name is defined and which files and lines mention it (matched by name).",
            "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}
        },
        {
            "name": "related_tests",
            "description": "Test files that use the symbols a file defines.",
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}
        }
    ])
}
