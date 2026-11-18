use super::*;
use crate::panels::test_support::caps;

fn text(line: &Line) -> String {
    line.plain_text()
}

fn tone_of(line: &Line) -> Tone {
    line.spans()[0].style.tone
}

#[test]
fn percent_is_rounded_down_so_a_value_under_a_threshold_stays_under_it() {
    assert_eq!(percent(0, 100), 0);
    assert_eq!(percent(699, 1000), 69);
    assert_eq!(percent(700, 1000), 70);
    assert_eq!(percent(7999, 10_000), 79);
    assert_eq!(percent(8000, 10_000), 80);
    assert_eq!(percent(8999, 10_000), 89);
    assert_eq!(percent(9000, 10_000), 90);
    assert_eq!(percent(5, 0), 0, "no division by zero");
}

#[test]
fn the_colors_change_at_70_and_90_and_the_hint_comes_at_80() {
    let c = caps(Motion::Full, true);
    for (used, tone, compact) in [
        (690, Tone::Ok, false),
        (700, Tone::Warn, false),
        (790, Tone::Warn, false),
        (800, Tone::Warn, true),
        (890, Tone::Warn, true),
        (900, Tone::Danger, true),
        (1000, Tone::Danger, true),
    ] {
        let l = line(Some(used), Some(1000), &c, true);
        assert_eq!(tone_of(&l), tone, "{used}: {}", text(&l));
        assert_eq!(
            text(&l).contains("compact?"),
            compact,
            "{used}: {}",
            text(&l)
        );
        assert!(
            text(&l).contains('%'),
            "the percent is always written: {}",
            text(&l)
        );
    }
}

#[test]
fn a_known_window_shows_the_numbers_the_percent_and_a_bar() {
    let c = caps(Motion::Full, true);
    assert_eq!(
        text(&line(Some(3400), Some(32768), &c, true)),
        "ctx ≈ 3,400 / 32,768  10%  █░░░░░░░░░"
    );
    assert_eq!(
        text(&line(Some(3400), Some(32768), &c, false)),
        "ctx ≈ 3,400 / 32,768  10%"
    );
}

#[test]
fn a_tiny_use_still_fills_one_cell_and_nothing_filled_is_empty() {
    let c = caps(Motion::Full, true);
    assert!(text(&line(Some(1), Some(1_000_000), &c, true)).ends_with("█░░░░░░░░░"));
    assert!(text(&line(Some(0), Some(1000), &c, true)).ends_with("░░░░░░░░░░"));
    assert!(
        text(&line(Some(5000), Some(1000), &c, true)).contains("██████████"),
        "over 100% fills the bar"
    );
}

#[test]
fn an_unknown_window_says_so_in_full_weight_text_and_the_empty_bar_is_faint() {
    let c = caps(Motion::Full, true);
    let l = line(Some(3400), None, &c, true);
    assert_eq!(text(&l), "ctx ≈ 3,400 / limit unknown  ░░░░░░░░░░");
    for span in l.spans().iter().filter(|s| !s.text().trim().is_empty()) {
        // Words and numbers are muted (full weight); only the empty bar is faint.
        let expected = if span.text().contains('░') {
            Tone::Dim
        } else {
            Tone::Muted
        };
        assert_eq!(span.style.tone, expected, "{:?}", span.text());
    }
    assert!(!text(&l).contains('%'), "no percent is made up");
}

#[test]
fn an_unknown_use_is_a_dash_not_zero() {
    let c = caps(Motion::Full, true);
    assert_eq!(
        text(&line(None, Some(32768), &c, false)),
        "ctx ≈ – / 32,768"
    );
    assert_eq!(
        text(&line(None, None, &c, false)),
        "ctx ≈ – / limit unknown"
    );
}

#[test]
fn ascii_uses_plain_characters() {
    let c = caps(Motion::Full, false);
    let l = text(&line(Some(900), Some(1000), &c, true));
    assert_eq!(l, "ctx ~ 900 / 1,000  90%  #########-  compact?");
    assert!(l.is_ascii());
}

#[test]
fn the_number_eases_in_600_ms_with_full_motion_and_jumps_otherwise() {
    let meter = Meter::new(Some(1000), Some(10_000), Some(0));
    let ms = Duration::from_millis;
    let full = Motion::Full;
    assert_eq!(meter.shown(ms(0), full), Some(0));
    let mid = meter.shown(ms(300), full).unwrap();
    assert!(mid > 0 && mid < 1000, "{mid}");
    assert_eq!(meter.shown(ms(600), full), Some(1000));
    assert!(meter.moving(ms(100), full));
    assert!(!meter.moving(ms(600), full));
    for motion in [Motion::Reduced, Motion::Off] {
        assert_eq!(
            meter.shown(ms(0), motion),
            Some(1000),
            "{motion:?}: no easing"
        );
        assert!(!meter.moving(ms(0), motion), "{motion:?}: nothing moves");
    }
}

#[test]
fn a_meter_with_no_use_never_moves() {
    let meter = Meter::new(None, Some(1000), None);
    assert_eq!(meter.shown(Duration::ZERO, Motion::Full), None);
    assert!(!meter.moving(Duration::ZERO, Motion::Full));
    assert_eq!(
        Meter::new(Some(5), Some(0), None)
            .line(&caps(Motion::Off, true), Duration::ZERO)
            .plain_text(),
        "ctx ≈ 5 / limit unknown  ░░░░░░░░░░"
    );
}
