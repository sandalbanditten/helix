//! The compilation buffer runs commands in the root of the workspace of the working directory,
//! which the whole process shares, so its tests run in a binary of their own and one after
//! another, each in a workspace of its own.

#[cfg(feature = "integration")]
mod test {
    #[allow(dead_code)]
    mod helpers;

    use std::{
        fs,
        path::PathBuf,
        sync::{Mutex, MutexGuard, PoisonError},
        time::{Duration, Instant},
    };

    use helix_core::{diagnostic::Severity, Position};
    use helix_term::application::Application;
    use helix_view::{current_ref, input::parse_macro, Document};
    use tempfile::TempDir;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
    use tokio_stream::wrappers::UnboundedReceiverStream;

    #[cfg(windows)]
    use crossterm::event::{Event, KeyEvent};
    #[cfg(not(windows))]
    use termina::event::{Event, KeyEvent};

    use self::helpers::{test_syntax_loader, AppBuilder};

    static WORKING_DIRECTORY: Mutex<()> = Mutex::new(());

    /// A temporary workspace that is the working directory while it lives.
    struct Workspace {
        dir: TempDir,
        _working_directory: MutexGuard<'static, ()>,
    }

    impl Workspace {
        /// Creates the files at `paths`, each holding `"one\ntwo\nthree\n"`.
        fn new(paths: &[&str]) -> anyhow::Result<Self> {
            let working_directory = WORKING_DIRECTORY
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let dir = tempfile::tempdir()?;
            fs::create_dir(dir.path().join(".git"))?;
            for path in paths {
                let path = dir.path().join(path);
                fs::create_dir_all(path.parent().unwrap())?;
                fs::write(&path, "one\ntwo\nthree\n")?;
            }
            helix_stdx::env::set_current_working_dir(dir.path())?;
            Ok(Self {
                dir,
                _working_directory: working_directory,
            })
        }

        fn path(&self, path: &str) -> PathBuf {
            helix_stdx::path::canonicalize(self.dir.path().join(path))
        }

        /// The editor with `open` open, and `languages` layered over the languages.
        fn session(&self, open: &str, languages: Option<&str>) -> anyhow::Result<Session> {
            let app = AppBuilder::new()
                .with_file(self.path(open), None)
                .with_lang_loader(test_syntax_loader(languages.map(ToOwned::to_owned)))
                .build()?;
            Ok(Session::new(app))
        }
    }

    /// An editor fed keys, whose event loop runs until what is asked for happened.
    struct Session {
        app: Application,
        keys: UnboundedSender<std::io::Result<Event>>,
        input: UnboundedReceiverStream<std::io::Result<Event>>,
    }

    impl Session {
        fn new(app: Application) -> Self {
            let (keys, input) = unbounded_channel();
            Self {
                app,
                keys,
                input: UnboundedReceiverStream::new(input),
            }
        }

        /// Types `keys` and runs the event loop until it idles.
        async fn keys(&mut self, keys: &str) -> anyhow::Result<()> {
            self.send(keys)?;
            self.app.event_loop_until_idle(&mut self.input).await;
            Ok(())
        }

        /// Types `keys` without running the event loop.
        fn send(&self, keys: &str) -> anyhow::Result<()> {
            for key in parse_macro(keys)? {
                self.keys.send(Ok(Event::Key(KeyEvent::from(key))))?;
            }
            Ok(())
        }

        /// Runs the event loop in slices of a millisecond until `done` holds.
        async fn pump_until(&mut self, done: impl Fn(&Application) -> bool) {
            while !done(&self.app) {
                let idle = self.app.event_loop_until_idle(&mut self.input);
                let _ = tokio::time::timeout(Duration::from_millis(1), idle).await;
            }
        }

        /// Runs the event loop until `done` holds.
        async fn until(&mut self, what: &str, done: impl Fn(&Application) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !done(&self.app) {
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                let idle = self.app.event_loop_until_idle(&mut self.input);
                let _ = tokio::time::timeout(Duration::from_millis(50), idle).await;
            }
        }

        /// Runs the event loop until the run of the compilation buffer ended.
        async fn finished(&mut self) {
            self.until("the compilation to end", |app| {
                buffer(app).is_some_and(|doc| {
                    let compilation = doc.compilation.as_ref().unwrap();
                    compilation.process.is_none()
                })
            })
            .await;
        }

        async fn quit(mut self) -> anyhow::Result<()> {
            self.keys("<esc>:qa!<ret>").await?;
            let errors = self.app.close().await;
            anyhow::ensure!(errors.is_empty(), "errors closing: {errors:?}");
            Ok(())
        }
    }

    /// The keys that run `command` from the command line, with `<` and `>` typed as such.
    fn typed(command: &str) -> String {
        let mut keys = String::from(":");
        for c in command.chars() {
            match c {
                '<' => keys.push_str("<lt>"),
                '>' => keys.push_str("<gt>"),
                c => keys.push(c),
            }
        }
        keys.push_str("<ret>");
        keys
    }

    fn buffer(app: &Application) -> Option<&Document> {
        app.editor.documents().find(|doc| doc.compilation.is_some())
    }

    fn text(app: &Application) -> String {
        buffer(app)
            .expect("no compilation buffer")
            .text()
            .to_string()
    }

    fn status(app: &Application) -> String {
        app.editor
            .get_status()
            .map(|(status, _)| status.to_string())
            .unwrap_or_default()
    }

    /// The name of the focused file and the line and column of its cursor, from 1.
    fn cursor(app: &Application) -> (String, usize, usize) {
        let (view, doc) = current_ref!(app.editor);
        let text = doc.text().slice(..);
        let pos = helix_core::coords_at_pos(text, doc.selection(view.id).primary().cursor(text));
        let name = doc.display_name().into_owned();
        (name, pos.row + 1, pos.col + 1)
    }

    /// Whether the process whose pid the file at `path` holds still runs.
    #[cfg(unix)]
    fn running(path: &std::path::Path) -> bool {
        let pid = fs::read_to_string(path).unwrap();
        let stat = fs::read_to_string(format!("/proc/{}/stat", pid.trim()));
        stat.is_ok_and(|stat| !stat.contains(") Z "))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn output_streams_between_a_header_and_a_footer() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["src/lib.rs"])?;
        let mut session = workspace.session("src/lib.rs", None)?;
        session
            .keys(":compile-any printf 'one\\r\\nt\\033[31mw\\033[0mo\\n'; exit 3<ret>")
            .await?;
        session.finished().await;
        let text = text(&session.app);
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(
            lines[0],
            "printf 'one\\r\\nt\\033[31mw\\033[0mo\\n'; exit 3"
        );
        let dir = workspace.path("").display().to_string();
        assert!(
            lines[1].starts_with(&format!("in {dir}, started ")),
            "{text}"
        );
        assert_eq!(lines[2..5], ["", "one", "two"]);
        assert!(lines[6].starts_with("Exited with code 3 at "), "{text}");
        assert_eq!(status(&session.app), "Compilation exited with code 3");
        // It covers the editor, and is not a file.
        assert_eq!(
            cursor(&session.app).0,
            format!("[compilation] {}", lines[0])
        );
        assert!(session.app.editor.tree.zoomed().is_some());
        session.keys(":w<ret>").await?;
        assert_eq!(
            status(&session.app),
            "'write': A compilation buffer cannot be written"
        );
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn loci_are_diagnostics_that_open_beside_the_output() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["src/lib.rs", "src/main.rs"])?;
        let mut session = workspace.session("src/main.rs", None)?;
        let output = "error[E0425]: cannot find value `c` in this scope\\n --> src/lib.rs:2:3\\n\
                      warning: unused\\n --> src/main.rs:3:1\\nmissing.rs:1:1: error: gone\\n";
        session
            .keys(&typed(&format!("compile-any printf '{output}'")))
            .await?;
        session.finished().await;
        let doc = buffer(&session.app).unwrap();
        let loci: Vec<_> = doc
            .diagnostics()
            .iter()
            .map(|locus| (locus.line + 1, locus.severity, locus.message.clone()))
            .collect();
        assert_eq!(
            loci,
            [
                (
                    5,
                    Some(Severity::Error),
                    "error[E0425]: cannot find value `c` in this scope".to_owned()
                ),
                (7, Some(Severity::Warning), "warning: unused".to_owned()),
            ]
        );
        assert_eq!(
            status(&session.app),
            "Compilation finished (1 error, 1 warning)"
        );

        // `]d` goes to the first, `gf` opens it beside the output, where it was run from.
        session.keys("]dgf").await?;
        assert_eq!(cursor(&session.app), ("src/lib.rs".to_owned(), 2, 3));
        assert!(session.app.editor.tree.zoomed().is_none());
        assert_eq!(session.app.editor.tree.views().count(), 2);
        // `]q` goes on from there, `[q` back, from the file.
        session.keys("]q").await?;
        assert_eq!(cursor(&session.app), ("src/main.rs".to_owned(), 3, 1));
        session.keys("]q").await?;
        assert_eq!(status(&session.app), "No next locus");
        session.keys("[q").await?;
        assert_eq!(cursor(&session.app), ("src/lib.rs".to_owned(), 2, 3));
        // The output's cursor shows the locus.
        let (view, _) = session
            .app
            .editor
            .tree
            .views()
            .find(|(view, _)| view.doc == buffer(&session.app).unwrap().id())
            .unwrap();
        let doc = buffer(&session.app).unwrap();
        let text = doc.text().slice(..);
        let at = helix_core::coords_at_pos(text, doc.selection(view.id).primary().from());
        assert_eq!(at, Position::new(4, 5));
        // Running again from the file covers the editor again.
        session.keys(":compile-any true<ret>").await?;
        assert!(session.app.editor.tree.zoomed().is_some());
        session.keys("gd").await?;
        assert_eq!(status(&session.app), "No locus on this line");
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bare_file_names_are_found_by_name() -> anyhow::Result<()> {
        let workspace = Workspace::new(&[
            "src/test/java/demo/AppTest.java",
            "src/main/java/demo/App.java",
            "src/main/java/other/App.java",
        ])?;
        let mut session = workspace.session("src/main/java/demo/App.java", None)?;
        let output = "AppTest > adds() FAILED\\n    AssertionFailedError at AppTest.java:2\\n\
                      \\tat other.App.run(App.java:3)\\n";
        session
            .keys(&typed(&format!("compile-any printf '{output}'")))
            .await?;
        session.finished().await;
        session.keys("]dgf").await?;
        assert_eq!(
            cursor(&session.app),
            ("src/test/java/demo/AppTest.java".to_owned(), 2, 1)
        );
        session.keys("]q").await?;
        assert_eq!(
            cursor(&session.app),
            ("src/main/java/other/App.java".to_owned(), 3, 1)
        );
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn configured_commands_run_in_the_language_root() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["Cargo.toml", "member/Cargo.toml", "member/src/lib.rs"])?;
        let languages = r#"
            [[language]]
            name = "rust"
            compile-command = "pwd; echo %{buffer_name}"
            test-command = "echo testing"
        "#;
        let mut session = workspace.session("member/src/lib.rs", Some(languages))?;
        session.keys(":compile<ret>").await?;
        session.finished().await;
        let output = text(&session.app);
        let root = workspace.path("").display().to_string();
        assert!(
            output.contains(&format!("\n\n{root}\nmember/src/lib.rs\n")),
            "{output}"
        );

        // From the output, the commands are those of the run's language, and `:reload` runs
        // the last again.
        session.keys(":compile-test<ret>").await?;
        session.finished().await;
        assert!(text(&session.app).starts_with("echo testing\n"));
        session.keys(":reload<ret>").await?;
        session.finished().await;
        assert!(text(&session.app).contains("\n\ntesting\n"));
        session.keys(":new<ret>:compile<ret>").await?;
        assert_eq!(
            status(&session.app),
            "'compile': No compile-command without a language"
        );
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsaved_buffers_hold_back_runs_but_forced_ones() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["src/lib.rs"])?;
        let mut session = workspace.session("src/lib.rs", None)?;
        session.keys("ix<esc>:compile-any true<ret>").await?;
        assert_eq!(
            status(&session.app),
            r#"'compile-any': 1 unsaved buffer: ["src/lib.rs"]; :compile-any! true runs anyway"#
        );
        assert!(buffer(&session.app).is_none());
        session.keys(":compile-any! echo forced<ret>").await?;
        session.finished().await;
        assert!(text(&session.app).contains("\n\nforced\n"));
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_cursor_on_the_last_line_follows_the_output() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["src/lib.rs"])?;
        let mut session = workspace.session("src/lib.rs", None)?;
        session
            .keys(":compile-any sleep 0.3; echo one; sleep 0.3; echo two<ret>ge")
            .await?;
        session.finished().await;
        let lines = text(&session.app).lines().count();
        assert_eq!(cursor(&session.app).1, lines);
        session
            .keys(":compile-any sleep 0.3; echo one; sleep 0.3; echo two<ret>")
            .await?;
        session.finished().await;
        assert_eq!(cursor(&session.app).1, 1);
        session.quit().await
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn runs_stop_when_killed_rerun_or_closed() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["src/lib.rs"])?;
        let mut session = workspace.session("src/lib.rs", None)?;
        let pids: Vec<_> = (0..3).map(|i| workspace.path(&format!("pid{i}"))).collect();
        let run = |pid: &PathBuf| {
            typed(&format!(
                "compile-any sh -c 'echo $$ > {}; exec sleep 30'",
                pid.display()
            ))
        };
        let started = |pid: PathBuf| move |_: &Application| pid.exists();

        // `:compile-kill` stops the run and keeps the output.
        session.keys(&run(&pids[0])).await?;
        session
            .until("the first run", started(pids[0].clone()))
            .await;
        assert!(running(&pids[0]));
        session.keys(":compile-kill<ret>").await?;
        session.finished().await;
        assert!(!running(&pids[0]));
        assert!(text(&session.app)
            .lines()
            .last()
            .unwrap()
            .starts_with("Killed at "));
        assert_eq!(status(&session.app), "Compilation killed");

        // Running again stops the run before.
        session.keys(&run(&pids[1])).await?;
        session
            .until("the second run", started(pids[1].clone()))
            .await;
        session.keys(":compile-any true<ret>").await?;
        session.finished().await;
        session
            .until("the second run to stop", |_| !running(&pids[1]))
            .await;

        // Closing the buffer stops it too.
        session.keys(&run(&pids[2])).await?;
        session
            .until("the third run", started(pids[2].clone()))
            .await;
        session.keys(":bc<ret>").await?;
        assert!(buffer(&session.app).is_none());
        session
            .until("the third run to stop", |_| !running(&pids[2]))
            .await;
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unfinished_lines_show_until_they_end() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["src/lib.rs"])?;
        let mut session = workspace.session("src/lib.rs", None)?;
        let tail = |app: &Application| {
            let text = text(app);
            text.lines().last().unwrap_or_default().to_owned()
        };
        session
            .keys(&typed(
                "compile-any printf 'Password: '; sleep 1; printf 'one'; sleep 1; printf '\\rdone\\n'",
            ))
            .await?;
        session
            .until("the prompt", |app| tail(app) == "Password: ")
            .await;
        session
            .until("the progress", |app| tail(app) == "Password: one")
            .await;
        session.finished().await;
        let text = text(&session.app);
        let lines: Vec<_> = text.lines().skip(3).collect();
        // The carriage return lets the end of the line overwrite its start.
        assert_eq!(lines[0], "doneword: one", "{text}");
        assert!(lines[2].starts_with("Finished at "), "{text}");
        session.quit().await
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn what_the_command_leaves_running_is_stopped() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["src/lib.rs"])?;
        let mut session = workspace.session("src/lib.rs", None)?;
        let pid = workspace.path("pid");
        // Left behind holding the output, and ignoring terms.
        let start = Instant::now();
        session
            .keys(&typed(&format!(
                "compile-any sleep 30 & echo $! > {}; echo left",
                pid.display()
            )))
            .await?;
        session.finished().await;
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(!running(&pid));
        assert!(text(&session.app).contains("\nleft\n\nFinished at "));

        let start = Instant::now();
        session
            .keys(&typed(&format!(
                "compile-any trap '' TERM; sleep 30 & echo $! > {}; wait",
                pid.display()
            )))
            .await?;
        session.until("the run", |_| pid.exists()).await;
        session.keys(":compile-kill<ret>").await?;
        session.finished().await;
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(!running(&pid));
        assert!(text(&session.app)
            .lines()
            .last()
            .unwrap()
            .starts_with("Killed at "));
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn columns_follow_the_compiler_on_tabbed_lines() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["tab.c"])?;
        fs::write(workspace.path("tab.c"), "int main() {\n\treturn y;\n}\n")?;
        let mut session = workspace.session("tab.c", None)?;
        // gcc counts display columns, clang chars; both point at `y`.
        for column in [16, 9] {
            let output = format!(
                "tab.c:2:{column}: error: y\\n    2 |         return y;\\n      |                ^\\n"
            );
            session
                .keys(&typed(&format!("compile-any printf '{output}'")))
                .await?;
            session.finished().await;
            session.keys("]dgf").await?;
            assert_eq!(cursor(&session.app), ("tab.c".to_owned(), 2, 9), "{column}");
        }
        session.quit().await
    }

    /// Times how fast output arrives in the buffer, and how fast keys are handled meanwhile, in
    /// release: `cargo test --release --features integration --test compilation measure -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement, not a check"]
    async fn measure() -> anyhow::Result<()> {
        let workspace = Workspace::new(&["src/lib.rs"])?;
        let mut session = workspace.session("src/lib.rs", None)?;
        for (what, command) in [
            ("1M lines", "seq 1 1000000"),
            (
                "100k loci",
                "seq 1 100000 | sed 's/.*/src\\/lib.rs:&:1: error: bad/'",
            ),
        ] {
            let start = Instant::now();
            session
                .keys(&typed(&format!("compile-any {command}")))
                .await?;
            session.finished().await;
            let loci = buffer(&session.app).unwrap().diagnostics().len();
            println!(
                "{what}: {:?} until the footer, {loci} loci",
                start.elapsed()
            );
        }

        // Starting a run over the output of one with many loci.
        let run = |app: &Application| buffer(app).unwrap().compilation.as_ref().unwrap().run;
        let last = run(&session.app);
        let start = Instant::now();
        session.send(&typed("compile-any true"))?;
        session.pump_until(|app| run(app) != last).await;
        println!("a run started over 100k loci: {:?}", start.elapsed());
        session.finished().await;

        // The first key in a finished buffer.
        session.keys(&typed("compile-any seq 1 200000")).await?;
        session.finished().await;
        let line = cursor(&session.app).1;
        let start = Instant::now();
        session.send("j")?;
        session.pump_until(|app| cursor(app).1 != line).await;
        println!("first key in a finished buffer: {:?}", start.elapsed());

        // Keys typed while output streams in: how long until the cursor moved. The event loop
        // runs in slices, as it never idles while output arrives.
        let last = run(&session.app);
        session.send(&typed("compile-any seq 1 20000000"))?;
        let streamed = |app: &Application, lines: usize| {
            let doc = buffer(app).unwrap();
            let compilation = doc.compilation.as_ref().unwrap();
            compilation.process.is_none() || doc.text().len_lines() > lines
        };
        session
            .pump_until(|app| run(app) != last && streamed(app, 100_000))
            .await;
        let mut latencies = Vec::new();
        for _ in 0..20 {
            let line = cursor(&session.app).1;
            let start = Instant::now();
            session.send("j")?;
            session.pump_until(|app| cursor(app).1 != line).await;
            latencies.push(start.elapsed());
            // Some output arrives between the keys.
            let lines = buffer(&session.app).unwrap().text().len_lines();
            session
                .pump_until(|app| streamed(app, lines + 100_000))
                .await;
        }
        let running = buffer(&session.app).unwrap().compilation.as_ref().unwrap();
        assert!(
            running.process.is_some(),
            "the output ended before the keys"
        );
        println!("  each: {latencies:?}");
        latencies.sort();
        println!(
            "keys while 20M lines arrive: median {:?}, slowest {:?}",
            latencies[latencies.len() / 2],
            latencies.last().unwrap()
        );
        session.keys(":compile-kill<ret>").await?;
        session.finished().await;
        session.quit().await
    }
}
