//! Tests of the `rm` sites and the file changes the analyzer collects (S18).

use std::path::Path;

use omsh_syntax::parse;

use crate::changes::{ChangeKind, NotTrashable};
use crate::{Analysis, Context, FsProbe, analyze};

fn run(line: &str) -> Analysis {
    let list = parse(line).unwrap();
    let ctx = Context {
        cwd: Some(Path::new("/w")),
        home: Some(Path::new("/home/me")),
        probe: &FsProbe,
    };
    analyze(&list, line, &ctx)
}

fn word(line: &str, site: &crate::changes::RmSite) -> String {
    line[site.name.start..site.name.end].to_owned()
}

#[test]
fn every_rm_is_a_site_with_the_span_of_its_name() {
    let line = "echo hi; rm -rf a && /bin/rm b | cat";
    let a = run(line);
    let names: Vec<_> = a.deletes.iter().map(|s| word(line, s)).collect();
    assert_eq!(names, ["rm", "/bin/rm"]);
    assert!(a.deletes.iter().all(|s| s.trashable.is_ok()));
}

#[test]
fn rm_in_a_substitution_a_loop_and_a_group_is_found() {
    let line = "for f in a b; do rm $f; done; echo $(rm x); { rm y; }; (rm z)";
    assert_eq!(run(line).deletes.len(), 4);
}

#[test]
fn a_wrapped_rm_is_not_trashable() {
    let a = run("sudo rm -rf a; xargs rm; env X=1 rm b");
    assert_eq!(a.deletes.len(), 3);
    assert!(
        a.deletes
            .iter()
            .all(|s| s.trashable == Err(NotTrashable::Wrapped))
    );
}

#[test]
fn rm_in_a_string_or_a_function_definition_is_not_trashable() {
    let a = run("eval 'rm a'; sh -c 'rm b'");
    assert_eq!(a.deletes.len(), 2);
    assert!(
        a.deletes
            .iter()
            .all(|s| s.trashable == Err(NotTrashable::InText))
    );
    let a = run("clean() { rm -rf build; }");
    assert_eq!(a.deletes.len(), 1);
    assert_eq!(a.deletes[0].trashable, Err(NotTrashable::InFunction));
}

#[test]
fn a_help_request_is_not_a_site() {
    assert!(run("rm --help").deletes.is_empty());
}

#[test]
fn mkdir_mv_and_redirects_are_changes_with_the_directory_they_run_in() {
    let a =
        run("mkdir -p out/x; cd /tmp; mv a.txt out/; echo hi > b.txt; echo 1 >> c; ls > /dev/null");
    let kinds: Vec<_> = a.changes.iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        [
            ChangeKind::Mkdir { parents: true },
            ChangeKind::Move {
                no_target_dir: false
            },
            ChangeKind::Write,
            ChangeKind::Write,
        ]
    );
    assert_eq!(a.changes[0].cwd.as_deref(), Some("/w"));
    assert_eq!(a.changes[1].cwd.as_deref(), Some("/tmp"), "after the cd");
    assert_eq!(a.changes[2].targets[0].text, "b.txt");
}

#[test]
fn reading_and_dup_redirects_are_not_changes() {
    assert!(run("cat < a; echo x 2>&1; cat <<< hi").changes.is_empty());
}

#[test]
fn the_body_of_a_function_being_defined_is_not_a_change() {
    assert!(run("f() { mkdir x; echo y > z; }").changes.is_empty());
    assert_eq!(run("f() { :; }; mkdir x").changes.len(), 1);
}
