# Oh My Shell (omsh) — UI/UX Upgrade Spec

Status: optional upgrade on top of v1.0.0 (commit 8323395). Target release: v1.1.0.
Work on branch `ui-upgrade`. Keep `v1.0.0` untouched. Every phase must be shippable on its own; the project may stop after any phase.

How to use this file: the task prompt names ONE phase. Read the whole spec once, implement only that phase, run the gates in section 12, then stop and report. Do not start the next phase.

Where this spec says "verify", the claim was not confirmed. Check it in code or by test before relying on it, and say so in the report if it turns out wrong.

---

## Status: phases 1 to 7 are done on the branch `ui-upgrade` (version 1.1.0, not released)

Phase 1 foundation, 2 shell look, 3 safety panels, 4 AI panels, 5 install and first run and update, 6 jobs and
plan-running panels, 7 docs, site, changelog and version. Where the result differs from the text below:

- **Colors.** A ninth theme token `key` (cyan) next to `accent` (blue). **Faint text is decoration only** (rules,
  separators, the empty part of a bar). Text the spec calls "dim" but that carries information (hint bar words,
  update line, `Alt+? explain`, the info line, timers, the "ctx" meter text) is drawn in the terminal's own text
  color (token `muted`), because faint text was hard to read on some themes. A test fails on any faint letter or
  digit. `ui.motion = "on"` means the same as `"full"`.
- **Shell look.** `ui.style` has `plain` and `spaced`; **`blocks` was not built**. The rule is printed above the
  header and the status chip is at the right of the header. Plain arguments have no color. The hint bar is not shown
  in the session that showed the welcome, or under `--no-config`.
- **Explain hint.** It names `Alt+?`, not `[e]` (`e` would take `echo`, `exit`, `env`). It sends the failed command
  and its exit status, not stderr (the error text is never captured).
- **Safety panels.** On the `undo` picker Enter opens a Restore / Cancel question and never restores at once. A
  critical command keeps the typed `yes`; its panel has no buttons.
- **AI panels.** Run all is chosen when the plan panel opens (the safe answer is still asked again for a riskier
  step). The context limit comes from small lookups of our own (Gemini, Anthropic and Ollama) or from
  `ai.context_limit`; with neither it is shown as unknown. `ai.inline.fields` and `ai.inline.thinking_warn` are
  settings.
- **Update.** The check keeps the GitHub `releases/latest` redirect and a helper thread; **`version.json` on Pages and
  a detached process were not used**. The news is one line at the next start in the hint bar's place. "Stay up to
  date?" has **No** chosen (the spec chose Yes), because the daily check stays off by default. **`omsh changelog`
  does not exist**, so the line does not say what is new. `ui.ls` (use `eza` for `ls`) was added at the owner's
  request.
- **Jobs and plans.** `jobs -i` and the palette open the table; plain `jobs` is unchanged. The time comes before the
  command in the table, so that a cut command never takes the time with it. **No live progress while a plan step
  runs:** the step owns the terminal and the shell waits for it, so there is no spinner or running clock; Pause is
  Ctrl+Z, and the panels appear where the shell has the keyboard. `undo 3` and `cmd | ? q` still ask in text.
- **Not verified:** the plan and pause panels with a real AI key, and the panels in tmux and over ssh (except the
  Run it? panel, captured in tmux for the website); `cargo deny` and an aarch64 build for this version.

---

## 1. Hard constraints (apply to every phase)

1. **Colors come from the terminal palette only.** ANSI 16 colors (SGR 30-37, 90-97 where needed) plus default fg/bg. NO RGB, NO 256-color indexes, NO background fills, NO fixed greys.
   - Dim = SGR 2. Selection = reverse video (SGR 7). Bold/underline allowed.
   - Add a test that fails if any `38;2`, `48;2`, `38;5`, `48;5` escape exists in source or in rendered output.
2. **`NO_COLOR`**: when set and non-empty, disable colors only. Keep bold, reverse, underline so menus and selection still work.
3. **Meaning is never color-only.** Words/symbols (`safe`, `danger`, `✓`, `✘`, `%`) always accompany a color.
4. **Color meanings:** red = danger/error, green = ok/safe, yellow = caution, cyan = keys/paths/interactive, magenta = AI, blue = info.
5. **Performance budget:** `--version` and start-to-prompt must not regress from v1.0.0 (about 0.79 ms and about 2 ms). Measure before and after each phase. Any UI work at startup must be lazy.
6. **No network on the normal path** (Gate C). Network is allowed only for: AI calls the user triggers, and the opt-in update check (section 9). Add/keep a test that proves no socket is opened on the normal path when update check is off.
7. **Fallbacks are mandatory.** Not a TTY, `-c` mode, tests, `TERM=dumb`: no cursor control, no animation, numbered-list selection or plain text.
8. **Non-UTF-8 locale:** boxes fall back to `+ - |` ASCII. Spinner falls back to `| / - \`.
9. **Bangla/combining text:** column width can be wrong. Panels containing user text (filenames, commands) use an OPEN right edge (left rail + rules, no right border). Full boxes only for text we fully control.
10. Code and comments in English only. Follow existing crate boundaries. No new heavy dependencies without a one-line justification in the report.
11. Do not change safety semantics. Dangerous-command detection and its decisions stay as they are; only the presentation changes.

## 2. Architecture

- All UI lives in `omsh-ui`. Other crates call it.
- **Pure render rule:** every panel is `render(state, caps) -> Vec<Line>` where a `Line` is a list of styled segments (text + semantic style such as `Danger`, `Ok`, `Ai`, `Key`, `Dim`). A separate small layer turns lines into escape sequences according to `caps`. Panels must be testable as plain data without a TTY.
- **`Caps`** (detected once, cached): is_tty (stdin/stdout/stderr separately), utf8, no_color, motion (`on|reduced|off`), sync_output (DEC 2026 supported), width/height, ci.
- **Semantic styles, not colors, in panel code.** Only the output layer maps a style to SGR.
- **Inline, not alternate screen.** Menus draw below the prompt and erase themselves on exit, preserving scrollback.
- If `ratatui` inline viewport or a similar crate is used, justify it in the report; own small widgets are the default (decision: "our own", not inquire/cliclack).

## 3. Visual language

### Boxes
- Rounded `╭─╮│╰╯`. Title in the top rule, key hints in the bottom rule.
- Full box (right border) only for panels with text entirely ours (setup, help, tour, welcome, details).
- Open-right panels (left rail `│` + top/bottom rules) for Run it?, AI plan, and anything showing user text.
- ASCII fallback `+ - |`.

### Widgets (all keyboard driven)
- **Buttons row:** `←→` or `Tab` move, `Enter` confirm, `Esc` cancel, plus letter shortcuts (t/y/n etc.). Selected = reverse video; unselected = `[label]`.
- **List:** `↑↓`, `Enter`, `Esc`, optional type-to-filter, scroll hints `↑ n more` / `↓ n more`.
- **Numbered fallback:** when not a TTY, `-c`, tests: print numbered options and read a number or letter.
- Default highlighted choice must match the `[y/N]` convention: the safe choice (Cancel) is the default.

## 4. Shell look (Phase 2)

### Style 2 (default)
- A blank line and a thin dim rule between commands. cwd on top of each entry (dim/blue). Status on the right: `✓ 4 ms` (green) or `✘ 127` (red). Input starting with `?` shows an `ask AI` tag (magenta).
- Config: `ui.style = "plain" | "spaced" | "blocks"`, default `spaced` (this is style 2).

### Syntax highlighting while typing
- valid command = green, invalid command = red, flags = cyan, paths = underlined, strings = yellow, operators = magenta.
- Must stay correct with Bangla/combining text (cursor column). Add tests with Bangla filenames.
- Must not slow typing: highlight must be incremental or cheap. Measure.

### Hint bar
- Only at the top of a session. Text: `? ask AI · Ctrl+P palette · Ctrl+R history · omsh tour`. Add `omsh ai setup` while AI is not configured.
- Fade-out: full hint for the first 5 sessions, then nothing. Counter stored in a small state file. Config: `ui.greeting = "auto" | "always" | "never"`.

### OSC 133
- Emit `OSC 133;A` before the prompt and `OSC 133;C` before running a command (and `D` with exit code if straightforward). Purpose: kitty can jump between prompts and open last output. Verify this works when omsh runs inside kitty (kitty's own integration loads in the outer shell, so omsh must emit the marks itself). Only emit on a TTY.

## 5. Panels (Phases 3, 4, 6)

### Run it? (Phase 3)
Options: Move to trash / Run anyway / Cancel. Default highlight = Cancel. Open-right panel (shows file names). Shortcuts `t`, `y`, `n`.

### `undo` picker (Phase 3)
Arrow-key list of recently trashed items with preview (name, original path, time). Enter restores. Esc cancels.

### Explain hint (Phase 3)
After a failed command, show a dim hint `[e] explain` on the status line. Pressing `e` at the next prompt sends the failed command, its exit code, and short captured stderr (if available) to AI. Only if AI is configured; otherwise show nothing.

### AI plan (Phase 4)
Shows steps with risk tags (`safe`, `caution`, `danger` as words plus color). Buttons: Run all / Step by step / Edit / Chat / Cancel. Keep the existing keys (`Enter` run, `e` edit, `c` chat, `v` view context, `Esc` cancel). Open-right panel.

### AI setup (Phase 4)
- Provider list and model list as filterable lists.
- Gemini model list: show only text/chat models. Filter by `supportedGenerationMethods` containing `generateContent`, and drop image, tts, transcribe, deep-research style ids. Verify the filter against the live model list; keep an "show all" toggle.
- If AI is not set up and the user types `? question`: show a small panel `Set up AI now / Not now` instead of a bare error.

### Plan running (Phase 6)
Step states: `✓` done, spinner running, `○` pending, `?` needs confirmation. Progress bar, elapsed time, buttons Pause / Skip / Stop / To background. Open-right panel.

### Jobs table (Phase 6)
Columns: `%id`, state (`● Running`, `◼ Stopped`, `✓ Done`, `✘ Killed`), command, mm:ss. Actions: Foreground / Background / Stop / Kill.

## 6. AI usage, inline info, details (Phase 4)

### Normalized usage struct (in `omsh-ai`)
All numeric fields are `Option`. Never invent a value.
`provider, model, effort, input, thinking, output, cached, total, time, tokens_per_sec, context_used, context_limit`.

Rule: a field the provider does not report is shown as `–`, never 0 and never a guess. Totals come from the provider's own total when present; do not double count thinking tokens.

### Provider mapping
- **Gemini:** `promptTokenCount`, `candidatesTokenCount`, `thoughtsTokenCount`, `totalTokenCount`, `cachedContentTokenCount`. Whether `candidatesTokenCount` includes thoughts differs between the Gemini API and Vertex; handle by trusting `totalTokenCount`. `thoughtsTokenCount` has been reported missing for some models, so keep it `Option`. Effort is `thinkingLevel` on newer models. Context limit: `inputTokenLimit` from `models.get`.
- **Anthropic:** `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`; total input is their sum. Context limit: `max_input_tokens` from the models API; it can be null, so fall back to `–`. Effort values (`low|medium|high|max`) came from a third-party doc mirror: verify before hardcoding.
- **OpenAI:** `output_tokens_details.reasoning_tokens` / `input_tokens_details.cached_tokens` / `total_tokens` (Responses API); Chat Completions uses `completion_tokens_details.reasoning_tokens`. Hidden reasoning is inside completion tokens. The models list gives NO context window: use `–` or a user config value.
- **Ollama:** `prompt_eval_count`, `eval_count`, `total_duration`, `load_duration`, `prompt_eval_duration`, `eval_duration` (nanoseconds, in the final streamed chunk). Tokens/s is derived (`eval_count / eval_duration`) and must be labeled as derived. Context: use the EFFECTIVE `num_ctx` of the request (or the loaded model's effective context), NOT the model maximum (`llama.context_length` from `/api/show`); Ollama's own docs give different defaults on different pages.

### Context meter
- `used / limit  pct%` plus a bar. If limit is unknown: `used / limit unknown`, bar dim.
- "Used" for the next call is approximated by the last call's total; label it `ctx ≈` and treat it as approximate.
- Color: green below 70%, yellow from 70%, red from 90%. The percent number always shown.
- At 80%+ show a `compact?` hint (summarize older history).

### Inline info line (under each AI answer)
- Default fields: `time · in · out`. Header of the chat box: model, effort (dim), and `ctx` meter.
- `Tab` toggles an expanded line: thinking, cached, total.
- Config: `[ai.inline] fields = ["time","in","out"]`; allowed names: `model, effort, time, in, out, thinking, cached, total, ctx, speed`.
- Warn (yellow) when thinking tokens spike far above output (threshold configurable).
- Cost shown only if the user configured a price table.

### AI call details panel
Rows: Provider, Model, Effort, Context (used / limit, pct, bar), Input, Thinking, Output, Cached, Total, Time, Speed. Provider tabs switch with `←→`. A source note under the table says where the context limit came from. Full box.

## 7. Motion (applies from Phase 1 engine, used in later phases)

Principle: the shell feels alive during work and silent when idle.

- **Where:** AI thinking (spinner + running timer, magenta), streaming answer (character reveal + cursor), context meter eases to new value (~600 ms), details panel rows appear one by one with count-up numbers, job/step finish = short reverse-video flash on the status chip, failure = one short red flash then settle, progress bars, idle = only cursor blink (or none).
- **Rules:**
  1. Animate only on a TTY. Never on pipe, `-c`, tests, CI.
  2. Max about 12-15 fps; redraw only the lines that changed. When idle, no timers (0% CPU).
  3. Always settle into a static final symbol (`✓`/`✘`) so scrollback stays clean.
  4. Motion never carries meaning alone.
  5. `ui.motion = "on" | "reduced" | "off"`; env `OMSH_MOTION=off` wins. Auto `reduced` over SSH or very slow terminals (heuristic, verify). `NO_COLOR` does not disable motion.
  6. Palette colors only; shimmer is done with dim/normal/bold changes.
  7. No startup animation of any kind.
- **Synchronized output (DEC mode 2026):** wrap each redraw in `ESC[?2026h` ... `ESC[?2026l`. Unsupported terminals ignore the sequences. Detect with a DECRQM query (`CSI ? 2026 $ p`): no reply or `;0$y` means unsupported. Do not allowlist by terminal name (sources disagree about some terminals). Under tmux, lower the frame rate. Never block waiting for the query reply for long: use a short timeout, cache the answer.

## 8. Installer UI (Phase 5)

`install.sh` is POSIX `sh`. Test with dash and bash. Constraints: stdin is the pipe in `curl | sh`, so NO prompts. Use `[ -t 1 ]` to detect a TTY on stdout.

TTY output (palette colors only):
```
  Oh My Shell · installer

  ⠹ downloading omsh 1.0.0   ████████░░░░░░  58%  1.9 MB/s
  ✓ platform      linux x86_64
  ✓ checksum      sha256 OK
  ✓ installed     ~/.local/bin/omsh
  ✓ verified      starts in 0.8 ms
```
- Spinner per step, settles into `✓`. Real download progress with speed. After install, run `omsh --version` and print the MEASURED time (real number, never fabricated).
- Next-steps box appears line by line (about 30 ms per line).
- Not a TTY: one plain line per step, no escapes.
- Hide the cursor only with a `trap` that restores it on exit, Ctrl+C, and error.
- Respect `NO_COLOR` and `OMSH_MOTION=off`. Total animation at most 1-2 seconds.
- **PATH check:** if `~/.local/bin` is not in PATH, print the exact fix for the user's shell (fish: `fish_add_path ~/.local/bin`; bash/zsh: export line). Do NOT edit any startup file. Do not run `chsh`. Touch `/etc/shells` only with `--add-to-shells` (existing behavior).
- Final box: `omsh` (try now, login shell unchanged), `omsh tour`, `omsh ai setup` (optional), `omsh update`.
- Also update the website: add a PATH note near the install command.

## 9. First run and update notice (Phase 5)

### First run (once, TTY only, never in `-c`/pipe/tests)
Welcome panel (full box): Take the 2-minute tour / Set up AI now / Skip. `Esc` skips, nothing blocks the prompt. Marker file ensures once. `omsh tour` replays it. Then a second panel: "Stay up to date?" with `Yes, check daily (recommended)` (default highlighted) / `No, I'll run omsh update --check myself`.

### Each open
Nothing but the prompt, except the hint bar rules in section 4 and the update notice below. No banner, no logo, no system-info dump.

### Update check design (opt-in)
- Gate C wording becomes: no network unless the user enabled AI or the update check.
- If enabled and the cache is older than 24 h: spawn a detached background process AFTER the first prompt is shown; it fetches the latest version and writes a small cache file. The shell never waits for it. A fetch failure is silent.
- Source of truth: a tiny `version.json` on the project's GitHub Pages site. Do not use the GitHub REST API: unauthenticated limits are 60 requests per hour per IP, and campus networks share IPs. Verify Pages behavior (caching, limits) before relying on it.
- At the next session start, read the cache file (no network). If newer: one dim line in the hint-bar slot, at most once per 24 h: `↑ omsh 1.1.0 available · run: omsh update · what's new: omsh changelog`.
- Never auto-install. `omsh update` verifies the checksum.
- Skip entirely on non-TTY, `-c`, CI. Opt-outs: `OMSH_NO_UPDATE_CHECK=1`, config `update.check = false`.
- **Offline nudge** for users who opted out: if the build is older than 60 days, show once `this build is N days old · check for a new one: omsh update --check`. No network.
- `omsh update --check` does an explicit one-off check (network by user request).
- `omsh status` (or `omsh doctor`): a box with version, build age, AI configured?, update check on/off, motion mode, terminal capabilities detected.

## 10. Config summary (config.toml)

```
[ui]
style = "spaced"          # plain | spaced | blocks
greeting = "auto"         # auto | always | never
motion = "on"             # on | reduced | off

[ai.inline]
fields = ["time", "in", "out"]

[update]
check = true              # set by the first-run panel; false if declined
```

## 11. Out of scope

Truecolor themes, background fills, alternate-screen TUI, mouse support, auto-updating, telemetry, any new network call beyond section 9.

## 12. Gates (run at the end of EVERY phase)

1. `cargo build --release`, `cargo test`, `cargo clippy` clean. Known baseline: about 25 sh-oracle tests fail on Arch because `/bin/sh` is bash; CI uses dash and passes. Compare against the baseline; do not "fix" those here.
2. Performance: re-measure `--version` and start-to-prompt; report before/after numbers. Stop and report if worse than about 10%.
3. No-RGB/256-color guard test passes.
4. Snapshot tests of rendered panels for: normal, `NO_COLOR`, non-UTF-8 (ASCII), non-TTY (numbered fallback), and Bangla filenames.
5. Gate C test: no socket on the normal path (update check off).
6. Report: what changed, what was verified, what could not be verified (the "verify" items), and anything left for later phases. No marketing language.

## 13. Phases

1. **Foundation (`omsh-ui`):** Caps detection, semantic styles + palette output layer, NO_COLOR, ASCII fallback, boxes (full/open), buttons/list/filter widgets with numbered fallback, motion engine (tick, spinner, easing, reduced mode), sync-output with DECRQM and timeout, the no-RGB guard test, snapshot test harness. No shell behavior changes yet.
2. **Shell look:** style 2 layout, right-side status, syntax highlighting, hint bar with fade-out and counter, OSC 133 marks.
3. **Safety panels:** Run it?, undo picker, explain hint.
4. **AI panels:** normalized usage struct and provider mapping, AI plan panel, inline info line, details panel with context window, AI setup lists with text-model filter, "AI not set up" panel.
5. **Install, first run, update:** installer UI, website PATH note, first-run panels, opt-in update check, offline nudge, `omsh status`.
6. **Jobs and plan-running panels.**
7. **Polish and release:** docs, site, final perf numbers, changelog, v1.1.0.

Suggested stopping points that already give most of the value: after Phase 2, after Phase 4.
