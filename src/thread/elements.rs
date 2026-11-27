//! Native GPUI implementations of the shadcn/ui AI Elements patterns:
//! - Chain of Thought (`chain_of_thought_header`, `trace_content`)
//! - Inline Citation (`Citation`, `extract_citations`, `inline_citation`, `inline_citations_row`)
//! - Reasoning (`reasoning_header`, `reasoning_body`, `format_duration`)
//! - Tool (`tool_kind_icon`, `tool_header`, `tool_output_block`, `tool_diff_block`)
//!
//! Built on composable GPUI primitives (`trace_line`, `trace_content`, `code_block`, `CitationHoverCard`)
//! styled to m4code's monochrome dark theme (`panels::theme`).

use std::collections::HashSet;
use std::time::Duration;

use gpui::{Hsla, 
    AnyElement, AppContext, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px, relative,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::Button;

use crate::harness::thread::{ToolCall, ToolStatus};
use crate::panels::scale;
use crate::panels::theme::theme;
use crate::panels::widgets::{icon};
use crate::panels::ui::button::focusable;
use crate::panels::ui::spinner::spinner;

/// Execution status of a trace step (matches shadcn `TraceLine` status variants).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceStatus {
    Pending,
    Running,
    Done,
    Ended,
    Error,
}

impl From<ToolStatus> for TraceStatus {
    fn from(status: ToolStatus) -> Self {
        match status {
            ToolStatus::Pending => Self::Pending,
            ToolStatus::Ended => Self::Ended,
            ToolStatus::Running => Self::Running,
            ToolStatus::Done => Self::Done,
            ToolStatus::Failed => Self::Error,
        }
    }
}

/// Base `TraceLine` primitive: compact single-line row with leading icon/spinner, title, optional description,
/// optional trailing chips/badges, and optional collapsible chevron.
#[allow(clippy::too_many_arguments)]
pub fn trace_line(
    id: impl Into<SharedString>,
    status: TraceStatus,
    leading: AnyElement,
    title: String,
    description: Option<String>,
    chips: Vec<AnyElement>,
    collapsible: bool,
    expanded: bool,
) -> Button {
    let title_color = match status {
        TraceStatus::Error => theme().destructive,
        TraceStatus::Running => theme().busy,
        TraceStatus::Done if expanded => theme().muted_foreground,
        TraceStatus::Ended => theme().muted_foreground,
        TraceStatus::Pending | TraceStatus::Done => theme().subtle_foreground,
    };
    let has_desc = description.as_ref().is_some_and(|d| !d.trim().is_empty());
    let mut row = focusable(id.into())
        .focusable(collapsible)
        .tab_stop(collapsible)
        .w_full()
        .overflow_hidden()
        .px(px(6.0))
        .py(px(3.0))
        .rounded(px(6.0))
        .justify_start()
        .gap(px(7.0))
        .text_size(scale::px(12.0))
        .line_height(relative(1.4))
        .when(collapsible, |d| d.cursor_pointer().hover(|s| s.bg(theme().accent)))
        .child(div().flex_none().w(px(14.0)).flex().items_center().justify_center().child(leading))
        .child(
            div()
                .min_w_0()
                .when(has_desc, |d| d.flex_shrink(1.0).max_w(px(360.0)))
                .when(!has_desc, |d| d.flex_1())
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(title_color)
                .when(status == TraceStatus::Running, |d| d.font_weight(FontWeight::MEDIUM))
                .child(one_line(&title)),
        );

    if let Some(desc) = description.map(|d| one_line(&d)).filter(|d| !d.is_empty()) {
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(scale::px(11.5))
                .text_color(theme().subtle_foreground)
                .child(desc),
        );
    }

    for chip in chips {
        row = row.child(chip);
    }

    if collapsible {
        row = row.child(
            div()
                .flex_none()
                .text_color(theme().subtle_foreground)
                .child(icon(if expanded { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0)),
        );
    }

    row
}

/// Fold a step's title or description onto one line: a multi-line shell command or title would otherwise make its
/// row taller, as if it were expanded.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Base `TraceCollapsibleContent` primitive: indented content block with the vertical rail aligned under `trace_line`'s icon.
pub fn trace_content(child: AnyElement) -> AnyElement {
    div()
        .ml(px(12.0))
        .pl(px(12.0))
        .border_l_1()
        .border_color(theme().border)
        .child(child)
        .into_any_element()
}

/// Compact pill badge used inside trace lines (e.g. step counts, tool status, duration).
pub fn trace_badge(label: impl Into<SharedString>, color: Hsla, bg: Hsla, border: Hsla) -> AnyElement {
    div()
        .flex_none()
        .px(px(5.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_size(scale::px(10.5))
        .text_color(color)
        .child(label.into())
        .into_any_element()
}

/// Native `CodeBlock` primitive for tool outputs and structured text.
pub fn code_block(header: Option<String>, lines: &[&str], shown: usize, total: usize, border_color: Option<Hsla>) -> AnyElement {
    let mut block = div()
        .font_family(crate::platform::monospace_font())
        .text_size(scale::px(12.0))
        .rounded(px(6.0))
        .bg(theme().window)
        .border_1()
        .border_color(border_color.unwrap_or(theme().panel_border))
        .flex()
        .flex_col()
        .overflow_hidden();

    if let Some(title) = header {
        block = block.child(
            div()
                .px(px(9.0))
                .py(px(4.0))
                .bg(theme().card)
                .border_b_1()
                .border_color(theme().border)
                .text_size(scale::px(11.0))
                .text_color(theme().muted_foreground)
                .child(title),
        );
    }

    let mut body = div().px(px(9.0)).py(px(6.0)).text_color(theme().muted_foreground).flex().flex_col().gap(px(1.0));
    for line in &lines[..shown] {
        body = body.child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().child((*line).to_string()));
    }
    if shown < total {
        body = body.child(
            div()
                .pt(px(2.0))
                .text_size(scale::px(11.0))
                .text_color(theme().subtle_foreground)
                .child(format!("… {} more lines (Ctrl+O)", total - shown)),
        );
    }
    block.child(body).into_any_element()
}

// ---------------------------------------------------------------------------
// 1. Chain of Thought
// ---------------------------------------------------------------------------

/// Collapsible summary header for a run of agent steps (`ChainOfThought`).
#[allow(clippy::too_many_arguments)]
pub fn chain_of_thought_header(
    start: usize,
    label: String,
    latest_preview: String,
    step_count: usize,
    running: bool,
    failed: bool,
    open: bool,
) -> Button {
    let status = if running {
        TraceStatus::Running
    } else if failed {
        TraceStatus::Error
    } else {
        TraceStatus::Done
    };
    let leading = if running {
        spinner(SharedString::from(format!("cot-spin-{start}")), 12.0).into_any_element()
    } else if failed {
        icon(IconName::CircleX, 12.5).text_color(theme().destructive).into_any_element()
    } else {
        icon(IconName::CircleCheck, 12.5).text_color(if open { theme().muted_foreground } else { theme().subtle_foreground }).into_any_element()
    };
    let mut chips = Vec::new();
    if step_count > 1 {
        let step_badge = format!("{step_count} steps");
        chips.push(trace_badge(step_badge, theme().subtle_foreground, theme().card, theme().border));
    }
    let preview = (!open && !latest_preview.is_empty()).then_some(latest_preview);
    trace_line(
        SharedString::from(format!("tool-run-{start}")),
        status,
        leading,
        label,
        preview,
        chips,
        true,
        open,
    )
    .debug_selector(move || format!("tool-run-{start}"))
}

// ---------------------------------------------------------------------------
// 2. Reasoning
// ---------------------------------------------------------------------------

/// Format a reasoning duration concisely (e.g. `"<1s"`, `"4s"`, `"1m 12s"`).
pub fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs == 0 {
        "<1s".to_string()
    } else if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// Header for a `Reasoning` block: shows `"Thinking…"` with a live spinner while streaming,
/// or `"Thought"` with measured duration once finished.
pub fn reasoning_header(ix: usize, preview: String, streaming: bool, duration: Option<Duration>, expanded: bool) -> Button {
    let status = if streaming { TraceStatus::Running } else { TraceStatus::Done };
    let leading = if streaming {
        spinner(SharedString::from(format!("reasoning-spin-{ix}")), 12.0).into_any_element()
    } else {
        icon(IconName::CircleCheck, 12.5)
            .text_color(if expanded { theme().muted_foreground } else { theme().subtle_foreground })
            .into_any_element()
    };
    let title = if streaming {
        "Thinking…".to_string()
    } else if let Some(d) = duration {
        format!("Thought for {}", format_duration(d))
    } else {
        "Thought".to_string()
    };
    let desc = (!expanded && !preview.is_empty()).then_some(preview);
    trace_line(
        SharedString::from(format!("thought-row-{ix}")),
        status,
        leading,
        title,
        desc,
        vec![],
        true,
        expanded,
    )
}

/// Body of a `Reasoning` block: muted, readable thinking text indented along the trace rail.
pub fn reasoning_body(text: &str) -> AnyElement {
    trace_content(
        div()
            .py(px(2.0))
            .italic()
            .text_size(scale::px(12.0))
            .text_color(theme().muted_foreground)
            .child(text.trim().to_string())
            .into_any_element(),
    )
}

// ---------------------------------------------------------------------------
// 3. Tool
// ---------------------------------------------------------------------------

/// Choose a Lucide icon matching ACP's tool kind (`read`, `edit`, `execute`, `search`, `fetch`, `think`, ...).
pub fn tool_kind_icon(kind: &str) -> IconName {
    match kind {
        "execute" => IconName::Terminal,
        "read" => IconName::FileText,
        "edit" | "delete" | "move" => IconName::Pencil,
        "search" => IconName::Search,
        "fetch" => IconName::Link,
        "think" => IconName::Clock,
        _ => IconName::Brackets,
    }
}

/// Header for a `Tool` call: kind-specific icon (or spinner when running / error icon when failed),
/// title, status badge for running/error, location chips, and collapsible chevron.
pub fn tool_header(
    ix: usize,
    call: &ToolCall,
    title: String,
    location_chips: Vec<AnyElement>,
    has_details: bool,
    expanded: bool,
) -> Button {
    let status = TraceStatus::from(call.status);
    let leading = match call.status {
        ToolStatus::Running => spinner(SharedString::from(format!("tool-spin-{ix}")), 12.0).into_any_element(),
        ToolStatus::Failed => icon(IconName::CircleX, 12.5).text_color(theme().destructive).into_any_element(),
        ToolStatus::Pending => icon(IconName::CircleDotDashed, 12.5).text_color(theme().subtle_foreground).into_any_element(),
        ToolStatus::Done | ToolStatus::Ended => icon(tool_kind_icon(&call.kind), 12.5)
            .text_color(if expanded { theme().muted_foreground } else { theme().subtle_foreground })
            .into_any_element(),
    };
    let mut chips = Vec::new();
    match call.status {
        ToolStatus::Running => chips.push(trace_badge("Running", theme().busy, theme().card, theme().panel_border)),
        ToolStatus::Failed => chips.push(trace_badge("Failed", theme().destructive, theme().destructive_subtle, theme().destructive_subtle_border)),
        ToolStatus::Ended => chips.push(trace_badge("Ended", theme().muted_foreground, theme().card, theme().panel_border)),
        _ => {}
    }
    chips.extend(location_chips);

    trace_line(
        SharedString::from(format!("tool-row-{ix}")),
        status,
        leading,
        title,
        None,
        chips,
        has_details,
        expanded,
    )
    .debug_selector(move || format!("tool-row-{ix}"))
}

/// Render a tool's text output inside a native `CodeBlock`.
pub fn tool_output_block(label: Option<&str>, text: &str, folded_lines: usize, full: bool, query: Option<&str>) -> AnyElement {
    let text = if label.is_none() {
        text.trim_matches('\n').trim_start_matches("```console\n").trim_end_matches("```").trim_end()
    } else {
        text
    };
    let lines: Vec<&str> = text.lines().collect();
    let shown = if full { lines.len() } else { lines.len().min(folded_lines) };
    let last_start = lines.len() - shown;
    let default_start = if label.is_some() { last_start } else { 0 };
    let match_line = query.filter(|_| !full).and_then(|query| lines.iter().position(|line| line.to_lowercase().contains(query)));
    let start = match match_line {
        Some(line) if !(default_start..default_start + shown).contains(&line) => line.min(last_start),
        _ => default_start,
    };
    let snippet = if label.is_some() || start > 0 { &lines[start..start + shown] } else { &lines };
    let title = if start != default_start {
        Some(format!("{} from line {}", label.unwrap_or("Output"), start + 1))
    } else {
        label.map(str::to_owned)
    };
    code_block(title, snippet, shown, lines.len(), None)
}

// ---------------------------------------------------------------------------
// 4. Inline Citation (Button chip + HoverCard)
// ---------------------------------------------------------------------------

/// A single web source citation extracted from an assistant response or provided directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Citation {
    pub title: String,
    pub hostname: String,
    pub url: String,
}

/// Native GPUI HoverCard view shown when hovering an `inline_citation` chip.
struct CitationHoverCard {
    citation: Citation,
}

impl Render for CitationHoverCard {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(272.0))
            .p(px(10.0))
            .rounded(px(8.0))
            .bg(theme().card)
            .border_1()
            .border_color(theme().panel_border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(scale::px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme().foreground)
                    .child(self.citation.title.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .text_size(scale::px(11.0))
                    .text_color(theme().muted_foreground)
                    .child(icon(IconName::ExternalLink, 10.5))
                    .child(self.citation.hostname.clone()),
            )
            .child(
                div()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(scale::px(10.5))
                    .text_color(theme().subtle_foreground)
                    .child(self.citation.url.clone()),
            )
    }
}

/// Native `InlineCitation` chip: compact secondary button showing the source hostname, opening a
/// `CitationHoverCard` on hover and navigating to the URL on click.
pub fn inline_citation(id: impl Into<SharedString>, citation: Citation) -> AnyElement {
    let hover = citation.clone();
    let open_url = citation.url.clone();
    focusable(id.into())
        .flex_none()
        .px(px(6.0))
        .py(px(2.0))
        .rounded(px(5.0))
        .bg(theme().card)
        .border_color(theme().panel_border)
        .gap(px(4.0))
        .text_size(scale::px(11.0))
        .text_color(theme().muted_foreground)
        .cursor_pointer()
        .hover(|s| s.bg(theme().accent).border_color(theme().ring).text_color(theme().foreground))
        .child(icon(IconName::Link, 10.5).text_color(theme().subtle_foreground))
        .child(citation.hostname)
        .tooltip(move |_window, cx| cx.new(|_| CitationHoverCard { citation: hover.clone() }).into())
        .on_click(move |_: &ClickEvent, _, cx| {
            cx.stop_propagation();
            if open_url.starts_with("https://") || open_url.starts_with("http://") {
                cx.open_url(&open_url);
            }
        })
        .into_any_element()
}

/// Compact wrapped row of `InlineCitation` chips beneath an assistant response.
pub fn inline_citations_row(entry_ix: usize, citations: Vec<Citation>) -> Option<AnyElement> {
    if citations.is_empty() {
        return None;
    }
    let mut row = div().pt(px(6.0)).flex().flex_wrap().items_center().gap(px(5.0));
    for (n, citation) in citations.into_iter().enumerate() {
        row = row.child(inline_citation(SharedString::from(format!("citation-{entry_ix}-{n}")), citation));
    }
    Some(row.into_any_element())
}

/// Extract unique HTTP(S) citations from markdown text, skipping fenced code blocks and inline code spans.
pub fn extract_citations(markdown: &str) -> Vec<Citation> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut in_fence: Option<(char, usize)> = None;

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(c) = trimmed.chars().next().filter(|&c| c == '`' || c == '~') {
            let count = trimmed.chars().take_while(|&x| x == c).count();
            if count >= 3 {
                match in_fence {
                    None => {
                        in_fence = Some((c, count));
                        continue;
                    }
                    Some((fc, fn_)) if fc == c && count >= fn_ => {
                        in_fence = None;
                        continue;
                    }
                    _ => {}
                }
            }
        }
        if in_fence.is_some() {
            continue;
        }
        extract_line_citations(line, &mut seen, &mut out);
    }

    out
}

fn extract_line_citations(line: &str, seen: &mut HashSet<String>, out: &mut Vec<Citation>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_code = false;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if in_code {
            i += 1;
            continue;
        }
        // Markdown link: [label](https://...)
        if bytes[i] == b'['
            && let Some(close_bracket) = line[i + 1..].find(']').map(|o| i + 1 + o)
            && line[close_bracket + 1..].starts_with('(')
            && let Some(close_paren) = line[close_bracket + 2..].find(')').map(|o| close_bracket + 2 + o)
        {
            let label = line[i + 1..close_bracket].trim();
            let raw_url = line[close_bracket + 2..close_paren].trim();
            if push_citation(label, raw_url, seen, out) {
                i = close_paren + 1;
                continue;
            }
        }
        // Bare URL: https://... or http://...
        if line[i..].starts_with("https://") || line[i..].starts_with("http://") {
            let end = line[i..]
                .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\''))
                .map_or(line.len(), |o| i + o);
            let raw_url = line[i..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
            push_citation("", raw_url, seen, out);
            i = end;
            continue;
        }
        i += line[i..].chars().next().map_or(1, char::len_utf8);
    }
}

fn push_citation(label: &str, raw_url: &str, seen: &mut HashSet<String>, out: &mut Vec<Citation>) -> bool {
    let Ok(parsed) = url::Url::parse(raw_url) else { return false };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str() else { return false };
    let hostname = host.strip_prefix("www.").unwrap_or(host).to_string();
    let url = parsed.to_string();
    if !seen.insert(url.clone()) {
        return true;
    }
    let title = if label.is_empty() || label == raw_url {
        hostname.clone()
    } else {
        label.to_string()
    };
    out.push(Citation { title, hostname, url });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_titles_fold_onto_one_line() {
        assert_eq!(one_line("cd src &&\n  cargo test \\\n\t--all\r\n"), "cd src && cargo test \\ --all");
    }

    #[test]
    fn extracts_markdown_and_bare_citations_skipping_code_and_duplicates() {
        let md = "\
See [shadcn/ui](https://ui.shadcn.com/docs/ai-elements) and https://docs.rs/gpui/latest.
Repeat [same](https://ui.shadcn.com/docs/ai-elements) and inline `https://ignored.example.com`.
```rust
let u = \"https://also-ignored.example.com\";
```
Also [GitHub](https://www.github.com/m4rocks/m4code).";
        let got = extract_citations(md);
        assert_eq!(
            got,
            vec![
                Citation {
                    title: "shadcn/ui".into(),
                    hostname: "ui.shadcn.com".into(),
                    url: "https://ui.shadcn.com/docs/ai-elements".into(),
                },
                Citation {
                    title: "docs.rs".into(),
                    hostname: "docs.rs".into(),
                    url: "https://docs.rs/gpui/latest".into(),
                },
                Citation {
                    title: "GitHub".into(),
                    hostname: "github.com".into(),
                    url: "https://www.github.com/m4rocks/m4code".into(),
                },
            ]
        );
    }

    #[test]
    fn formats_reasoning_durations_cleanly() {
        assert_eq!(format_duration(Duration::from_millis(400)), "<1s");
        assert_eq!(format_duration(Duration::from_secs(7)), "7s");
        assert_eq!(format_duration(Duration::from_secs(75)), "1m 15s");
    }
}
