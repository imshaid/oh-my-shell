//! Drawing a menu below the prompt and erasing it again (UI upgrade, U1). No alternate screen:
//! the menu is a few lines under the cursor, it is redrawn in place, and when it ends it is
//! gone and the scrollback is as it was.
//!
//! - [`Screen`] is the pure part: given the lines now wanted, it returns the bytes that change
//!   what is on the terminal into them. Only rows that changed are written, rows are cleared
//!   before they are written, rows that are no longer wanted are cleared, and the cursor moves
//!   with relative moves only. The redraw can be wrapped in synchronized output.
//! - [`run`] is the loop: hide the cursor, draw, read a key, let the widget handle it, draw
//!   again, and at the end erase and show the cursor, **also when something fails**.
//! - [`TerminalKeys`] reads real keys (raw mode, switched on for the life of the value).
//!
//! The menu starts at the cursor, which must be at the start of an empty line: the caller moves
//! below the prompt first. A frame is cut to the window (one column less than its width, and one
//! row less than its height), so that nothing wraps or scrolls out of reach of a relative move.
//! A window that is resized while a menu is open is not followed until the next key (known gap).

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};

use crate::caps::Caps;
use crate::motion::FrameClock;
use crate::sync;
use crate::text::{Line, render_line};
use crate::theme::Theme;
use crate::widgets::{Key, Step, Widget};

const CLEAR_LINE: &str = "\x1b[2K";
const CLEAR_BELOW: &str = "\x1b[J";
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";

/// What is on the terminal now, and where the cursor is in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Screen {
    drawn: Vec<String>,
    /// The cursor's row in the frame (0 is the first row).
    row: usize,
}

impl Screen {
    /// A screen with nothing drawn.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The rows now on the terminal.
    #[must_use]
    pub fn drawn(&self) -> &[String] {
        &self.drawn
    }

    /// The bytes that bring the terminal to `lines`, for `caps`. The lines are cut to the window
    /// first and written with the theme. Nothing is returned when nothing changed.
    pub fn draw(&mut self, lines: &[Line], theme: &Theme, caps: &Caps) -> Vec<u8> {
        let columns = usize::from(caps.panel_width(u16::MAX));
        let rows = usize::from(caps.height.saturating_sub(1).max(1));
        let wanted: Vec<String> = lines
            .iter()
            .take(rows)
            .map(|line| render_line(&line.fit(columns, caps.unicode), theme, caps))
            .collect();
        self.update(wanted, caps.sync_output)
    }

    /// The bytes that bring the terminal to `wanted` (rows already written with their escapes).
    pub fn update(&mut self, wanted: Vec<String>, sync_output: bool) -> Vec<u8> {
        if wanted == self.drawn {
            return Vec::new();
        }
        let mut out = String::new();
        if sync_output {
            out.push_str(sync::BEGIN);
        }
        if self.row > 0 {
            out.push_str(&format!("\x1b[{}A", self.row));
        }
        out.push('\r');
        let total = self.drawn.len().max(wanted.len());
        for index in 0..total {
            if index > 0 {
                out.push_str("\r\n");
            }
            match wanted.get(index) {
                Some(row) if self.drawn.get(index) != Some(row) => {
                    out.push_str(CLEAR_LINE);
                    out.push_str(row);
                }
                Some(_) => {}
                None => out.push_str(CLEAR_LINE),
            }
        }
        // The cursor is on the last row that was visited: go back up to the end of the frame.
        let last = total.saturating_sub(1);
        let end = wanted.len().saturating_sub(1);
        if end < last {
            out.push_str(&format!("\x1b[{}A", last - end));
        }
        if sync_output {
            out.push_str(sync::END);
        }
        self.row = end;
        self.drawn = wanted;
        out.into_bytes()
    }

    /// The bytes that remove the menu: back to its first row, then clear to the end of the
    /// screen. The cursor is left where the menu started.
    pub fn erase(&mut self) -> Vec<u8> {
        if self.drawn.is_empty() {
            return Vec::new();
        }
        let mut out = String::new();
        if self.row > 0 {
            out.push_str(&format!("\x1b[{}A", self.row));
        }
        out.push('\r');
        out.push_str(CLEAR_BELOW);
        self.drawn.clear();
        self.row = 0;
        out.into_bytes()
    }
}

/// What a key source found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// A key the widgets know.
    Key(Key),
    /// The wait ran out, or the window changed: draw again.
    Tick,
    /// There will be no more keys (the terminal went away).
    Closed,
}

/// Where keys come from.
pub trait KeySource {
    /// The next input. With `wait` set, [`Input::Tick`] after that long without one; with
    /// `None`, wait for ever (nothing moves, so no timer runs).
    ///
    /// # Errors
    ///
    /// When the terminal cannot be read.
    fn next(&mut self, wait: Option<Duration>) -> io::Result<Input>;
}

/// Shows `widget` below the cursor and returns what it chose: `Some` for an answer, `None` for
/// Esc, Ctrl+C or a closed terminal. The menu is erased and the cursor is shown again before it
/// returns, on every path.
///
/// Only for a person at a terminal: when [`Caps::interactive`] is false this writes nothing and
/// returns an [`io::ErrorKind::Unsupported`] error, so the caller uses the numbered fallback.
///
/// # Errors
///
/// When the terminal cannot be written or read, or when it is not interactive.
pub fn run<W, K, O>(
    widget: &mut W,
    keys: &mut K,
    out: &mut O,
    theme: &Theme,
    caps: &Caps,
) -> io::Result<Option<W::Output>>
where
    W: Widget,
    K: KeySource,
    O: Write,
{
    if !caps.interactive() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "an inline menu needs a terminal for input and output",
        ));
    }
    let mut screen = Screen::new();
    out.write_all(HIDE_CURSOR.as_bytes())?;
    let answer = drive(widget, keys, out, theme, caps, &mut screen);
    // Put the screen back whatever happened above; a failure here must not hide the first one.
    let cleanup = out
        .write_all(&screen.erase())
        .and_then(|()| out.write_all(SHOW_CURSOR.as_bytes()))
        .and_then(|()| out.flush());
    let answer = answer?;
    cleanup?;
    Ok(answer)
}

fn drive<W, K, O>(
    widget: &mut W,
    keys: &mut K,
    out: &mut O,
    theme: &Theme,
    caps: &Caps,
    screen: &mut Screen,
) -> io::Result<Option<W::Output>>
where
    W: Widget,
    K: KeySource,
    O: Write,
{
    let clock = FrameClock::new(caps.motion, caps.tmux);
    let width = caps.panel_width(u16::MAX);
    let opened = Instant::now();
    loop {
        widget.tick(opened.elapsed());
        out.write_all(&screen.draw(&widget.render(caps, width), theme, caps))?;
        out.flush()?;
        let drawn_at = Instant::now();
        let wait = clock.next_wake(widget.animating(), drawn_at.elapsed());
        match keys.next(wait)? {
            Input::Key(key) => match widget.handle(key) {
                Step::Stay => {}
                Step::Done(answer) => return Ok(Some(answer)),
                Step::Cancel => return Ok(None),
            },
            Input::Tick => {}
            Input::Closed => return Ok(None),
        }
    }
}

/// Raw mode for as long as the value lives, and the real keys. Switch it on only for the menu.
pub struct TerminalKeys {
    switched_on: bool,
}

impl TerminalKeys {
    /// Puts the terminal in raw mode.
    ///
    /// # Errors
    ///
    /// When raw mode cannot be switched on.
    pub fn open() -> io::Result<Self> {
        let already = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
        if !already {
            crossterm::terminal::enable_raw_mode()?;
        }
        Ok(Self {
            switched_on: !already,
        })
    }
}

impl Drop for TerminalKeys {
    fn drop(&mut self) {
        if self.switched_on {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

impl KeySource for TerminalKeys {
    fn next(&mut self, wait: Option<Duration>) -> io::Result<Input> {
        loop {
            if let Some(wait) = wait
                && !event::poll(wait)?
            {
                return Ok(Input::Tick);
            }
            match event::read()? {
                Event::Key(key) => {
                    if let Some(key) = Key::from_crossterm(key) {
                        return Ok(Input::Key(key));
                    }
                }
                Event::Resize(..) => return Ok(Input::Tick),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests;
