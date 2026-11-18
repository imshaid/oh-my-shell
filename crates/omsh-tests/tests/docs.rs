//! The Markdown files of the repository: every relative link goes to a file that exists, every
//! `#anchor` goes to a heading that exists, and nothing public points at private files.
//!
//! The pages link to source files by relative path (`docs/report.md` has a table of them), so
//! this test also keeps those links from going stale when a file is moved or renamed.
//!
//! The second half checks the website (`docs/index.html`, `docs/style.css`): it loads nothing
//! but its own style sheet, links only into this repository, and says what the docs say.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `README.md`, `CONTRIBUTING.md`, `CHANGELOG.md` and every `docs/*.md`.
fn pages() -> Vec<PathBuf> {
    let root = root();
    let mut pages = vec![
        root.join("README.md"),
        root.join("CONTRIBUTING.md"),
        root.join("CHANGELOG.md"),
    ];
    let mut docs: Vec<PathBuf> = fs::read_dir(root.join("docs"))
        .unwrap_or_else(|error| panic!("cannot read docs/: {error}"))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    docs.sort();
    pages.extend(docs);
    pages
}

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// The text without fenced code blocks, which may hold anything.
fn prose(text: &str) -> String {
    let mut fenced = false;
    let mut out = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The anchor GitHub makes for a heading: lower case, letters, digits, `-` and `_` kept, spaces
/// turned into `-`, everything else dropped.
fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .collect()
}

fn anchors(text: &str) -> BTreeSet<String> {
    prose(text)
        .lines()
        .filter(|line| line.starts_with('#'))
        .map(|line| slug(line.trim_start_matches('#')))
        .collect()
}

/// The targets of the `[text](target)` links in `text`.
fn links(text: &str) -> Vec<String> {
    let prose = prose(text);
    let mut targets = Vec::new();
    let mut rest = prose.as_str();
    while let Some(at) = rest.find("](") {
        rest = &rest[at + 2..];
        if let Some(end) = rest.find(')') {
            targets.push(rest[..end].to_owned());
            rest = &rest[end..];
        }
    }
    targets
}

#[test]
fn there_are_pages_to_check() {
    let pages = pages();
    let names: Vec<String> = pages
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    for wanted in [
        "README.md",
        "CONTRIBUTING.md",
        "user-guide.md",
        "commands.md",
        "design.md",
        "report.md",
    ] {
        assert!(names.iter().any(|n| n == wanted), "{wanted} is missing");
    }
}

#[test]
fn every_relative_link_and_anchor_goes_somewhere() {
    let mut broken = Vec::new();
    let mut checked = 0;
    for page in pages() {
        let text = read(&page);
        let folder = page.parent().map_or_else(root, Path::to_path_buf);
        for target in links(&text) {
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            checked += 1;
            let (path_part, anchor) = match target.split_once('#') {
                Some((path, anchor)) => (path, Some(anchor)),
                None => (target.as_str(), None),
            };
            let file = if path_part.is_empty() {
                page.clone()
            } else {
                folder.join(path_part)
            };
            if !file.exists() {
                broken.push(format!("{}: {target} (no such file)", page.display()));
                continue;
            }
            if let Some(anchor) = anchor
                && file.extension().is_some_and(|ext| ext == "md")
                && !anchors(&read(&file)).contains(anchor)
            {
                broken.push(format!("{}: {target} (no such heading)", page.display()));
            }
        }
    }
    assert!(
        checked > 20,
        "only {checked} relative links found; is the parser broken?"
    );
    assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
}

#[test]
fn public_pages_do_not_mention_private_files() {
    let mut found = Vec::new();
    for page in pages() {
        let text = read(&page);
        for word in [".private", "CLAUDE.local", "BLUEPRINT", "PROGRESS.md"] {
            if text.contains(word) {
                found.push(format!("{} mentions {word}", page.display()));
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

// -- The website: docs/index.html and docs/style.css ------------------------------------------
//
// The page is served by GitHub Pages from `docs/`. These tests keep it static (nothing loaded
// but its own style sheet), keep its links and its install command true, and keep its claims
// equal to the documentation: a passage marked `data-doc="FILE"` must be in FILE word for word.

/// The address of the repository; the only site the page may link to.
const REPO: &str = env!("CARGO_PKG_REPOSITORY");
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The files a `data-doc` attribute may name.
const SOURCES: [&str; 4] = [
    "README.md",
    "docs/user-guide.md",
    "docs/design.md",
    "docs/report.md",
];

fn site(name: &str) -> String {
    read(&root().join("docs").join(name))
}

/// One piece of the page.
#[derive(Debug)]
enum Node {
    Open(String, Vec<(String, String)>),
    Close(String),
    Text(String),
}

/// Elements that have no closing tag.
const VOID: [&str; 2] = ["meta", "link"];

/// `&amp;` and its four friends turned back into characters. Any other `&` is a panic.
fn decode(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let known = [
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
            ("&#39;", '\''),
        ];
        let Some((entity, char)) = known.iter().find(|(entity, _)| rest.starts_with(entity)) else {
            panic!(
                "an `&` the reader does not know: {:?}",
                &rest[..rest.len().min(20)]
            );
        };
        out.push(*char);
        rest = &rest[entity.len()..];
    }
    out.push_str(rest);
    out
}

/// The page as a list of tags and text. A strict reader on purpose: attribute values must be in
/// double quotes, and anything it does not understand is a panic, so markup cannot slip past it.
fn parse(html: &str) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        if at > 0 {
            nodes.push(Node::Text(decode(&rest[..at])));
        }
        rest = &rest[at..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            let end = comment
                .find("-->")
                .unwrap_or_else(|| panic!("a comment without an end"));
            rest = &comment[end + 3..];
            continue;
        }
        let end = rest
            .find('>')
            .unwrap_or_else(|| panic!("a tag without an end: {:?}", &rest[..rest.len().min(40)]));
        let tag = &rest[1..end];
        rest = &rest[end + 1..];
        if tag.eq_ignore_ascii_case("!doctype html") {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            nodes.push(Node::Close(name.to_owned()));
            continue;
        }
        let (name, mut attributes) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
        assert!(
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()),
            "a tag the reader does not understand: <{tag}>"
        );
        let mut attrs = Vec::new();
        loop {
            attributes = attributes.trim_start();
            if attributes.is_empty() {
                break;
            }
            let (key, after) = attributes
                .split_once("=\"")
                .unwrap_or_else(|| panic!("an attribute without a quoted value in <{tag}>"));
            assert!(
                !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "an attribute the reader does not understand in <{tag}>"
            );
            let (value, after) = after
                .split_once('"')
                .unwrap_or_else(|| panic!("an open quote in <{tag}>"));
            attrs.push((key.to_owned(), decode(value)));
            attributes = after;
        }
        nodes.push(Node::Open(name.to_owned(), attrs));
    }
    assert!(rest.trim().is_empty(), "text after the last tag");
    nodes
}

fn attr<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    match node {
        Node::Open(_, attrs) => attrs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str()),
        _ => None,
    }
}

fn is_open(node: &Node, name: &str) -> bool {
    matches!(node, Node::Open(tag, _) if tag == name)
}

/// The index just after the element that opens at `open`.
fn end_of(nodes: &[Node], open: usize) -> usize {
    let mut depth = 0;
    for (index, node) in nodes.iter().enumerate().skip(open) {
        match node {
            Node::Open(name, _) if !VOID.contains(&name.as_str()) => depth += 1,
            Node::Close(_) => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return index + 1;
        }
    }
    panic!("an element that is never closed");
}

/// The text of `nodes`. With `markdown`, `<code>` reads as a backtick and `<strong>` as `**`,
/// which is how the same words are written in the Markdown files.
fn text_of(nodes: &[Node], markdown: bool) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Open(name, _) | Node::Close(name) if markdown => match name.as_str() {
                "code" => out.push('`'),
                "strong" => out.push_str("**"),
                _ => {}
            },
            _ => {}
        }
    }
    out
}

/// White space folded to single blanks, and the `\|` of a Markdown table cell read as `|`.
fn fold(text: &str) -> String {
    text.replace("\\|", "|")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a `data-doc` element quotes: its text, or for a row its cells joined the way a Markdown
/// table row has them. Cells with class `src` hold links, not quoted words.
fn quoted(element: &[Node]) -> String {
    let mut cells = Vec::new();
    let mut index = 0;
    while index < element.len() {
        let node = &element[index];
        if is_open(node, "td") || is_open(node, "dt") || is_open(node, "dd") {
            let end = end_of(element, index);
            if attr(node, "class") != Some("src") {
                cells.push(fold(&text_of(&element[index..end], true)));
            }
            index = end;
        } else {
            index += 1;
        }
    }
    if cells.is_empty() {
        fold(&text_of(element, true))
    } else {
        cells.join(" | ")
    }
}

/// The page's own words: its text outside the quoted passages and outside `<pre>`.
fn own_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    let mut index = 0;
    while index < nodes.len() {
        let node = &nodes[index];
        if attr(node, "data-doc").is_some() || is_open(node, "pre") {
            index = end_of(nodes, index);
            out.push(' ');
            continue;
        }
        if let Node::Text(text) = node {
            out.push_str(text);
        }
        index += 1;
    }
    out
}

/// The first fenced block of `text` that holds `needle`, without its fences.
fn fenced_block_with(text: &str, needle: &str) -> String {
    text.split("```")
        .skip(1)
        .step_by(2)
        .find(|block| block.contains(needle))
        .and_then(|block| block.split_once('\n'))
        .map(|(_, body)| body.trim_end().to_owned())
        .unwrap_or_else(|| panic!("no fenced block with {needle}"))
}

#[test]
fn the_site_is_static_files() {
    let docs = root().join("docs");
    for name in ["index.html", "style.css", ".nojekyll"] {
        assert!(docs.join(name).is_file(), "docs/{name} is missing");
    }
    assert!(
        read(&docs.join(".nojekyll")).is_empty(),
        "docs/.nojekyll must be empty"
    );
    let html = site("index.html");
    assert!(html.starts_with("<!doctype html>\n<html lang=\"en\">"));
    let nodes = parse(&html);
    let count = |name: &str| nodes.iter().filter(|node| is_open(node, name)).count();
    assert_eq!(count("title"), 1);
    assert_eq!(count("h1"), 1);
    let description = nodes
        .iter()
        .find(|node| attr(node, "name") == Some("description"))
        .and_then(|node| attr(node, "content"))
        .unwrap_or_else(|| panic!("no meta description"));
    assert!(
        fold(&read(&root().join("README.md"))).contains(&fold(description)),
        "the meta description is not the README's first paragraph"
    );
    // What a link preview shows is the same README text, not a second wording.
    let preview = nodes
        .iter()
        .find(|node| attr(node, "property") == Some("og:description"))
        .and_then(|node| attr(node, "content"))
        .unwrap_or_else(|| panic!("no og:description"));
    assert_eq!(preview, description);

    // The panels are `<details>` that share one name (the browser then opens one at a time),
    // each with its `<summary>` first: nothing here needs a script.
    let mut panels = 0;
    for (index, node) in nodes.iter().enumerate() {
        if !is_open(node, "details") {
            continue;
        }
        panels += 1;
        assert_eq!(
            attr(node, "name"),
            Some("panel"),
            "a <details> without name=\"panel\""
        );
        let first = nodes[index + 1..]
            .iter()
            .find(|node| !matches!(node, Node::Text(text) if text.trim().is_empty()));
        assert!(
            first.is_some_and(|node| is_open(node, "summary")),
            "a <details> that does not begin with its <summary>"
        );
    }
    assert!(panels >= 4, "only {panels} panels");
    let css = site("style.css");
    for scheme in ["prefers-color-scheme: light", "prefers-reduced-motion"] {
        assert!(css.contains(scheme), "style.css has no rule for {scheme}");
    }
}

#[test]
fn the_site_loads_nothing_but_its_style_sheet() {
    const ELEMENTS: [&str; 36] = [
        "html",
        "head",
        "meta",
        "title",
        "link",
        "body",
        "a",
        "header",
        "nav",
        "main",
        "footer",
        "section",
        "h1",
        "h2",
        "h3",
        "p",
        "ul",
        "li",
        "pre",
        "code",
        "span",
        "strong",
        "figure",
        "figcaption",
        "table",
        "thead",
        "tbody",
        "tr",
        "th",
        "td",
        "dl",
        "dt",
        "dd",
        "div",
        "details",
        "summary",
    ];
    const ATTRIBUTES: [&str; 14] = [
        "lang",
        "charset",
        "name",
        "content",
        "rel",
        "href",
        "id",
        "class",
        "tabindex",
        "role",
        "aria-label",
        "scope",
        "aria-hidden",
        "data-basis",
    ];
    let nodes = parse(&site("index.html"));
    // The rows of the terminal blocks carry their number; nothing else may.
    let scenes: Vec<(usize, usize)> = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            is_open(node, "pre") && attr(node, "id").is_some_and(|id| id.starts_with("scene"))
        })
        .map(|(index, _)| (index, end_of(&nodes, index)))
        .collect();
    assert!(!scenes.is_empty(), "no terminal block on the page");
    let mut links = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let Node::Open(name, attrs) = node else {
            continue;
        };
        assert!(
            ELEMENTS.contains(&name.as_str()),
            "<{name}> is not allowed on the page"
        );
        for (key, _) in attrs {
            let allowed = match key.as_str() {
                "data-doc" => true,
                "property" | "media" => name == "meta",
                "data-l" => scenes
                    .iter()
                    .any(|(start, end)| (*start..*end).contains(&index)),
                key => ATTRIBUTES.contains(&key),
            };
            assert!(
                allowed,
                "the attribute {key} (on <{name}>) is not allowed there"
            );
        }
        // A tag in the head may describe the page in words; it may not name an address.
        if name == "meta"
            && let Some(content) = attr(node, "content")
        {
            assert!(
                !content.contains("://") && !content.starts_with("//"),
                "a <meta> names an address: {content}"
            );
        }
        if attr(node, "href").is_some() {
            assert!(
                name == "a" || name == "link",
                "<{name}> may not have an href"
            );
        }
        if name == "link" {
            links.push((attr(node, "rel"), attr(node, "href")));
        }
    }
    assert_eq!(
        links,
        [
            (Some("icon"), Some("data:,")),
            (Some("stylesheet"), Some("style.css"))
        ],
        "the page may load its own style sheet and nothing else"
    );
    let css = site("style.css");
    for bad in ["url(", "@import", "@font-face", "//", "http"] {
        assert!(!css.contains(bad), "style.css contains {bad}");
    }
    // Flat colors, borders and spacing only; and the page is never locked to the window.
    for bad in [
        "gradient(",
        "backdrop-filter",
        "filter",
        "blur(",
        "box-shadow",
        "text-shadow",
        "overflow: hidden",
        "overflow:hidden",
        "position: fixed",
        "position:fixed",
    ] {
        assert!(!css.contains(bad), "style.css contains {bad}");
    }
}

#[test]
fn the_site_links_go_somewhere() {
    let nodes = parse(&site("index.html"));
    let ids: BTreeSet<&str> = nodes.iter().filter_map(|node| attr(node, "id")).collect();
    let release = format!("/releases/tag/v{VERSION}");
    let mut broken = Vec::new();
    let mut checked = 0;
    for node in nodes.iter().filter(|node| is_open(node, "a")) {
        let href = attr(node, "href").unwrap_or_else(|| panic!("a link without an href"));
        checked += 1;
        if let Some(id) = href.strip_prefix('#') {
            if !ids.contains(id) {
                broken.push(format!("{href} (no such id on the page)"));
            }
            continue;
        }
        let Some(rest) = href.strip_prefix(REPO) else {
            broken.push(format!("{href} (not in this repository)"));
            continue;
        };
        if rest.is_empty() || rest == "/releases" || rest == release {
            continue;
        }
        let Some(target) = rest.strip_prefix("/blob/main/") else {
            broken.push(format!("{href} (not a kind of link the page may have)"));
            continue;
        };
        let (path, anchor) = match target.split_once('#') {
            Some((path, anchor)) => (path, Some(anchor)),
            None => (target, None),
        };
        let file = root().join(path);
        if !file.is_file() {
            broken.push(format!("{href} (no such file)"));
        } else if let Some(anchor) = anchor
            && !anchors(&read(&file)).contains(anchor)
        {
            broken.push(format!("{href} (no such heading)"));
        }
    }
    assert!(
        checked > 40,
        "only {checked} links found; is the reader broken?"
    );
    assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
}

#[test]
fn the_site_install_command_is_the_readmes() {
    let nodes = parse(&site("index.html"));
    let open = nodes
        .iter()
        .position(|node| attr(node, "id") == Some("install-command"))
        .unwrap_or_else(|| panic!("no install command on the page"));
    let shown = text_of(&nodes[open..end_of(&nodes, open)], false);
    let needle = "releases/latest/download/install.sh | sh";
    for page in ["README.md", "docs/user-guide.md"] {
        assert_eq!(
            shown,
            fenced_block_with(&read(&root().join(page)), needle),
            "the install command differs from the one in {page}"
        );
    }
}

#[test]
fn the_site_says_what_the_docs_say() {
    let nodes = parse(&site("index.html"));
    let report = read(&root().join("docs/report.md"));
    let mut wrong = Vec::new();
    let mut quotes = 0;
    for (open, node) in nodes.iter().enumerate() {
        let Some(source) = attr(node, "data-doc") else {
            continue;
        };
        assert!(SOURCES.contains(&source), "data-doc names {source}");
        quotes += 1;
        let element = &nodes[open..end_of(&nodes, open)];
        let quote = quoted(element);
        if !fold(&read(&root().join(source))).contains(&quote) {
            wrong.push(format!("not in {source}: {quote}"));
            continue;
        }
        // A row with source links: each file must be named on that same row of the report.
        let sources = element
            .iter()
            .skip_while(|node| attr(node, "class") != Some("src"))
            .filter_map(|node| attr(node, "href"));
        for href in sources {
            let path = href
                .rsplit_once("/blob/main/")
                .map_or(href, |(_, path)| path);
            let row = report.lines().find(|line| fold(line).contains(&quote));
            if !row.is_some_and(|row| row.contains(&format!("(../{path})"))) {
                wrong.push(format!("{path} is not on the report's row: {quote}"));
            }
        }
    }
    assert!(
        quotes > 50,
        "only {quotes} quoted passages; is the reader broken?"
    );

    // The page's own short words (a headline): each names the sentence of the docs it rests on,
    // and there are few of them and they are short.
    let mut own_lines = 0;
    for (open, node) in nodes.iter().enumerate() {
        let Some(basis) = attr(node, "data-basis") else {
            continue;
        };
        own_lines += 1;
        let text = fold(&text_of(&nodes[open..end_of(&nodes, open)], false));
        if text.chars().count() > 60 {
            wrong.push(format!("own words longer than 60 characters: {text}"));
        }
        match basis.split_once(": ") {
            Some((source, sentence)) if SOURCES.contains(&source) => {
                if !fold(&read(&root().join(source))).contains(&fold(sentence)) {
                    wrong.push(format!("the basis is not in {source}: {sentence}"));
                }
            }
            _ => wrong.push(format!(
                "a data-basis that is not `FILE: sentence`: {basis}"
            )),
        }
    }
    if own_lines > 3 {
        wrong.push(format!(
            "{own_lines} passages of own words; at most 3 are allowed"
        ));
    }

    // The limits are on the main screen, not only inside a panel.
    let limits = nodes
        .iter()
        .position(|node| attr(node, "id") == Some("limits-line"))
        .unwrap_or_else(|| panic!("no limits line on the page"));
    let inside_a_panel = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| is_open(node, "details"))
        .any(|(open, _)| (open..end_of(&nodes, open)).contains(&limits));
    if inside_a_panel {
        wrong.push("the limits line is inside a panel".to_owned());
    }
    let limits = &nodes[limits..end_of(&nodes, limits)];
    let said: Vec<String> = limits
        .iter()
        .enumerate()
        .filter(|(_, node)| attr(node, "data-doc").is_some())
        .map(|(open, _)| quoted(&limits[open..end_of(limits, open)]))
        .collect();
    for needed in ["It is young", "POSIX-compatible subset", "aarch64"] {
        if !said.iter().any(|quote| quote.contains(needed)) {
            wrong.push(format!("the limits line does not quote: {needed}"));
        }
    }

    // The page's own words: no number the docs do not have, and never the short name.
    let docs: String = pages().iter().map(|page| read(page)).collect();
    let own = own_text(&nodes);
    for word in own.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric());
        if word.chars().any(|c| c.is_ascii_digit()) && !docs.contains(word) {
            wrong.push(format!("a number that is in no page of the docs: {word}"));
        }
        if word.contains("omsh") {
            wrong.push(format!("`omsh` in the page's own words: {word}"));
        }
    }

    let version = nodes
        .iter()
        .position(|node| attr(node, "id") == Some("version"))
        .unwrap_or_else(|| panic!("no version on the page"));
    let shown = text_of(&nodes[version..end_of(&nodes, version)], false);
    if shown != VERSION {
        wrong.push(format!(
            "the page says version {shown}, Cargo.toml says {VERSION}"
        ));
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// `text` holds something shaped like an e-mail address: a letter or digit on both sides of `@`.
fn has_email_shape(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars
        .windows(3)
        .any(|w| w[1] == '@' && w[0].is_alphanumeric() && w[2].is_alphanumeric())
}

/// `text` holds digits grouped like an identity number: 3 digits, 2 digits, 3 or more digits,
/// with hyphens between. A date (4, 2, 2) is not one.
fn has_id_shape(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_digit() || c == '-'))
        .any(|chunk| {
            let parts: Vec<&str> = chunk.split('-').collect();
            parts
                .windows(3)
                .any(|w| w[0].len() == 3 && w[1].len() == 2 && w[2].len() >= 3)
        })
}

#[test]
fn the_site_has_no_private_or_personal_text() {
    let mut found = Vec::new();
    for name in ["index.html", "style.css"] {
        let text = site(name);
        for word in [".private", "CLAUDE.local", "BLUEPRINT", "PROGRESS"] {
            if text.contains(word) {
                found.push(format!("docs/{name} mentions {word}"));
            }
        }
        if has_email_shape(&text) {
            found.push(format!(
                "docs/{name} holds something shaped like an e-mail address"
            ));
        }
        if has_id_shape(&text) {
            found.push(format!(
                "docs/{name} holds something shaped like an identity number"
            ));
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
    // The two shapes are put together here, so that this file holds neither of them.
    let address = ["write to someone", "host.example"].join("@");
    assert!(has_email_shape(&address) && !has_email_shape("@media print"));
    let number = ["id 123", "45", "6789"].join("-");
    assert!(has_id_shape(&number) && !has_id_shape("on 2026-11-17, 1.0.0"));
}
