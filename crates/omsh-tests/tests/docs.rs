//! The Markdown files of the repository: every relative link goes to a file that exists, every
//! `#anchor` goes to a heading that exists, and nothing public points at private files.
//!
//! The pages link to source files by relative path (`docs/report.md` has a table of them), so
//! this test also keeps those links from going stale when a file is moved or renamed.
//!
//! The second half checks the website (`docs/index.html` and `docs/fonts/`): it loads nothing from
//! another host, has one small script and says so, links only into this repository, and says
//! what the docs say.

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

// -- The website: docs/index.html and docs/fonts/ ---------------------------------------------
//
// The page is served by GitHub Pages from `docs/`. It is one HTML file with its style inline,
// six self-hosted font files and one small inline script. These tests keep it that way: nothing
// is loaded from another host, the script does nothing but copy the command and move a light,
// the links and the install command are true, and what the page claims is in the documentation.

/// The address of the repository; the only site the page may name.
const REPO: &str = env!("CARGO_PKG_REPOSITORY");
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn site(name: &str) -> String {
    read(&root().join("docs").join(name))
}

/// One piece of the page.
#[derive(Debug)]
enum Node {
    Open(String, Vec<(String, String)>),
    Close,
    Text(String),
    /// The content of a `<script>` or `<style>`: code, kept as it is written.
    Raw(String),
}

/// Elements that have no closing tag (the shapes of an SVG are written `<path .../>`).
const VOID: [&str; 4] = ["meta", "link", "rect", "path"];

/// Elements whose content is code, not markup.
const RAW: [&str; 2] = ["script", "style"];

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
/// double quotes (a bare word such as `hidden` is fine), and anything it does not understand is a
/// panic, so markup cannot slip past it. Attribute names are read in lower case.
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
        if tag.starts_with('/') {
            nodes.push(Node::Close);
            continue;
        }
        let tag = tag.strip_suffix('/').unwrap_or(tag).trim_end();
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
            let key_end = attributes
                .find(|c: char| c == '=' || c.is_whitespace())
                .unwrap_or(attributes.len());
            let (key, after) = attributes.split_at(key_end);
            assert!(
                !key.is_empty() && key.chars().all(|c| c.is_ascii_alphabetic() || c == '-'),
                "an attribute the reader does not understand in <{tag}>"
            );
            let key = key.to_ascii_lowercase();
            if let Some(after) = after.strip_prefix("=\"") {
                let (value, after) = after
                    .split_once('"')
                    .unwrap_or_else(|| panic!("an open quote in <{tag}>"));
                attrs.push((key, decode(value)));
                attributes = after;
            } else {
                assert!(
                    !after.starts_with('='),
                    "an attribute value without double quotes in <{tag}>"
                );
                attrs.push((key, String::new()));
                attributes = after;
            }
        }
        nodes.push(Node::Open(name.to_owned(), attrs));
        if RAW.contains(&name) {
            let close = format!("</{name}>");
            let end = rest
                .find(&close)
                .unwrap_or_else(|| panic!("<{name}> is never closed"));
            nodes.push(Node::Raw(rest[..end].to_owned()));
            rest = &rest[end..];
        }
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
            Node::Close => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return index + 1;
        }
    }
    panic!("an element that is never closed");
}

/// The element with this `id`, from its opening tag to its closing tag.
fn element<'a>(nodes: &'a [Node], id: &str) -> &'a [Node] {
    let open = nodes
        .iter()
        .position(|node| attr(node, "id") == Some(id))
        .unwrap_or_else(|| panic!("no element with id {id} on the page"));
    &nodes[open..end_of(nodes, open)]
}

/// The first element called `name`.
fn first<'a>(nodes: &'a [Node], name: &str) -> &'a [Node] {
    let open = nodes
        .iter()
        .position(|node| is_open(node, name))
        .unwrap_or_else(|| panic!("no <{name}> on the page"));
    &nodes[open..end_of(nodes, open)]
}

/// The words a visitor can read: the text of `nodes`, never the code of a script or a style.
fn text_of(nodes: &[Node]) -> String {
    nodes
        .iter()
        .filter_map(|node| match node {
            Node::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The code inside every `<script>` or `<style>` (`name`).
fn code_of(nodes: &[Node], name: &str) -> Vec<String> {
    nodes
        .windows(2)
        .filter_map(|pair| match pair {
            [open, Node::Raw(code)] if is_open(open, name) => Some(code.clone()),
            _ => None,
        })
        .collect()
}

/// The page's style with every blank removed, so a rule can be looked for as one word.
fn compact_css(nodes: &[Node]) -> String {
    code_of(nodes, "style")
        .concat()
        .split_whitespace()
        .collect()
}

/// White space folded to single blanks.
fn fold(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` in lower case with the Markdown marks (`*` and a backtick) taken out and white space
/// folded, so that a sentence of the page can be looked for in a Markdown file.
fn plain(text: &str) -> String {
    fold(&text.replace(['*', '`'], "")).to_lowercase()
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

/// Everything inside `url(...)` in `css`.
fn urls(css: &str) -> Vec<String> {
    css.split("url(")
        .skip(1)
        .map(|rest| {
            rest.split_once(')')
                .unwrap_or_else(|| panic!("a url( without its )"))
                .0
                .trim_matches(['"', '\''])
                .to_owned()
        })
        .collect()
}

/// What is inside the braces that open `css` right after `start` (nested braces are counted).
fn block_after<'a>(css: &'a str, start: &str) -> &'a str {
    let from = css
        .find(start)
        .unwrap_or_else(|| panic!("the style has no {start}"))
        + start.len();
    let mut depth = 1;
    for (offset, char) in css[from..].char_indices() {
        match char {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return &css[from..from + offset];
        }
    }
    panic!("the block after {start} is never closed");
}

#[test]
fn the_site_is_one_page_with_its_own_fonts() {
    let docs = root().join("docs");
    for name in ["index.html", ".nojekyll", "fonts/LICENSE.txt"] {
        assert!(docs.join(name).is_file(), "docs/{name} is missing");
    }
    assert!(
        read(&docs.join(".nojekyll")).is_empty(),
        "docs/.nojekyll must be empty"
    );
    assert!(
        !docs.join("style.css").exists(),
        "docs/style.css is not used: the page has its style inline"
    );
    let html = site("index.html");
    assert!(html.starts_with("<!doctype html>\n<html lang=\"en\">"));
    let nodes = parse(&html);
    let count = |name: &str| nodes.iter().filter(|node| is_open(node, name)).count();
    assert_eq!(count("title"), 1);
    assert_eq!(count("h1"), 1);
    assert_eq!(count("style"), 1);
    let description = nodes
        .iter()
        .find(|node| attr(node, "name") == Some("description"))
        .and_then(|node| attr(node, "content"))
        .unwrap_or_else(|| panic!("no meta description"));
    assert!(!description.trim().is_empty());

    // Every font file the style names is on disk, and every font file on disk is named.
    let named: BTreeSet<String> = urls(&code_of(&nodes, "style").concat())
        .iter()
        .map(|url| {
            url.strip_prefix("fonts/")
                .unwrap_or_else(|| panic!("a url that is not in docs/fonts/: {url}"))
                .to_owned()
        })
        .collect();
    let on_disk: BTreeSet<String> = fs::read_dir(docs.join("fonts"))
        .unwrap_or_else(|error| panic!("docs/fonts cannot be read: {error}"))
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".woff2"))
        .collect();
    assert!(!named.is_empty(), "the page names no font file");
    assert_eq!(
        named, on_disk,
        "the fonts the style names and the files differ"
    );
    assert!(
        read(&docs.join("fonts/LICENSE.txt")).contains("SIL OPEN FONT LICENSE"),
        "docs/fonts/LICENSE.txt does not hold the font license"
    );
}

#[test]
fn the_site_loads_nothing_from_another_host() {
    const ELEMENTS: [&str; 23] = [
        "html", "head", "meta", "title", "link", "style", "script", "body", "div", "header", "nav",
        "a", "main", "h1", "h2", "p", "span", "button", "svg", "rect", "path", "footer", "code",
    ];
    const ATTRIBUTES: [&str; 23] = [
        "lang",
        "charset",
        "name",
        "content",
        "rel",
        "href",
        "id",
        "class",
        "style",
        "type",
        "hidden",
        "aria-label",
        "aria-hidden",
        "width",
        "height",
        "viewbox",
        "fill",
        "stroke",
        "stroke-width",
        "d",
        "x",
        "y",
        "rx",
    ];
    let html = site("index.html");
    let nodes = parse(&html);
    let mut links = Vec::new();
    for node in &nodes {
        let Node::Open(name, attrs) = node else {
            continue;
        };
        assert!(
            ELEMENTS.contains(&name.as_str()),
            "<{name}> is not allowed on the page"
        );
        for (key, value) in attrs {
            assert!(
                ATTRIBUTES.contains(&key.as_str()),
                "the attribute {key} (on <{name}>) is not allowed (no src, srcset, action ...)"
            );
            // A style attribute may not fetch anything.
            assert!(
                key != "style" || (!value.contains("url(") && !value.contains("@import")),
                "a style attribute fetches something: {value}"
            );
        }
        // A tag in the head may describe the page in words; it may not name an address.
        if name == "meta"
            && let Some(content) = attr(node, "content")
        {
            assert!(
                !content.contains("://"),
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
        [(Some("icon"), Some("data:,"))],
        "the only <link> is the empty icon (the fonts come from the style, from this site)"
    );

    // Every address anywhere in the file (links, the script's string, a comment) is this
    // repository's, and there is no protocol-relative address.
    let mut rest = html.as_str();
    while let Some(at) = rest.find("://") {
        let start = rest[..at]
            .rfind(|c: char| !c.is_ascii_alphabetic())
            .map_or(0, |index| index + 1);
        let url = &rest[start..];
        let after = url.strip_prefix(REPO).unwrap_or_else(|| {
            panic!(
                "an address outside this repository: {:?}",
                &url[..url.len().min(60)]
            )
        });
        assert!(
            after.is_empty() || after.starts_with(['/', '"', '\'', ' ', '#', '<']),
            "an address that only starts like the repository's: {:?}",
            &url[..url.len().min(60)]
        );
        rest = &rest[at + 3..];
    }
    assert!(
        !html.replace("://", "").contains("//"),
        "a protocol-relative address, or a comment in the code"
    );

    // The style fetches only the fonts of this site.
    let css = code_of(&nodes, "style").concat();
    assert!(!css.contains("@import"), "the style has an @import");
    for url in urls(&css) {
        assert!(
            url.starts_with("fonts/") && url.ends_with(".woff2") && !url.contains(".."),
            "the style fetches {url}"
        );
    }
}

#[test]
fn the_site_has_one_small_script_and_says_so() {
    let nodes = parse(&site("index.html"));
    let scripts = code_of(&nodes, "script");
    assert_eq!(scripts.len(), 1, "the footer says one script");
    let code = &scripts[0];
    assert!(
        code.lines().count() <= 40,
        "the script is no longer small ({} lines)",
        code.lines().count()
    );
    // It copies a string and follows the pointer; it has no way to send or keep anything.
    for bad in [
        "fetch(",
        "XMLHttpRequest",
        "WebSocket",
        "sendBeacon",
        "import(",
        "importScripts",
        "eval(",
        "Function(",
        "document.cookie",
        "localStorage",
        "sessionStorage",
        "indexedDB",
        "location",
        "createElement",
    ] {
        assert!(!code.contains(bad), "the script uses {bad}");
    }
    // The button that needs the script is hidden until the script has run.
    let button = element(&nodes, "copy");
    assert_eq!(
        attr(&button[0], "hidden"),
        Some(""),
        "the Copy button must be `hidden` in the markup"
    );
    assert!(code.contains("btn.hidden = false"));
    // The footer tells the visitor.
    let said = plain(&text_of(first(&nodes, "footer")));
    for needed in [
        "one small inline script",
        "works without it",
        "no cookies",
        "nothing loaded from another host",
    ] {
        assert!(said.contains(needed), "the footer does not say: {needed}");
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
        // A file of the site itself, next to the page.
        if !href.contains("://") {
            if !root().join("docs").join(href).is_file() {
                broken.push(format!("{href} (no such file in docs/)"));
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
        checked >= 8,
        "only {checked} links found; is the reader broken?"
    );
    assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
}

#[test]
fn the_site_install_command_is_the_readmes() {
    let nodes = parse(&site("index.html"));
    let shown = text_of(element(&nodes, "cmd"));
    let needle = "releases/latest/download/install.sh | sh";
    for page in ["README.md", "docs/user-guide.md"] {
        assert_eq!(
            shown,
            fenced_block_with(&read(&root().join(page)), needle),
            "the install command differs from the one in {page}"
        );
    }
    // The Copy button puts the same text on the clipboard.
    let code = &code_of(&nodes, "script")[0];
    let copied = code
        .split_once("var CMD = '")
        .and_then(|(_, rest)| rest.split_once("';"))
        .map(|(command, _)| command);
    assert_eq!(
        copied,
        Some(shown.as_str()),
        "the Copy button copies other text than the page shows"
    );
}

#[test]
fn the_site_shows_the_version_it_ships() {
    let nodes = parse(&site("index.html"));
    let version = element(&nodes, "version");
    assert_eq!(text_of(version), VERSION, "the page says another version");
    assert_eq!(
        attr(&version[0], "href"),
        Some(format!("{REPO}/releases/tag/v{VERSION}").as_str()),
        "the version links to another release"
    );
}

/// What the page says in its own words, each with the sentence of the documentation it rests
/// on: (the words on the page, the file, the words in that file). The comparison ignores case,
/// white space and Markdown marks.
const CLAIMS: [(&str, &str, &str); 14] = [
    (
        "Asks before the dangerous ones.",
        "README.md",
        "A dangerous one shows what it would touch and asks",
    ),
    (
        "A Linux shell written in Rust.",
        "README.md",
        "written in Rust",
    ),
    (
        "It reads each line before it runs.",
        "README.md",
        "Every typed line is analyzed first",
    ),
    (
        "it shows what would be touched and waits for you",
        "README.md",
        "A dangerous one shows what it would touch and asks",
    ),
    (
        "Linux, x86_64 or aarch64, glibc 2.35 or newer",
        "README.md",
        "Linux, x86_64 or aarch64, glibc 2.35 or newer",
    ),
    (
        "Every line is read before it runs.",
        "README.md",
        "Every typed line is analyzed first",
    ),
    (
        "No network on the normal command path",
        "README.md",
        "No network access on the normal command path",
    ),
    ("and no telemetry", "README.md", "No telemetry"),
    (
        "Off until you run omsh ai setup.",
        "README.md",
        "AI is off until you set it up",
    ),
    ("omsh ai setup", "README.md", "(`omsh ai setup`)"),
    (
        "Nothing it suggests runs without a preview and your confirmation.",
        "README.md",
        "Nothing the AI suggests runs without a preview and your confirmation",
    ),
    ("Young:", "README.md", "It is young"),
    (
        "a POSIX-compatible subset",
        "README.md",
        "A POSIX-compatible subset",
    ),
    (
        "aarch64 was smoke-tested on a CI runner only",
        "docs/report.md",
        "aarch64 was smoke-tested on a CI runner only",
    ),
];

#[test]
fn the_site_says_what_the_docs_say() {
    let nodes = parse(&site("index.html"));
    let page = plain(&text_of(&nodes));
    let mut wrong = Vec::new();
    for (said, file, basis) in CLAIMS {
        if !page.contains(&plain(said)) {
            wrong.push(format!("the page no longer says: {said}"));
        }
        if !plain(&read(&root().join(file))).contains(&plain(basis)) {
            wrong.push(format!("{file} no longer says: {basis}"));
        }
    }
    // The headline is the first heading and nothing else.
    let headline = fold(&text_of(first(&nodes, "h1")));
    if headline != "Asks before the dangerous ones." {
        wrong.push(format!("the headline changed: {headline}"));
    }
    // No number that the documentation does not have.
    let docs: String = pages().iter().map(|page| read(page)).collect();
    // (Each piece of text is read alone, so that `Shell` and `1.1.0` in two elements are two words.)
    for node in &nodes {
        let Node::Text(text) = node else {
            continue;
        };
        for word in text.split_whitespace() {
            let word = word.trim_matches(|c: char| !c.is_alphanumeric());
            if word.chars().any(|c| c.is_ascii_digit()) && !docs.contains(word) {
                wrong.push(format!("a number that is in no page of the docs: {word}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Regression tests for three faults of the first redesign: the three labels of the Copy button
/// were visible together, the selection was the browser's blue, and the command wrapped on a
/// phone. These look at the rules that fixed them.
#[test]
fn the_copy_button_shows_one_label_and_keeps_its_width() {
    let nodes = parse(&site("index.html"));
    // An inline `display` on a label beats every rule that hides it. That was the fault.
    for node in element(&nodes, "copy") {
        if let Some(class) = attr(node, "class")
            && ["s-idle", "s-ok", "s-fail"].contains(&class)
        {
            assert_eq!(attr(node, "style"), None, "the label {class} has a style");
        }
    }
    let css = compact_css(&nodes);
    for rule in [
        ".cbtn.ok.s-idle,.cbtn.fail.s-idle{display:none}",
        ".cbtn.s-ok,.cbtn.s-fail{display:none}",
        ".cbtn.ok.s-ok,.cbtn.fail.s-fail{display:inline-flex}",
    ] {
        // `.cbtn .s-ok` is written with a blank in the page; compact_css has removed it.
        assert!(css.contains(rule), "the style has no rule {rule}");
    }
    let button_rule = block_after(&css, ".cbtn{");
    assert!(
        button_rule.contains("width:112px"),
        "the button has no fixed width"
    );
    assert!(
        !button_rule.contains("min-width"),
        "the button width must not depend on its label"
    );
}

#[test]
fn the_selection_uses_the_accent_color() {
    let css = compact_css(&parse(&site("index.html")));
    assert!(
        css.contains("::selection{background:var(--accent);color:var(--bg)}"),
        "::selection is not the accent on the dark background"
    );
}

#[test]
fn on_a_narrow_screen_the_command_scrolls_and_the_button_goes_below() {
    let css = compact_css(&parse(&site("index.html")));
    // On every screen the command is one line that scrolls; nothing breaks or wraps it.
    assert!(css.contains(".cmd{flex:none;white-space:nowrap"));
    assert!(block_after(&css, ".line{").contains("overflow-x:auto"));
    for bad in [
        "word-break",
        "white-space:normal",
        "overflow-wrap",
        "break-all",
    ] {
        assert!(
            !css.contains(bad),
            "the style has {bad}: the command may wrap"
        );
    }
    // Below 1040 px the bar is a column: the command, then a full-width button, with a fade at
    // the right edge of the command and room after its last character.
    let narrow = block_after(&css, "@media(max-width:1040px){");
    for needed in [
        ".bar{flex-direction:column}",
        "mask-image:linear-gradient(toright,",
        ".line::after{content:\"\";flex:none;width:36px}",
        ".cbtn{width:100%;",
    ] {
        assert!(narrow.contains(needed), "the narrow layout has no {needed}");
    }
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
    let text = site("index.html");
    for word in [".private", "CLAUDE.local", "BLUEPRINT", "PROGRESS"] {
        if text.contains(word) {
            found.push(format!("docs/index.html mentions {word}"));
        }
    }
    if has_email_shape(&text) {
        found.push("docs/index.html holds something shaped like an e-mail address".to_owned());
    }
    if has_id_shape(&text) {
        found.push("docs/index.html holds something shaped like an identity number".to_owned());
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
    // The two shapes are put together here, so that this file holds neither of them.
    let address = ["write to someone", "host.example"].join("@");
    assert!(has_email_shape(&address) && !has_email_shape("@media print"));
    let number = ["id 123", "45", "6789"].join("-");
    assert!(has_id_shape(&number) && !has_id_shape("on 2026-11-17, 1.0.0"));
}
