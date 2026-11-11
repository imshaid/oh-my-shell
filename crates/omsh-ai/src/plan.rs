//! The plan a model sends back, and the checks it must pass (blueprint §13.4).
//!
//! The model is asked for one JSON object. Models sometimes wrap it in a code fence or a
//! sentence, so [`Plan::parse`] takes the text from the first `{` to the last `}`, and then
//! holds it to the schema strictly. Everything wrong is reported as a [`PlanError`] whose
//! words are fit for both the person (one line) and the model (the retry message).
//!
//! What is checked here is what the text alone can tell: the shape, 1 to 8 steps, no control
//! characters (a model, or a file it read, could try to smuggle terminal escape codes into the
//! panel), and no placeholders such as `/path/to/file`. Whether each command parses in our
//! shell is checked by the caller, which has the parser (`omsh-ai` does not).

use std::fmt;

use serde::Deserialize;

/// The most steps a plan may have (§13.4).
pub const MAX_STEPS: usize = 8;
/// The longest command we accept, in characters. Longer is a runaway answer, not a command.
pub const MAX_COMMAND_CHARS: usize = 4000;

/// How risky the model thinks a step is. Advisory: the analyzer's level wins if higher (§13.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRisk {
    Low,
    Medium,
    High,
}

impl ModelRisk {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// The language the model wrote the plan in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    En,
    Bn,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Step {
    pub title: String,
    pub command: String,
    #[serde(default)]
    pub explanation: String,
    pub risk: ModelRisk,
    #[serde(default)]
    pub needs_root: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Plan {
    pub summary: String,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub language: Lang,
}

/// Why an answer is not a usable plan. The text is short and has no model output in it beyond
/// a quoted placeholder, so it can be shown as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanError {
    pub reason: String,
}

impl PlanError {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for PlanError {}

impl Plan {
    /// Reads and checks a model's answer.
    ///
    /// # Errors
    ///
    /// A [`PlanError`] saying what is wrong.
    pub fn parse(answer: &str) -> Result<Self, PlanError> {
        let json = extract_object(answer)
            .ok_or_else(|| PlanError::new("the answer has no JSON object in it"))?;
        let plan: Self = serde_json::from_str(json).map_err(|error| {
            // serde_json's text names the field and the position, never the input.
            PlanError::new(format!("the JSON does not match the schema: {error}"))
        })?;
        plan.validate()?;
        Ok(plan)
    }

    fn validate(&self) -> Result<(), PlanError> {
        if self.summary.trim().is_empty() {
            return Err(PlanError::new("the summary is empty"));
        }
        no_controls("the summary", &self.summary, false)?;
        if self.steps.is_empty() {
            return Err(PlanError::new("the plan has no steps"));
        }
        if self.steps.len() > MAX_STEPS {
            return Err(PlanError::new(format!(
                "the plan has {} steps; the most allowed is {MAX_STEPS}",
                self.steps.len()
            )));
        }
        for (index, step) in self.steps.iter().enumerate() {
            let number = index + 1;
            if step.title.trim().is_empty() {
                return Err(PlanError::new(format!("step {number} has no title")));
            }
            if step.command.trim().is_empty() {
                return Err(PlanError::new(format!("step {number} has no command")));
            }
            if step.command.chars().count() > MAX_COMMAND_CHARS {
                return Err(PlanError::new(format!(
                    "the command of step {number} is longer than {MAX_COMMAND_CHARS} characters"
                )));
            }
            no_controls(&format!("the title of step {number}"), &step.title, false)?;
            no_controls(
                &format!("the explanation of step {number}"),
                &step.explanation,
                true,
            )?;
            no_controls(
                &format!("the command of step {number}"),
                &step.command,
                true,
            )?;
            if let Some(found) = placeholder_in(&step.command) {
                return Err(PlanError::new(format!(
                    "the command of step {number} has a placeholder ({found}); use real names, \
                     or a command that finds them"
                )));
            }
        }
        Ok(())
    }
}

/// The text from the first `{` to the last `}`.
fn extract_object(answer: &str) -> Option<&str> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    (start < end).then(|| &answer[start..=end])
}

/// Rejects control characters (escape codes, carriage returns, the bell). Line breaks and tabs
/// are allowed where `layout` is set (commands, explanations).
fn no_controls(what: &str, text: &str, layout: bool) -> Result<(), PlanError> {
    let bad = text
        .chars()
        .any(|c| c.is_control() && !(layout && matches!(c, '\n' | '\t')));
    if bad {
        Err(PlanError::new(format!("{what} has a control character")))
    } else {
        Ok(())
    }
}

/// The first placeholder in a command, as the text that matched. A placeholder is a made-up
/// name the user would have to replace before the command could work.
#[must_use]
pub fn placeholder_in(command: &str) -> Option<String> {
    let lower = command.to_ascii_lowercase();
    for marker in [
        "/path/to",
        "path/to/",
        "replace_me",
        "changeme",
        "change_me",
    ] {
        if lower.contains(marker) {
            return Some(marker.to_owned());
        }
    }
    let chars: Vec<char> = command.chars().collect();
    let boundary = |i: usize| i == 0 || !chars[i - 1].is_alphanumeric();
    for i in 0..chars.len() {
        // YOUR_FILE, your_file, your-file (at the start of a name).
        if boundary(i) {
            let rest: String = chars[i..].iter().take(5).collect();
            if matches!(rest.as_str(), "YOUR_" | "your_" | "your-") {
                return Some(rest);
            }
        }
        // <filename>, <your name>: angle brackets around a bare name.
        if chars[i] == '<'
            && (i == 0 || chars[i - 1].is_whitespace() || "='\"".contains(chars[i - 1]))
        {
            let mut j = i + 1;
            while j < chars.len()
                && (chars[j].is_alphanumeric() || matches!(chars[j], '_' | '-' | ' '))
                && j - i <= 32
            {
                j += 1;
            }
            // A space just before `>` is a redirection (`sort <in >out`), not a name.
            if j > i + 1
                && j < chars.len()
                && chars[j] == '>'
                && chars[j - 1] != ' '
                && chars[i + 1].is_alphabetic()
            {
                return Some(chars[i..=j].iter().collect());
            }
        }
    }
    // A word that is just `...` or `…`.
    command
        .split_whitespace()
        .find(|word| matches!(*word, "..." | "…"))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_json(steps: &str) -> String {
        format!(r#"{{"summary":"Do it","steps":[{steps}],"language":"en"}}"#)
    }

    fn step(command: &str) -> String {
        let command = serde_json::to_string(command).unwrap();
        format!(
            r#"{{"title":"T","command":{command},"explanation":"E","risk":"low","needs_root":false}}"#
        )
    }

    #[test]
    fn a_valid_plan_is_read() {
        let plan = Plan::parse(&plan_json(&step("ls -la"))).unwrap();
        assert_eq!(plan.summary, "Do it");
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].command, "ls -la");
        assert_eq!(plan.steps[0].risk, ModelRisk::Low);
        assert!(!plan.steps[0].needs_root);
        assert_eq!(plan.language, Lang::En);
    }

    #[test]
    fn a_fence_or_prose_around_the_json_is_ignored() {
        let json = plan_json(&step("pwd"));
        for answer in [
            format!("```json\n{json}\n```"),
            format!("Here is your plan:\n{json}\nHope that helps!"),
            format!("  \n{json}  "),
        ] {
            assert!(Plan::parse(&answer).is_ok(), "{answer}");
        }
    }

    #[test]
    fn missing_optional_fields_take_defaults_and_bangla_is_known() {
        let answer = r#"{"summary":"s","steps":[{"title":"t","command":"ls","risk":"high"}],"language":"bn"}"#;
        let plan = Plan::parse(answer).unwrap();
        assert_eq!(plan.language, Lang::Bn);
        assert_eq!(plan.steps[0].explanation, "");
        assert!(!plan.steps[0].needs_root);
        let no_language = r#"{"summary":"s","steps":[{"title":"t","command":"ls","risk":"low"}]}"#;
        assert_eq!(Plan::parse(no_language).unwrap().language, Lang::En);
    }

    #[test]
    fn answers_that_are_not_plans_are_refused_with_a_reason() {
        let nine = (0..9).map(|_| step("ls")).collect::<Vec<_>>().join(",");
        let cases: Vec<(String, &str)> = vec![
            ("I cannot help with that.".into(), "no JSON object"),
            ("{ not json }".into(), "schema"),
            (r#"{"summary":"s"}"#.into(), "schema"),
            (plan_json(""), "no steps"),
            (plan_json(&nine), "9 steps"),
            (
                r#"{"summary":"s","steps":[{"title":"t","command":"ls","risk":"extreme"}]}"#.into(),
                "schema",
            ),
            (
                r#"{"summary":"s","steps":[{"title":"t","command":"ls","risk":"low"}],"language":"fr"}"#.into(),
                "schema",
            ),
            (
                r#"{"summary":"  ","steps":[{"title":"t","command":"ls","risk":"low"}]}"#.into(),
                "summary is empty",
            ),
            (
                r#"{"summary":"s","steps":[{"title":" ","command":"ls","risk":"low"}]}"#.into(),
                "no title",
            ),
            (
                r#"{"summary":"s","steps":[{"title":"t","command":"  ","risk":"low"}]}"#.into(),
                "no command",
            ),
        ];
        for (answer, expect) in cases {
            let error = Plan::parse(&answer).unwrap_err();
            assert!(error.reason.contains(expect), "{answer}: {error}");
        }
    }

    #[test]
    fn eight_steps_are_allowed() {
        let eight = (0..8).map(|_| step("ls")).collect::<Vec<_>>().join(",");
        assert_eq!(Plan::parse(&plan_json(&eight)).unwrap().steps.len(), 8);
    }

    #[test]
    fn control_characters_are_refused_but_layout_in_a_command_is_fine() {
        let escape = plan_json(&step("echo \u{1b}[2J"));
        assert!(Plan::parse(&escape).unwrap_err().reason.contains("control"));
        let carriage = plan_json(&step("echo a\rrm -rf x"));
        assert!(Plan::parse(&carriage).is_err());
        let title = r#"{"summary":"s","steps":[{"title":"a\nb","command":"ls","risk":"low"}]}"#;
        assert!(Plan::parse(title).unwrap_err().reason.contains("title"));
        let summary = "{\"summary\":\"s\\u001b[31m\",\"steps\":[{\"title\":\"t\",\"command\":\"ls\",\"risk\":\"low\"}]}";
        assert!(Plan::parse(summary).unwrap_err().reason.contains("summary"));
        let multi = plan_json(&step("if true; then\n\techo hi\nfi"));
        assert!(Plan::parse(&multi).is_ok());
    }

    #[test]
    fn a_runaway_command_is_refused() {
        let long = plan_json(&step(&"a".repeat(MAX_COMMAND_CHARS + 1)));
        assert!(Plan::parse(&long).unwrap_err().reason.contains("longer"));
        let fits = plan_json(&step(&"a".repeat(MAX_COMMAND_CHARS)));
        assert!(Plan::parse(&fits).is_ok());
    }

    #[test]
    fn placeholders_are_found() {
        for command in [
            "cp /path/to/file .",
            "cat path/to/x",
            "cp file /PATH/TO/dest",
            "scp x user@host:YOUR_DIR",
            "echo your_name",
            "mkdir your-project",
            "rm <filename>",
            "cp <source file> <dest>",
            "export KEY=REPLACE_ME",
            "tar -czf out.tar.gz ...",
            "sleep CHANGEME",
        ] {
            assert!(placeholder_in(command).is_some(), "{command}");
        }
    }

    #[test]
    fn real_commands_are_not_placeholders() {
        for command in [
            "ls -la",
            "cat <in.txt >out.txt",
            "sort <in >out",
            "echo \"wait...\"",
            "grep -E '<[a-z]+>' page.html",
            "cd ..",
            "echo yourself",
            "find . -name '*.log' -mtime +7",
            "echo \"$HOME/path\"",
            "cp a b && ls ../..",
            "echo a > /dev/null 2>&1",
            "diff <(ls) <(ls)",
        ] {
            assert_eq!(placeholder_in(command), None, "{command}");
        }
    }

    #[test]
    fn a_placeholder_in_a_plan_is_refused_with_its_step_number() {
        let plan = plan_json(&format!("{},{}", step("ls"), step("rm /path/to/file")));
        let error = Plan::parse(&plan).unwrap_err();
        assert!(error.reason.contains("step 2"), "{error}");
        assert!(error.reason.contains("placeholder"), "{error}");
    }
}
