use std::path::Path;
use swisha::env::{Env, EnvFile, parse};

fn map(contents: &str) -> std::collections::HashMap<String, String> {
    parse(contents, Path::new("test.env")).expect("parse")
}

fn err(contents: &str) -> String {
    parse(contents, Path::new("test.env")).unwrap_err().to_string()
}

#[test]
fn reads_plain_assignments() {
    let m = map("DB_NAME=swisha\nDB_USER=taeha\n");
    assert_eq!(m["DB_NAME"], "swisha");
    assert_eq!(m["DB_USER"], "taeha");
}

#[test]
fn skips_blanks_and_comments() {
    let m = map("\n# a comment\n\n   # indented\nA=1\n");
    assert_eq!(m.len(), 1);
    assert_eq!(m["A"], "1");
}

#[test]
fn accepts_the_export_prefix() {
    assert_eq!(map("export A=1\n")["A"], "1");
}

#[test]
fn tolerates_whitespace_and_crlf() {
    let m = map("  A = 1  \r\nB=2\r\n");
    assert_eq!(m["A"], "1");
    assert_eq!(m["B"], "2");
}

#[test]
fn keeps_everything_after_the_first_equals() {
    // Connection strings and base64 both contain '='.
    assert_eq!(map("URL=postgres://u:p@h/db?x=1\n")["URL"], "postgres://u:p@h/db?x=1");
    assert_eq!(map("B64=aGVsbG8=\n")["B64"], "aGVsbG8=");
}

#[test]
fn strips_matching_quotes_without_interpreting_escapes() {
    assert_eq!(map(r#"A="value with spaces""#)["A"], "value with spaces");
    assert_eq!(map("B='single'")["B"], "single");
    // A bcrypt hash is full of '$' and must survive verbatim.
    assert_eq!(map("H='$2b$10$abc'")["H"], "$2b$10$abc");
    assert_eq!(map(r#"C="a\nb""#)["C"], r"a\nb");
}

#[test]
fn an_unquoted_value_is_cut_at_a_trailing_comment() {
    assert_eq!(map("A=1 # why\n")["A"], "1");
    // A '#' with no leading space belongs to the value.
    assert_eq!(map("P=pa#ss\n")["P"], "pa#ss");
    // Quoting protects a value that really contains ' #'.
    assert_eq!(map("Q='pa #ss'\n")["Q"], "pa #ss");
}

#[test]
fn empty_values_are_allowed() {
    let m = map("A=\nB=\n");
    assert_eq!(m["A"], "");
    assert_eq!(m["B"], "");
}

#[test]
fn rejects_a_line_that_is_not_an_assignment() {
    assert!(err("NOPE\n").contains("expected KEY=value"));
    assert!(err("NOPE\n").contains("test.env:1"));
}

#[test]
fn rejects_invalid_variable_names() {
    for bad in ["1A=x", "A-B=x", "A B=x", "=x", "A.B=x"] {
        assert!(err(bad).contains("not a valid variable name"), "{bad}");
    }
}

// A duplicate is how a service ends up running with configuration nobody intended.
#[test]
fn rejects_duplicate_keys() {
    let message = err("A=1\nB=2\nA=3\n");
    assert!(message.contains("set more than once"), "{message}");
    assert!(message.contains("test.env:3"), "{message}");
}

#[test]
fn reports_the_line_number_of_the_offending_entry() {
    assert!(err("A=1\n\n# fine\nBROKEN\n").contains("test.env:4"));
}

#[test]
fn a_named_file_that_cannot_be_read_is_an_error() {
    let result = EnvFile::load(Path::new("/nonexistent/swisha.env"));
    assert!(result.unwrap_err().to_string().contains("Cannot read env file"));
}

// The process environment must win, so systemd or docker can override without editing.
// Uses PATH, which the test runner always sets, rather than mutating the environment: this
// crate forbids unsafe code and set_var is unsafe in edition 2024.
#[test]
fn process_environment_takes_precedence_over_the_file() {
    let file = EnvFile::load(Path::new("tests/fixtures/sample.env")).expect("load");
    let real_path = std::env::var("PATH").expect("PATH is set for the test runner");

    // The fixture also defines PATH, and it must lose.
    assert_eq!(file.get("PATH"), Some("shadowed-by-the-process"));
    let env = Env::with_file(file);
    assert_eq!(env.get("PATH").as_deref(), Some(real_path.as_str()));

    // A key only the file defines still resolves.
    assert_eq!(env.get("FROM_FILE_ONLY").as_deref(), Some("file"));
}

#[test]
fn required_names_the_file_when_one_is_loaded() {
    let env = Env::with_file(EnvFile::load(Path::new("tests/fixtures/sample.env")).unwrap());
    let message = env.required("DEFINITELY_ABSENT").unwrap_err().to_string();
    assert!(message.contains("DEFINITELY_ABSENT"), "{message}");
    assert!(message.contains("sample.env"), "{message}");
}

// The shipped templates carry every field blank, so blank has to mean "not configured" on both
// paths. If `optional` returned the blank instead, a template would bind an empty address, name
// an empty table, and set a SWISH_ENV that is not "production" and so skips the IP allowlist.
#[test]
fn a_blank_value_is_never_treated_as_configured() {
    let env = Env::with_file(EnvFile::load(Path::new("tests/fixtures/sample.env")).unwrap());
    assert!(env.required("BLANK").is_err());
    assert_eq!(env.optional("BLANK", "fallback"), "fallback");
    assert_eq!(env.optional("DEFINITELY_ABSENT", "fallback"), "fallback");
}

// The shipped templates document each field with a trailing comment on an otherwise blank line.
// If the comment were not cut before the value is trimmed, every one of those fields would parse
// as the comment text: an empty address to bind, an empty table name, an unquoted SWISH_ENV.
#[test]
fn a_blank_field_with_a_trailing_comment_parses_as_blank() {
    let values = map(
        "DB_HOST=              # host:port of your database. Default localhost:5432\n\
         SWISH_ENV=            # test or production. Default test\n",
    );
    assert_eq!(values["DB_HOST"], "");
    assert_eq!(values["SWISH_ENV"], "");
}

#[test]
fn a_trailing_comment_never_eats_a_real_value() {
    let values = map(
        "DB_PASS=hunter2 # the password\n\
         HASHED=pa#ss\n\
         QUOTED=\"keeps # its hash\"\n",
    );
    assert_eq!(values["DB_PASS"], "hunter2");
    assert_eq!(values["HASHED"], "pa#ss", "no space before # means it is part of the value");
    assert_eq!(values["QUOTED"], "keeps # its hash", "quoting keeps a comment character");
}

// Parses the templates that actually ship. They document every field with a trailing comment on
// a blank line, so this is where a parser regression would show up as a service that binds an
// empty address or names an empty table.
#[test]
fn the_shipped_templates_parse_with_every_field_blank() {
    for name in ["examples/swisha.dev.example", "examples/swisha.prod.example"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
        let file = EnvFile::load(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(file.len(), 26, "{name} should document all 26 settings");

        let env = Env::with_file(file);
        for key in [
            "DB_HOST", "DB_NAME", "DB_USER", "DB_PASS",
            "TABLE_PAYOUTS", "TABLE_LOGS", "TABLE_EVENTS",
            "SWISH_SERVER_SOCKET", "SWISH_SERVER_ADDR", "SWISH_CALLBACK_ADDR",
            "TRUSTED_PROXY", "NOTIFY_PREFIX", "SWISH_NUMBER",
            "SWISH_MAX_PAYOUT", "SWISH_CALLBACK_URL", "SWISH_CERT", "SWISH_KEY",
            "SWISH_SIGNING_CERT", "SWISH_SIGNING_KEY", "SWISH_CA", "API_SHARED_SECRET",
            "SWISH_PAYOUT_MESSAGE", "SWISH_ERROR_LANG", "SWISH_REQUIRE_SSN", "RUST_LOG",
        ] {
            assert_eq!(env.get(key).as_deref(), Some(""), "{name}: {key} should be blank");
            assert_eq!(env.optional(key, "fallback"), "fallback", "{name}: {key} should fall through");
        }
    }
}

// SWISH_ENV is the one field the templates fill in, because blank means "test" and a prod.env
// that quietly runs against the simulator would also run with the callback IP allowlist off.
#[test]
fn the_templates_pin_swish_env_to_their_own_environment() {
    for (name, expected, base) in [
        ("examples/swisha.dev.example", "test", "https://mss.cpc.getswish.net"),
        ("examples/swisha.prod.example", "production", "https://cpc.getswish.net"),
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
        let env = Env::with_file(EnvFile::load(&path).unwrap_or_else(|e| panic!("{name}: {e}")));

        let value = env.get("SWISH_ENV");
        assert_eq!(value.as_deref(), Some(expected), "{name}: SWISH_ENV is pre-filled");
        assert_eq!(
            env.optional("SWISH_ENV", "test"),
            expected,
            "{name}: the filled value is used, not the default"
        );
        assert_eq!(
            swisha::swish::client::swish_base_url(expected),
            base,
            "{name}: sends payouts to the right host"
        );
    }
}

// A file of nothing but comments is still a loaded file, it just carries no settings. Both
// halves of the len/is_empty pair are public, so both are pinned.
#[test]
fn a_file_with_no_settings_reports_itself_as_empty() {
    let path = std::env::temp_dir().join(format!(
        "swisha-empty-{}.env",
        swisha::swish::random_payout_uuid()
    ));
    std::fs::write(&path, "# only a comment\n\n").expect("write");
    let empty = EnvFile::load(&path).expect("load");
    let _ = std::fs::remove_file(&path);

    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);

    let populated = EnvFile::load(Path::new("tests/fixtures/sample.env")).expect("load");
    assert!(!populated.is_empty());
    assert_eq!(populated.len(), 4, "the fixture carries four settings");
}

// SWISHA_ENV_FILE set to blank means no file was named, not a file called "". Driven through a
// real process because setting an environment variable in-process is exactly the data race
// env.rs exists to avoid, so the only honest way to test it is from the outside.
#[cfg(feature = "http")]
#[test]
fn a_blank_env_file_path_is_not_treated_as_a_filename() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_swisha"))
        .env("SWISHA_ENV_FILE", "")
        .env_remove("DB_NAME")
        .output()
        .expect("run swisha");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stderr}{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        !combined.contains("Cannot read") && !combined.contains("No such file"),
        "a blank path should not be opened as a file: {combined}"
    );
    assert!(
        combined.contains("Missing required setting") || combined.contains("Missing required env var"),
        "it should fall through to the process environment and report what is missing: {combined}"
    );
}
