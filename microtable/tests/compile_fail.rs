//! Compile isolated consumer crates. No UI-test crate or network access is needed.
use std::fs;
use std::path::PathBuf;
use std::process::Command;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn rejected_apis_fail_in_an_external_offline_consumer() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(
        std::env::temp_dir().join(format!("microtable-api-{}-{unique}", std::process::id())),
    );
    fs::create_dir_all(fixture.0.join("src")).unwrap();
    let library = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::write(fixture.0.join("Cargo.toml"), format!(
        "[package]\nname = \"microtable-api-check\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n[dependencies]\nmicrotable = {{ path = {:?} }}\n", library
    )).unwrap();
    let cases = [
        (
            "valid control",
            true,
            r#"
            #[derive(Debug)] struct Row { key: String }
            microtable::table! {
                pub struct Rows([Row]);
                #[id] pub struct RowId(u8);
                #[index(unique)] fn key(row: &Row) -> &str { &row.key }
            }
            fn main() {
                let mut rows = Rows::new();
                let id = rows.insert(Row { key: "a".into() }).unwrap();
                assert_eq!(id.index(), 0);
                assert!(rows.get_by_key("a").is_some());
            }
        "#,
        ),
        (
            "cached keys cannot borrow owned row fields",
            false,
            r#"
            struct Row { key: String }
            microtable::table! {
                struct Rows([Row]);
                #[index(unique, cached)] fn key(row: &Row) -> &str { &row.key }
            }
            fn main() { let _ = Rows::new(); }
        "#,
        ),
        (
            "cached nested keys cannot borrow owned row fields",
            false,
            r#"
            struct Row { key: String }
            microtable::table! {
                struct Rows([Row]);
                #[index(many, cached)] fn key(row: &Row) -> ((u8, &str), u8) { ((1, &row.key), 2) }
            }
            fn main() { let _ = Rows::new(); }
        "#,
        ),
        (
            "extractor must take a whole row reference",
            false,
            r#"
            struct Row { key: u32 }
            microtable::table! {
                struct Rows([Row]);
                #[index(unique)] fn key(row: u32) -> u32 { row }
            }
            fn main() { let _ = Rows::new(); }
        "#,
        ),
        (
            "extractor cannot take a mutable row reference",
            false,
            r#"
            struct Row { key: u32 }
            microtable::table! {
                struct Rows([Row]);
                #[index(unique)] fn key(row: &mut Row) -> u32 { row.key }
            }
            fn main() { let _ = Rows::new(); }
        "#,
        ),
        (
            "append-only tables have no remove method",
            false,
            r#"
            struct Row;
            microtable::table! { #[table(append_only)] struct Rows([Row]); }
            fn main() { let _ = Rows::new().remove(0); }
        "#,
        ),
        (
            "append-only tables have no replace method",
            false,
            r#"
            struct Row;
            microtable::table! { #[table(append_only)] struct Rows([Row]); }
            fn main() { let _ = Rows::new().replace(0, Row); }
        "#,
        ),
        (
            "ID backing value is private",
            false,
            r#"
            mod schema {
                pub struct Row;
                microtable::table! { pub struct Rows([Row]); #[id] pub struct RowId(u8); }
            }
            fn main() { let _ = schema::RowId::new(0).0; }
        "#,
        ),
    ];
    for (name, succeeds, source) in cases {
        fs::write(fixture.0.join("src/main.rs"), source).unwrap();
        let output = Command::new(env!("CARGO"))
            .args(["check", "--offline", "--quiet", "--manifest-path"])
            .arg(fixture.0.join("Cargo.toml"))
            // Never share the parent cargo process's target lock.
            .arg("--target-dir")
            .arg(fixture.0.join("target"))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.success(), succeeds, "case: {name}\n{stderr}");
        if !succeeds {
            assert!(
                stderr.contains("error"),
                "case: {name} did not produce a compiler diagnostic\n{stderr}"
            );
        }
    }
}
