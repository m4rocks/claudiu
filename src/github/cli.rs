//! GitHub through the user's own `gh` CLI, the same way Claudiu runs the user's own `claude`: `gh` owns the login,
//! the tokens and Enterprise hosts, so Claudiu stores no credentials. Every call is blocking and meant for a
//! background thread. Request bodies go in as JSON on stdin, so user-written markdown never meets argument quoting.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrKind {
    /// `gh` is not installed.
    NoCli,
    /// `gh` is installed but not logged in (or the token expired).
    SignedOut,
    /// The folder has no GitHub remote.
    NotGithub,
    Other,
}

#[derive(Clone, Debug)]
pub struct Error {
    pub kind: ErrKind,
    pub msg: String,
}

impl Error {
    fn other(msg: impl Into<String>) -> Self {
        Self { kind: ErrKind::Other, msg: msg.into() }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

/// `gh` on PATH, or where the usual installers put it (a GUI-launched macOS app has a minimal PATH).
pub fn locate() -> Option<PathBuf> {
    crate::platform::find_executable("gh").or_else(|| {
        ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "C:\\Program Files\\GitHub CLI\\gh.exe"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    })
}

/// The GitHub repository a folder belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
    pub url: String,
    /// `None` for github.com; the host name for GitHub Enterprise.
    pub host: Option<String>,
}

impl RepoRef {
    pub fn full(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// A `gh` bound to one working folder (and Enterprise host, if any).
#[derive(Clone, Debug)]
pub struct Gh {
    pub exe: PathBuf,
    pub cwd: PathBuf,
    pub host: Option<String>,
}

fn classify(stderr: &str) -> ErrKind {
    let s = stderr.to_lowercase();
    if s.contains("gh auth login") || s.contains("bad credentials") || s.contains("http 401") || s.contains("authentication token") {
        ErrKind::SignedOut
    } else if s.contains("none of the git remotes") || s.contains("not a git repository") || s.contains("no git remotes") {
        ErrKind::NotGithub
    } else {
        ErrKind::Other
    }
}

impl Gh {
    pub fn new(exe: PathBuf, cwd: PathBuf) -> Self {
        Self { exe, cwd, host: None }
    }

    /// Run `gh <args>`; returns stdout. GraphQL errors arrive as JSON on stdout with a non-zero exit, so both
    /// streams are searched for the message.
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<String, Error> {
        let mut cmd = Command::new(&self.exe);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(cwd) = Some(&self.cwd).filter(|c| c.is_dir()) {
            cmd.current_dir(cwd);
        }
        cmd.env("GH_PROMPT_DISABLED", "1").env("NO_COLOR", "1").env("GH_NO_UPDATE_NOTIFIER", "1");
        crate::platform::no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| Error {
            kind: if e.kind() == std::io::ErrorKind::NotFound { ErrKind::NoCli } else { ErrKind::Other },
            msg: format!("could not run gh: {e}"),
        })?;
        if let Some(mut pipe) = child.stdin.take() {
            if let Some(text) = stdin {
                let _ = pipe.write_all(text.as_bytes());
            }
        }
        let out = child.wait_with_output().map_err(|e| Error::other(e.to_string()))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            return Ok(stdout);
        }
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let msg = graphql_errors(&stdout).filter(|m| !m.is_empty()).unwrap_or_else(|| {
            let e = stderr.trim();
            if e.is_empty() { stdout.trim().to_string() } else { e.to_string() }
        });
        Err(Error { kind: classify(&format!("{stderr}\n{msg}")), msg })
    }

    fn api(&self, args: &[&str], stdin: Option<&str>) -> Result<String, Error> {
        let mut all = vec!["api"];
        if let Some(h) = &self.host {
            all.extend(["--hostname", h.as_str()]);
        }
        all.extend_from_slice(args);
        self.run(&all, stdin)
    }

    /// Run a GraphQL query or mutation; returns its `data`.
    pub fn graphql(&self, query: &str, vars: Value) -> Result<Value, Error> {
        let body = json!({ "query": query, "variables": vars }).to_string();
        let text = self.api(&["graphql", "--input", "-"], Some(&body))?;
        let mut v: Value = serde_json::from_str(&text).map_err(|e| Error::other(format!("unexpected answer from gh: {e}")))?;
        if let Some(m) = graphql_errors(&text).filter(|m| !m.is_empty()) {
            return Err(Error::other(m));
        }
        Ok(v["data"].take())
    }

    /// REST call (`method` is GET, POST, PUT, PATCH or DELETE). An empty answer (204) is `Value::Null`.
    pub fn rest(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, Error> {
        let mut args = vec!["-X", method, path];
        let text = body.map(|b| b.to_string());
        if text.is_some() {
            args.extend(["--input", "-"]);
        }
        let out = self.api(&args, text.as_deref())?;
        if out.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&out).map_err(|e| Error::other(format!("unexpected answer from gh: {e}")))
    }

    /// `gh pr checkout <n>` in this folder.
    pub fn checkout(&self, number: u32) -> Result<String, Error> {
        self.run(&["pr", "checkout", &number.to_string()], None)?;
        Ok(format!("Checked out pull request #{number}"))
    }

    /// Which GitHub repository this folder is (resolved by `gh` from the git remotes).
    pub fn repo(&self) -> Result<RepoRef, Error> {
        let text = self.run(&["repo", "view", "--json", "nameWithOwner,url"], None)?;
        let v: Value = serde_json::from_str(&text).map_err(|e| Error::other(e.to_string()))?;
        let url = v["url"].as_str().unwrap_or_default().to_string();
        let (owner, name) = v["nameWithOwner"].as_str().and_then(|n| n.split_once('/')).ok_or_else(|| Error::other("no repository"))?;
        let host = url.split("://").nth(1).and_then(|r| r.split('/').next()).filter(|h| *h != "github.com").map(str::to_string);
        Ok(RepoRef { owner: owner.into(), name: name.into(), url, host })
    }
}

/// Messages of a GraphQL `errors` array, joined; `None` when the text has none.
fn graphql_errors(text: &str) -> Option<String> {
    let v: Value = serde_json::from_str(text).ok()?;
    let errors = v.get("errors")?.as_array()?;
    Some(errors.iter().filter_map(|e| e["message"].as_str()).collect::<Vec<_>>().join("; "))
}

// ---------------------------------------------------------------------------------------------------------- queries

pub const REPO_QUERY: &str = "query($owner:String!,$name:String!){
  viewer{login}
  repository(owner:$owner,name:$name){
    id nameWithOwner url isPrivate viewerPermission hasIssuesEnabled
    mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed deleteBranchOnMerge autoMergeAllowed
    defaultBranchRef{name}
  }
}";

/// `$q` is the search, `$qa` / `$qb` the same search with `is:open` / `is:closed` (for the tab counts).
pub const LIST_QUERY: &str = "query($q:String!,$qa:String!,$qb:String!,$after:String){
  open:search(query:$qa,type:ISSUE,first:1){issueCount}
  closed:search(query:$qb,type:ISSUE,first:1){issueCount}
  search(query:$q,type:ISSUE,first:30,after:$after){
    issueCount pageInfo{hasNextPage endCursor}
    nodes{
      __typename
      ... on Issue{id number title state stateReason url createdAt updatedAt author{login} comments{totalCount}
        labels(first:10){nodes{id name color}} assignees(first:5){nodes{login}} milestone{title}}
      ... on PullRequest{id number title state isDraft merged url createdAt updatedAt author{login} comments{totalCount}
        reviewDecision labels(first:10){nodes{id name color}} assignees(first:5){nodes{login}} headRefName baseRefName
        milestone{title} commits(last:1){nodes{commit{statusCheckRollup{state}}}}}
    }
  }
}";

const REACTIONS: &str = "reactionGroups{content viewerHasReacted reactors{totalCount}}";
const TIMELINE: &str = "timelineItems(first:100,after:$after){ totalCount pageInfo{hasNextPage endCursor} nodes{ ...T } }";

fn detail_query() -> String {
    format!(
        "query($owner:String!,$name:String!,$number:Int!,$after:String){{
 viewer{{login}}
 repository(owner:$owner,name:$name){{
  issueOrPullRequest(number:$number){{
   __typename
   ... on Issue{{
     id number title body url state stateReason createdAt updatedAt lastEditedAt locked viewerDidAuthor viewerCanUpdate viewerCanReact
     author{{login}} authorAssociation
     labels(first:50){{nodes{{id name color}}}} assignees(first:20){{nodes{{id login}}}} milestone{{id title}}
     {REACTIONS}
     {TIMELINE}
   }}
   ... on PullRequest{{
     id number title body url state createdAt updatedAt lastEditedAt locked viewerDidAuthor viewerCanUpdate viewerCanReact
     author{{login}} authorAssociation
     labels(first:50){{nodes{{id name color}}}} assignees(first:20){{nodes{{id login}}}} milestone{{id title}}
     {REACTIONS}
     isDraft merged mergedAt mergeable mergeStateStatus reviewDecision baseRefName headRefName headRefOid isCrossRepository
     headRepository{{nameWithOwner}} additions deletions changedFiles
     mergedBy{{login}}
     viewerCanMergeAsAdmin viewerCanEnableAutoMerge viewerCanDisableAutoMerge
     autoMergeRequest{{enabledBy{{login}} mergeMethod}}
     reviewRequests(first:20){{nodes{{requestedReviewer{{... on User{{login}} ... on Team{{name}}}}}}}}
     latestOpinionatedReviews(first:20){{nodes{{state author{{login}}}}}}
     commitCount: commits{{totalCount}}
     lastCommit: commits(last:1){{nodes{{commit{{oid statusCheckRollup{{state contexts(first:60){{totalCount nodes{{__typename
       ... on CheckRun{{name status conclusion detailsUrl}} ... on StatusContext{{context state targetUrl}}}}}}}}}}}}}}
     {TIMELINE}
   }}
  }}
 }}
}}
fragment T on PullRequestTimelineItems {{
  __typename
  ... on IssueComment{{id body createdAt lastEditedAt viewerDidAuthor viewerCanUpdate viewerCanDelete authorAssociation author{{login}} {REACTIONS}}}
  ... on LabeledEvent{{createdAt actor{{login}} label{{name color}}}}
  ... on UnlabeledEvent{{createdAt actor{{login}} label{{name color}}}}
  ... on AssignedEvent{{createdAt actor{{login}} assignee{{... on User{{login}} ... on Bot{{login}}}}}}
  ... on UnassignedEvent{{createdAt actor{{login}} assignee{{... on User{{login}} ... on Bot{{login}}}}}}
  ... on ClosedEvent{{createdAt actor{{login}}}}
  ... on ReopenedEvent{{createdAt actor{{login}}}}
  ... on RenamedTitleEvent{{createdAt actor{{login}} previousTitle currentTitle}}
  ... on MilestonedEvent{{createdAt actor{{login}} milestoneTitle}}
  ... on DemilestonedEvent{{createdAt actor{{login}} milestoneTitle}}
  ... on LockedEvent{{createdAt actor{{login}}}}
  ... on UnlockedEvent{{createdAt actor{{login}}}}
  ... on CrossReferencedEvent{{createdAt actor{{login}} source{{__typename ... on Issue{{number title repository{{nameWithOwner}}}} ... on PullRequest{{number title repository{{nameWithOwner}}}}}}}}
  ... on PullRequestCommit{{commit{{oid abbreviatedOid messageHeadline committedDate author{{name user{{login}}}}}}}}
  ... on PullRequestReview{{id state body createdAt viewerDidAuthor authorAssociation author{{login}}
    comments(first:30){{nodes{{id body path line originalLine diffHunk createdAt author{{login}} outdated}}}}}}
  ... on MergedEvent{{createdAt actor{{login}} mergeRefName}}
  ... on ReadyForReviewEvent{{createdAt actor{{login}}}}
  ... on ConvertToDraftEvent{{createdAt actor{{login}}}}
  ... on ReviewRequestedEvent{{createdAt actor{{login}} requestedReviewer{{... on User{{login}} ... on Team{{name}}}}}}
  ... on ReviewRequestRemovedEvent{{createdAt actor{{login}} requestedReviewer{{... on User{{login}} ... on Team{{name}}}}}}
  ... on HeadRefDeletedEvent{{createdAt actor{{login}} headRefName}}
  ... on HeadRefForcePushedEvent{{createdAt actor{{login}}}}
  ... on ReviewDismissedEvent{{createdAt actor{{login}}}}
}}"
    )
}

pub fn detail_query_text() -> &'static str {
    static Q: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    Q.get_or_init(detail_query)
}

pub const LABELS_QUERY: &str = "query($owner:String!,$name:String!){repository(owner:$owner,name:$name){
  labels(first:100,orderBy:{field:NAME,direction:ASC}){nodes{id name color}}}}";
pub const USERS_QUERY: &str = "query($owner:String!,$name:String!){repository(owner:$owner,name:$name){
  assignableUsers(first:100){nodes{id login name}}}}";
pub const MILESTONES_QUERY: &str = "query($owner:String!,$name:String!){repository(owner:$owner,name:$name){
  milestones(first:50,states:OPEN,orderBy:{field:DUE_DATE,direction:ASC}){nodes{id title}}}}";

// ----------------------------------------------------------------------------------------------------- mutations

pub const M_COMMENT: &str = "mutation($id:ID!,$body:String!){addComment(input:{subjectId:$id,body:$body}){clientMutationId}}";
pub const M_EDIT_COMMENT: &str = "mutation($id:ID!,$body:String!){updateIssueComment(input:{id:$id,body:$body}){clientMutationId}}";
pub const M_DELETE_COMMENT: &str = "mutation($id:ID!){deleteIssueComment(input:{id:$id}){clientMutationId}}";
pub const M_CLOSE_ISSUE: &str = "mutation($id:ID!,$reason:IssueClosedStateReason){closeIssue(input:{issueId:$id,stateReason:$reason}){clientMutationId}}";
pub const M_REOPEN_ISSUE: &str = "mutation($id:ID!){reopenIssue(input:{issueId:$id}){clientMutationId}}";
pub const M_CLOSE_PR: &str = "mutation($id:ID!){closePullRequest(input:{pullRequestId:$id}){clientMutationId}}";
pub const M_REOPEN_PR: &str = "mutation($id:ID!){reopenPullRequest(input:{pullRequestId:$id}){clientMutationId}}";
pub const M_EDIT_ISSUE: &str = "mutation($id:ID!,$title:String,$body:String,$milestone:ID){updateIssue(input:{id:$id,title:$title,body:$body,milestoneId:$milestone}){clientMutationId}}";
pub const M_EDIT_PR: &str = "mutation($id:ID!,$title:String,$body:String,$milestone:ID){updatePullRequest(input:{pullRequestId:$id,title:$title,body:$body,milestoneId:$milestone}){clientMutationId}}";
pub const M_ADD_LABEL: &str = "mutation($id:ID!,$ids:[ID!]!){addLabelsToLabelable(input:{labelableId:$id,labelIds:$ids}){clientMutationId}}";
pub const M_REMOVE_LABEL: &str = "mutation($id:ID!,$ids:[ID!]!){removeLabelsFromLabelable(input:{labelableId:$id,labelIds:$ids}){clientMutationId}}";
pub const M_ADD_ASSIGNEE: &str = "mutation($id:ID!,$ids:[ID!]!){addAssigneesToAssignable(input:{assignableId:$id,assigneeIds:$ids}){clientMutationId}}";
pub const M_REMOVE_ASSIGNEE: &str = "mutation($id:ID!,$ids:[ID!]!){removeAssigneesFromAssignable(input:{assignableId:$id,assigneeIds:$ids}){clientMutationId}}";
pub const M_REQUEST_REVIEW: &str = "mutation($id:ID!,$ids:[ID!]!){requestReviews(input:{pullRequestId:$id,userIds:$ids,union:true}){clientMutationId}}";
pub const M_CREATE_ISSUE: &str = "mutation($repo:ID!,$title:String!,$body:String,$labels:[ID!],$assignees:[ID!]){createIssue(input:{repositoryId:$repo,title:$title,body:$body,labelIds:$labels,assigneeIds:$assignees}){issue{number}}}";
pub const M_CREATE_PR: &str = "mutation($repo:ID!,$base:String!,$head:String!,$title:String!,$body:String,$draft:Boolean){createPullRequest(input:{repositoryId:$repo,baseRefName:$base,headRefName:$head,title:$title,body:$body,draft:$draft}){pullRequest{number}}}";
pub const M_MERGE: &str = "mutation($id:ID!,$method:PullRequestMergeMethod!,$head:String,$headline:String,$body:String){mergePullRequest(input:{pullRequestId:$id,mergeMethod:$method,expectedHeadOid:$head,commitHeadline:$headline,commitBody:$body}){clientMutationId}}";
pub const M_AUTO_MERGE: &str = "mutation($id:ID!,$method:PullRequestMergeMethod!){enablePullRequestAutoMerge(input:{pullRequestId:$id,mergeMethod:$method}){clientMutationId}}";
pub const M_NO_AUTO_MERGE: &str = "mutation($id:ID!){disablePullRequestAutoMerge(input:{pullRequestId:$id}){clientMutationId}}";
pub const M_READY: &str = "mutation($id:ID!){markPullRequestReadyForReview(input:{pullRequestId:$id}){clientMutationId}}";
pub const M_DRAFT: &str = "mutation($id:ID!){convertPullRequestToDraft(input:{pullRequestId:$id}){clientMutationId}}";
pub const M_UPDATE_BRANCH: &str = "mutation($id:ID!){updatePullRequestBranch(input:{pullRequestId:$id}){clientMutationId}}";
pub const M_REVIEW: &str = "mutation($id:ID!,$event:PullRequestReviewEvent!,$body:String){addPullRequestReview(input:{pullRequestId:$id,event:$event,body:$body}){clientMutationId}}";
pub const M_REACT: &str = "mutation($id:ID!,$content:ReactionContent!){addReaction(input:{subjectId:$id,content:$content}){clientMutationId}}";
pub const M_UNREACT: &str = "mutation($id:ID!,$content:ReactionContent!){removeReaction(input:{subjectId:$id,content:$content}){clientMutationId}}";
pub const M_LOCK: &str = "mutation($id:ID!){lockLockable(input:{lockableId:$id}){clientMutationId}}";
pub const M_UNLOCK: &str = "mutation($id:ID!){unlockLockable(input:{lockableId:$id}){clientMutationId}}";

/// Files of a pull request (REST: GraphQL does not expose patches), up to `pages` x 100.
pub fn pull_files(gh: &Gh, repo: &RepoRef, number: u32, pages: u32) -> Result<Vec<Value>, Error> {
    let mut all = vec![];
    for page in 1..=pages {
        let path = format!("repos/{}/pulls/{number}/files?per_page=100&page={page}", repo.full());
        let v = gh.rest("GET", &path, None)?;
        let batch = v.as_array().cloned().unwrap_or_default();
        let done = batch.len() < 100;
        all.extend(batch);
        if done {
            break;
        }
    }
    Ok(all)
}

/// Delete a branch on GitHub (the head of a merged pull request).
pub fn delete_branch(gh: &Gh, repo: &RepoRef, branch: &str) -> Result<(), Error> {
    gh.rest("DELETE", &format!("repos/{}/git/refs/heads/{}", repo.full(), encode_path(branch)), None).map(|_| ())
}

/// Percent-encode a ref name for a URL path, keeping `/`.
fn encode_path(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphql_errors_are_joined() {
        let text = r#"{"data":null,"errors":[{"message":"a"},{"message":"b"}]}"#;
        assert_eq!(graphql_errors(text).as_deref(), Some("a; b"));
        assert_eq!(graphql_errors(r#"{"data":{}}"#), None);
        assert_eq!(graphql_errors("not json"), None);
    }

    #[test]
    fn errors_are_classified() {
        assert_eq!(classify("To get started with GitHub CLI, please run:  gh auth login"), ErrKind::SignedOut);
        assert_eq!(classify("none of the git remotes configured for this repository point to a known GitHub host"), ErrKind::NotGithub);
        assert_eq!(classify("boom"), ErrKind::Other);
    }

    #[test]
    fn branch_names_are_path_encoded() {
        assert_eq!(encode_path("feat/a b#1"), "feat/a%20b%231");
    }
}
