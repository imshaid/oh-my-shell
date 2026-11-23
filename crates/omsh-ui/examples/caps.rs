//! Prints what `omsh-ui` finds out about the terminal this runs in, and asks it whether it
//! understands synchronized output (DEC mode 2026). Meant to be run by hand in the terminals the
//! UI upgrade has to work in (kitty, tmux inside kitty, ssh, a plain console):
//!
//! ```text
//! cargo run -p omsh-ui --example caps            # the report
//! cargo run -p omsh-ui --example caps -- --demo  # the report, then a real inline menu
//! cargo run -p omsh-ui --example caps 2>&1 | cat # what a pipe gets (nothing is asked)
//! OMSH_MOTION=off NO_COLOR=1 cargo run -p omsh-ui --example caps -- --demo
//! ```
//!
//! The report goes to standard output, the demo menu to standard error (where the shell's own
//! panels go). The terminal is asked one question (about 1 ms in a local terminal; at most
//! 150 ms when it says nothing); keys typed before it starts are not touched.

use std::io::{self, Write};

use omsh_config::Icons;
use omsh_ui::boxes::{Panel, open};
use omsh_ui::caps::{Caps, Out};
use omsh_ui::inline::{TerminalKeys, run};
use omsh_ui::sync::{self, Probe, Support};
use omsh_ui::text::{Line, Style, Tone};
use omsh_ui::theme::Theme;
use omsh_ui::widgets::buttons::Buttons;
use omsh_ui::widgets::list::{List, ListItem};
use omsh_ui::widgets::{Key, Step, Widget};

fn main() -> io::Result<()> {
    let demo = std::env::args().any(|argument| argument == "--demo");
    let caps = Caps::detect(Out::Stderr, Icons::Auto, None);
    let mut out = io::stdout().lock();

    writeln!(
        out,
        "omsh-ui {}: terminal capabilities",
        env!("CARGO_PKG_VERSION")
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "environment (only whether it is set, for the ones that hold addresses)"
    )?;
    for name in [
        "TERM",
        "COLORTERM",
        "LC_ALL",
        "LC_CTYPE",
        "LANG",
        "NO_COLOR",
        "CI",
        "OMSH_MOTION",
    ] {
        writeln!(out, "  {name:<16}{}", value(name))?;
    }
    for name in ["TMUX", "SSH_CONNECTION"] {
        let state = if std::env::var_os(name).is_some_and(|v| !v.is_empty()) {
            "set"
        } else {
            "not set"
        };
        writeln!(out, "  {name:<16}{state}")?;
    }

    writeln!(out)?;
    writeln!(out, "detected (the panels go to standard error)")?;
    let yes = |flag: bool| if flag { "yes" } else { "no" };
    writeln!(
        out,
        "  terminal        stdin {}, stdout {}, stderr {}  -> panels on a terminal: {}, interactive menu: {}",
        yes(caps.stdin_tty),
        yes(caps.stdout_tty),
        yes(caps.stderr_tty),
        yes(caps.out_tty),
        yes(caps.interactive())
    )?;
    writeln!(
        out,
        "  utf-8 locale   {}   unicode glyphs {}",
        yes(caps.utf8),
        yes(caps.unicode)
    )?;
    writeln!(
        out,
        "  color          {}   (NO_COLOR {}, TERM=dumb {})",
        yes(caps.color),
        yes(caps.no_color),
        yes(caps.dumb)
    )?;
    writeln!(
        out,
        "  motion         {:?}   (CI {}, ssh {}, tmux {})",
        caps.motion,
        yes(caps.ci),
        yes(caps.ssh),
        yes(caps.tmux)
    )?;
    writeln!(
        out,
        "  window         {} x {} (panels up to {} columns)",
        caps.width,
        caps.height,
        caps.panel_width(u16::MAX)
    )?;
    writeln!(out, "  spinner        {}", spinner(&caps))?;

    writeln!(out)?;
    writeln!(out, "synchronized output (DEC mode 2026, DECRQM)")?;
    let probe = sync::query(sync::LIMIT);
    report(&mut out, &probe)?;
    let sync_output = probe.support == Support::Yes;
    writeln!(
        out,
        "  -> redraws would be wrapped in synchronized output: {}",
        yes(sync_output)
    )?;
    out.flush()?;
    drop(out);

    if demo {
        demo_menus(caps.with_sync_output(sync_output))?;
    } else {
        println!();
        println!("run with --demo to open the real inline menus in this terminal");
    }
    Ok(())
}

fn value(name: &str) -> String {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => value,
        Ok(_) => "(empty)".to_owned(),
        Err(_) => "(not set)".to_owned(),
    }
}

fn spinner(caps: &Caps) -> String {
    let spinner = caps.spinner();
    (0..8)
        .map(|step| spinner.frame(std::time::Duration::from_millis(step * 100)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn report(out: &mut impl Write, probe: &Probe) -> io::Result<()> {
    let verdict = match probe.support {
        Support::Yes => "supported",
        Support::No => {
            "not supported (the terminal answered, and the mode is unknown or fixed off)"
        }
        Support::Silent => "no answer within the limit (taken as not supported)",
        Support::NotAsked => "not asked (input or output is not a terminal, or keys were waiting)",
    };
    writeln!(out, "  verdict         {verdict}")?;
    if probe.support != Support::NotAsked {
        let mode = match probe.mode {
            Some(0) => "0, not recognized",
            Some(1) => "1, set (on)",
            Some(2) => "2, reset (off, can be turned on)",
            Some(3) => "3, permanently set",
            Some(4) => "4, permanently reset (cannot be turned on)",
            Some(_) => "an unknown number",
            None => "none (no DECRQM reply)",
        };
        writeln!(out, "  mode reply      {mode}")?;
        writeln!(out, "  bytes received  {}", escaped(&probe.reply))?;
        writeln!(
            out,
            "  took            {:.2} ms",
            probe.elapsed.as_secs_f64() * 1000.0
        )?;
    }
    Ok(())
}

fn escaped(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "(nothing)".to_owned();
    }
    bytes
        .iter()
        .map(|byte| match byte {
            0x1b => "\\e".to_owned(),
            0x20..=0x7e => char::from(*byte).to_string(),
            other => format!("\\x{other:02x}"),
        })
        .collect()
}

/// A question like the "Run it?" panel: an open panel with the buttons inside the rail.
struct Question {
    panel: Panel,
    buttons: Buttons,
}

impl Widget for Question {
    type Output = usize;

    fn handle(&mut self, key: Key) -> Step<usize> {
        self.buttons.handle(key)
    }

    fn render(&self, caps: &Caps, width: u16) -> Vec<Line> {
        let width = width.min(64);
        let mut lines = open(&self.panel, caps, width);
        let bottom = lines.pop();
        let rail = if caps.unicode { "│ " } else { "| " };
        for row in self.buttons.render(caps, width.saturating_sub(2)) {
            let mut line = Line::new().then(rail, Style::of(Tone::Danger));
            line.append(&row);
            lines.push(line);
        }
        lines.extend(bottom);
        lines
    }
}

fn demo_menus(caps: Caps) -> io::Result<()> {
    let theme = Theme::default();
    let files = ["notes.txt", "রিপোর্ট.txt", "নথি/ছবি.png"];

    println!();
    println!("demo 1 of 2: a safety question. Try arrows, Tab, t / y / n, Enter, Esc.");
    let mut question = Question {
        panel: Panel::new("Run it?")
            .tone(Tone::Danger)
            .line(
                Line::new()
                    .tone(Tone::Danger, "danger")
                    .tone(Tone::Plain, "  rm -rf ")
                    .then(&files.join(" "), Style::of(Tone::Path)),
            )
            .line(Line::plain("This deletes 3 files and cannot be undone."))
            .hint("t", "trash")
            .hint("y", "run")
            .hint("n", "cancel"),
        buttons: Buttons::new(
            &[("Move to trash", 't'), ("Run anyway", 'y'), ("Cancel", 'n')],
            2,
        ),
    };
    let names = ["Move to trash", "Run anyway", "Cancel"];
    let numbered = question
        .buttons
        .to_numbered("Run it? rm -rf notes.txt রিপোর্ট.txt নথি/ছবি.png");
    let answer = ask(&mut question, &caps, &theme, || {
        numbered.ask(&mut io::stdin().lock(), &mut io::stderr())
    })?;
    println!(
        "answer: {}",
        answer.map_or("cancelled".to_owned(), |index| names[index].to_owned())
    );

    println!();
    println!("demo 2 of 2: a filterable list (type to filter, arrows, Enter, Esc).");
    let items: Vec<ListItem> = (1..=12)
        .map(|n| {
            ListItem::new(
                &format!("trashed-item-{n:02}"),
                &format!("/home/user/project/dir-{n}  {n} min ago"),
            )
        })
        .chain([ListItem::new("রিপোর্ট.txt", "/home/user/নথি  3 h ago")])
        .collect();
    let mut list = List::new(items, 6).filterable();
    let numbered = list.to_numbered("Restore which?");
    let answer = ask(&mut list, &caps, &theme, || {
        numbered.ask(&mut io::stdin().lock(), &mut io::stderr())
    })?;
    println!(
        "answer: {}",
        answer.map_or("cancelled".to_owned(), |index| format!(
            "item index {index}"
        ))
    );
    Ok(())
}

/// The inline menu when somebody can press keys, the numbered question when not.
fn ask<W: Widget<Output = usize>>(
    widget: &mut W,
    caps: &Caps,
    theme: &Theme,
    numbered: impl FnOnce() -> io::Result<Option<usize>>,
) -> io::Result<Option<usize>> {
    if !caps.interactive() {
        return numbered();
    }
    // The menu starts on an empty line under the (pretend) prompt.
    eprintln!("$ rm -rf notes.txt");
    let mut keys = TerminalKeys::open()?;
    let mut stderr = io::stderr().lock();
    let answer = run(widget, &mut keys, &mut stderr, theme, caps);
    drop(keys);
    stderr.flush()?;
    answer
}
