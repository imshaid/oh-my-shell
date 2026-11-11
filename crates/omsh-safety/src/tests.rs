//! Tests of the analyzer. The big table is `CASES`: every rule has lines it must flag, with
//! the level it must give, and lines it must leave alone. A meta-test fails if a rule has no
//! case of either kind.

use std::path::Path;

use omsh_syntax::{parse, parse_tolerant};
use proptest::prelude::*;

use crate::{Analysis, Context, Level, Probe, RULES, analyze};

use Level::{Caution, Critical, Dangerous};

const CWD: &str = "/home/me/work";
const HOME: &str = "/home/me";

/// A file system with two files that have content.
struct FakeProbe;

impl Probe for FakeProbe {
    fn is_nonempty_file(&self, path: &Path) -> bool {
        ["/home/me/work/notes.txt", "/etc/hosts"].contains(&path.to_str().unwrap_or(""))
    }
}

fn run(line: &str) -> Analysis {
    let list = parse(line).unwrap_or_else(|_| panic!("the test line does not parse: {line:?}"));
    let ctx = Context {
        cwd: Some(Path::new(CWD)),
        home: Some(Path::new(HOME)),
        probe: &FakeProbe,
    };
    analyze(&list, line, &ctx)
}

fn level_of(line: &str) -> Level {
    run(line).level()
}

/// The level the rule `id` gives to `line`, or `None` if it does not match.
fn rule_level(id: &str, line: &str) -> Option<Level> {
    run(line)
        .findings
        .iter()
        .filter(|finding| finding.rule.id == id)
        .map(|finding| finding.level)
        .max()
}

struct Case {
    id: &'static str,
    /// Lines the rule must flag, with the level it must give.
    positives: &'static [(&'static str, Level)],
    /// Lines the rule must not flag (other rules may).
    negatives: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        id: "rm-critical-path",
        positives: &[
            ("rm -rf /", Critical),
            ("rm -rf /*", Critical),
            ("rm -fr ~", Critical),
            ("rm -rf ~/", Critical),
            ("rm -r ~/*", Critical),
            ("rm -rf $HOME", Critical),
            ("rm -rf ..", Critical),
            ("rm -rf ../..", Critical),
            ("rm -rf /etc", Critical),
            ("rm -rf /home", Critical),
            ("rm -rf \"$d\"/", Critical),
            ("rm -rf \"$d\"/*", Critical),
            ("rm -rf \"$d/$e\"", Critical),
            ("rm -rf --no-preserve-root /", Critical),
            ("rm /*", Dangerous),
            ("cd / && rm -rf *", Critical),
            ("cd && rm -rf *", Critical),
            ("sudo rm -rf /", Critical),
        ],
        negatives: &[
            "echo rm -rf /",
            "rm -rf ./build",
            "rm -rf /tmp/x",
            "rm -rf /etc/nginx",
            "rm -rf ~/projects/old",
            "rm file",
            "rm -rf '/ not'",
            "ls /",
            "rm --help",
            "rm -rf \"$d\"",
            "cd /tmp && rm -rf *",
        ],
    },
    Case {
        id: "rm-recursive-or-force",
        positives: &[
            ("rm -rf build", Dangerous),
            ("rm -r build", Dangerous),
            ("rm -R build", Dangerous),
            ("rm -f x.log", Dangerous),
            ("rm --recursive build", Dangerous),
            ("rm --force x", Dangerous),
            ("/bin/rm -rf build", Dangerous),
            ("command rm -rf build", Dangerous),
            ("rm -rf *", Dangerous),
            ("sudo rm -rf build", Critical),
        ],
        negatives: &[
            "rm build",
            "rm -i x",
            "rm -rf /",
            "echo rm -rf x",
            "rm -rf",
            "ls -rf x",
            "rm --help",
        ],
    },
    Case {
        id: "rm-glob",
        positives: &[
            ("rm *.tmp", Caution),
            ("rm file?.txt", Caution),
            ("rm [ab].c", Caution),
            ("sudo rm *.o", Dangerous),
        ],
        negatives: &[
            "rm '*.tmp'",
            "rm \"*.tmp\"",
            "rm a.tmp",
            "rm -f *.tmp",
            "ls *.tmp",
            "rm /*",
        ],
    },
    Case {
        id: "destructive-unknown-target",
        positives: &[
            ("rm $(find . -name '*.o')", Dangerous),
            ("rm `ls`", Dangerous),
            ("rm -rf \"$dir\"/build", Dangerous),
            ("rm -f $x", Dangerous),
            ("find . | xargs rm", Dangerous),
            ("chmod -R 755 $d", Dangerous),
            ("chown -R me \"$d\"", Dangerous),
            ("dd if=a of=$dev", Dangerous),
            ("sudo rm $(ls)", Critical),
        ],
        negatives: &[
            "rm $x",
            "rm file",
            "chmod 755 $d",
            "chmod -R 755 ./x",
            "echo $(ls)",
            "dd if=a of=b",
            "rm -rf \"$d\"/",
            "ls $(pwd)",
        ],
    },
    Case {
        id: "dd-to-device",
        positives: &[
            ("dd if=a.img of=/dev/sda bs=4M", Critical),
            ("dd of=/dev/nvme0n1", Critical),
            ("dd of=/dev/mmcblk0", Critical),
            ("sudo dd if=x of=/dev/sdb", Critical),
        ],
        negatives: &[
            "dd if=/dev/zero of=out.img",
            "dd if=/dev/sda of=disk.img",
            "dd of=/dev/null",
            "dd of=/dev/stdout",
            "dd of=out.img",
            "echo dd of=/dev/sda",
        ],
    },
    Case {
        id: "format-or-wipe",
        positives: &[
            ("mkfs.ext4 /dev/sdb1", Critical),
            ("mkfs -t ext4 /dev/sdb1", Critical),
            ("wipefs -a /dev/sdb", Critical),
            ("shred -u secret.txt", Critical),
            ("mke2fs /dev/sdb", Critical),
            ("sudo mkfs.ext4 x", Critical),
        ],
        negatives: &[
            "mkdir x",
            "mkfs --help",
            "echo mkfs.ext4",
            "ls /sbin/mkfs*",
            "shredder x",
        ],
    },
    Case {
        id: "write-to-disk-device",
        positives: &[
            ("echo x > /dev/sda", Critical),
            ("cat img >> /dev/nvme0n1", Critical),
            ("cat img > /dev/sdb1", Critical),
            ("cat a &> /dev/vda", Critical),
            ("echo > /dev/mmcblk0", Critical),
            ("true >| /dev/sda", Critical),
        ],
        negatives: &[
            "ls > /dev/null",
            "echo x > /dev/tty",
            "cat /dev/sda > disk.img",
            "echo > sda",
            "echo x > /dev/stdout",
            "cat < /dev/sda",
            "echo > \"$d\"",
        ],
    },
    Case {
        id: "chmod-chown-system-recursive",
        positives: &[
            ("chmod -R 755 /", Critical),
            ("chown -R me /etc", Critical),
            ("chmod -R 700 /usr", Critical),
            ("chgrp -R x /var", Critical),
            ("chmod -R 755 /bin /tmp/x", Critical),
            ("chown --recursive me /boot", Critical),
            ("chmod -R 755 /home", Critical),
            ("chmod -R a-w /*", Critical),
        ],
        negatives: &[
            "chmod -R 755 ./x",
            "chmod 755 /etc",
            "chown -R me /var/www",
            "chmod -R 755 /etc/nginx",
            "chown -R me:me ~/.config",
            "chmod -R 755 $HOME",
            "chmod -x /etc/cron.d",
        ],
    },
    Case {
        id: "chmod-777-recursive",
        positives: &[
            ("chmod -R 777 ./x", Dangerous),
            ("chmod -R a+rwx dir", Dangerous),
            ("chmod --recursive 777 /tmp/x", Dangerous),
            ("chmod -R 0777 .", Dangerous),
        ],
        negatives: &[
            "chmod 777 x",
            "chmod -R 755 x",
            "chmod -R u+rwx x",
            "chmod -R 777",
            "echo chmod -R 777 x",
        ],
    },
    Case {
        id: "fork-bomb",
        positives: &[
            ("bomb() { bomb | bomb & }; bomb", Critical),
            ("f() { f & }", Critical),
            ("f() { f | f; }", Critical),
            ("f() { if true; then f | f; fi; }", Critical),
            ("f() ( f | f & )", Critical),
        ],
        negatives: &[
            "f() { echo hi; }",
            "f() { f; }",
            "f() { g | g & }",
            "f() { echo f | cat; }",
            "f() { true | cat & }",
        ],
    },
    Case {
        id: "firewall-open",
        positives: &[
            ("ufw allow 22", Dangerous),
            ("ufw disable", Dangerous),
            ("ufw reset", Dangerous),
            ("ufw default allow incoming", Dangerous),
            ("sudo ufw allow 80/tcp", Critical),
            ("iptables -A INPUT -p tcp --dport 22 -j ACCEPT", Dangerous),
            ("iptables -F", Dangerous),
            ("ip6tables -P INPUT ACCEPT", Dangerous),
            ("nft flush ruleset", Dangerous),
            ("firewall-cmd --add-port=80/tcp", Dangerous),
            ("firewall-cmd --permanent --add-service=http", Dangerous),
        ],
        negatives: &[
            "ufw status",
            "ufw deny 22",
            "ufw default deny incoming",
            "iptables -L -n",
            "iptables -A INPUT -j DROP",
            "nft list ruleset",
            "firewall-cmd --list-all",
            "echo ufw allow 22",
        ],
    },
    Case {
        id: "listen-socket",
        positives: &[
            ("nc -l 8080", Dangerous),
            ("nc -lvp 4444", Dangerous),
            ("ncat -l 9000", Dangerous),
            ("netcat --listen 9000", Dangerous),
            ("socat TCP-LISTEN:8080,fork -", Dangerous),
            ("socat - tcp4-listen:80", Dangerous),
        ],
        negatives: &[
            "nc example.com 80",
            "nc -zv example.com 80",
            "nc -w 5 host 80",
            "socat - TCP:example.com:80",
            "echo nc -l",
        ],
    },
    Case {
        id: "account-create",
        positives: &[
            ("useradd bob", Dangerous),
            ("useradd -m -s /bin/bash bob", Dangerous),
            ("adduser --disabled-password bob", Dangerous),
            ("sudo useradd bob", Critical),
        ],
        negatives: &[
            "useradd -m -p '$6$x' bob",
            "useradd --password x bob",
            "useradd -D",
            "adduser bob",
            "userdel bob",
            "echo useradd bob",
        ],
    },
    Case {
        id: "account-no-password",
        positives: &[
            ("passwd -d bob", Dangerous),
            ("passwd --delete bob", Dangerous),
            ("sudo passwd -d bob", Critical),
        ],
        negatives: &["passwd bob", "passwd -l bob", "passwd", "echo passwd -d"],
    },
    Case {
        id: "account-admin-group",
        positives: &[
            ("usermod -aG sudo bob", Dangerous),
            ("usermod -G wheel,docker bob", Dangerous),
            ("usermod --groups admin bob", Dangerous),
            ("gpasswd -a bob sudo", Dangerous),
            ("adduser bob sudo", Dangerous),
            ("sudo usermod -aG sudo bob", Critical),
        ],
        negatives: &[
            "usermod -aG docker bob",
            "usermod -aG docker admin",
            "gpasswd -a bob docker",
            "adduser bob",
            "adduser bob docker",
            "usermod -s /bin/sh bob",
        ],
    },
    Case {
        id: "download-to-shell",
        positives: &[
            ("curl -fsSL https://x.sh | sh", Dangerous),
            ("wget -qO- https://x.sh | bash", Dangerous),
            ("curl x | tee f | sh", Dangerous),
            ("curl x | sh -s -- -y", Dangerous),
            ("curl x | sudo bash", Critical),
        ],
        negatives: &[
            "curl https://x.sh > install.sh",
            "curl x | grep a",
            "echo hi | sh",
            "curl x | sh -c 'cat'",
            "sh | curl x",
        ],
    },
    Case {
        id: "download-eval",
        positives: &[
            ("eval \"$(curl -fsSL https://x.sh)\"", Dangerous),
            ("sh -c \"$(wget -qO- https://x.sh)\"", Dangerous),
            ("bash -c \"$(curl x)\"", Dangerous),
        ],
        negatives: &[
            "eval \"$(ssh-agent)\"",
            "sh -c 'echo hi'",
            "eval 'echo hi'",
            "x=$(curl y)",
            "echo \"$(curl x)\"",
        ],
    },
    Case {
        id: "git-force-push",
        positives: &[
            ("git push --force", Dangerous),
            ("git push -f origin main", Dangerous),
            ("git push origin +main", Dangerous),
            ("git push --force-with-lease", Caution),
            ("git -C repo push -f", Dangerous),
            ("git push -fu origin x", Dangerous),
            ("sudo git push -f", Critical),
        ],
        negatives: &[
            "git push",
            "git push origin main",
            "git push -u origin main",
            "git pull --force",
            "git log -f",
            "echo git push -f",
            "git commit -m 'push -f'",
        ],
    },
    Case {
        id: "git-reset-hard",
        positives: &[
            ("git reset --hard", Dangerous),
            ("git reset --hard HEAD~3", Dangerous),
            ("git -C x reset --hard origin/main", Dangerous),
        ],
        negatives: &[
            "git reset",
            "git reset --soft HEAD~1",
            "git reset HEAD file",
            "git status --hard",
            "echo git reset --hard",
        ],
    },
    Case {
        id: "git-clean",
        positives: &[
            ("git clean -fdx", Dangerous),
            ("git clean -fx", Dangerous),
            ("git clean -fX", Dangerous),
            ("git clean -f", Caution),
            ("git clean -fd", Caution),
            ("git clean --force", Caution),
        ],
        negatives: &[
            "git clean -n",
            "git clean -nfd",
            "git clean --dry-run -f",
            "git clean",
            "git clean -d",
            "git status",
            "echo git clean -f",
        ],
    },
    Case {
        id: "overwrite-file",
        positives: &[
            ("echo x > notes.txt", Caution),
            ("echo x > /etc/hosts", Caution),
            ("echo x >| notes.txt", Caution),
            ("cat a &> notes.txt", Caution),
            ("echo x > ./notes.txt", Caution),
            ("echo x > ~/work/notes.txt", Caution),
            ("cd /etc && echo x > hosts", Caution),
        ],
        negatives: &[
            "echo x >> notes.txt",
            "echo x > new.txt",
            "echo x > /dev/null",
            "echo x > \"$f\"",
            "echo x > *.txt",
            "cat < notes.txt",
            "echo x 2>&1",
            "cd /tmp && echo x > notes.txt",
            "echo x > ../notes.txt",
        ],
    },
];

#[test]
fn every_rule_has_a_positive_and_a_negative_case() {
    for rule in RULES {
        let case = CASES
            .iter()
            .find(|case| case.id == rule.id)
            .unwrap_or_else(|| panic!("rule {} has no cases", rule.id));
        assert!(
            !case.positives.is_empty(),
            "{} has no positive case",
            rule.id
        );
        assert!(
            !case.negatives.is_empty(),
            "{} has no negative case",
            rule.id
        );
    }
}

#[test]
fn no_case_names_a_rule_that_does_not_exist_and_ids_are_unique() {
    for case in CASES {
        assert!(
            RULES.iter().any(|rule| rule.id == case.id),
            "unknown rule {}",
            case.id
        );
        assert_eq!(
            CASES.iter().filter(|c| c.id == case.id).count(),
            1,
            "{}",
            case.id
        );
    }
    for rule in RULES {
        assert_eq!(
            RULES.iter().filter(|r| r.id == rule.id).count(),
            1,
            "{}",
            rule.id
        );
    }
    assert_eq!(CASES.len(), RULES.len());
}

#[test]
fn every_rule_has_a_reason_in_words() {
    for rule in RULES {
        assert!(rule.reason.ends_with('.'), "{}", rule.id);
        assert!(rule.reason.len() > 20, "{}", rule.id);
        assert!(rule.level > Level::Safe, "{}", rule.id);
    }
}

#[test]
fn positive_cases_are_flagged_with_the_expected_level() {
    for case in CASES {
        for &(line, expected) in case.positives {
            assert_eq!(
                rule_level(case.id, line),
                Some(expected),
                "{}: {line:?}",
                case.id
            );
        }
    }
}

#[test]
fn negative_cases_are_not_flagged_by_their_rule() {
    for case in CASES {
        for &line in case.negatives {
            assert_eq!(rule_level(case.id, line), None, "{}: {line:?}", case.id);
        }
    }
}

#[test]
fn the_classic_fork_bomb_is_found_when_it_parses() {
    // dash refuses `:` as a function name; bash accepts it. Either the line is refused before
    // anything runs, or the rule must catch it.
    let line = ":(){ :|:& };:";
    if let Ok(list) = parse(line) {
        let ctx = Context {
            cwd: Some(Path::new(CWD)),
            home: Some(Path::new(HOME)),
            probe: &FakeProbe,
        };
        assert_eq!(analyze(&list, line, &ctx).level(), Critical);
    }
}

// ---------------------------------------------------------------------------------------
// The walk.

#[test]
fn a_harmless_line_has_no_findings() {
    for line in [
        "ls -la",
        "echo hello",
        "cd /tmp",
        "git status",
        "sudo apt update",
        "sudo ls",
        "",
    ] {
        let analysis = run(line);
        assert!(
            analysis.findings.is_empty(),
            "{line:?}: {:?}",
            analysis.findings
        );
        assert_eq!(analysis.level(), Level::Safe);
    }
}

#[test]
fn sudo_raises_a_flagged_command_one_step_and_stops_at_critical() {
    assert_eq!(level_of("rm -rf build"), Dangerous);
    assert_eq!(level_of("sudo rm -rf build"), Critical);
    assert_eq!(level_of("sudo sudo rm -rf build"), Critical);
    assert_eq!(level_of("rm *.o"), Caution);
    assert_eq!(level_of("sudo rm *.o"), Dangerous);
    assert_eq!(level_of("sudo -u root rm -rf /"), Critical);
    assert_eq!(level_of("env A=1 rm -rf ~"), Critical);
    assert_eq!(level_of("doas rm -rf build"), Critical);
}

#[test]
fn sudo_leaves_a_safe_command_safe() {
    assert_eq!(level_of("sudo systemctl restart nginx"), Level::Safe);
}

#[test]
fn the_reason_says_when_sudo_raised_the_level() {
    let analysis = run("sudo rm -rf build");
    assert!(analysis.findings[0].reason.contains("sudo"));
    assert!(!run("rm -rf build").findings[0].reason.contains("sudo"));
}

#[test]
fn commands_are_found_wherever_they_sit() {
    for line in [
        "echo $(rm -rf /)",
        "echo `rm -rf /`",
        "echo \"$(rm -rf /)\"",
        "x=$(rm -rf /)",
        "echo ${y:-$(rm -rf /)}",
        "ls > $(rm -rf /)",
        "cat <<EOF\n$(rm -rf /)\nEOF",
        "if true; then rm -rf /; fi",
        "if rm -rf /; then :; fi",
        "for i in 1 $(rm -rf /); do :; done",
        "for i in 1; do rm -rf /; done",
        "while rm -rf /; do :; done",
        "until true; do rm -rf /; done",
        "case $(rm -rf /) in x) :;; esac",
        "case x in x) rm -rf /;; esac",
        "( rm -rf / )",
        "{ rm -rf /; }",
        "true && rm -rf /",
        "false || rm -rf /",
        "true; rm -rf /",
        "sleep 1 & rm -rf /",
        "echo hi | rm -rf /",
        "f() { rm -rf /; }",
        "! rm -rf /",
    ] {
        assert_eq!(level_of(line), Critical, "{line:?}");
    }
}

#[test]
fn a_quoted_here_document_is_not_expanded() {
    assert_eq!(level_of("cat <<'EOF'\n$(rm -rf /)\nEOF"), Level::Safe);
}

#[test]
fn text_that_only_mentions_a_command_is_safe() {
    for line in [
        "echo 'rm -rf /'",
        "echo \"$(echo rm -rf /)\"",
        "grep -r 'rm -rf /' .",
        "git commit -m 'rm -rf /'",
        "# rm -rf /",
    ] {
        assert_eq!(level_of(line), Level::Safe, "{line:?}");
    }
}

#[test]
fn text_held_in_eval_and_shell_strings_is_analyzed() {
    for line in [
        "eval 'rm -rf /'",
        "eval rm -rf /",
        "eval \"rm\" \"-rf\" /",
        "bash -c 'rm -rf /'",
        "sh -c \"rm -rf /\"",
        "sh -lc 'rm -rf /'",
        "sudo sh -c 'rm -rf /'",
        "eval 'eval \"rm -rf /\"'",
        "bash -c 'echo hi; rm -rf /'",
    ] {
        assert_eq!(level_of(line), Critical, "{line:?}");
    }
    for line in [
        "sh -c 'echo hi'",
        "bash script.sh",
        "eval \"$x\"",
        "eval",
        "bash -c \"$x\"",
    ] {
        assert_eq!(level_of(line), Level::Safe, "{line:?}");
    }
}

#[test]
fn a_finding_in_a_string_points_at_the_command_that_holds_it() {
    let line = "echo a; eval 'rm -rf /'";
    let analysis = run(line);
    let finding = &analysis.findings[0];
    assert_eq!(finding.span.text(line), "eval 'rm -rf /'");
}

#[test]
fn strings_nested_deeply_are_followed_to_a_limit_and_then_left() {
    // `eval eval rm -rf /` joins its words and runs `eval rm -rf /`, and so on.
    let near = format!("{}rm -rf /", "eval ".repeat(5));
    assert_eq!(level_of(&near), Critical);
    // Far past the limit the string is not followed. It must neither crash nor hang.
    let far = format!("{}rm -rf /", "eval ".repeat(60));
    assert!(run(&far).level() <= Critical);
}

#[test]
fn the_working_directory_follows_cd() {
    assert_eq!(level_of("cd / && rm -rf *"), Critical);
    assert_eq!(level_of("cd /tmp && rm -rf *"), Dangerous);
    assert_eq!(level_of("cd .. && cd .. && rm -rf *"), Critical);
    assert_eq!(level_of("cd /etc; rm -rf *"), Critical);
}

#[test]
fn a_cd_that_may_not_have_happened_or_is_not_known_is_not_trusted() {
    // The directory is unknown afterwards, and `*` in an unknown directory is not judged.
    assert_eq!(level_of("if true; then cd /; fi; rm -rf *"), Dangerous);
    assert_eq!(level_of("cd $d && rm -rf *"), Dangerous);
    assert_eq!(level_of("cd - && rm -rf *"), Dangerous);
    assert_eq!(level_of("pushd /; rm -rf *"), Dangerous);
}

#[test]
fn a_cd_in_a_subshell_or_a_pipeline_stage_does_not_reach_the_line() {
    assert_eq!(level_of("(cd /; true); rm -rf *"), Dangerous);
    assert_eq!(level_of("cd / | cat; rm -rf *"), Dangerous);
    assert_eq!(level_of("echo $(cd /); rm -rf *"), Dangerous);
    assert_eq!(level_of("(cd /; rm -rf *)"), Critical);
}

#[test]
fn assigning_home_makes_the_tilde_unknown_but_still_the_home_folder() {
    assert_eq!(level_of("HOME=/tmp/x; rm -rf ~"), Critical);
    assert_eq!(level_of("HOME=/tmp/x; rm -rf $HOME/*"), Critical);
}

#[test]
fn an_empty_variable_in_a_path_is_named_in_the_reason() {
    let analysis = run("rm -rf \"$d\"/");
    let finding = &analysis.findings[0];
    assert_eq!(finding.rule.id, "rm-critical-path");
    assert_eq!(finding.target.as_deref(), Some("\"$d\"/"));
    assert!(
        finding.reason.contains("may be empty"),
        "{}",
        finding.reason
    );
    // A path with no variable does not say so.
    assert!(!run("rm -rf /").findings[0].reason.contains("empty"));
}

#[test]
fn the_target_is_the_word_as_written() {
    let analysis = run("rm -rf ~/");
    assert_eq!(analysis.findings[0].target.as_deref(), Some("~/"));
    let analysis = run("rm -rf $HOME");
    assert_eq!(analysis.findings[0].target.as_deref(), Some("$HOME"));
}

#[test]
fn a_command_that_only_asks_for_help_is_not_judged() {
    for line in ["rm --help", "mkfs --version", "sudo shred --help"] {
        assert!(run(line).findings.is_empty(), "{line:?}");
    }
}

#[test]
fn xargs_gives_its_program_targets_nobody_wrote() {
    assert_eq!(level_of("find . -name '*.o' | xargs rm -f"), Dangerous);
    assert_eq!(level_of("echo x | xargs echo"), Level::Safe);
}

#[test]
fn a_finding_is_reported_once() {
    let analysis = run("rm -rf /");
    assert_eq!(analysis.findings.len(), 1, "{:?}", analysis.findings);
}

#[test]
fn findings_come_in_the_order_of_the_commands() {
    let analysis = run("git clean -f; rm -rf /; git reset --hard");
    let ids: Vec<_> = analysis.findings.iter().map(|f| f.rule.id).collect();
    assert_eq!(ids, ["git-clean", "rm-critical-path", "git-reset-hard"]);
    assert_eq!(analysis.level(), Critical);
    assert_eq!(analysis.at_least(Dangerous).count(), 2);
}

#[test]
fn the_working_directory_may_be_missing() {
    let list = parse("rm -rf * ../..").unwrap();
    let ctx = Context {
        cwd: None,
        home: None,
        probe: &FakeProbe,
    };
    // Relative paths are not judged; the rule for forced deletes still speaks.
    assert_eq!(analyze(&list, "rm -rf * ../..", &ctx).level(), Dangerous);
}

fn word() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "rm",
        "-rf",
        "-f",
        "/",
        "~",
        "$x",
        "$(rm -rf /)",
        "`ls`",
        "sudo",
        "|",
        "&&",
        ";",
        "&",
        "cd",
        "..",
        "*",
        "'a b'",
        "\"$d\"/",
        ">",
        ">>",
        "/dev/sda",
        "eval",
        "sh",
        "-c",
        "'rm -rf /'",
        "git",
        "push",
        "-f",
        "(",
        ")",
        "{",
        "}",
        "if",
        "then",
        "fi",
        "f()",
        "xargs",
        "dd",
        "of=/dev/sda",
        "curl",
        "wget",
        "chmod",
        "-R",
        "777",
        "ufw",
        "allow",
        "\n",
        "<<EOF",
        "EOF",
        "usermod",
        "-aG",
        "sudo",
        "nc",
        "-l",
        "env",
        "A=1",
        "$HOME",
        "${x:-y}",
        "!",
    ])
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    /// Whatever the parser accepts, the analyzer finishes, its findings are real, and their
    /// spans lie inside the text.
    #[test]
    fn the_analyzer_never_panics_and_its_spans_are_in_bounds(words in prop::collection::vec(word(), 0..12)) {
        let line = words.join(" ");
        let parsed = parse_tolerant(&line);
        let ctx = Context {
            cwd: Some(Path::new(CWD)),
            home: Some(Path::new(HOME)),
            probe: &FakeProbe,
        };
        let analysis = analyze(&parsed.list, &line, &ctx);
        for finding in &analysis.findings {
            prop_assert!(finding.level > Level::Safe);
            prop_assert!(finding.span.start <= finding.span.end);
            prop_assert!(finding.span.end <= line.len());
            prop_assert!(finding.level <= Critical);
        }
    }
}
