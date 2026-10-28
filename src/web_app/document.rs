use ego_tree::iter::Edge;
use miette::{Result, miette};
use scraper::{Html, Node};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Link {
    pub id: usize,
    pub offset: usize,
    pub text: String,
    pub url: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Document {
    pub requested_url: String,
    pub url: String,
    pub title: String,
    pub source: String,
    pub media_type: String,
    pub captured_at_ms: i64,
    pub text: String,
    pub original: String,
    pub links: Vec<Link>,
    pub main_offsets: Vec<usize>,
    pub anchors: std::collections::BTreeMap<String, usize>,
    pub notices: Vec<String>,
}

impl Document {
    pub fn parse(
        requested_url: &str,
        url: &str,
        source: &str,
        media_type: &str,
        body: &str,
        mut notices: Vec<String>,
    ) -> Result<Self> {
        super::validate_url(url)?;
        let mut document = Self {
            requested_url: requested_url.into(),
            url: url.into(),
            title: String::new(),
            source: source.into(),
            media_type: media_type.into(),
            captured_at_ms: chrono::Utc::now().timestamp_millis(),
            text: String::new(),
            original: body.into(),
            links: vec![],
            main_offsets: vec![],
            anchors: Default::default(),
            notices: vec![],
        };
        if !media_type.contains("html") {
            document.text = body.into();
            document.notices = notices;
            return Ok(document);
        }
        let html = Html::parse_document(body);
        let base_selector = scraper::Selector::parse("base[href]").expect("static selector");
        let page_url = url::Url::parse(url).map_err(|_| miette!("Invalid captured URL"))?;
        let base = html
            .select(&base_selector)
            .next()
            .and_then(|element| element.attr("href"))
            .and_then(|href| page_url.join(href).ok())
            .filter(|base| super::validate_url(base.as_str()).is_ok())
            .unwrap_or(page_url);
        let title_selector = scraper::Selector::parse("title").expect("static selector");
        document.title = html
            .select(&title_selector)
            .next()
            .map(|title| title.text().collect::<String>())
            .unwrap_or_default();
        let mut text = Text::default();
        let mut skipped = 0usize;
        let mut pre = 0usize;
        let mut anchors: Vec<Option<(String, usize)>> = vec![];
        for edge in html.tree.root().traverse() {
            match edge {
                Edge::Open(node) => {
                    if skipped > 0 {
                        skipped += 1;
                        continue;
                    }
                    match node.value() {
                        Node::Element(element) => {
                            let name = element.name();
                            if matches!(name, "head" | "script" | "style" | "template" | "noscript")
                                || element.attr("hidden").is_some()
                                || element.attr("style").is_some_and(inline_hidden)
                            {
                                skipped = 1;
                                continue;
                            }
                            if block(name) {
                                text.line();
                            }
                            if let Some(id) = element.attr("id").or_else(|| {
                                if name == "a" {
                                    element.attr("name")
                                } else {
                                    None
                                }
                            }) {
                                let mut target = base.clone();
                                target.set_fragment(Some(id));
                                document
                                    .anchors
                                    .entry(target.fragment().unwrap_or("").into())
                                    .or_insert(text.value.len());
                            }
                            if name == "main" || element.attr("role") == Some("main") {
                                document.main_offsets.push(text.value.len());
                            }
                            if name == "pre" {
                                pre += 1;
                            }
                            if name == "br" {
                                text.line();
                            }
                            if name == "a" {
                                let url = element
                                    .attr("href")
                                    .and_then(|href| base.join(href).ok())
                                    .filter(|url| super::validate_url(url.as_str()).is_ok());
                                text.flush_space();
                                anchors.push(url.map(|url| (url.into(), text.value.len())));
                            }
                            if name == "img"
                                && let Some(alt) = element.attr("alt").filter(|alt| !alt.is_empty())
                            {
                                text.push(alt, false);
                            }
                            if matches!(name, "iframe" | "frame") && source == "http" {
                                notices.push(format!(
                                    "Frame content is not included in HTTP source: {}",
                                    element.attr("src").unwrap_or("(no URL)")
                                ));
                            }
                        }
                        Node::Text(value) => text.push(value, pre > 0),
                        _ => {}
                    }
                }
                Edge::Close(node) => {
                    if skipped > 0 {
                        skipped -= 1;
                        continue;
                    }
                    if let Node::Element(element) = node.value() {
                        let name = element.name();
                        if name == "a"
                            && let Some(Some((url, start))) = anchors.pop()
                        {
                            document.links.push(Link {
                                id: document.links.len(),
                                offset: start,
                                text: text.value[start..].trim().into(),
                                url,
                            });
                        }
                        if name == "pre" {
                            pre = pre.saturating_sub(1);
                        }
                        if matches!(name, "td" | "th") {
                            text.cell();
                        } else if block(name) {
                            text.line();
                        }
                    }
                }
            }
        }
        // Keep offsets into this immutable text stable: never trim its prefix.
        document.text = text.value.trim_end().into();
        if source == "http" {
            notices.push("HTTP source does not execute JavaScript; request source=rendered for the loaded page".into());
        }
        if document.text.trim().is_empty() {
            notices.push("No readable text was found in the captured document".into());
        }
        document.notices = notices;
        Ok(document)
    }
    pub fn local_link_offset(&self, target: &str) -> Option<usize> {
        let mut target = url::Url::parse(target).ok()?;
        let fragment = target.fragment()?.to_owned();
        let mut current = url::Url::parse(&self.url).ok()?;
        target.set_fragment(None);
        current.set_fragment(None);
        (target == current).then(|| {
            self.anchors
                .get(&fragment)
                .copied()
                .unwrap_or(0)
                .min(self.text.len())
        })
    }
    pub fn view(
        &self,
        ref_id: &str,
        offset: usize,
        limit: usize,
        original: bool,
        link_offset: Option<usize>,
    ) -> Result<serde_json::Value> {
        let text = if original { &self.original } else { &self.text };
        if offset > text.len() || !text.is_char_boundary(offset) {
            return Err(miette!("Invalid document offset; use a returned offset"));
        }
        let mut end = offset.saturating_add(limit).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut links = Vec::new();
        let mut link_bytes = 0;
        let mut next_link_offset = None;
        if link_offset.is_some_and(|start| start > self.links.len()) {
            return Err(miette!("Invalid link offset; use next_link_offset"));
        }
        for link in self.links.iter().filter(|link| {
            !original
                && link_offset.map_or(
                    link.offset >= offset && (link.offset < end || end == text.len()),
                    |start| link.id >= start,
                )
        }) {
            // Labels remain intact in the document text. Metadata need not echo them.
            if !links.is_empty() && (links.len() >= 32 || link_bytes + link.url.len() > limit / 2) {
                next_link_offset = Some(link.id);
                break;
            }
            link_bytes += link.url.len();
            links.push(serde_json::json!({"id":link.id,"offset":link.offset,"url":link.url}));
        }
        let mut value = serde_json::json!({"ref_id":ref_id,"requested_url":self.requested_url,"url":self.url,"title":self.title,"source":self.source,"view":if original{"source"}else{"text"},"media_type":self.media_type,"captured_at_ms":self.captured_at_ms,"offset_unit":"utf8_bytes","offset":offset,"next_offset":(end<text.len()).then_some(end),"total_bytes":text.len(),"main_offset":self.main_offsets.first(),"content":&text[offset..end],"links":links,"next_link_offset":next_link_offset,"total_links":self.links.len(),"notices":self.notices});
        if self.media_type.contains("json")
            && offset == 0
            && end == text.len()
            && let Ok(data) = serde_json::from_str::<serde_json::Value>(text)
        {
            value["structured_content"] = data;
        }
        Ok(value)
    }
    pub fn find(&self, ref_id: &str, pattern: &str, offset: usize) -> Result<serde_json::Value> {
        if pattern.is_empty() || offset > self.text.len() || !self.text.is_char_boundary(offset) {
            return Err(miette!("Invalid Web find pattern or offset"));
        }
        let mut matches = self.text[offset..]
            .match_indices(pattern)
            .map(|(position, _)| position + offset);
        let mut found = Vec::new();
        for position in matches.by_ref().take(20) {
            let mut start = position.saturating_sub(180);
            while !self.text.is_char_boundary(start) {
                start -= 1;
            }
            let mut end = (position + pattern.len() + 180).min(self.text.len());
            while !self.text.is_char_boundary(end) {
                end -= 1;
            }
            found.push(serde_json::json!({"offset":position,"context_offset":start,"content":&self.text[start..end]}));
        }
        Ok(
            serde_json::json!({"ref_id":ref_id,"pattern":pattern,"matches":found,"next_offset":matches.next(),"total_bytes":self.text.len(),"offset_unit":"utf8_bytes"}),
        )
    }
}
fn block(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "caption"
            | "dd"
            | "details"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "summary"
            | "table"
            | "tr"
            | "ul"
    )
}
fn inline_hidden(style: &str) -> bool {
    style.split(';').any(|declaration| {
        declaration.split_once(':').is_some_and(|(name, value)| {
            let name = name.trim();
            let value = value.split('!').next().unwrap_or("").trim();
            (name.eq_ignore_ascii_case("display") && value.eq_ignore_ascii_case("none"))
                || (name.eq_ignore_ascii_case("visibility") && value.eq_ignore_ascii_case("hidden"))
        })
    })
}
#[derive(Default)]
struct Text {
    value: String,
    space: bool,
}
impl Text {
    fn flush_space(&mut self) {
        if self.space && !self.value.is_empty() && !self.value.ends_with(['\n', '\t', ' ']) {
            self.value.push(' ');
        }
        self.space = false;
    }
    fn push(&mut self, value: &str, pre: bool) {
        if pre {
            self.flush_space();
            self.value.push_str(value);
            return;
        }
        for ch in value.chars() {
            if ch.is_whitespace() {
                self.space = true;
            } else {
                self.flush_space();
                self.value.push(ch);
            }
        }
    }
    fn line(&mut self) {
        self.space = false;
        if !self.value.is_empty() && !self.value.ends_with('\n') {
            self.value.push('\n');
        }
    }
    fn cell(&mut self) {
        self.space = false;
        if !self.value.ends_with(['\n', '\t']) {
            self.value.push('\t');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_document_preserves_deep_prose_secondary_regions_tables_and_link_targets() {
        let html = format!(
            "<title>Example</title><nav><a href='/nav'>Navigation</a></nav><main><div><div><div><div><div><div><div><p>{}</p></div></div></div></div></div></div></div></main><main>SECOND_MAIN</main><aside>ASIDE</aside><table><tr><th>A</th><th>B</th></tr><tr><td>one</td><td>two</td></tr></table><pre>  code\n    indent</pre><p>Before <a href='../next?x=1&amp;y=2'>Next <img alt='diagram' src='a.png'></a> after.</p><script>PRIVATE_SCRIPT</script>",
            "中文长正文".repeat(100)
        );
        let doc = Document::parse(
            "https://example.com/a/b",
            "https://example.com/a/b",
            "rendered",
            "text/html",
            &html,
            vec![],
        )
        .unwrap();
        for value in [
            "中文长正文".repeat(100),
            "SECOND_MAIN".into(),
            "ASIDE".into(),
            "one\ttwo".into(),
            "  code\n    indent".into(),
            "Before Next diagram after.".into(),
        ] {
            assert!(doc.text.contains(&value), "missing {value}");
        }
        assert!(!doc.text.contains("PRIVATE_SCRIPT"));
        assert_eq!(doc.links[1].url, "https://example.com/next?x=1&y=2");
        assert_eq!(doc.links[1].text, "Next diagram");
        assert_eq!(&doc.text[doc.links[1].offset..][..12], "Next diagram");
    }
    #[test]
    fn pagination_reassembles_unicode_source_and_find_reaches_unreturned_content() {
        let text = format!("{}\nTAIL_NEEDLE", "段落🙂".repeat(2000));
        let doc = Document::parse(
            "https://example.com",
            "https://example.com",
            "http",
            "text/plain",
            &text,
            vec![],
        )
        .unwrap();
        let mut reconstructed = String::new();
        let mut offset = 0;
        loop {
            let view = doc.view("doc", offset, 97, false, None).unwrap();
            reconstructed.push_str(view["content"].as_str().unwrap());
            if let Some(next) = view["next_offset"].as_u64() {
                offset = next as usize;
            } else {
                break;
            }
        }
        assert_eq!(reconstructed, text);
        let found = doc.find("doc", "TAIL_NEEDLE", 0).unwrap();
        assert_eq!(
            found["matches"][0]["offset"],
            text.find("TAIL_NEEDLE").unwrap()
        );
        assert!(doc.view("doc", 1, 97, false, None).is_err());
    }
    #[test]
    fn hidden_application_templates_stay_in_source_without_polluting_readable_text() {
        let html = "<textarea style='DISPLAY: none !important'>&lt;style&gt;INTERNAL_CSS&lt;/style&gt;</textarea><div hidden>HIDDEN_DATA</div><main>Visible article</main>";
        let document = Document::parse(
            "https://example.com",
            "https://example.com",
            "rendered",
            "text/html",
            html,
            vec![],
        )
        .unwrap();
        assert_eq!(document.text, "Visible article");
        assert_eq!(
            document.view("doc", 0, 1000, true, None).unwrap()["content"],
            html
        );
    }
    #[test]
    fn dense_links_are_pageable_even_when_their_labels_are_empty() {
        let html = (0..101)
            .map(|id| format!("<a href='/destination/{id}'></a>"))
            .collect::<String>();
        let document = Document::parse(
            "https://example.com",
            "https://example.com",
            "rendered",
            "text/html",
            &html,
            vec![],
        )
        .unwrap();
        let mut next = None;
        let mut ids = Vec::new();
        loop {
            let page = document.view("doc", 0, 1000, false, next).unwrap();
            ids.extend(
                page["links"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|link| link["id"].as_u64().unwrap()),
            );
            next = page["next_link_offset"]
                .as_u64()
                .map(|value| value as usize);
            if next.is_none() {
                break;
            }
        }
        assert_eq!(ids, (0..101).collect::<Vec<u64>>());
    }
    #[test]
    #[ignore = "Opt-in native website captures; set DAAT_LOCUS_WEB_CAPTURES to the WebView probe output"]
    fn native_website_captures_preserve_known_document_content() {
        let path = std::env::var_os("DAAT_LOCUS_WEB_CAPTURES")
            .expect("DAAT_LOCUS_WEB_CAPTURES is required");
        let bytes = std::fs::read(&path).unwrap();
        let captures: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            captures.len(),
            7,
            "Probe the seven documented website cases"
        );
        let mut report = Vec::new();
        for capture in captures {
            let url = capture["url"].as_str().unwrap();
            let native: daat_locus_client_protocol::web::Reply =
                serde_json::from_value(capture["reply"].clone()).unwrap();
            assert!(native.error.is_none(), "{url}: {:?}", native.error);
            let native = native.capture.expect("native capture missing");
            let document = Document::parse(
                url,
                &native.url,
                "rendered",
                "text/html",
                &native.html,
                native.notices,
            )
            .unwrap();
            let expected: &[&str] = if url.contains("example.com") {
                &[
                    "This domain is for use in documentation examples",
                    "Learn more",
                ]
            } else if url.contains("mozilla.org") {
                &["Array.prototype.map()", "Array.from()"]
            } else if url.contains("wikipedia.org") {
                &["memory safety", "ownership"]
            } else if url.contains("github.com") {
                &["Codex", "README"]
            } else if url.contains("ycombinator.com") {
                &["Hacker News", "comments"]
            } else if url.contains("baidu.com") {
                &["新闻", "地图"]
            } else {
                &["番剧", "首页"]
            };
            for text in expected {
                assert!(
                    document.text.contains(text),
                    "{url}: missing known source text {text}"
                );
            }
            eprintln!(
                "{}: {} bytes, {} links, main {:?}",
                url,
                document.text.len(),
                document.links.len(),
                document.main_offsets
            );
            report.push(serde_json::json!({"url":url,"title":document.title,"text_bytes":document.text.len(),"links":document.links.len(),"main_offsets":document.main_offsets,"notices":document.notices,"text":document.text}));
        }
        let output = std::path::Path::new(&path).with_file_name("extracted.json");
        std::fs::write(output, serde_json::to_vec(&report).unwrap()).unwrap();
    }
}
