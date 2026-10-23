//! What the GitHub pane shows, parsed out of `gh` / GraphQL answers by hand: the unions in a timeline have dozens of
//! event types, and anything unknown must simply be skipped rather than fail the whole page. No GPUI in here.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Issue,
    Pull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub id: String,
    pub name: String,
    /// 0xRRGGBB
    pub color: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct User {
    pub id: String,
    pub login: String,
}

/// One row of the list, and the header facts of a detail page.
#[derive(Clone, Debug)]
pub struct Item {
    pub kind: Kind,
    pub id: String,
    pub number: u32,
    pub title: String,
    pub state: State,
    /// Closed as "not planned" (issues only).
    pub not_planned: bool,
    pub draft: bool,
    pub url: String,
    pub author: String,
    pub created: i64,
    pub updated: i64,
    pub comments: u32,
    pub labels: Vec<Label>,
    pub assignees: Vec<User>,
    pub milestone: Option<(String, String)>,
    /// APPROVED / CHANGES_REQUESTED / REVIEW_REQUIRED
    pub review: Option<String>,
    /// SUCCESS / FAILURE / ERROR / PENDING / EXPECTED
    pub checks: Option<String>,
    pub head: String,
    pub base: String,
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub items: Vec<Item>,
    pub total: usize,
    pub open: usize,
    pub closed: usize,
    pub next: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Reaction {
    pub content: String,
    pub count: u32,
    pub mine: bool,
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub assoc: String,
    pub body: String,
    pub created: i64,
    pub edited: bool,
    pub can_edit: bool,
    pub can_delete: bool,
    pub reactions: Vec<Reaction>,
}

#[derive(Clone, Debug)]
pub struct ReviewComment {
    pub author: String,
    pub body: String,
    pub path: String,
    pub line: Option<u32>,
    pub hunk: String,
    pub outdated: bool,
}

#[derive(Clone, Debug)]
pub struct Review {
    pub author: String,
    /// APPROVED / CHANGES_REQUESTED / COMMENTED / DISMISSED / PENDING
    pub state: String,
    pub body: String,
    pub created: i64,
    pub comments: Vec<ReviewComment>,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub actor: String,
    pub what: String,
    pub label: Option<Label>,
    pub created: i64,
}

#[derive(Clone, Debug)]
pub struct CommitEntry {
    pub short: String,
    pub headline: String,
    pub author: String,
    pub created: i64,
}

#[derive(Clone, Debug)]
pub enum Entry {
    Comment(Comment),
    Event(Event),
    Review(Review),
    Commit(CommitEntry),
}

#[derive(Clone, Debug)]
pub struct Check {
    pub name: String,
    /// SUCCESS / FAILURE / PENDING / NEUTRAL / SKIPPED
    pub result: String,
    pub url: String,
}

#[derive(Clone, Debug, Default)]
pub struct Pr {
    pub merged_by: Option<String>,
    /// MERGEABLE / CONFLICTING / UNKNOWN
    pub mergeable: String,
    /// CLEAN / BLOCKED / BEHIND / DIRTY / UNSTABLE / HAS_HOOKS / DRAFT / UNKNOWN
    pub merge_state: String,
    pub head_oid: String,
    pub head_repo: String,
    pub cross_repo: bool,
    pub additions: u32,
    pub deletions: u32,
    pub files: u32,
    pub commits: u32,
    pub requested: Vec<String>,
    pub reviews: Vec<(String, String)>,
    pub checks: Vec<Check>,
    pub auto_merge: Option<(String, String)>,
    pub can_admin_merge: bool,
    pub can_auto_merge: bool,
}

#[derive(Clone, Debug)]
pub struct Detail {
    pub item: Item,
    pub body: String,
    pub assoc: String,
    pub edited: bool,
    pub locked: bool,
    pub mine: bool,
    pub can_update: bool,
    pub reactions: Vec<Reaction>,
    pub timeline: Vec<Entry>,
    pub next: Option<String>,
    pub pr: Option<Pr>,
}

#[derive(Clone, Debug, Default)]
pub struct RepoInfo {
    pub id: String,
    pub private: bool,
    pub viewer: String,
    /// ADMIN / MAINTAIN / WRITE / TRIAGE / READ
    pub permission: String,
    pub issues_enabled: bool,
    pub allow_merge: bool,
    pub allow_squash: bool,
    pub allow_rebase: bool,
    pub delete_branch_on_merge: bool,
    pub allow_auto_merge: bool,
    pub default_branch: String,
}

impl RepoInfo {
    /// Triage and up can label and assign; write and up can merge.
    pub fn can_triage(&self) -> bool {
        matches!(self.permission.as_str(), "ADMIN" | "MAINTAIN" | "WRITE" | "TRIAGE")
    }
    pub fn can_write(&self) -> bool {
        matches!(self.permission.as_str(), "ADMIN" | "MAINTAIN" | "WRITE")
    }
}

// ----------------------------------------------------------------------------------------------------- parsing

pub fn parse_ts(s: &str) -> i64 {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).map(|d| d.unix_timestamp()).unwrap_or(0)
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn nodes(v: &Value) -> impl Iterator<Item = &Value> {
    v["nodes"].as_array().into_iter().flatten().filter(|n| !n.is_null())
}

/// "ghost" is what GitHub itself calls a deleted account.
fn login(v: &Value) -> String {
    v["login"].as_str().unwrap_or("ghost").to_string()
}

fn parse_color(s: &str) -> u32 {
    u32::from_str_radix(s.trim_start_matches('#'), 16).unwrap_or(0x888888)
}

fn parse_labels(v: &Value) -> Vec<Label> {
    nodes(v).map(|l| Label { id: text(&l["id"]), name: text(&l["name"]), color: parse_color(l["color"].as_str().unwrap_or("")) }).collect()
}

fn parse_users(v: &Value) -> Vec<User> {
    nodes(v).map(|u| User { id: text(&u["id"]), login: login(u) }).collect()
}

fn parse_reactions(v: &Value) -> Vec<Reaction> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|g| Reaction { content: text(&g["content"]), count: g["reactors"]["totalCount"].as_u64().unwrap_or(0) as u32, mine: g["viewerHasReacted"].as_bool().unwrap_or(false) })
        .collect()
}

pub fn parse_item(v: &Value) -> Option<Item> {
    let kind = match v["__typename"].as_str()? {
        "Issue" => Kind::Issue,
        "PullRequest" => Kind::Pull,
        _ => return None,
    };
    let merged = v["merged"].as_bool().unwrap_or(false);
    let state = match v["state"].as_str()? {
        "OPEN" => State::Open,
        _ if merged || v["state"] == "MERGED" => State::Merged,
        _ => State::Closed,
    };
    Some(Item {
        kind,
        id: text(&v["id"]),
        number: v["number"].as_u64()? as u32,
        title: text(&v["title"]),
        state,
        not_planned: v["stateReason"] == "NOT_PLANNED",
        draft: v["isDraft"].as_bool().unwrap_or(false),
        url: text(&v["url"]),
        author: login(&v["author"]),
        created: parse_ts(v["createdAt"].as_str().unwrap_or("")),
        updated: parse_ts(v["updatedAt"].as_str().unwrap_or("")),
        comments: v["comments"]["totalCount"].as_u64().unwrap_or(0) as u32,
        labels: parse_labels(&v["labels"]),
        assignees: parse_users(&v["assignees"]),
        milestone: v["milestone"].as_object().map(|m| (text(&m.get("id").cloned().unwrap_or_default()), text(&m["title"]))),
        review: v["reviewDecision"].as_str().map(str::to_string),
        checks: nodes(&v["commits"]).next().and_then(|c| c["commit"]["statusCheckRollup"]["state"].as_str()).map(str::to_string),
        head: text(&v["headRefName"]),
        base: text(&v["baseRefName"]),
    })
}

pub fn parse_page(data: &Value) -> Page {
    let s = &data["search"];
    Page {
        items: s["nodes"].as_array().into_iter().flatten().filter_map(parse_item).collect(),
        total: s["issueCount"].as_u64().unwrap_or(0) as usize,
        open: data["open"]["issueCount"].as_u64().unwrap_or(0) as usize,
        closed: data["closed"]["issueCount"].as_u64().unwrap_or(0) as usize,
        next: s["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false).then(|| text(&s["pageInfo"]["endCursor"])),
    }
}

pub fn parse_repo(data: &Value) -> Option<RepoInfo> {
    let r = &data["repository"];
    if r.is_null() {
        return None;
    }
    let b = |k: &str| r[k].as_bool().unwrap_or(false);
    Some(RepoInfo {
        id: text(&r["id"]),
        private: b("isPrivate"),
        viewer: text(&data["viewer"]["login"]),
        permission: text(&r["viewerPermission"]),
        issues_enabled: b("hasIssuesEnabled"),
        allow_merge: b("mergeCommitAllowed"),
        allow_squash: b("squashMergeAllowed"),
        allow_rebase: b("rebaseMergeAllowed"),
        delete_branch_on_merge: b("deleteBranchOnMerge"),
        allow_auto_merge: b("autoMergeAllowed"),
        default_branch: text(&r["defaultBranchRef"]["name"]),
    })
}

fn reviewer(v: &Value) -> String {
    v["login"].as_str().or_else(|| v["name"].as_str()).unwrap_or("someone").to_string()
}

fn event(v: &Value, what: String, label: Option<Label>) -> Entry {
    Entry::Event(Event { actor: login(&v["actor"]), what, label, created: parse_ts(v["createdAt"].as_str().unwrap_or("")) })
}

pub fn parse_timeline(nodes_v: &Value) -> Vec<Entry> {
    let mut out = vec![];
    for n in nodes(nodes_v) {
        let label = || Label { id: String::new(), name: text(&n["label"]["name"]), color: parse_color(n["label"]["color"].as_str().unwrap_or("")) };
        let entry = match n["__typename"].as_str().unwrap_or("") {
            "IssueComment" => Entry::Comment(Comment {
                id: text(&n["id"]),
                author: login(&n["author"]),
                assoc: text(&n["authorAssociation"]),
                body: text(&n["body"]),
                created: parse_ts(n["createdAt"].as_str().unwrap_or("")),
                edited: !n["lastEditedAt"].is_null(),
                can_edit: n["viewerCanUpdate"].as_bool().unwrap_or(false),
                can_delete: n["viewerCanDelete"].as_bool().unwrap_or(false),
                reactions: parse_reactions(&n["reactionGroups"]),
            }),
            "LabeledEvent" => event(n, "added".into(), Some(label())),
            "UnlabeledEvent" => event(n, "removed".into(), Some(label())),
            "AssignedEvent" => {
                let who = login(&n["assignee"]);
                if who == login(&n["actor"]) { event(n, "self-assigned this".into(), None) } else { event(n, format!("assigned {who}"), None) }
            }
            "UnassignedEvent" => event(n, format!("unassigned {}", login(&n["assignee"])), None),
            "ClosedEvent" => event(n, "closed this".into(), None),
            "ReopenedEvent" => event(n, "reopened this".into(), None),
            "RenamedTitleEvent" => event(n, format!("renamed this from “{}” to “{}”", text(&n["previousTitle"]), text(&n["currentTitle"])), None),
            "MilestonedEvent" => event(n, format!("added this to the {} milestone", text(&n["milestoneTitle"])), None),
            "DemilestonedEvent" => event(n, format!("removed this from the {} milestone", text(&n["milestoneTitle"])), None),
            "LockedEvent" => event(n, "locked the conversation".into(), None),
            "UnlockedEvent" => event(n, "unlocked the conversation".into(), None),
            "CrossReferencedEvent" => {
                let s = &n["source"];
                event(n, format!("mentioned this in {}#{} {}", text(&s["repository"]["nameWithOwner"]), s["number"], text(&s["title"])), None)
            }
            "MergedEvent" => event(n, format!("merged this into {}", text(&n["mergeRefName"])), None),
            "ReadyForReviewEvent" => event(n, "marked this as ready for review".into(), None),
            "ConvertToDraftEvent" => event(n, "marked this as a draft".into(), None),
            "ReviewRequestedEvent" => event(n, format!("requested a review from {}", reviewer(&n["requestedReviewer"])), None),
            "ReviewRequestRemovedEvent" => event(n, format!("removed the review request for {}", reviewer(&n["requestedReviewer"])), None),
            "HeadRefDeletedEvent" => event(n, format!("deleted the {} branch", text(&n["headRefName"])), None),
            "HeadRefForcePushedEvent" => event(n, "force-pushed the branch".into(), None),
            "ReviewDismissedEvent" => event(n, "dismissed a review".into(), None),
            "PullRequestCommit" => {
                let c = &n["commit"];
                Entry::Commit(CommitEntry {
                    short: text(&c["abbreviatedOid"]),
                    headline: text(&c["messageHeadline"]),
                    author: c["author"]["user"]["login"].as_str().or_else(|| c["author"]["name"].as_str()).unwrap_or("someone").to_string(),
                    created: parse_ts(c["committedDate"].as_str().unwrap_or("")),
                })
            }
            "PullRequestReview" => Entry::Review(Review {
                author: login(&n["author"]),
                state: text(&n["state"]),
                body: text(&n["body"]),
                created: parse_ts(n["createdAt"].as_str().unwrap_or("")),
                comments: nodes(&n["comments"])
                    .map(|c| ReviewComment {
                        author: login(&c["author"]),
                        body: text(&c["body"]),
                        path: text(&c["path"]),
                        line: c["line"].as_u64().or_else(|| c["originalLine"].as_u64()).map(|l| l as u32),
                        hunk: text(&c["diffHunk"]),
                        outdated: c["outdated"].as_bool().unwrap_or(false),
                    })
                    .collect(),
            }),
            _ => continue,
        };
        out.push(entry);
    }
    out
}

pub fn parse_detail(data: &Value) -> Option<Detail> {
    let v = &data["repository"]["issueOrPullRequest"];
    let mut item = parse_item(v)?;
    // Checks come from the last commit on a pull request (the list query shape differs from the detail one).
    let last = nodes(&v["lastCommit"]).next().map(|c| &c["commit"]);
    item.checks = last.and_then(|c| c["statusCheckRollup"]["state"].as_str()).map(str::to_string);
    let pr = (item.kind == Kind::Pull).then(|| {
        let checks = last
            .map(|c| {
                nodes(&c["statusCheckRollup"]["contexts"])
                    .map(|k| match k["__typename"].as_str() {
                        Some("CheckRun") => Check {
                            name: text(&k["name"]),
                            // Unfinished runs have no conclusion yet.
                            result: if k["status"] == "COMPLETED" { text(&k["conclusion"]) } else { "PENDING".into() },
                            url: text(&k["detailsUrl"]),
                        },
                        _ => Check { name: text(&k["context"]), result: text(&k["state"]), url: text(&k["targetUrl"]) },
                    })
                    .collect()
            })
            .unwrap_or_default();
        Pr {
            merged_by: v["mergedBy"]["login"].as_str().map(str::to_string),
            mergeable: text(&v["mergeable"]),
            merge_state: text(&v["mergeStateStatus"]),
            head_oid: text(&v["headRefOid"]),
            head_repo: text(&v["headRepository"]["nameWithOwner"]),
            cross_repo: v["isCrossRepository"].as_bool().unwrap_or(false),
            additions: v["additions"].as_u64().unwrap_or(0) as u32,
            deletions: v["deletions"].as_u64().unwrap_or(0) as u32,
            files: v["changedFiles"].as_u64().unwrap_or(0) as u32,
            commits: v["commitCount"]["totalCount"].as_u64().unwrap_or(0) as u32,
            requested: nodes(&v["reviewRequests"]).map(|r| reviewer(&r["requestedReviewer"])).collect(),
            reviews: nodes(&v["latestOpinionatedReviews"]).map(|r| (login(&r["author"]), text(&r["state"]))).collect(),
            checks,
            auto_merge: v["autoMergeRequest"].as_object().map(|a| (text(&a["enabledBy"]["login"]), text(&a["mergeMethod"]))),
            can_admin_merge: v["viewerCanMergeAsAdmin"].as_bool().unwrap_or(false),
            can_auto_merge: v["viewerCanEnableAutoMerge"].as_bool().unwrap_or(false),
        }
    });
    let t = &v["timelineItems"];
    Some(Detail {
        body: text(&v["body"]),
        assoc: text(&v["authorAssociation"]),
        edited: !v["lastEditedAt"].is_null(),
        locked: v["locked"].as_bool().unwrap_or(false),
        mine: v["viewerDidAuthor"].as_bool().unwrap_or(false),
        can_update: v["viewerCanUpdate"].as_bool().unwrap_or(false),
        reactions: parse_reactions(&v["reactionGroups"]),
        timeline: parse_timeline(t),
        next: t["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false).then(|| text(&t["pageInfo"]["endCursor"])),
        pr,
        item,
    })
}

pub fn parse_options_labels(data: &Value) -> Vec<Label> {
    parse_labels(&data["repository"]["labels"])
}

pub fn parse_options_users(data: &Value) -> Vec<User> {
    parse_users(&data["repository"]["assignableUsers"])
}

pub fn parse_options_milestones(data: &Value) -> Vec<(String, String)> {
    nodes(&data["repository"]["milestones"]).map(|m| (text(&m["id"]), text(&m["title"]))).collect()
}

/// One changed file of a pull request, with its patch split into display lines.
#[derive(Clone, Debug)]
pub struct FileDiff {
    pub path: String,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub lines: Vec<DiffLine>,
    /// GitHub omits the patch for binary / huge files.
    pub no_patch: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiffKind {
    Hunk,
    Add,
    Del,
    Ctx,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

pub fn parse_diff(patch: &str) -> Vec<DiffLine> {
    let (mut old, mut new) = (0u32, 0u32);
    let mut out = vec![];
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            // @@ -a,b +c,d @@ context
            let nums = |marker: char| {
                rest.split_whitespace()
                    .find(|t| t.starts_with(marker))
                    .and_then(|t| t[1..].split(',').next())
                    .and_then(|n| n.parse::<u32>().ok())
                    .unwrap_or(1)
            };
            (old, new) = (nums('-'), nums('+'));
            out.push(DiffLine { kind: DiffKind::Hunk, old: None, new: None, text: line.to_string() });
        } else if let Some(t) = line.strip_prefix('+') {
            out.push(DiffLine { kind: DiffKind::Add, old: None, new: Some(new), text: t.to_string() });
            new += 1;
        } else if let Some(t) = line.strip_prefix('-') {
            out.push(DiffLine { kind: DiffKind::Del, old: Some(old), new: None, text: t.to_string() });
            old += 1;
        } else if line.starts_with('\\') {
            continue; // "\ No newline at end of file"
        } else {
            let t = line.strip_prefix(' ').unwrap_or(line);
            out.push(DiffLine { kind: DiffKind::Ctx, old: Some(old), new: Some(new), text: t.to_string() });
            old += 1;
            new += 1;
        }
    }
    out
}

pub fn parse_files(files: &[Value]) -> Vec<FileDiff> {
    files
        .iter()
        .map(|f| FileDiff {
            path: text(&f["filename"]),
            status: text(&f["status"]),
            additions: f["additions"].as_u64().unwrap_or(0) as u32,
            deletions: f["deletions"].as_u64().unwrap_or(0) as u32,
            lines: parse_diff(f["patch"].as_str().unwrap_or("")),
            no_patch: f["patch"].is_null(),
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------ search query

/// Split a search query into tokens, keeping `label:"two words"` together.
pub fn tokens(q: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (vec![], String::new(), false);
    for c in q.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn unquote(s: &str) -> String {
    s.trim_matches('"').to_string()
}

/// Quote a qualifier value when it has spaces.
pub fn quote(v: &str) -> String {
    if v.contains(char::is_whitespace) { format!("\"{v}\"") } else { v.to_string() }
}

/// Values of `key:` tokens in the query (`label:bug label:"good first issue"` gives both).
pub fn qualifiers(q: &str, key: &str) -> Vec<String> {
    let prefix = format!("{key}:");
    tokens(q).iter().filter_map(|t| t.strip_prefix(&prefix)).map(unquote).collect()
}

/// Replace every `key:` token by one `key:value` (or drop them when `value` is `None`).
pub fn set_qualifier(q: &str, key: &str, value: Option<&str>) -> String {
    let prefix = format!("{key}:");
    let mut t: Vec<String> = tokens(q).into_iter().filter(|t| !t.starts_with(&prefix)).collect();
    if let Some(v) = value {
        t.push(format!("{prefix}{}", quote(v)));
    }
    t.join(" ")
}

/// Add the `key:value` token, or remove it when the query already has it (multi-select pickers).
pub fn toggle_qualifier(q: &str, key: &str, value: &str) -> String {
    let want = format!("{key}:{}", quote(value));
    let mut t = tokens(q);
    match t.iter().position(|x| x.eq_ignore_ascii_case(&want)) {
        Some(i) => {
            t.remove(i);
        }
        None => t.push(want),
    }
    t.join(" ")
}

/// The state words of a query: `is:open`, `is:closed`, `is:merged`.
pub fn set_state(q: &str, state: Option<&str>) -> String {
    let mut t: Vec<String> = tokens(q).into_iter().filter(|t| !matches!(t.as_str(), "is:open" | "is:closed" | "is:merged")).collect();
    if let Some(s) = state {
        t.insert(0, format!("is:{s}"));
    }
    t.join(" ")
}

pub fn state_of(q: &str) -> Option<String> {
    tokens(q).iter().find_map(|t| match t.as_str() {
        "is:open" => Some("open".to_string()),
        "is:closed" => Some("closed".to_string()),
        "is:merged" => Some("merged".to_string()),
        _ => None,
    })
}

/// The full GitHub search for a repository: scope qualifiers plus what the user typed.
pub fn scoped(repo: &str, kind: Kind, q: &str) -> String {
    let what = if kind == Kind::Issue { "is:issue" } else { "is:pr" };
    let q = q.trim();
    // Without an explicit sort GitHub ranks by best match, which is wrong for a plain list.
    let sort = if q.contains("sort:") { "" } else { " sort:created-desc" };
    format!("repo:{repo} {what} {q}{sort}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn qualifiers_are_edited_in_place() {
        let q = "is:open label:bug label:\"good first issue\" author:me crash";
        assert_eq!(qualifiers(q, "label"), vec!["bug", "good first issue"]);
        assert_eq!(set_qualifier(q, "label", None), "is:open author:me crash");
        assert_eq!(set_qualifier(q, "author", Some("bob")), "is:open label:bug label:\"good first issue\" crash author:bob");
        assert_eq!(toggle_qualifier("is:open", "label", "help wanted"), "is:open label:\"help wanted\"");
        assert_eq!(toggle_qualifier("is:open label:bug", "label", "bug"), "is:open");
        assert_eq!(set_state(q, Some("closed")), "is:closed label:bug label:\"good first issue\" author:me crash");
        assert_eq!(state_of(q).as_deref(), Some("open"));
        assert_eq!(set_state("", None), "");
    }

    #[test]
    fn search_is_scoped_and_sorted() {
        assert_eq!(scoped("a/b", Kind::Issue, " is:open "), "repo:a/b is:issue is:open sort:created-desc");
        assert_eq!(scoped("a/b", Kind::Pull, "sort:updated-desc"), "repo:a/b is:pr sort:updated-desc");
    }

    #[test]
    fn diff_lines_are_numbered() {
        let lines = parse_diff("@@ -10,3 +10,4 @@ fn x\n a\n-b\n+c\n+d\n e\n\\ No newline at end of file");
        let nums: Vec<_> = lines.iter().map(|l| (l.kind.clone(), l.old, l.new)).collect();
        assert_eq!(
            nums,
            vec![
                (DiffKind::Hunk, None, None),
                (DiffKind::Ctx, Some(10), Some(10)),
                (DiffKind::Del, Some(11), None),
                (DiffKind::Add, None, Some(11)),
                (DiffKind::Add, None, Some(12)),
                (DiffKind::Ctx, Some(12), Some(13)),
            ]
        );
    }

    #[test]
    fn list_page_parses_issues_and_pulls() {
        let data = json!({
            "open": {"issueCount": 2}, "closed": {"issueCount": 5},
            "search": {"issueCount": 2, "pageInfo": {"hasNextPage": true, "endCursor": "abc"}, "nodes": [
                {"__typename": "Issue", "id": "I1", "number": 7, "title": "Crash", "state": "CLOSED", "stateReason": "NOT_PLANNED",
                 "url": "u", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z", "author": null,
                 "comments": {"totalCount": 3}, "labels": {"nodes": [{"id": "L", "name": "bug", "color": "d73a4a"}]},
                 "assignees": {"nodes": [{"login": "me"}]}, "milestone": null},
                {"__typename": "PullRequest", "id": "P1", "number": 8, "title": "Fix", "state": "MERGED", "isDraft": false, "merged": true,
                 "url": "u", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z", "author": {"login": "bob"},
                 "comments": {"totalCount": 0}, "reviewDecision": "APPROVED", "labels": {"nodes": []}, "assignees": {"nodes": []},
                 "headRefName": "fix", "baseRefName": "main", "milestone": null,
                 "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]}},
                null, {"__typename": "Repository"}
            ]}
        });
        let page = parse_page(&data);
        assert_eq!((page.total, page.open, page.closed, page.next.as_deref()), (2, 2, 5, Some("abc")));
        assert_eq!(page.items.len(), 2);
        let (i, p) = (&page.items[0], &page.items[1]);
        assert!(i.kind == Kind::Issue && i.state == State::Closed && i.not_planned && i.author == "ghost");
        assert_eq!(i.labels[0].color, 0xd73a4a);
        assert!(p.kind == Kind::Pull && p.state == State::Merged);
        assert_eq!((p.review.as_deref(), p.checks.as_deref(), p.head.as_str()), (Some("APPROVED"), Some("SUCCESS"), "fix"));
    }

    #[test]
    fn timeline_skips_unknown_events() {
        let t = json!({"nodes": [
            {"__typename": "MentionedEvent"},
            {"__typename": "LabeledEvent", "createdAt": "2026-01-01T00:00:00Z", "actor": {"login": "a"}, "label": {"name": "bug", "color": "ff0000"}},
            {"__typename": "IssueComment", "id": "C", "body": "hi", "createdAt": "2026-01-01T00:00:00Z", "lastEditedAt": null,
             "viewerDidAuthor": true, "viewerCanUpdate": true, "viewerCanDelete": true, "authorAssociation": "OWNER", "author": {"login": "a"},
             "reactionGroups": [{"content": "HEART", "viewerHasReacted": true, "reactors": {"totalCount": 2}}]},
            null
        ]});
        let entries = parse_timeline(&t);
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0], Entry::Event(e) if e.what == "added" && e.label.as_ref().is_some_and(|l| l.name == "bug")));
        assert!(matches!(&entries[1], Entry::Comment(c) if c.reactions[0].count == 2));
    }
}
