//! Rich rendering for the PR description: markdown plus the media GitHub
//! embeds in PR bodies. Images load through the app's HTTP client; videos,
//! which GPUI cannot play natively, render as cards that open in the browser
//! when clicked.

use gpui_kit::component::text::{MarkdownNode, MarkdownParseContext, MarkdownPlugin, markdown_ast};
use gpui_kit::component::{ActiveTheme, Icon, IconName, h_flex, v_flex};
use gpui_kit::*;

const VIDEO_NODE: &str = "diffy-video";

/// A `<video>` embed (or a bare autolinked URL to a video file) in a PR body.
#[derive(Clone, Debug, PartialEq)]
struct Video {
    src: SharedString,
    poster: Option<SharedString>,
}

/// Block plugin: a `<video>` tag on its own lines in the PR body.
pub struct VideoBlockPlugin;

impl MarkdownPlugin for VideoBlockPlugin {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        VIDEO_NODE
    }

    fn parse(
        &self,
        node: &markdown_ast::Node,
        cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        parse_video_html(node, cx)
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        render_video(node, window, cx)
    }
}

/// Inline plugin: a `<video>` tag inside a paragraph, or a bare autolinked
/// URL to a video file.
pub struct VideoInlinePlugin;

impl MarkdownPlugin for VideoInlinePlugin {
    fn name(&self) -> &str {
        VIDEO_NODE
    }

    fn parse(
        &self,
        node: &markdown_ast::Node,
        cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        if let Some(node) = parse_video_html(node, cx) {
            return Some(node);
        }
        parse_video_link(node, cx)
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        render_video(node, window, cx)
    }
}

fn parse_video_html(
    node: &markdown_ast::Node,
    cx: &MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Html(html) = node else {
        return None;
    };
    let video = parse_video_tag(&html.value)?;
    Some(
        MarkdownNode::new(VIDEO_NODE, video.clone())
            .text(format!("[video: {}]", video.src))
            .markdown(cx.node_source(node).unwrap_or(&html.value)),
    )
}

/// A bare URL to a video file (what GitHub turns into a player) parses as a
/// link whose text is the URL itself; textual links stay links.
fn parse_video_link(
    node: &markdown_ast::Node,
    cx: &MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Link(link) = node else {
        return None;
    };
    if !is_video_url(&link.url) {
        return None;
    }
    let is_autolink = link.children.len() == 1
        && matches!(&link.children[0], markdown_ast::Node::Text(text) if text.value == link.url);
    if !is_autolink {
        return None;
    }
    let video = Video {
        src: link.url.clone().into(),
        poster: None,
    };
    Some(
        MarkdownNode::new(VIDEO_NODE, video.clone())
            .text(format!("[video: {}]", video.src))
            .markdown(cx.node_source(node).unwrap_or(&link.url)),
    )
}

/// The video card: the poster image when there is one, otherwise a play
/// affordance, with the source URL underneath. Clicking opens the video in
/// the browser, since GPUI has no video element.
fn render_video(node: &MarkdownNode, _window: &mut Window, cx: &mut App) -> impl IntoElement {
    let video = node.data::<Video>().expect("video node data");
    let src = video.src.clone();
    let media = match &video.poster {
        Some(poster) => img(poster.clone())
            .w_full()
            .h_auto()
            .object_fit(ObjectFit::Cover)
            .into_any_element(),
        None => v_flex()
            .w_full()
            .h(rems(10.))
            .items_center()
            .justify_center()
            .bg(cx.theme().muted)
            .child(
                Icon::new(IconName::Play)
                    .size(px(32.))
                    .text_color(cx.theme().muted_foreground),
            )
            .into_any_element(),
    };
    div()
        .id(ElementId::Name(format!("pr-video-{src}").into()))
        .test_support()
        .w_full()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .overflow_hidden()
        .cursor_pointer()
        .on_click(move |_, _, cx| cx.open_url(&src))
        .child(media)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .px_3()
                .py_2()
                .min_w_0()
                .child(
                    Icon::new(IconName::Play)
                        .size(px(14.))
                        .text_color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .text_ellipsis()
                        .child(video.src.clone()),
                ),
        )
}

/// File extensions GitHub renders as an inline video player.
fn is_video_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let Some(extension) = path.rsplit('.').next() else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "mp4" | "webm" | "mov" | "m4v" | "ogv" | "mkv"
    )
}

/// Extracts `src` (or the first `<source src=...>`) and `poster` from an HTML
/// chunk that opens with a `<video` tag.
fn parse_video_tag(html: &str) -> Option<Video> {
    let lower = html.to_lowercase();
    let start = lower.find("<video")?;
    let after = lower[start + "<video".len()..].chars().next()?;
    if !after.is_whitespace() && after != '>' && after != '/' {
        return None;
    }
    let tag_end = lower[start..].find('>')? + start;
    let open_tag = &html[start..tag_end];
    let attrs = parse_attrs(open_tag);
    let src = attrs
        .iter()
        .find(|(name, _)| name == "src")
        .map(|(_, value)| value.clone())
        .or_else(|| {
            let rest = &html[tag_end + 1..];
            let source_start = rest.to_lowercase().find("<source")?;
            let source_end = rest[source_start..].find('>')? + source_start;
            parse_attrs(&rest[source_start..source_end])
                .into_iter()
                .find(|(name, _)| name == "src")
                .map(|(_, value)| value)
        })?;
    let poster = attrs
        .iter()
        .find(|(name, _)| name == "poster")
        .map(|(_, value)| value.clone());
    Some(Video {
        src: src.into(),
        poster: poster.map(Into::into),
    })
}

/// Parses the attributes of one opening tag, lowercasing names. Values may be
/// double-quoted, single-quoted, or unquoted.
fn parse_attrs(tag: &str) -> Vec<(String, String)> {
    let bytes = tag.as_bytes();
    let mut ix = tag.find(char::is_whitespace).unwrap_or(tag.len());
    let mut attrs = Vec::new();
    while ix < bytes.len() {
        while ix < bytes.len() && (bytes[ix] as char).is_whitespace() {
            ix += 1;
        }
        if ix >= bytes.len() || bytes[ix] == b'>' || bytes[ix] == b'/' {
            break;
        }
        let name_start = ix;
        while ix < bytes.len()
            && !matches!(bytes[ix], b'=' | b'>' | b'/')
            && !(bytes[ix] as char).is_whitespace()
        {
            ix += 1;
        }
        let name = tag[name_start..ix].to_ascii_lowercase();
        while ix < bytes.len() && (bytes[ix] as char).is_whitespace() {
            ix += 1;
        }
        let mut value = String::new();
        if ix < bytes.len() && bytes[ix] == b'=' {
            ix += 1;
            while ix < bytes.len() && (bytes[ix] as char).is_whitespace() {
                ix += 1;
            }
            if ix < bytes.len() && (bytes[ix] == b'"' || bytes[ix] == b'\'') {
                let quote = bytes[ix];
                ix += 1;
                let value_start = ix;
                while ix < bytes.len() && bytes[ix] != quote {
                    ix += 1;
                }
                value = tag[value_start..ix].to_string();
                ix += 1;
            } else {
                let value_start = ix;
                while ix < bytes.len() && !(bytes[ix] as char).is_whitespace() && bytes[ix] != b'>'
                {
                    ix += 1;
                }
                value = tag[value_start..ix].to_string();
            }
        }
        if !name.is_empty() {
            attrs.push((name, value));
        }
    }
    attrs
}

#[cfg(test)]
mod tests {
    use super::{is_video_url, parse_video_tag};

    #[test]
    fn parses_a_github_video_attachment() {
        let html = r#"<video src="https://github.com/user-attachments/assets/abc123" controls="controls" autoplay="autoplay" muted="muted" loop="loop" playsinline="playsinline" style="max-width: 100%"></video>"#;
        let video = parse_video_tag(html).expect("video tag parses");
        assert_eq!(
            video.src.as_ref(),
            "https://github.com/user-attachments/assets/abc123"
        );
        assert_eq!(video.poster, None);
    }

    #[test]
    fn parses_source_children_and_poster() {
        let html = "<video controls poster=\"https://example.com/p.png\">\n  <source src=\"https://example.com/v.mp4\" type=\"video/mp4\">\n</video>";
        let video = parse_video_tag(html).expect("video tag parses");
        assert_eq!(video.src.as_ref(), "https://example.com/v.mp4");
        assert_eq!(video.poster.as_deref(), Some("https://example.com/p.png"));
    }

    #[test]
    fn ignores_non_video_html() {
        assert!(parse_video_tag(r#"<img src="https://example.com/a.png">"#).is_none());
        assert!(parse_video_tag(r#"<div>watch this</div>"#).is_none());
        assert!(parse_video_tag(r#"<video>no src</video>"#).is_none());
    }

    #[test]
    fn recognizes_video_file_urls() {
        assert!(is_video_url("https://example.com/clip.mp4"));
        assert!(is_video_url("https://example.com/clip.MOV?raw=1"));
        assert!(!is_video_url("https://example.com/page.html"));
        assert!(!is_video_url("https://example.com/image.png"));
    }
}
