//! The little HTML the Studio's long-form strings carry.
//!
//! `*.infoBody` keys are written as web markup — `<strong>`, `<br>`, the odd
//! link — because the web renders them into a modal with `innerHTML`. Rather
//! than strip the tags and lose the structure (these texts are lists of "term:
//! explanation" pairs, and the terms carry the scanning), this turns them into
//! egui rich text: bold runs, paragraph breaks, and real links.
//!
//! It is not an HTML parser and does not try to be. It handles exactly the tags
//! these strings use, and anything else is shown as the text it is.

use egui::{RichText, Ui};

use super::theme;

/// One piece of a rendered line.
enum Piece {
    Text(String),
    Strong(String),
    Link { text: String, href: String },
}

/// Split a body into lines (on `<br>`) of styled pieces.
fn parse(body: &str) -> Vec<Vec<Piece>> {
    let mut lines = vec![Vec::new()];
    let mut rest = body;
    let mut strong = false;
    let mut link: Option<String> = None;
    let mut buffer = String::new();

    let flush =
        |buffer: &mut String, strong: bool, link: &Option<String>, lines: &mut Vec<Vec<Piece>>| {
            if buffer.is_empty() {
                return;
            }
            let text = std::mem::take(buffer);
            let piece = match (link, strong) {
                (Some(href), _) => Piece::Link {
                    text,
                    href: href.clone(),
                },
                (None, true) => Piece::Strong(text),
                (None, false) => Piece::Text(text),
            };
            lines.last_mut().expect("one line").push(piece);
        };

    while let Some(open) = rest.find('<') {
        buffer.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            // An unclosed angle bracket is text, not a tag.
            buffer.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let tag = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        let name = tag
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match name.as_str() {
            "br" | "br/" => {
                flush(&mut buffer, strong, &link, &mut lines);
                lines.push(Vec::new());
            }
            "strong" | "b" | "em" | "i" => {
                flush(&mut buffer, strong, &link, &mut lines);
                strong = true;
            }
            "/strong" | "/b" | "/em" | "/i" => {
                flush(&mut buffer, strong, &link, &mut lines);
                strong = false;
            }
            "a" => {
                flush(&mut buffer, strong, &link, &mut lines);
                link = href_of(tag);
            }
            "/a" => {
                flush(&mut buffer, strong, &link, &mut lines);
                link = None;
            }
            // Any other tag is dropped and its content kept: losing a wrapper
            // is better than showing its angle brackets.
            _ => flush(&mut buffer, strong, &link, &mut lines),
        }
    }
    buffer.push_str(rest);
    flush(&mut buffer, strong, &link, &mut lines);
    lines
}

fn href_of(tag: &str) -> Option<String> {
    let at = tag.find("href=")? + 5;
    let rest = &tag[at..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let end = rest[1..].find(quote)? + 1;
    Some(rest[1..end].to_owned())
}

/// Draw a `*.infoBody` string.
///
/// A line's text and bold runs go into one layout job rather than a label
/// each: separate labels can only wrap *between* them, so a bold term followed
/// by its explanation would break the line at the comma after the term rather
/// than where the paragraph runs out of width. A link interrupts the job,
/// because egui's hyperlink is its own widget.
pub fn info_body(ui: &mut Ui, body: &str) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    for line in parse(body) {
        if line.is_empty() {
            // A `<br><br>` pair is a paragraph break, and the empty line is
            // what carries it.
            ui.add_space(theme::ROW_GAP);
            continue;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = ui.available_width();
            for piece in line {
                match piece {
                    Piece::Text(text) => append(&mut job, &text, &font, theme::TEXT),
                    Piece::Strong(text) => append(&mut job, &text, &font, theme::TEXT_STRONG),
                    Piece::Link { text, href } => {
                        flush(ui, &mut job);
                        ui.hyperlink_to(RichText::new(text).color(theme::ACCENT), href);
                    }
                }
            }
            flush(ui, &mut job);
        });
    }
}

fn append(job: &mut egui::text::LayoutJob, text: &str, font: &egui::FontId, colour: egui::Color32) {
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: font.clone(),
            color: colour,
            ..Default::default()
        },
    );
}

fn flush(ui: &mut Ui, job: &mut egui::text::LayoutJob) {
    if job.is_empty() {
        return;
    }
    ui.label(std::mem::take(job));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten(body: &str) -> Vec<Vec<(&'static str, String)>> {
        parse(body)
            .into_iter()
            .map(|line| {
                line.into_iter()
                    .map(|piece| match piece {
                        Piece::Text(t) => ("text", t),
                        Piece::Strong(t) => ("strong", t),
                        Piece::Link { text, .. } => ("link", text),
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_term_and_its_explanation_keep_their_weights() {
        let lines = flatten("<strong>Host</strong>: the target.<br><br>Second.");
        assert_eq!(
            lines[0],
            vec![
                ("strong", "Host".to_owned()),
                ("text", ": the target.".to_owned())
            ]
        );
        // `<br><br>` is a paragraph break: the empty line between is what
        // carries it.
        assert!(lines[1].is_empty());
        assert_eq!(lines[2], vec![("text", "Second.".to_owned())]);
    }

    #[test]
    fn a_link_keeps_its_text_and_its_target() {
        let lines = parse(r#"see <a href="https://example.test" target="_blank">here</a>."#);
        match &lines[0][1] {
            Piece::Link { text, href } => {
                assert_eq!(text, "here");
                assert_eq!(href, "https://example.test");
            }
            _ => panic!("the anchor did not survive"),
        }
    }

    #[test]
    fn text_that_is_not_markup_survives_unchanged() {
        // A bare angle bracket is text: these strings talk about values.
        let lines = flatten("ratio < 1 means slower");
        assert_eq!(lines[0][0].1, "ratio < 1 means slower");
        // An unknown wrapper loses the tag and keeps the words.
        let lines = flatten("<span class=x>kept</span>");
        assert_eq!(lines[0][0].1, "kept");
    }
}
