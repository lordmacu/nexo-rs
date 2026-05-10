//! Email-safe block types + table-based renderer.
//!
//! Email clients (especially Outlook's Word renderer + Gmail's
//! mobile webview) ignore most modern CSS. This renderer
//! emits the table-based, inline-CSS markup that survives the
//! lowest common denominator: 600px-wide centered table, role
//! presentation hints for screen readers, web-safe font stack.
//!
//! The renderer is intentionally a pure fn — same blocks +
//! same vars always produce the same HTML — so tests + the
//! frontend's preview pane (which ports the same logic) stay
//! aligned.
//!
//! Variable substitution: `{{name}}` style, looked up in a
//! `HashMap<String, String>`. Unknown placeholders pass
//! through unchanged so a partial render still ships.

use std::collections::HashMap;

use nexo_sanitize::{sanitize_color, sanitize_url};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

impl Default for TextAlign {
    fn default() -> Self {
        Self::Left
    }
}

impl TextAlign {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
        }
    }
}

/// How an `Image` block delivers its bytes to the recipient.
///
/// - `Url` — `<img src="https://.../t/asset/.../{sha}.png">`,
///   recipient's mail client fetches over HTTPS. Smaller email
///   payload + works as a secondary engagement signal (every
///   image fetch hits our public route on the same host as the
///   tracking pixel).
/// - `Cid` — bytes ride along inline as a `multipart/related`
///   part the HTML body references via `<img src="cid:{sha}">`.
///   Email is heavier (image bytes baked in, ~33% base64
///   overhead on the wire) but renders even when the
///   recipient's client blocks external images (Outlook
///   default, Gmail "ask before showing").
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageEmbed {
    #[default]
    Url,
    Cid,
}

/// One block in an email template. Persisted as JSON inside
/// `marketing_email_templates.blocks_json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmailBlock {
    Heading {
        /// Plain-text body. Used when `text_html` is `None`.
        /// Always HTML-escaped during render.
        text: String,
        /// Optional rich-text body emitted by the TipTap
        /// editor. When present, supersedes `text` and is
        /// rendered VERBATIM after passing through the
        /// email-safe sanitizer (whitelist tags + attrs).
        /// Stored on the wire as the post-sanitization HTML
        /// so a reload doesn't re-sanitize.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text_html: Option<String>,
        /// 1, 2, 3 — clamped server-side. Anything else falls
        /// back to 2.
        #[serde(default = "default_heading_level")]
        level: u8,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        align: TextAlign,
    },
    Paragraph {
        text: String,
        /// See `Heading.text_html`. Same semantics, applies
        /// inside the `<p>` element.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text_html: Option<String>,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        align: TextAlign,
        /// Px size, clamped 10..=32 server-side.
        #[serde(default = "default_paragraph_size")]
        font_size: u8,
    },
    Button {
        text: String,
        url: String,
        #[serde(default = "default_button_bg")]
        bg_color: String,
        #[serde(default = "default_button_text")]
        text_color: String,
        #[serde(default = "default_align_center")]
        align: TextAlign,
    },
    Image {
        url: String,
        #[serde(default)]
        alt: String,
        /// `None` → 100% (use full container width).
        #[serde(default)]
        width: Option<u32>,
        #[serde(default = "default_align_center")]
        align: TextAlign,
        /// Optional click-through. When set, image becomes a
        /// link.
        #[serde(default)]
        link_url: Option<String>,
        /// Where the recipient's mail client fetches the
        /// bytes from. `Url` (default) hits our public asset
        /// route; `Cid` ships the bytes inline as a
        /// `multipart/related` part the HTML references via
        /// `<img src="cid:{sha}">`. URL gives engagement
        /// signals + smaller emails; CID gives guaranteed
        /// rendering even when the recipient's client
        /// blocks external images. Defaults to `Url` so
        /// pre-existing templates behave unchanged.
        #[serde(default)]
        embed: ImageEmbed,
    },
    Divider {
        #[serde(default = "default_divider_color")]
        color: String,
    },
    Spacer {
        #[serde(default = "default_spacer_height")]
        height_px: u32,
    },
    /// Legacy fixed 50/50 two-column block. Kept for back-
    /// compat with templates saved before the generalised
    /// Row/Column model landed; new authoring goes through
    /// `Row` instead. The render path treats it identically
    /// to a 2-column Row of 50% each.
    TwoColumn {
        #[serde(default)]
        left: Vec<EmailBlock>,
        #[serde(default)]
        right: Vec<EmailBlock>,
    },
    /// Generalised Elementor-style row container. Holds 1-N
    /// columns whose `width_pct` values sum to 100. Each
    /// column holds element blocks (heading, paragraph, …);
    /// nested rows are not rendered (the type permits them
    /// but the UI won't author them).
    Row {
        #[serde(default)]
        columns: Vec<Column>,
        /// Operator-set background colour for the row's
        /// outer TD (covers all columns). `None` ⇒ inherits
        /// the email card's white background. Hex `#rrggbb`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<String>,
        /// Optional background image URL for the row. Combines
        /// with `background` (colour shows through transparent
        /// areas + acts as fallback when the client blocks
        /// remote images). Must be `http://` or `https://`;
        /// other schemes (`javascript:`, `data:`, `file:`) are
        /// rejected by `sanitize_url` at render time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background_image: Option<String>,
    },
    List {
        items: Vec<String>,
        #[serde(default)]
        ordered: bool,
        #[serde(default)]
        color: Option<String>,
    },
}

/// One column inside a `Row`. Width is a percentage of the
/// row's container; sum across siblings should be 100. The
/// renderer divides the difference evenly when the sum
/// drifts so a malformed template still produces a sane
/// table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Column {
    #[serde(default)]
    pub blocks: Vec<EmailBlock>,
    #[serde(default = "default_column_width")]
    pub width_pct: u8,
    /// Operator-set background colour for THIS column only
    /// (overrides the row's background). `None` ⇒ transparent.
    /// Hex `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    /// Optional background image URL for the column. Same
    /// `http(s)://` allowlist as `Row::background_image`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_image: Option<String>,
}

fn default_column_width() -> u8 {
    100
}

fn default_heading_level() -> u8 {
    2
}
fn default_paragraph_size() -> u8 {
    16
}
fn default_button_bg() -> String {
    "#4f46e5".into()
}
fn default_button_text() -> String {
    "#ffffff".into()
}
fn default_align_center() -> TextAlign {
    TextAlign::Center
}
fn default_divider_color() -> String {
    "#e5e7eb".into()
}
fn default_spacer_height() -> u32 {
    24
}

/// Render the block array into one self-contained HTML string
/// suitable for an email body. Wraps the content in a 600-px-
/// max centered table that collapses to 100% width on
/// narrower viewports (mobile mail clients) so the operator
/// doesn't have to author the email-safe boilerplate AND the
/// recipient doesn't horizontal-scroll on a phone.
///
/// The `viewport` meta tag tells iOS Mail / Gmail to render at
/// device width instead of zooming out to fit the 600px
/// canvas. The `mso-table-lspace` / `mso-table-rspace` zeros
/// strip Outlook's default 1.5pt cell padding that breaks
/// pixel-precise layouts.
pub fn render_template(blocks: &[EmailBlock], vars: &HashMap<String, String>) -> String {
    render_template_with_bg(blocks, vars, None)
}

/// Same as `render_template` but takes an operator-set page
/// background colour (and optional image, via the `_image`
/// helper). `None` falls back to the conventional `#f5f5f5`
/// (the email-on-page card look). Caller-supplied values pass
/// through the sanitisers so a malformed string can't smuggle
/// CSS injection.
pub fn render_template_with_bg(
    blocks: &[EmailBlock],
    vars: &HashMap<String, String>,
    page_background: Option<&str>,
) -> String {
    render_template_with_page_bg(blocks, vars, page_background, None)
}

/// Full-fat helper — colour + image. The two-arg variant
/// above stays for callers that haven't surfaced an image
/// field yet (compose pre-image, tests). Image URL gates on
/// `sanitize_url` (http/https only); colour gates on
/// `sanitize_color` (default `#f5f5f5`).
pub fn render_template_with_page_bg(
    blocks: &[EmailBlock],
    vars: &HashMap<String, String>,
    page_background: Option<&str>,
    page_background_image: Option<&str>,
) -> String {
    let body = blocks.iter().map(|b| render_block(b, vars)).collect::<String>();
    let bg_color = page_background
        .map(|c| sanitize_color(c, "#f5f5f5"))
        .unwrap_or_else(|| "#f5f5f5".to_string());
    let bg_image = page_background_image.and_then(sanitize_url);
    // Build the page-level background fragment. We always emit
    // a `background:` colour so dark-mode + missing-image
    // fallback both work; image stacks on top via shorthand.
    let img_css = bg_image
        .as_ref()
        .map(|u| format!("background-image:url({u});background-size:cover;background-position:center;background-repeat:no-repeat;"))
        .unwrap_or_default();
    let img_attr = bg_image
        .as_ref()
        .map(|u| format!(r#" background="{}""#, escape_html_attr(u)))
        .unwrap_or_default();
    format!(
        r#"<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd"><html xmlns="http://www.w3.org/1999/xhtml"><head><meta http-equiv="Content-Type" content="text/html; charset=UTF-8"/><meta name="viewport" content="width=device-width,initial-scale=1"/><meta name="color-scheme" content="light dark"/><meta name="supported-color-schemes" content="light dark"/><style type="text/css">:root{{color-scheme:light dark;supported-color-schemes:light dark}}table{{mso-table-lspace:0;mso-table-rspace:0}}img{{max-width:100%;height:auto;display:block}}@media only screen and (max-width:520px){{.email-col{{display:block!important;width:100%!important;padding:0 0 12px 0!important}}}}@media (prefers-color-scheme:dark){{body,.email-bg{{background:#1a1a1a!important}}.email-card{{background:#262626!important}}.ed-h{{color:#f3f4f6!important}}.ed-p{{color:#cbd5e1!important}}.email-divider{{border-color:#404040!important}}}}</style></head><body class="email-bg" style="margin:0;padding:0;background:{bg_color};{img_css}"{img_attr}><table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" class="email-bg" style="background:{bg_color};{img_css}"{img_attr}><tr><td align="center" style="padding:24px 12px;"><table role="presentation" cellpadding="0" cellspacing="0" border="0" class="email-card" style="max-width:600px;width:100%;background:#ffffff;font-family:-apple-system,BlinkMacSystemFont,Segoe UI,Roboto,Helvetica,Arial,sans-serif;color:#1a1a1a;"><tbody>{body}</tbody></table></td></tr></table></body></html>"#
    )
}

/// Render a single block as a `<tr>...</tr>` (the parent
/// renderer wraps everything in a 600px table). TwoColumn is
/// the one exception — emits its own nested table inside a tr.
pub fn render_block(block: &EmailBlock, vars: &HashMap<String, String>) -> String {
    match block {
        EmailBlock::Heading { text, text_html, level, color, align } => {
            // Prefer the rich-text body when set. The sanitizer
            // already ran when the operator saved; we run it
            // again defensively so a forged blocks_json can't
            // sneak `<script>` past the renderer.
            let body = match text_html.as_deref() {
                Some(html) if !html.is_empty() => {
                    substitute_vars(&crate::email_template::email_safe_html(html), vars)
                }
                _ => {
                    let plain = substitute_vars(text, vars);
                    escape_html(&plain)
                }
            };
            let level = match *level {
                1 | 2 | 3 => *level,
                _ => 2,
            };
            let size = match level {
                1 => 32,
                2 => 24,
                _ => 20,
            };
            // `ed-h` ("email default heading") is the dark-mode
            // hook. Only emitted when the operator left color
            // unset — so a operator-chosen colour wins via the
            // `style` attribute (inline non-!important > class
            // rule under dark-mode media query, since the class
            // rule omits `!important` for operator-set values
            // and only the default branch carries the class).
            let (color, class_attr) = match color.as_deref() {
                Some(c) => (c, ""),
                None => ("#1a1a1a", r#" class="ed-h""#),
            };
            format!(
                r#"<tr><td style="padding:12px 24px;text-align:{align};"><h{level}{class_attr} style="margin:0;font-size:{size}px;line-height:1.3;color:{color};font-weight:600;">{body}</h{level}></td></tr>"#,
                align = align.as_str(),
            )
        }
        EmailBlock::Paragraph { text, text_html, color, align, font_size } => {
            let body = match text_html.as_deref() {
                Some(html) if !html.is_empty() => {
                    substitute_vars(&crate::email_template::email_safe_html(html), vars)
                }
                _ => {
                    let plain = substitute_vars(text, vars);
                    escape_html(&plain).replace('\n', "<br/>")
                }
            };
            // Same dark-mode opt-in pattern as Heading: the
            // `ed-p` class is only emitted on the default-colour
            // branch so an operator-chosen colour wins inline.
            let (color, class_attr) = match color.as_deref() {
                Some(c) => (c, ""),
                None => ("#374151", r#" class="ed-p""#),
            };
            let size = (*font_size).clamp(10, 32);
            format!(
                r#"<tr><td style="padding:8px 24px;text-align:{align};"><p{class_attr} style="margin:0;font-size:{size}px;line-height:1.5;color:{color};">{body}</p></td></tr>"#,
                align = align.as_str(),
            )
        }
        EmailBlock::Button { text, url, bg_color, text_color, align } => {
            let text = substitute_vars(text, vars);
            let text = escape_html(&text);
            let url = substitute_vars(url, vars);
            let url_attr = escape_html_attr(&url);
            let bg = sanitize_color(bg_color, "#4f46e5");
            let fg = sanitize_color(text_color, "#ffffff");
            format!(
                r#"<tr><td style="padding:16px 24px;text-align:{align};"><a href="{url_attr}" style="display:inline-block;padding:12px 28px;background:{bg};color:{fg};text-decoration:none;border-radius:6px;font-weight:600;font-size:15px;">{text}</a></td></tr>"#,
                align = align.as_str(),
            )
        }
        EmailBlock::Image { url, alt, width, align, link_url, embed } => {
            let url = substitute_vars(url, vars);
            // CID embed rewrites the src to `cid:{sha}` so the
            // recipient's mail client resolves to the inline
            // multipart/related part the publisher attached.
            // The SHA is the last URL segment minus extension —
            // see `extract_sha_from_asset_url` for the parser.
            let final_src = match embed {
                ImageEmbed::Cid => match extract_sha_from_asset_url(&url) {
                    Some(sha) => format!("cid:{sha}"),
                    None => url.clone(), // fallback: send URL anyway
                },
                ImageEmbed::Url => url.clone(),
            };
            let url_attr = escape_html_attr(&final_src);
            let alt = escape_html_attr(alt);
            // Alt-fallback styling — props applied to the
            // `<img>` propagate to the placeholder rendering
            // when the client blocks images (Outlook default,
            // Gmail "ask before showing"). Without these the
            // alt text lands as 12-px Times-New-Roman in a
            // blue-bordered box; with them it reads like a
            // styled message:
            //   border:0  → kill Outlook's blue link-image border
            //   display:block → kill the 4-px baseline gap
            //   font-family/size/style/color → style the alt
            //     text the recipient sees on a blocked-image
            let base_style = "border:0;display:block;outline:none;\
                              font-family:-apple-system,BlinkMacSystemFont,Segoe UI,Roboto,Helvetica,Arial,sans-serif;\
                              font-size:14px;font-style:italic;color:#6b7280;line-height:1.4;";
            let (width_attr, size_style) = match width {
                Some(w) => (
                    format!(r#" width="{w}""#),
                    format!("max-width:{w}px;width:100%;"),
                ),
                None => (String::new(), "max-width:100%;".to_string()),
            };
            let img_tag = format!(
                r#"<img src="{url_attr}" alt="{alt}"{width_attr} style="{base_style}{size_style}"/>"#
            );
            let inner = match link_url {
                Some(link) if !link.is_empty() => {
                    let link = substitute_vars(link, vars);
                    let link_attr = escape_html_attr(&link);
                    format!(r#"<a href="{link_attr}">{img_tag}</a>"#)
                }
                _ => img_tag,
            };
            format!(
                r#"<tr><td style="padding:8px 24px;text-align:{align};">{inner}</td></tr>"#,
                align = align.as_str(),
            )
        }
        EmailBlock::Divider { color } => {
            let color = sanitize_color(color, "#e5e7eb");
            // Same dark-mode opt-in as Heading / Paragraph:
            // class is only emitted when the colour is the
            // bundled default so operator-chosen colours win.
            let class_attr = if color == "#e5e7eb" {
                r#" class="email-divider""#
            } else {
                ""
            };
            format!(
                r#"<tr><td style="padding:12px 24px;"><hr{class_attr} style="border:0;border-top:1px solid {color};margin:0;"/></td></tr>"#
            )
        }
        EmailBlock::Spacer { height_px } => {
            let h = (*height_px).clamp(4, 200);
            format!(
                r#"<tr><td style="height:{h}px;line-height:{h}px;font-size:0;">&nbsp;</td></tr>"#
            )
        }
        EmailBlock::TwoColumn { left, right } => {
            let left_html = left.iter().map(|b| render_block(b, vars)).collect::<String>();
            let right_html = right.iter().map(|b| render_block(b, vars)).collect::<String>();
            // Two nested tables side by side. `valign="top"` so
            // unequal heights don't push one column to the middle.
            // Width 50% each on desktop; the `email-col` class
            // hooks the head <style> media query that stacks them
            // to 100% on viewports < 520px (Gmail mobile, Apple
            // Mail iOS, Outlook iOS — Outlook desktop ignores
            // <style> but its viewport is wide, so the 50/50 lands
            // ~270px per column which is still readable).
            format!(
                r#"<tr><td style="padding:8px 12px;"><table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0"><tr><td valign="top" width="50%" class="email-col" style="padding:0 12px 0 0;"><table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0"><tbody>{left_html}</tbody></table></td><td valign="top" width="50%" class="email-col" style="padding:0 0 0 12px;"><table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0"><tbody>{right_html}</tbody></table></td></tr></table></td></tr>"#
            )
        }
        EmailBlock::Row { columns, background, background_image } => {
            if columns.is_empty() {
                return r#"<tr><td style="padding:4px 12px;height:24px;"></td></tr>"#
                    .to_string();
            }
            let widths = normalize_column_widths(columns);
            let mut tds = String::new();
            for (i, col) in columns.iter().enumerate() {
                let inner = col
                    .blocks
                    .iter()
                    .map(|b| render_block(b, vars))
                    .collect::<String>();
                let pad_left = if i == 0 { 0 } else { 12 };
                let pad_right = if i == columns.len() - 1 { 0 } else { 12 };
                let w = widths[i];
                // Per-column background — sanitised so a
                // malicious string ("expression()", javascript
                // url, …) can't slip into the inline style.
                // Returns both the inline CSS and the legacy
                // `background="URL"` HTML attr for clients that
                // ignore CSS-set background-images.
                let (col_bg, col_attr) = build_bg(
                    col.background.as_deref(),
                    col.background_image.as_deref(),
                );
                tds.push_str(&format!(
                    r#"<td valign="top" width="{w}%" class="email-col"{col_attr} style="{col_bg}padding:0 {pad_right}px 0 {pad_left}px;"><table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0"><tbody>{inner}</tbody></table></td>"#
                ));
            }
            // Row-level background lives on the outer TD that
            // wraps the column-table; covers the whole row
            // band (operator's intent for a "section" colour).
            let (row_bg, row_attr) = build_bg(
                background.as_deref(),
                background_image.as_deref(),
            );
            format!(
                r#"<tr><td{row_attr} style="{row_bg}padding:8px 12px;"><table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0"><tr>{tds}</tr></table></td></tr>"#
            )
        }
        EmailBlock::List { items, ordered, color } => {
            let tag = if *ordered { "ol" } else { "ul" };
            let color = color.as_deref().unwrap_or("#374151");
            let list_items = items
                .iter()
                .map(|i| {
                    let i = substitute_vars(i, vars);
                    format!(
                        r#"<li style="margin:4px 0;font-size:16px;line-height:1.5;color:{color};">{}</li>"#,
                        escape_html(&i)
                    )
                })
                .collect::<String>();
            format!(
                r#"<tr><td style="padding:8px 24px 8px 40px;"><{tag} style="margin:0;padding:0 0 0 16px;">{list_items}</{tag}></td></tr>"#
            )
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────

/// One block-walk pass that collects every `Image` whose
/// `embed` is `Cid`. The compose / approve handlers call this
/// after rendering to know which assets to load from the
/// `AssetStore` and attach as `multipart/related` parts.
///
/// The walk recurses into `TwoColumn` so an image nested in
/// either column is included. Returned in document order so
/// the attachment list mirrors the HTML's rendering order
/// (helps Outlook's heuristic that walks parts top-to-bottom).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImageRef {
    pub sha256: String,
    pub content_id: String,
    /// MIME may not be known until the asset is loaded; the
    /// caller fills it in then. Kept here so the helper can
    /// inherit the file extension hint from the URL.
    pub url_extension_hint: Option<String>,
}

pub fn collect_inline_image_refs(
    blocks: &[EmailBlock],
    vars: &HashMap<String, String>,
) -> Vec<InlineImageRef> {
    let mut out = Vec::new();
    walk_for_inline(blocks, vars, &mut out);
    out
}

fn walk_for_inline(
    blocks: &[EmailBlock],
    vars: &HashMap<String, String>,
    out: &mut Vec<InlineImageRef>,
) {
    for b in blocks {
        match b {
            EmailBlock::Image { url, embed: ImageEmbed::Cid, .. } => {
                let url = substitute_vars(url, vars);
                if let Some(sha) = extract_sha_from_asset_url(&url) {
                    let ext = url
                        .rsplit_once('.')
                        .map(|(_, e)| e.to_string())
                        .filter(|e| e.len() <= 5);
                    out.push(InlineImageRef {
                        // CID == SHA so the render and the
                        // attachment list always agree without
                        // a separate id table.
                        content_id: sha.clone(),
                        sha256: sha,
                        url_extension_hint: ext,
                    });
                }
            }
            EmailBlock::TwoColumn { left, right } => {
                walk_for_inline(left, vars, out);
                walk_for_inline(right, vars, out);
            }
            EmailBlock::Row { columns, .. } => {
                for col in columns {
                    walk_for_inline(&col.blocks, vars, out);
                }
            }
            _ => {}
        }
    }
}

/// Extract the SHA-256 from an asset URL of the shape
/// `{base}/t/asset/{tenant}/{sha}.{ext}`. Returns `None` if
/// the path doesn't match — handler keeps the URL src in
/// that case (fallback). Lowercases so the cid: ref matches
/// the AssetStore's lowercase keying.
fn extract_sha_from_asset_url(url: &str) -> Option<String> {
    let last = url.rsplit('/').next()?;
    let stem = last.split('.').next()?;
    if stem.len() == 64 && stem.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(stem.to_ascii_lowercase())
    } else {
        None
    }
}

/// Convert a legacy flat-blocks template into the Row /
/// Column model. Each top-level non-Row block becomes a
/// 1-column Row of width 100; existing Rows pass through
/// untouched. `TwoColumn` is rewritten to a 2-column Row at
/// 50/50 so the canvas treats both shapes uniformly. Idempotent
/// — running on already-migrated data is a no-op (every
/// block is already a Row, so nothing changes).
///
/// Called at API load time so the editor always sees Rows.
/// Saves go back as Rows, completing the migration on the
/// first round-trip.
pub fn migrate_legacy_to_rows(blocks: Vec<EmailBlock>) -> Vec<EmailBlock> {
    let mut out: Vec<EmailBlock> = Vec::with_capacity(blocks.len());
    for b in blocks {
        match b {
            EmailBlock::Row { .. } => out.push(b),
            EmailBlock::TwoColumn { left, right } => {
                out.push(EmailBlock::Row {
                    columns: vec![
                        Column {
                            blocks: migrate_legacy_to_rows_inner(left),
                            width_pct: 50,
                            background: None,
                            background_image: None,
                        },
                        Column {
                            blocks: migrate_legacy_to_rows_inner(right),
                            width_pct: 50,
                            background: None,
                            background_image: None,
                        },
                    ],
                    background: None,
                    background_image: None,
                });
            }
            other => {
                out.push(EmailBlock::Row {
                    columns: vec![Column {
                        blocks: vec![other],
                        width_pct: 100,
                        background: None,
                        background_image: None,
                    }],
                    background: None,
                    background_image: None,
                });
            }
        }
    }
    out
}

/// Same as `migrate_legacy_to_rows` but keeps non-Row blocks
/// flat — used for the contents INSIDE a column / TwoColumn
/// pane where wrapping each element in another Row would
/// create infinite nesting.
fn migrate_legacy_to_rows_inner(blocks: Vec<EmailBlock>) -> Vec<EmailBlock> {
    blocks
}

/// Spread the operator-set `width_pct` values across the
/// columns so they sum to exactly 100. Operators usually
/// pick from preset layouts (50/50, 33/33/33, etc.) that sum
/// correctly; this guard catches manual fiddling and legacy
/// data so a malformed template still renders sane widths
/// instead of overflowing or under-filling the row.
fn normalize_column_widths(columns: &[Column]) -> Vec<u8> {
    if columns.is_empty() {
        return Vec::new();
    }
    let raw: Vec<u32> = columns
        .iter()
        .map(|c| c.width_pct.max(1) as u32)
        .collect();
    let sum: u32 = raw.iter().sum();
    if sum == 100 {
        return raw.iter().map(|&w| w as u8).collect();
    }
    // Scale proportionally; round; redistribute the rounding
    // delta to the widest column so the total lands at 100.
    let mut out: Vec<u32> =
        raw.iter().map(|&w| (w * 100) / sum.max(1)).collect();
    let total: u32 = out.iter().sum();
    if total < 100 {
        // Funnel the missing % to the widest column.
        let widest = out
            .iter()
            .enumerate()
            .max_by_key(|(_, &w)| w)
            .map(|(i, _)| i)
            .unwrap_or(0);
        out[widest] += 100 - total;
    } else if total > 100 {
        let widest = out
            .iter()
            .enumerate()
            .max_by_key(|(_, &w)| w)
            .map(|(i, _)| i)
            .unwrap_or(0);
        out[widest] = out[widest].saturating_sub(total - 100);
    }
    out.iter().map(|&w| w.min(100) as u8).collect()
}

fn substitute_vars(template: &str, vars: &HashMap<String, String>) -> String {
    if !template.contains("{{") {
        return template.to_string();
    }
    let mut out = template.to_string();
    for (k, v) in vars {
        let needle = format!("{{{{{k}}}}}");
        out = out.replace(&needle, v);
    }
    out
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_html_attr(s: &str) -> String {
    escape_html(s).replace('"', "&quot;")
}

/// Build the `background:` CSS shorthand for a row/column/page
/// taking both an optional colour and an optional image. The
/// image URL passes through `sanitize_url` so a malicious
/// template can't inject `javascript:` / break out of the
/// `url(...)` quoting. The colour passes through
/// `sanitize_color` (the caller can pass `"transparent"` as
/// default and we filter it out so we don't emit a useless
/// rule).
///
/// Returns `(style_fragment, html_attr)`:
/// - `style_fragment` is the inline CSS, ends with a semicolon
///   so the caller can concatenate, empty when both inputs
///   are absent / rejected.
/// - `html_attr` is the legacy `background="URL"` attribute
///   for clients that ignore CSS background-image (Outlook
///   2007+ desktop, some older webmail). Empty when no image.
fn build_bg(color: Option<&str>, image: Option<&str>) -> (String, String) {
    let col = color
        .map(|c| sanitize_color(c, "transparent"))
        .filter(|c| c != "transparent");
    let img = image.and_then(sanitize_url);
    let mut style = String::new();
    if let Some(c) = &col {
        style.push_str(&format!("background-color:{c};"));
    }
    if let Some(u) = &img {
        // url() with no quotes — sanitize_url already banned
        // every char that could break it. background-size:cover
        // + center positioning is the most common operator
        // intent for a "hero" or section bg.
        style.push_str(&format!(
            "background-image:url({u});background-size:cover;background-position:center;background-repeat:no-repeat;"
        ));
    }
    let attr = match &img {
        Some(u) => format!(r#" background="{}""#, escape_html_attr(u)),
        None => String::new(),
    };
    (style, attr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn heading_renders_with_level_and_color() {
        let b = EmailBlock::Heading {
            text: "Hola Juan".into(),
            text_html: None,
            level: 1,
            color: Some("#000000".into()),
            align: TextAlign::Center,
        };
        let out = render_block(&b, &HashMap::new());
        assert!(out.contains("<h1"));
        assert!(out.contains("Hola Juan"));
        assert!(out.contains("text-align:center"));
        assert!(out.contains("#000000"));
    }

    #[test]
    fn heading_invalid_level_falls_back_to_h2() {
        let b = EmailBlock::Heading {
            text: "x".into(),
            text_html: None,
            level: 7,
            color: None,
            align: TextAlign::default(),
        };
        let out = render_block(&b, &HashMap::new());
        assert!(out.contains("<h2"));
    }

    #[test]
    fn variables_substitute_in_text() {
        let b = EmailBlock::Paragraph {
            text: "Hola {{name}}, gracias.".into(),
            text_html: None,
            color: None,
            align: TextAlign::Left,
            font_size: 16,
        };
        let out = render_block(&b, &vars(&[("name", "Camila")]));
        assert!(out.contains("Hola Camila, gracias."));
    }

    #[test]
    fn rich_text_html_renders_inline_formatting() {
        let b = EmailBlock::Paragraph {
            text: "fallback".into(),
            text_html: Some(
                "Hola <strong>{{name}}</strong>, gracias.".into(),
            ),
            color: None,
            align: TextAlign::Left,
            font_size: 16,
        };
        let out = render_block(&b, &vars(&[("name", "Camila")]));
        assert!(out.contains("<strong>Camila</strong>"));
        // Plain `text` ignored when text_html present.
        assert!(!out.contains("fallback"));
    }

    #[test]
    fn rich_text_html_strips_script_defensively() {
        // Even if a forged blocks_json carries a `<script>` in
        // text_html, the renderer's defensive sanitizer drops
        // it before output.
        let b = EmailBlock::Paragraph {
            text: "x".into(),
            text_html: Some("<script>alert(1)</script>safe".into()),
            color: None,
            align: TextAlign::Left,
            font_size: 16,
        };
        let out = render_block(&b, &HashMap::new());
        assert!(!out.contains("<script"));
        assert!(out.contains("safe"));
    }

    #[test]
    fn unknown_variables_pass_through() {
        let b = EmailBlock::Paragraph {
            text: "Hi {{unknown}} there".into(),
            text_html: None,
            color: None,
            align: TextAlign::Left,
            font_size: 16,
        };
        let out = render_block(&b, &HashMap::new());
        assert!(out.contains("{{unknown}}"));
    }

    #[test]
    fn html_escapes_user_text() {
        let b = EmailBlock::Heading {
            text: "<script>alert(1)</script>".into(),
            text_html: None,
            level: 2,
            color: None,
            align: TextAlign::default(),
        };
        let out = render_block(&b, &HashMap::new());
        assert!(!out.contains("<script"));
        assert!(out.contains("&lt;script&gt;"));
    }

    #[test]
    fn button_renders_as_anchor_tag() {
        let b = EmailBlock::Button {
            text: "Click".into(),
            url: "https://example.com".into(),
            bg_color: "#ff0000".into(),
            text_color: "#ffffff".into(),
            align: TextAlign::Center,
        };
        let out = render_block(&b, &HashMap::new());
        assert!(out.contains("<a "));
        assert!(out.contains("href=\"https://example.com\""));
        assert!(out.contains("background:#ff0000"));
    }

    #[test]
    fn button_url_substitutes_vars() {
        let b = EmailBlock::Button {
            text: "Demo".into(),
            url: "https://app.com/u/{{token}}".into(),
            bg_color: "#000".into(),
            text_color: "#fff".into(),
            align: TextAlign::Left,
        };
        let out = render_block(&b, &vars(&[("token", "abc123")]));
        assert!(out.contains("/u/abc123"));
    }

    #[test]
    fn malicious_color_falls_back_to_default() {
        let b = EmailBlock::Button {
            text: "Click".into(),
            url: "x".into(),
            bg_color: "javascript:alert(1)".into(),
            text_color: "#fff".into(),
            align: TextAlign::Left,
        };
        let out = render_block(&b, &HashMap::new());
        // Default button color is the fallback, not the
        // malicious value.
        assert!(!out.contains("javascript"));
        assert!(out.contains("#4f46e5"));
    }

    #[test]
    fn image_with_link_wraps_in_anchor() {
        let b = EmailBlock::Image {
            url: "https://x.com/banner.png".into(),
            alt: "Banner".into(),
            width: Some(300),
            align: TextAlign::Center,
            link_url: Some("https://example.com/promo".into()),
            embed: ImageEmbed::Url,
        };
        let out = render_block(&b, &HashMap::new());
        assert!(out.contains("<a href=\"https://example.com/promo\">"));
        assert!(out.contains("<img src=\"https://x.com/banner.png\""));
        assert!(out.contains("width=\"300\""));
    }

    #[test]
    fn divider_emits_hr() {
        let b = EmailBlock::Divider {
            color: "#cccccc".into(),
        };
        let out = render_block(&b, &HashMap::new());
        assert!(out.contains("<hr"));
        assert!(out.contains("#cccccc"));
    }

    #[test]
    fn spacer_clamps_to_safe_range() {
        let big = EmailBlock::Spacer { height_px: 9999 };
        let small = EmailBlock::Spacer { height_px: 1 };
        let out_big = render_block(&big, &HashMap::new());
        let out_small = render_block(&small, &HashMap::new());
        assert!(out_big.contains("height:200px"));
        assert!(out_small.contains("height:4px"));
    }

    #[test]
    fn two_column_nests_blocks_side_by_side() {
        let b = EmailBlock::TwoColumn {
            left: vec![EmailBlock::Paragraph {
                text: "left side".into(),
                text_html: None,
                color: None,
                align: TextAlign::Left,
                font_size: 14,
            }],
            right: vec![EmailBlock::Paragraph {
                text: "right side".into(),
                text_html: None,
                color: None,
                align: TextAlign::Right,
                font_size: 14,
            }],
        };
        let out = render_block(&b, &HashMap::new());
        assert!(out.contains("left side"));
        assert!(out.contains("right side"));
        // Two cells with valign=top.
        assert_eq!(out.matches("valign=\"top\"").count(), 2);
    }

    #[test]
    fn list_renders_ul_or_ol() {
        let ul = EmailBlock::List {
            items: vec!["one".into(), "two".into()],
            ordered: false,
            color: None,
        };
        let ol = EmailBlock::List {
            items: vec!["a".into(), "b".into()],
            ordered: true,
            color: None,
        };
        let ul_out = render_block(&ul, &HashMap::new());
        let ol_out = render_block(&ol, &HashMap::new());
        assert!(ul_out.contains("<ul"));
        assert!(ol_out.contains("<ol"));
        assert_eq!(ul_out.matches("<li").count(), 2);
    }

    #[test]
    fn render_template_wraps_in_600px_table() {
        let blocks = vec![EmailBlock::Heading {
            text: "Hi".into(),
            text_html: None,
            level: 1,
            color: None,
            align: TextAlign::default(),
        }];
        let out = render_template(&blocks, &HashMap::new());
        assert!(out.contains("<!DOCTYPE"));
        // Responsive: max-width:600px lets clients < 600px scale.
        assert!(out.contains("max-width:600px"));
        // viewport meta tells iOS Mail / Gmail to render at device
        // width — without it the email zooms out to fit the canvas.
        assert!(out.contains(r#"name="viewport""#));
        assert!(out.contains("<h1"));
        assert!(out.contains("Hi"));
    }

    /// Dark-mode hooks: meta tags + media query + opt-in class
    /// on default-colour heading. Operator-set colour does NOT
    /// receive the class (so an explicit picker choice wins
    /// over the dark-mode override).
    #[test]
    fn dark_mode_hooks_only_on_default_colours() {
        let blocks = vec![
            EmailBlock::Heading {
                text: "Default".into(),
                text_html: None,
                level: 1,
                color: None,
                align: TextAlign::default(),
            },
            EmailBlock::Heading {
                text: "Operator".into(),
                text_html: None,
                level: 2,
                color: Some("#ff0000".into()),
                align: TextAlign::default(),
            },
            EmailBlock::Paragraph {
                text: "p".into(),
                text_html: None,
                color: None,
                align: TextAlign::default(),
                font_size: 16,
            },
        ];
        let out = render_template(&blocks, &HashMap::new());
        // Meta + supported-color-schemes + media query present.
        assert!(out.contains(r#"name="color-scheme""#));
        assert!(out.contains("prefers-color-scheme:dark"));
        assert!(out.contains(".email-card"));
        // Wrapper backgrounds are class-tagged for inversion.
        assert!(out.contains(r#"class="email-bg""#));
        assert!(out.contains(r#"class="email-card""#));
        // Default-coloured Heading + Paragraph carry the
        // ed-h / ed-p hook; the operator-coloured Heading does
        // not (otherwise dark mode would override the picker).
        assert!(out.contains(r#"<h1 class="ed-h""#));
        assert!(!out.contains(r#"<h2 class="ed-h""#));
        assert!(out.contains(r#"<p class="ed-p""#));
    }

    /// Image carries alt-fallback styling so blocked-image
    /// placeholders render readable instead of 12-px Times
    /// New Roman in a blue-bordered box.
    #[test]
    /// CID embed rewrites the `<img src>` to `cid:{sha}` and
    /// `collect_inline_image_refs` returns the SHA so the
    /// publisher knows to attach the bytes as a related part.
    #[test]
    fn cid_embed_rewrites_src_and_collects_ref() {
        let sha = "a".repeat(64);
        let url = format!("https://h.example/t/asset/tenant1/{sha}.png");
        let blocks = vec![EmailBlock::Image {
            url: url.clone(),
            alt: "logo".into(),
            width: None,
            align: TextAlign::default(),
            link_url: None,
            embed: ImageEmbed::Cid,
        }];
        let html = render_template(&blocks, &HashMap::new());
        // Public URL gone; cid: ref present.
        assert!(html.contains(&format!("cid:{sha}")), "cid src missing in: {html}");
        assert!(!html.contains(&url), "url should not appear when CID");
        let refs = collect_inline_image_refs(&blocks, &HashMap::new());
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].sha256, sha);
        assert_eq!(refs[0].content_id, sha);
        assert_eq!(refs[0].url_extension_hint.as_deref(), Some("png"));
    }

    /// CID inside a TwoColumn still gets collected.
    #[test]
    fn cid_embed_in_twocolumn_collected() {
        let sha = "b".repeat(64);
        let url = format!("https://h.example/t/asset/t/{sha}.jpg");
        let blocks = vec![EmailBlock::TwoColumn {
            left: vec![EmailBlock::Image {
                url,
                alt: "left".into(),
                width: None,
                align: TextAlign::default(),
                link_url: None,
                embed: ImageEmbed::Cid,
            }],
            right: vec![],
        }];
        let refs = collect_inline_image_refs(&blocks, &HashMap::new());
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].sha256, sha);
    }

    fn image_has_alt_fallback_styling() {
        let blocks = vec![EmailBlock::Image {
            url: "https://x.com/banner.png".into(),
            alt: "company logo".into(),
            width: Some(300),
            align: TextAlign::default(),
            link_url: None,
            embed: ImageEmbed::Url,
        }];
        let out = render_template(&blocks, &HashMap::new());
        // border:0 kills Outlook's blue link-image border.
        assert!(out.contains("border:0"));
        // display:block kills the 4-px baseline gap.
        assert!(out.contains("display:block"));
        // Alt-text styling props that propagate to the
        // blocked-image placeholder.
        assert!(out.contains("font-style:italic"));
        assert!(out.contains("font-size:14px"));
    }

    #[test]
    /// Row renders N columns side-by-side with normalised
    /// widths. Two columns at 60/40 stay 60/40; weird sums
    /// round to a clean 100.
    #[test]
    fn row_renders_columns_with_widths() {
        let blocks = vec![EmailBlock::Row {
            columns: vec![
                Column {
                    blocks: vec![EmailBlock::Heading {
                        text: "L".into(),
                        text_html: None,
                        level: 1,
                        color: None,
                        align: TextAlign::default(),
                    }],
                    width_pct: 60,
                    background: None,
                    background_image: None,
                },
                Column {
                    blocks: vec![EmailBlock::Paragraph {
                        text: "R".into(),
                        text_html: None,
                        color: None,
                        align: TextAlign::default(),
                        font_size: 16,
                    }],
                    width_pct: 40,
                    background: None,
                    background_image: None,
                },
            ],
            background: None,
            background_image: None,
        }];
        let html = render_template(&blocks, &HashMap::new());
        assert!(html.contains(r#"width="60%""#));
        assert!(html.contains(r#"width="40%""#));
        assert!(html.contains("<h1"));
        assert!(html.contains("<p"));
        // Mobile-stacking class lands on every column TD.
        assert!(html.matches("email-col").count() >= 2);
    }

    #[test]
    fn row_widths_normalize_to_100() {
        let widths = normalize_column_widths(&[
            Column { blocks: vec![], width_pct: 30, background: None, background_image: None },
            Column { blocks: vec![], width_pct: 30, background: None, background_image: None },
            Column { blocks: vec![], width_pct: 30, background: None, background_image: None },
        ]);
        // 30+30+30=90 → scales then redistributes the 10 to widest.
        let sum: u32 = widths.iter().map(|&w| w as u32).sum();
        assert_eq!(sum, 100);
    }

    #[test]
    fn row_empty_renders_placeholder() {
        let blocks = vec![EmailBlock::Row { columns: vec![], background: None, background_image: None }];
        let html = render_template(&blocks, &HashMap::new());
        // A row with no columns still emits a <tr> so a fresh
        // canvas drop has visual feedback.
        assert!(html.matches("<tr>").count() >= 1);
    }

    #[test]
    fn migrate_legacy_wraps_flat_blocks_in_rows() {
        let legacy = vec![
            EmailBlock::Heading {
                text: "Hi".into(),
                text_html: None,
                level: 1,
                color: None,
                align: TextAlign::default(),
            },
            EmailBlock::Paragraph {
                text: "p".into(),
                text_html: None,
                color: None,
                align: TextAlign::default(),
                font_size: 16,
            },
        ];
        let migrated = migrate_legacy_to_rows(legacy);
        assert_eq!(migrated.len(), 2);
        for b in &migrated {
            match b {
                EmailBlock::Row { columns, .. } => {
                    assert_eq!(columns.len(), 1);
                    assert_eq!(columns[0].width_pct, 100);
                }
                _ => panic!("expected Row, got {b:?}"),
            }
        }
    }

    #[test]
    fn migrate_legacy_rewrites_twocolumn_to_row() {
        let legacy = vec![EmailBlock::TwoColumn {
            left: vec![EmailBlock::Heading {
                text: "L".into(),
                text_html: None,
                level: 1,
                color: None,
                align: TextAlign::default(),
            }],
            right: vec![EmailBlock::Paragraph {
                text: "R".into(),
                text_html: None,
                color: None,
                align: TextAlign::default(),
                font_size: 16,
            }],
        }];
        let migrated = migrate_legacy_to_rows(legacy);
        assert_eq!(migrated.len(), 1);
        match &migrated[0] {
            EmailBlock::Row { columns, .. } => {
                assert_eq!(columns.len(), 2);
                assert_eq!(columns[0].width_pct, 50);
                assert_eq!(columns[1].width_pct, 50);
            }
            other => panic!("expected Row, got {other:?}"),
        }
    }

    #[test]
    fn migrate_legacy_idempotent_on_rows() {
        let already = vec![EmailBlock::Row {
            columns: vec![Column {
                blocks: vec![EmailBlock::Spacer { height_px: 20 }],
                width_pct: 100,
                background: None,
                background_image: None,
            }],
            background: None,
            background_image: None,
        }];
        let migrated = migrate_legacy_to_rows(already.clone());
        assert_eq!(migrated, already);
    }

    #[test]
    fn cid_image_inside_row_collected() {
        let sha = "c".repeat(64);
        let url = format!("https://h.example/t/asset/t/{sha}.png");
        let blocks = vec![EmailBlock::Row {
            columns: vec![Column {
                blocks: vec![EmailBlock::Image {
                    url,
                    alt: "logo".into(),
                    width: None,
                    align: TextAlign::default(),
                    link_url: None,
                    embed: ImageEmbed::Cid,
                }],
                width_pct: 100,
                background: None,
                background_image: None,
            }],
            background: None,
            background_image: None,
        }];
        let refs = collect_inline_image_refs(&blocks, &HashMap::new());
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].sha256, sha);
    }

    #[test]
    fn json_round_trip_preserves_blocks() {
        let blocks = vec![
            EmailBlock::Heading {
                text: "Title".into(),
                text_html: None,
                level: 1,
                color: Some("#000".into()),
                align: TextAlign::Center,
            },
            EmailBlock::Spacer { height_px: 16 },
            EmailBlock::Paragraph {
                text: "Body".into(),
                text_html: None,
                color: None,
                align: TextAlign::Left,
                font_size: 14,
            },
        ];
        let json = serde_json::to_string(&blocks).unwrap();
        let back: Vec<EmailBlock> = serde_json::from_str(&json).unwrap();
        assert_eq!(blocks, back);
    }

    // sanitize_url / sanitize_color tests moved to
    // crates/sanitize once the helpers were lifted to the
    // framework. The integration tests below still exercise
    // them via render_template_with_page_bg.

    #[test]
    fn page_background_image_renders_on_body() {
        let html = render_template_with_page_bg(
            &[],
            &HashMap::new(),
            Some("#fafafa"),
            Some("https://cdn.example/page-bg.jpg"),
        );
        // CSS shorthand on <body> + the wrapper table.
        assert!(html.contains("background-image:url(https://cdn.example/page-bg.jpg)"));
        assert!(html.contains("background-size:cover"));
        // Legacy HTML attr for Outlook desktop / clients that
        // ignore CSS background-image.
        assert!(html.contains(r#"background="https://cdn.example/page-bg.jpg""#));
        // Colour fallback still emitted.
        assert!(html.contains("background:#fafafa"));
    }

    #[test]
    fn row_background_image_renders_on_outer_td() {
        let blocks = vec![EmailBlock::Row {
            columns: vec![Column {
                blocks: vec![],
                width_pct: 100,
                background: None,
                background_image: None,
            }],
            background: Some("#eef".into()),
            background_image: Some("https://cdn.example/row-bg.jpg".into()),
        }];
        let html = render_template(&blocks, &HashMap::new());
        assert!(html.contains("background-color:#eef"));
        assert!(html.contains("background-image:url(https://cdn.example/row-bg.jpg)"));
        assert!(html.contains(r#"background="https://cdn.example/row-bg.jpg""#));
    }

    #[test]
    fn column_background_image_renders_on_column_td() {
        let blocks = vec![EmailBlock::Row {
            columns: vec![Column {
                blocks: vec![],
                width_pct: 100,
                background: None,
                background_image: Some("https://cdn.example/col-bg.jpg".into()),
            }],
            background: None,
            background_image: None,
        }];
        let html = render_template(&blocks, &HashMap::new());
        assert!(html.contains("background-image:url(https://cdn.example/col-bg.jpg)"));
    }

    #[test]
    fn malicious_background_image_dropped() {
        // sanitize_url rejects javascript: → no background-image
        // emitted at all. Colour passes through.
        let blocks = vec![EmailBlock::Row {
            columns: vec![],
            background: Some("#fff".into()),
            background_image: Some("javascript:alert(1)".into()),
        }];
        let html = render_template(&blocks, &HashMap::new());
        assert!(!html.contains("javascript:"));
        assert!(!html.contains("background-image"));
    }
}
