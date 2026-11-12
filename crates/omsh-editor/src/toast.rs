//! The toast (§15.3): one line above the prompt that goes away by itself.
//!
//! A toast is for something the person should know that is not worth a line in the scrollback:
//! "2 settings change at the next start". It is drawn as a line between the prompt's header and
//! its symbol, so it lives in the editor's live region and leaves nothing behind when it is gone. Two things
//! remove it: its time running out ([`LIFETIME`]), and the next line the person submits.
//!
//! [`ToastHandle`] is cheap to clone and can be used from any thread. Posting asks the editor for
//! a repaint (the same signal the late git branch uses), and a short-lived thread asks for one
//! more when the time is up, so the line disappears without a key press.
//!
//! The text is shown exactly as given except that control characters become blanks and it is
//! cut to [`MAX_CHARS`], so a message can never move the cursor or hide part of the screen.

use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// How long a toast stays.
pub const LIFETIME: Duration = Duration::from_secs(4);

/// The longest toast, in characters. A longer text is cut and ends with `...`.
pub const MAX_CHARS: usize = 100;

struct Slot {
    text: Option<String>,
    until: Instant,
}

/// Where toasts are posted and read. Clones share one slot.
#[derive(Clone)]
pub struct ToastHandle {
    slot: Arc<Mutex<Slot>>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    lifetime: Duration,
}

impl std::fmt::Debug for ToastHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToastHandle").finish_non_exhaustive()
    }
}

impl ToastHandle {
    /// A handle that calls `repaint` when the line should be drawn again.
    #[must_use]
    pub fn new(repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::with_lifetime(repaint, LIFETIME)
    }

    /// Like [`new`](Self::new) with another lifetime, so that a test need not wait four seconds.
    #[must_use]
    pub fn with_lifetime(repaint: Arc<dyn Fn() + Send + Sync>, lifetime: Duration) -> Self {
        Self {
            slot: Arc::new(Mutex::new(Slot {
                text: None,
                until: Instant::now(),
            })),
            repaint,
            lifetime,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Shows `text` for the toast's lifetime, replacing any toast that is showing. Nothing is
    /// shown for text that is empty after cleaning.
    pub fn post(&self, text: &str) {
        let text = clean(text);
        if text.is_empty() {
            return;
        }
        let until = Instant::now() + self.lifetime;
        {
            let mut slot = self.lock();
            slot.text = Some(text);
            slot.until = until;
        }
        (self.repaint)();
        // One more repaint when the time is up. Only the newest toast matters: an older timer
        // that fires early finds the newer text still alive and repaints for nothing, which is
        // harmless.
        let repaint = Arc::clone(&self.repaint);
        let lifetime = self.lifetime;
        let _ = thread::Builder::new()
            .name("toast-timer".to_owned())
            .spawn(move || {
                thread::sleep(lifetime);
                repaint();
            });
    }

    /// The text to draw now, if a toast is alive.
    #[must_use]
    pub fn current(&self) -> Option<String> {
        self.current_at(Instant::now())
    }

    fn current_at(&self, now: Instant) -> Option<String> {
        let slot = self.lock();
        if now < slot.until {
            slot.text.clone()
        } else {
            None
        }
    }

    /// Removes the toast at once (the person submitted a line).
    pub fn clear(&self) {
        self.lock().text = None;
    }
}

/// Control characters (and the line and text-direction characters that act like them) become
/// blanks, runs of blanks become one, and the text is cut to [`MAX_CHARS`].
fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut blank = true;
    for c in text.chars() {
        let hidden = c.is_control()
            || matches!(c, '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
        if hidden || c == ' ' {
            if !blank {
                out.push(' ');
                blank = true;
            }
        } else {
            out.push(c);
            blank = false;
        }
    }
    let trimmed = out.trim_end();
    if trimmed.chars().count() > MAX_CHARS {
        let mut cut: String = trimmed.chars().take(MAX_CHARS - 3).collect();
        cut.push_str("...");
        cut
    } else {
        trimmed.to_owned()
    }
}

/// The header line for `text`: a bullet and the text.
#[must_use]
pub fn line(text: &str, unicode: bool) -> String {
    format!("\x1b[1m{}\x1b[0m {text}", if unicode { "•" } else { "*" })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn handle(lifetime: Duration) -> (ToastHandle, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let handle = ToastHandle::with_lifetime(
            Arc::new(move || {
                seen.fetch_add(1, Ordering::SeqCst);
            }),
            lifetime,
        );
        (handle, count)
    }

    #[test]
    fn a_posted_toast_is_current_and_asks_for_a_repaint() {
        let (toast, repaints) = handle(Duration::from_secs(60));
        assert_eq!(toast.current(), None);
        toast.post("2 settings change at the next start");
        assert_eq!(
            toast.current().as_deref(),
            Some("2 settings change at the next start")
        );
        assert_eq!(repaints.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_toast_is_gone_when_its_time_is_up() {
        let (toast, _) = handle(Duration::from_secs(60));
        toast.post("hello");
        let alive = Instant::now() + Duration::from_secs(59);
        let over = Instant::now() + Duration::from_secs(61);
        assert_eq!(toast.current_at(alive).as_deref(), Some("hello"));
        assert_eq!(toast.current_at(over), None);
    }

    #[test]
    fn the_timer_asks_for_one_more_repaint_when_the_time_is_up() {
        let (toast, repaints) = handle(Duration::from_millis(30));
        toast.post("hello");
        let deadline = Instant::now() + Duration::from_secs(5);
        while repaints.load(Ordering::SeqCst) < 2 {
            assert!(Instant::now() < deadline, "no second repaint");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(toast.current(), None);
    }

    #[test]
    fn clearing_removes_the_toast_at_once() {
        let (toast, _) = handle(Duration::from_secs(60));
        toast.post("hello");
        toast.clear();
        assert_eq!(toast.current(), None);
    }

    #[test]
    fn a_newer_toast_replaces_the_older_and_restarts_the_time() {
        let (toast, _) = handle(Duration::from_secs(60));
        toast.post("first");
        toast.post("second");
        assert_eq!(toast.current().as_deref(), Some("second"));
    }

    #[test]
    fn clones_share_one_toast() {
        let (toast, _) = handle(Duration::from_secs(60));
        let other = toast.clone();
        other.post("shared");
        assert_eq!(toast.current().as_deref(), Some("shared"));
    }

    #[test]
    fn control_characters_never_reach_the_screen() {
        let (toast, _) = handle(Duration::from_secs(60));
        toast.post("a\x1b[2Jb\nc\u{202e}d\u{0}e");
        let shown = toast.current().expect("a toast");
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
        assert!(!shown.contains('\u{202e}'), "{shown:?}");
        assert_eq!(shown, "a [2Jb c d e");
    }

    #[test]
    fn text_with_nothing_to_show_is_not_posted() {
        let (toast, repaints) = handle(Duration::from_secs(60));
        toast.post(" \n\t ");
        assert_eq!(toast.current(), None);
        assert_eq!(repaints.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_long_text_is_cut_to_the_limit_with_dots() {
        let (toast, _) = handle(Duration::from_secs(60));
        toast.post(&"x".repeat(500));
        let shown = toast.current().expect("a toast");
        assert_eq!(shown.chars().count(), MAX_CHARS);
        assert!(shown.ends_with("..."));
        toast.post(&"y".repeat(MAX_CHARS));
        assert_eq!(toast.current().expect("a toast").chars().count(), MAX_CHARS);
    }

    #[test]
    fn the_line_has_a_bullet_that_is_ascii_without_unicode_and_no_color() {
        assert_eq!(line("hi", true), "\x1b[1m•\x1b[0m hi");
        assert_eq!(line("hi", false), "\x1b[1m*\x1b[0m hi");
        // Bold only: a toast sets no color, so NO_COLOR needs no special case.
        assert!(!line("hi", true).contains("\x1b[3"));
    }
}
