//! The loci of compilation output: the file positions in it, like `src/lib.rs:3:9`, which become
//! diagnostics of the buffer that open their files.

use std::{
    collections::HashMap,
    ffi::OsString,
    ops::Range,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use helix_core::{diagnostic::Severity, Position, RopeSlice};
use helix_view::editor::FilePickerConfig;
use serde_json::json;

/// A file position in a line of output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The byte range of the path and its position in the line.
    pub range: Range<usize>,
    /// The path, as written.
    pub path: String,
    /// The line and column, counting from 0.
    pub position: Position,
    /// The directories of the package a stack frame names, like `demo` in
    /// `at demo.App.main(App.java:5)`.
    pub package: Option<PathBuf>,
}

/// The file positions in `line`, in order. Their paths need not exist.
pub fn find(line: &str) -> Vec<Found> {
    // Every position has digits; most lines of output have none.
    if !line.bytes().any(|byte| byte.is_ascii_digit()) {
        return Vec::new();
    }
    let mut found: Vec<Found> = helix_stdx::path::find_path_positions(RopeSlice::from(line))
        .map(|found| Found {
            path: line[found.path.clone()].to_owned(),
            position: Position::new(
                found.line.saturating_sub(1),
                found.column.unwrap_or(1).saturating_sub(1),
            ),
            package: package(&line[..found.range.start]),
            range: found.range,
        })
        .collect();
    if let Some(frame) = python_frame(line) {
        found.retain(|found| found.range.end <= frame.range.start);
        found.push(frame);
    }
    found
}

/// The frame of a Python traceback: `File "src/app.py", line 4, in main`.
fn python_frame(line: &str) -> Option<Found> {
    const FILE: &str = "File \"";
    const LINE: &str = "\", line ";
    let start = line.find(FILE)? + FILE.len();
    let end = start + line[start..].find('"')?;
    let number = line[end..].strip_prefix(LINE)?;
    let digits = number.bytes().take_while(u8::is_ascii_digit).count();
    let row: usize = number[..digits].parse().ok()?;
    Some(Found {
        range: start - 1..end + LINE.len() + digits,
        path: line[start..end].to_owned(),
        position: Position::new(row.saturating_sub(1), 0),
        package: None,
    })
}

/// The package directories of the stack frame that `before` ends with, like `demo` for
/// `at demo.App.main(`: the frame's name without its class and method.
fn package(before: &str) -> Option<PathBuf> {
    let frame = before.strip_suffix('(')?;
    let name = frame
        .rsplit(|c: char| !(c.is_alphanumeric() || matches!(c, '.' | '_' | '$')))
        .next()?;
    let parts: Vec<&str> = name.split('.').collect();
    (parts.len() > 2).then(|| parts[..parts.len() - 2].iter().collect())
}

/// The severity a word of a message names.
fn keyword(word: &str) -> Option<Severity> {
    match word.to_ascii_lowercase().as_str() {
        "error" | "fatal" | "panicked" => Some(Severity::Error),
        "warning" | "warn" => Some(Severity::Warning),
        "note" | "info" => Some(Severity::Info),
        "help" | "hint" => Some(Severity::Hint),
        _ => None,
    }
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
}

/// The severity of a header line like `error[E0425]: …` or `warning: …`, which the locus on the
/// next line belongs to, as with rustc and Typst.
fn header(line: &str) -> Option<Severity> {
    let word = line.split(|c: char| !c.is_alphabetic()).next()?;
    let after = line[word.len()..].chars().next()?;
    matches!(after, ':' | '[').then(|| keyword(word)).flatten()
}

/// How severe the message is that the locus at `range` in `line` belongs to: as the word after
/// it says (`main.c:3:7: warning: …`), or a word before it (`[error] …`, Kotlin's `w: …`,
/// `… panicked at …`), or the header line `above` it; else it is a note.
pub fn severity(line: &str, range: &Range<usize>, above: &str) -> Severity {
    let kotlin = || match line.get(..3)? {
        "e: " => Some(Severity::Error),
        "w: " => Some(Severity::Warning),
        "i: " | "v: " => Some(Severity::Info),
        _ => None,
    };
    words(&line[range.end..])
        .next()
        .and_then(keyword)
        .or_else(kotlin)
        .or_else(|| words(&line[..range.start]).find_map(keyword))
        .or_else(|| header(above))
        .unwrap_or(Severity::Info)
}

/// The message of the locus at `range` in `line`, as the `space d` picker lists it: the line, or
/// the header `above` it when the line holds the locus alone, like ` --> src/lib.rs:3:9`.
pub fn message(line: &str, range: &Range<usize>, above: &str) -> String {
    let after = &line[range.end..];
    let alone = after
        .trim_matches(|c: char| !c.is_alphanumeric())
        .is_empty();
    if alone && header(above).is_some() {
        above.trim().to_owned()
    } else {
        line.trim().to_owned()
    }
}

/// What a locus diagnostic carries: the file it names and where in it.
pub fn target_data(path: &Path, position: Position) -> serde_json::Value {
    json!({ "path": path, "line": position.row, "column": position.col })
}

/// The file and position a locus diagnostic names.
pub fn target(data: &serde_json::Value) -> Option<(PathBuf, Position)> {
    let path = data.get("path")?.as_str()?;
    let row = data.get("line")?.as_u64()?;
    let col = data.get("column")?.as_u64()?;
    Some((
        PathBuf::from(path),
        Position::new(row.try_into().ok()?, col.try_into().ok()?),
    ))
}

/// Finds the files that the loci of a run name, from the directory it ran in.
#[derive(Debug)]
pub struct Resolver {
    dir: PathBuf,
    /// The directories relative paths are tried in: the run's, then its parents up to the root
    /// of its workspace.
    dirs: Vec<PathBuf>,
    picker: FilePickerConfig,
    resolved: HashMap<(String, Option<PathBuf>), Option<PathBuf>>,
    /// The files under the run's directory by name, listed at the first bare name not found.
    names: Option<HashMap<OsString, Vec<PathBuf>>>,
}

impl Resolver {
    /// The index of bare names stops after this many files, or after [`Self::INDEX_TIME`].
    const INDEX_FILES: usize = 50_000;
    const INDEX_TIME: Duration = Duration::from_millis(500);

    pub fn new(dir: PathBuf, picker: FilePickerConfig) -> Self {
        let workspace = helix_loader::find_workspace_in(&dir).0;
        let dirs = dir
            .ancestors()
            .take_while(|ancestor| ancestor.starts_with(&workspace))
            .map(Path::to_path_buf)
            .collect();
        Self {
            dir,
            dirs,
            picker,
            resolved: HashMap::new(),
            names: None,
        }
    }

    /// The existing file that `found` names, if any.
    pub fn resolve(&mut self, found: &Found) -> Option<PathBuf> {
        let key = (found.path.clone(), found.package.clone());
        if let Some(resolved) = self.resolved.get(&key) {
            return resolved.clone();
        }
        let resolved = self.look_up(&found.path, found.package.as_deref());
        self.resolved.insert(key, resolved.clone());
        resolved
    }

    fn look_up(&mut self, path: &str, package: Option<&Path>) -> Option<PathBuf> {
        let path = helix_stdx::path::expand_tilde(Path::new(path));
        if path.is_absolute() {
            return path.is_file().then(|| path.into_owned());
        }
        let relative = self
            .dirs
            .iter()
            .map(|dir| dir.join(&path))
            .find(|path| path.is_file());
        if let Some(found) = relative {
            return Some(helix_stdx::path::normalize(found));
        }
        // A bare file name, as stack traces and test runners print them.
        let mut components = path.components();
        let name = match (components.next(), components.next()) {
            (Some(name), None) => name.as_os_str(),
            _ => return None,
        };
        let candidates = self.names().get(name)?;
        let in_package = |candidate: &&PathBuf| {
            package
                .is_some_and(|package| candidate.parent().is_some_and(|dir| dir.ends_with(package)))
        };
        candidates
            .iter()
            .find(in_package)
            .or(candidates.first())
            .cloned()
    }

    fn names(&mut self) -> &HashMap<OsString, Vec<PathBuf>> {
        self.names.get_or_insert_with(|| {
            let start = Instant::now();
            let mut names: HashMap<OsString, Vec<PathBuf>> = HashMap::new();
            let files = crate::ui::workspace_files(&self.dir, &self.picker);
            for (count, file) in files.enumerate() {
                if count == Self::INDEX_FILES || start.elapsed() > Self::INDEX_TIME {
                    log::info!(
                        "compilation: stopped listing {} after {count} files",
                        self.dir.display()
                    );
                    break;
                }
                if let Some(name) = file.file_name() {
                    names.entry(name.to_owned()).or_default().push(file);
                }
            }
            log::debug!(
                "compilation: listed {} in {:?}",
                self.dir.display(),
                start.elapsed()
            );
            names
        })
    }
}

/// A locus found in output that the buffer doesn't hold yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locus {
    /// The chars from the start of the output it was found in to the locus.
    pub start: usize,
    /// Its length in chars.
    pub len: usize,
    pub severity: Severity,
    pub message: String,
    /// The file it names, which exists, and where in it.
    pub path: PathBuf,
    pub position: Position,
}

/// Finds the loci of output line by line, as it arrives.
#[derive(Debug)]
pub struct Finder {
    resolver: Resolver,
    /// The line before the next one, which may be its header.
    above: String,
}

impl Finder {
    pub fn new(resolver: Resolver) -> Self {
        Self {
            resolver,
            above: String::new(),
        }
    }

    /// The loci in `text`, lines that each end in a line break.
    pub fn find(&mut self, text: &str) -> Vec<Locus> {
        let mut loci = Vec::new();
        let mut start = 0;
        for line in text.split_inclusive('\n') {
            let content = line.strip_suffix('\n').unwrap_or(line);
            for found in find(content) {
                let Some(path) = self.resolver.resolve(&found) else {
                    continue;
                };
                loci.push(Locus {
                    start: start + content[..found.range.start].chars().count(),
                    len: content[found.range.clone()].chars().count(),
                    severity: severity(content, &found.range, &self.above),
                    message: message(content, &found.range, &self.above),
                    path,
                    position: found.position,
                });
            }
            start += line.chars().count();
            self.above.clear();
            self.above.push_str(content);
        }
        loci
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A project holding the files the outputs below name.
    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in [
            "src/lib.rs",
            "src/Main.hs",
            "main.cpp",
            "src/main/java/demo/App.java",
            "src/main/java/other/App.java",
            "src/test/java/demo/AppTest.java",
            "main.typ",
            "t.py",
            "a.ts",
            "Main.kt",
            "App.scala",
        ] {
            let path = dir.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        dir
    }

    /// The loci in `output` as `(text, severity, message, file, line, column)`, from 1.
    fn loci(dir: &Path, output: &str) -> Vec<(String, Severity, String, String, usize, usize)> {
        let mut finder = Finder::new(Resolver::new(
            dir.to_path_buf(),
            FilePickerConfig::default(),
        ));
        let chars: Vec<char> = output.chars().collect();
        finder
            .find(output)
            .into_iter()
            .map(|locus| {
                let text = chars[locus.start..locus.start + locus.len].iter().collect();
                let file = locus.path.strip_prefix(dir).unwrap();
                (
                    text,
                    locus.severity,
                    locus.message,
                    file.to_string_lossy().into_owned(),
                    locus.position.row + 1,
                    locus.position.col + 1,
                )
            })
            .collect()
    }

    fn locus(
        text: &str,
        severity: Severity,
        message: &str,
        file: &str,
        line: usize,
        column: usize,
    ) -> (String, Severity, String, String, usize, usize) {
        let text = text.to_owned();
        (
            text,
            severity,
            message.to_owned(),
            file.to_owned(),
            line,
            column,
        )
    }

    #[test]
    fn loci_of_cargo() {
        let dir = project();
        let build = "   Compiling demo v0.1.0 (/tmp/demo)
error[E0425]: cannot find value `c` in this scope
 --> src/lib.rs:3:9
  |
3 |     a + c
  |         ^ not found in this scope
  |
help: a local variable with a similar name exists
  |
3 -     a + c
3 +     a + a
  |

For more information about this error, try `rustc --explain E0425`.
error: could not compile `demo` (lib) due to 1 previous error
";
        let error = "error[E0425]: cannot find value `c` in this scope";
        assert_eq!(
            loci(dir.path(), build),
            [locus(
                "src/lib.rs:3:9",
                Severity::Error,
                error,
                "src/lib.rs",
                3,
                9
            )]
        );

        let test = "warning: unused variable: `unused`
 --> src/lib.rs:2:9
  |
2 |     let unused = 3;
  |         ^^^^^^ help: if this is intentional, prefix it with an underscore: `_unused`
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.11s
     Running unittests src/lib.rs (target/debug/deps/demo-a6641c61335302e1)

---- tests::it_fails stdout ----

thread 'tests::it_fails' (1258450) panicked at src/lib.rs:10:9:
assertion `left == right` failed
  left: 2
 right: 3
thread 'main' panicked at /rustc/1159e78c4/library/core/src/panicking.rs:75:14:
";
        let panic = "thread 'tests::it_fails' (1258450) panicked at src/lib.rs:10:9:";
        assert_eq!(
            loci(dir.path(), test),
            [
                locus(
                    "src/lib.rs:2:9",
                    Severity::Warning,
                    "warning: unused variable: `unused`",
                    "src/lib.rs",
                    2,
                    9
                ),
                locus(
                    "src/lib.rs:10:9",
                    Severity::Error,
                    panic,
                    "src/lib.rs",
                    10,
                    9
                ),
            ]
        );
    }

    #[test]
    fn loci_of_ghc_gcc_and_clang() {
        let dir = project();
        let output = "[1 of 1] Compiling Main             ( src/Main.hs, nothing )

src/Main.hs:6:19: error: [GHC-88464] Variable not in scope: foo
  |
6 |   putStrLn (show (foo + 1))
main.cpp: In function ‘int main()’:
main.cpp:4:24: error: conversion from ‘int’ to non-scalar type ‘std::vector<int>’ requested
    4 |   std::vector<int> v = 3;
                 from main.cpp:1:
main.cpp:3:7: warning: unused variable ‘unused’ [-Wunused-variable]
main.cpp:5:10: note: here
";
        let ghc = "src/Main.hs:6:19: error: [GHC-88464] Variable not in scope: foo";
        let gcc = "main.cpp:4:24: error: conversion from ‘int’ to non-scalar type ‘std::vector<int>’ requested";
        let unused = "main.cpp:3:7: warning: unused variable ‘unused’ [-Wunused-variable]";
        assert_eq!(
            loci(dir.path(), output),
            [
                locus(
                    "src/Main.hs:6:19",
                    Severity::Error,
                    ghc,
                    "src/Main.hs",
                    6,
                    19
                ),
                locus("main.cpp:4:24", Severity::Error, gcc, "main.cpp", 4, 24),
                locus(
                    "main.cpp:1",
                    Severity::Info,
                    "from main.cpp:1:",
                    "main.cpp",
                    1,
                    1
                ),
                locus("main.cpp:3:7", Severity::Warning, unused, "main.cpp", 3, 7),
                locus(
                    "main.cpp:5:10",
                    Severity::Info,
                    "main.cpp:5:10: note: here",
                    "main.cpp",
                    5,
                    10
                ),
            ]
        );
    }

    #[test]
    fn loci_of_jvm_builds_and_stack_traces() {
        let dir = project();
        let app = dir.path().join("src/main/java/demo/App.java");
        let output = format!(
            "{app}:4: error: incompatible types: String cannot be converted to int
AppTest > adds() FAILED
    org.opentest4j.AssertionFailedError at AppTest.java:6
\tat demo.App.main(App.java:5)
\tat other.App.run(App.java:7)
e: file://{kt}:12:5 Unresolved reference: foo
[error] {scala}:3:5: not found: value x
[ERROR] {app}:[4,13] cannot find symbol
",
            app = app.display(),
            kt = dir.path().join("Main.kt").display(),
            scala = dir.path().join("App.scala").display(),
        );
        let found = loci(dir.path(), &output);
        let files: Vec<_> = found
            .iter()
            .map(|(_, severity, _, file, line, column)| (*severity, file.as_str(), *line, *column))
            .collect();
        assert_eq!(
            files,
            [
                (Severity::Error, "src/main/java/demo/App.java", 4, 1),
                (Severity::Info, "src/test/java/demo/AppTest.java", 6, 1),
                (Severity::Info, "src/main/java/demo/App.java", 5, 1),
                (Severity::Info, "src/main/java/other/App.java", 7, 1),
                (Severity::Error, "Main.kt", 12, 5),
                (Severity::Error, "App.scala", 3, 5),
                (Severity::Error, "src/main/java/demo/App.java", 4, 13),
            ]
        );
        assert_eq!(found[1].0, "AppTest.java:6");
        assert_eq!(
            found[1].2,
            "org.opentest4j.AssertionFailedError at AppTest.java:6"
        );
        assert!(found[4].0.starts_with("file://"));
    }

    #[test]
    fn loci_of_typst_python_and_tsc() {
        let dir = project();
        let output = format!(
            "error: expected expression
  ┌─ main.typ:5:12
  │
Traceback (most recent call last):
  File \"{py}\", line 4, in <module>
a.ts(1,5): error TS2322: Type 'string' is not assignable to type 'number'.
Finished at 14:03:12
",
            py = dir.path().join("t.py").display(),
        );
        let found = loci(dir.path(), &output);
        assert_eq!(
            found[0],
            locus(
                "main.typ:5:12",
                Severity::Error,
                "error: expected expression",
                "main.typ",
                5,
                12
            )
        );
        assert_eq!(
            (found[1].1, found[1].3.as_str(), found[1].4),
            (Severity::Info, "t.py", 4)
        );
        assert!(found[1].0.starts_with('"') && found[1].0.ends_with("\", line 4"));
        assert_eq!(
            (found[2].0.as_str(), found[2].1, found[2].4, found[2].5),
            ("a.ts(1,5)", Severity::Error, 1, 5)
        );
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn locus_diagnostics_carry_their_target() {
        let path = Path::new("/home/me/demo/src/lib.rs");
        let data = target_data(path, Position::new(2, 8));
        assert_eq!(
            target(&data),
            Some((path.to_path_buf(), Position::new(2, 8)))
        );
        assert_eq!(target(&json!({ "line": 2 })), None);
    }

    #[test]
    fn loci_count_chars_across_lines() {
        let dir = project();
        let mut finder = Finder::new(Resolver::new(
            dir.path().to_path_buf(),
            FilePickerConfig::default(),
        ));
        // The header line is held over to the next output.
        assert_eq!(finder.find("ünïcödé\nerror: bad\n"), []);
        let loci = finder.find("  ┌─ main.typ:5:12\n");
        assert_eq!((loci[0].start, loci[0].len), (5, 13));
        assert_eq!(loci[0].severity, Severity::Error);
    }
}
