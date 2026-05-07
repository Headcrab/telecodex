use html_escape::encode_safe;

pub fn render_markdown_to_html(input: &str) -> String {
    let mut output = String::new();
    let mut in_code_block = false;
    let mut code_block_lang = String::new();

    for line in input.lines() {
        if let Some(rest) = line.strip_prefix("```") {
            if in_code_block {
                output.push_str("</code></pre>");
                in_code_block = false;
                code_block_lang.clear();
            } else {
                code_block_lang = rest.trim().to_string();
                if code_block_lang.is_empty() {
                    output.push_str("<pre><code>");
                } else {
                    output.push_str(&format!(
                        "<pre><code class=\"language-{}\">",
                        encode_safe(&code_block_lang)
                    ));
                }
                in_code_block = true;
            }
            output.push('\n');
            continue;
        }

        if in_code_block {
            output.push_str(&encode_safe(line));
            output.push('\n');
            continue;
        }

        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&render_inline(line));
    }

    if in_code_block {
        output.push_str("</code></pre>");
    }

    if output.is_empty() {
        "&nbsp;".to_string()
    } else {
        output
    }
}

pub fn split_text(input: &str, max_chars: usize) -> Vec<String> {
    if input.chars().count() <= max_chars {
        return vec![input.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for line in input.split_inclusive('\n') {
        let line_len = line.chars().count();
        if current_len + line_len <= max_chars {
            current.push_str(line);
            current_len += line_len;
            continue;
        }

        if !current.is_empty() {
            chunks.push(current);
            current = String::new();
            current_len = 0;
        }

        if line_len <= max_chars {
            current.push_str(line);
            current_len = line_len;
            continue;
        }

        let mut partial = String::new();
        let mut partial_len = 0usize;
        for ch in line.chars() {
            if partial_len >= max_chars {
                chunks.push(partial);
                partial = String::new();
                partial_len = 0;
            }
            partial.push(ch);
            partial_len += 1;
        }
        if !partial.is_empty() {
            current = partial;
            current_len = partial_len;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

fn render_inline(line: &str) -> String {
    let mut output = String::new();
    let chars: Vec<char> = line.chars().collect();
    let mut idx = 0usize;
    let mut bold = false;
    let mut italic = false;
    let mut code = false;

    while idx < chars.len() {
        if chars[idx] == '`' {
            output.push_str(if code { "</code>" } else { "<code>" });
            code = !code;
            idx += 1;
            continue;
        }
        if !code && idx + 1 < chars.len() && chars[idx] == '*' && chars[idx + 1] == '*' {
            output.push_str(if bold { "</b>" } else { "<b>" });
            bold = !bold;
            idx += 2;
            continue;
        }
        if !code && (chars[idx] == '*' || chars[idx] == '_') {
            output.push_str(if italic { "</i>" } else { "<i>" });
            italic = !italic;
            idx += 1;
            continue;
        }
        if !code && chars[idx] == '[' {
            if let Some((rendered, consumed)) = try_render_link(&chars[idx..]) {
                output.push_str(&rendered);
                idx += consumed;
                continue;
            }
        }

        output.push_str(&encode_safe(&chars[idx].to_string()));
        idx += 1;
    }

    if code {
        output.push_str("</code>");
    }
    if italic {
        output.push_str("</i>");
    }
    if bold {
        output.push_str("</b>");
    }

    output
}

fn try_render_link(slice: &[char]) -> Option<(String, usize)> {
    let closing_label = slice.iter().position(|ch| *ch == ']')?;
    let label: String = slice[1..closing_label].iter().collect();
    if slice.get(closing_label + 1) != Some(&'(') {
        return None;
    }
    let url_end = slice[(closing_label + 2)..]
        .iter()
        .position(|ch| *ch == ')')?;
    let url_start = closing_label + 2;
    let url_stop = url_start + url_end;
    let url: String = slice[url_start..url_stop].iter().collect();
    let consumed = url_stop + 1;
    if is_telegram_link_url(&url) {
        Some((
            format!(
                "<a href=\"{}\">{}</a>",
                encode_safe(&url),
                encode_safe(&label)
            ),
            consumed,
        ))
    } else {
        Some((
            format!("{} ({})", encode_safe(&label), encode_safe(&url)),
            consumed,
        ))
    }
}

fn is_telegram_link_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("tg://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_code_blocks() {
        let rendered = render_markdown_to_html("hello\n```rs\nfn main() {}\n```");
        assert!(rendered.contains("<pre><code class=\"language-rs\">"));
    }

    #[test]
    fn leaves_local_markdown_links_as_text() {
        let rendered = render_markdown_to_html("[repo](/home/s/projects/repo)");
        assert_eq!(rendered, "repo (&#x2F;home&#x2F;s&#x2F;projects&#x2F;repo)");
    }

    #[test]
    fn renders_http_markdown_links_as_anchors() {
        let rendered = render_markdown_to_html("[site](https://example.com)");
        assert_eq!(
            rendered,
            "<a href=\"https:&#x2F;&#x2F;example.com\">site</a>"
        );
    }

    #[test]
    fn does_not_render_markdown_inside_inline_code() {
        let rendered = render_markdown_to_html("`services/internal/project-catalog/**`");
        assert!(rendered.contains("<code>"));
        assert!(rendered.contains("</code>"));
        assert!(!rendered.contains("<b>"));
    }

    #[test]
    fn splits_large_text() {
        let parts = split_text(&"a".repeat(20), 7);
        assert_eq!(parts.len(), 3);
    }
}
